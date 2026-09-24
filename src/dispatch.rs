//! Dispatch and review: a worktree per task, agent runs within the batch
//! budget, verification by `pma` itself, and the review actions.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use crate::agent;
use crate::class::Class;
use crate::complexity::{self, Features};
use crate::config::Config;
use crate::rank::Quadrant;
use crate::route::{Policy, Subject};
use crate::scan;
use crate::store::{Attempt, Result, Run, RunState, Store};
use crate::todo::{self, Priority};
use crate::worker::Worker;

/// A task chosen for dispatch.
#[derive(Debug, Clone)]
pub struct Pick {
    /// The workflow unit this dispatch serves, where one does. A route
    /// matches on the node (W13) and a report groups by it.
    pub workflow: Option<UnitRef>,
    pub project: String,
    pub repo: PathBuf,
    /// The item's key, or a signal: `ci` or `deps`.
    pub key: String,
    pub text: String,
    pub gh: Option<i64>,
    /// The project's tier, recorded on the run because tiers change.
    pub tier: Option<u8>,
    /// Stated by a campaign, which has no item to read tags from.
    pub class: Option<Class>,
    /// Extra prompt lines a campaign carries.
    pub details: Option<String>,
    pub quadrant: Option<String>,
}

/// Which workflow unit a dispatch serves.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UnitRef {
    pub instance: i64,
    /// The qualified node name.
    pub node: String,
    pub unit: String,
    pub lap: i64,
}

/// An agent and a model named on the command line. Both override the
/// configuration and an applied route: a flag is the most recent statement of
/// intent there is, and it covers the whole invocation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overrides {
    pub agent: Option<String>,
    pub model: Option<String>,
    /// A preset named on the command line. Its fields sit below the flags and
    /// above an applied route, because naming one is a statement of intent
    /// about this invocation and a route is policy about the work.
    pub preset: Option<crate::store::Preset>,
}

/// What a target names after the colon.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// No colon: the project's open tasks, chosen from a list.
    Project,
    /// A TODO.md line from the last scan.
    Line(i64),
    /// `ci` or `deps`.
    Signal(String),
    /// Every open item under that heading.
    Priority(Priority),
    /// Every task the last scan placed in that quadrant.
    Quadrant(Quadrant),
}

impl Target {
    /// Whether it names at most one task, so a task it cannot take is an
    /// error rather than something to pass over.
    pub fn is_one(&self) -> bool {
        matches!(self, Target::Line(_) | Target::Signal(_))
    }
}

/// Splits `project:what`, or reads a bare project name.
pub fn target(s: &str) -> Result<(&str, Target)> {
    let Some((project, what)) = s.rsplit_once(':') else {
        return match s.is_empty() {
            true => Err("empty target; name a project such as `cynn`".into()),
            false => Ok((s, Target::Project)),
        };
    };
    if project.is_empty() {
        return Err(format!("`{s}`: no project before the colon").into());
    }
    let target = match what {
        "ci" | "deps" => Target::Signal(what.to_string()),
        _ => {
            if let Ok(line) = what.parse::<i64>() {
                Target::Line(line)
            } else if let Some(p) = Priority::parse(what) {
                Target::Priority(p)
            } else if let Some(q) = Quadrant::parse(what) {
                Target::Quadrant(q)
            } else {
                return Err(format!(
                    "`{s}`: after the colon expected a TODO.md line, ci, deps, \
                     critical, high, medium, low, or q1 to q4"
                )
                .into());
            }
        }
    };
    Ok((project, target))
}

/// Whether a task key names a signal rather than a TODO.md item.
pub fn is_signal(key: &str) -> bool {
    matches!(key, "ci" | "deps")
}

/// A task with no line in any `TODO.md`: a signal, or a campaign applying one
/// definition across repositories. There is nothing to check on origin before
/// dispatch and nothing to tick when it is published.
pub fn without_item(key: &str) -> bool {
    is_signal(key) || key.starts_with("campaign:") || key.starts_with("workflow:")
}

/// Consumed attempts after which a task revision is not dispatched again.
/// Two independent negative signals: an agent that cannot pass the check, or
/// one that passed it and produced work a reviewer refused.
pub const ATTEMPT_LIMIT: i64 = 2;

/// A task's identity for the attempt counter. The normalised text, so a
/// reworded specification starts fresh and a `ci` incident is identified by
/// the workflows it names rather than by the word `ci`, which recurs.
pub fn revision(text: &str) -> String {
    todo::normal_text(text)
}

/// What a run's attempts count against. A workflow unit counts against its
/// lineage, `workflow:<instance>:<root>`, so a retry and a lap draw from one
/// budget (W23); anything else counts against its task text.
pub fn attempt_key(run: &Run) -> String {
    match run.task_key.starts_with("workflow:") {
        true => run.task_key.clone(),
        false => revision(&run.text),
    }
}

/// Whether this attempt counts against the limit. A run that never reached
/// the agent, or whose agent could not start or was killed at the timeout,
/// says nothing about the task's suitability.
fn consumes(run: &Run) -> bool {
    run.state == RunState::Ready && run.verify_ok == Some(false)
}

/// Lines of failed CI log carried in a fix-CI prompt.
const CI_LOG_LINES: usize = 200;

/// Runs `git -C dir args`, returning trimmed stdout or an error naming the
/// command and its stderr. With no filesystem monitor: one names a program,
/// and these calls carry the user's credentials.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `git` in the run's worktree, once its `.git` is still the one `git
/// worktree add` wrote. An agent that replaced it would hand these calls,
/// which carry the user's credentials, a config and hooks of its own.
pub fn wt_git(run: &Run, args: &[&str]) -> Result<String> {
    own_gitdir(run)?;
    git(&run.worktree, args)
}

/// Refuses a worktree whose `.git` is not a file naming this worktree's
/// entry under the clone's `worktrees/`.
pub fn own_gitdir(run: &Run) -> Result<()> {
    let dot_git = run.worktree.join(".git");
    let replaced = || -> Box<dyn std::error::Error> {
        format!(
            "{} is not the file `git worktree add` wrote; the run's worktree was \
             tampered with, so pma runs no git in it. Reject the run.",
            dot_git.display()
        )
        .into()
    };
    let is_file = std::fs::symlink_metadata(&dot_git).is_ok_and(|m| m.is_file());
    let named = is_file
        .then(|| std::fs::read_to_string(&dot_git).ok())
        .flatten()
        .and_then(|t| Some(PathBuf::from(t.strip_prefix("gitdir: ")?.trim_end())))
        .and_then(|p| p.canonicalize().ok())
        .ok_or_else(replaced)?;
    let common = git(
        &run.repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let expected = Path::new(&common)
        .canonicalize()
        .map_err(|e| format!("{common}: {e}"))?
        .join("worktrees");
    // The entry names its worktree back, so `.git` cannot borrow another's.
    let back = std::fs::read_to_string(named.join("gitdir"))
        .ok()
        .and_then(|t| Path::new(t.trim_end()).canonicalize().ok());
    if named.parent() != Some(expected.as_path()) || back != dot_git.canonicalize().ok() {
        return Err(replaced());
    }
    Ok(())
}

/// What makes git run a program in the worktree, or push somewhere else,
/// that the diff does not show: each hook by content and executable bit, and
/// each repository setting that names a command or rewrites a URL, by a hash
/// of its value, since a URL may carry a token. Publishing compares it with the one
/// taken at dispatch. Hooks that run tracked files, such as husky's scripts or
/// a `.pre-commit-config.yaml`, run what the reviewer approved in the diff.
/// Global and system settings are the user's, so they are left out.
pub fn git_snapshot(run: &Run) -> Result<String> {
    use std::os::unix::fs::PermissionsExt;

    let mut lines = Vec::new();
    let hooks = wt_git(
        run,
        &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
    )?;
    let mut files: Vec<(String, PathBuf, bool)> = std::fs::read_dir(&hooks)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let meta = std::fs::metadata(e.path()).ok()?;
            (meta.is_file() && !name.ends_with(".sample"))
                .then(|| (name, e.path(), meta.permissions().mode() & 0o111 != 0))
        })
        .collect();
    files.sort();
    if !files.is_empty() {
        let mut args = vec!["hash-object", "--no-filters", "--"];
        args.extend(files.iter().filter_map(|(_, p, _)| p.to_str()));
        let hashes = git(&run.repo, &args)?;
        for ((name, _, executable), hash) in files.iter().zip(hashes.lines()) {
            let x = if *executable {
                "executable"
            } else {
                "not executable"
            };
            lines.push(format!("hook {name} {hash} {x}"));
        }
    }
    // NUL-separated scope and entry, each entry `key\nvalue`.
    let listing = wt_git(run, &["config", "--list", "--show-scope", "-z"])?;
    let mut settings: Vec<(String, String)> = Vec::new();
    let mut fields = listing.split('\0');
    while let (Some(scope), Some(entry)) = (fields.next(), fields.next()) {
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        if matches!(scope, "local" | "worktree") && runs_or_redirects(key) {
            settings.push((key.to_string(), value.to_string()));
        }
    }
    settings.sort();
    for (key, value) in settings {
        lines.push(format!("config {key} {}", hash_text(&run.repo, &value)?));
    }
    Ok(lines.join("\n"))
}

/// A setting that names a program for git to run, or changes where a push
/// goes. Keys arrive with section and name lowercased.
fn runs_or_redirects(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "core.hookspath",
        "core.fsmonitor",
        "core.sshcommand",
        "core.editor",
        "core.pager",
        "core.askpass",
        "core.gitproxy",
        "diff.external",
        "sequence.editor",
        "remote.origin.url",
        "remote.origin.pushurl",
        "remote.pushdefault",
    ]
    .contains(&key.as_str())
        || [
            "filter.",
            "credential.",
            "url.",
            "include.",
            "includeif.",
            "gpg.",
        ]
        .iter()
        .any(|p| key.starts_with(p))
        || [".textconv", ".command", ".driver"]
            .iter()
            .any(|s| key.ends_with(s))
}

/// Git's object id for `text`, so a value is compared without being stored.
fn hash_text(dir: &Path, text: &str) -> Result<String> {
    use std::io::Write;

    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["hash-object", "--no-filters", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("git: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("git hash-object: no stdin")?
        .write_all(text.as_bytes())
        .map_err(|e| format!("git hash-object: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git hash-object: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git hash-object: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Lowercase words of the task text joined by `-`, at most 40 bytes.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for word in text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if out.len() + word.len() + 1 > 40 {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&word.to_ascii_lowercase());
    }
    if out.is_empty() { "task".into() } else { out }
}

/// What `prepare` made of a pick.
/// How a run refused before it started says so, in `Run::error`. A run that
/// never reached the agent is not a run that failed: the caller leaves its
/// work where it was rather than recording a result for it.
pub const NOT_STARTED: &str = "not started: batch budget";

#[derive(Debug)]
pub enum Prepared {
    Queued(Box<Run>),
    /// The task is not dispatchable as the last scan described it. Other
    /// tasks of the project may still be.
    Refused(String),
}

/// Checks `pick` against the remote default branch, then creates its worktree
/// and branch and records a queued run. An error concerns the whole project,
/// such as a failed fetch.
pub fn prepare(
    store: &Store,
    home: &Path,
    cfg: &Config,
    over: &Overrides,
    pick: &Pick,
) -> Result<Prepared> {
    let repo = &pick.repo;
    git(repo, &["fetch", "--quiet", "origin"])?;
    let default_branch = scan::default_branch(repo).ok_or(scan::DEFAULT_BRANCH_UNKNOWN)?;
    let base = git(
        repo,
        &[
            "rev-parse",
            &format!("refs/remotes/origin/{default_branch}"),
        ],
    )?;
    let refuse = |why: String| Ok(Prepared::Refused(format!("{}: {why}", pick.project)));

    let (details, tags) = match pick.key.as_str() {
        "ci" => {
            let Some(scan::Ci::Failing(workflows)) = store.project(&pick.project)?.map(|p| p.ci)
            else {
                return refuse("CI was not failing at the last scan".into());
            };
            match failed_ci_logs(repo, &default_branch, &workflows)? {
                Ok(logs) => (logs, Vec::new()),
                Err(why) => return refuse(why),
            }
        }
        "deps" => {
            let detail = store
                .project(&pick.project)?
                .map(|p| p.deps_detail)
                .unwrap_or_default();
            (
                format!(
                    "Outdated dependencies at the last `pma scan --deps`:\n\n```\n{detail}\n```\n\n\
                     Update them within the project's version constraints first. Change a \
                     constraint only where the tests still pass, and list any dependency you \
                     left behind, with the reason.\n"
                ),
                Vec::new(),
            )
        }
        key if without_item(key) && !is_signal(key) => {
            (pick.details.clone().unwrap_or_default(), Vec::new())
        }
        _ => {
            let file = git(repo, &["show", &format!("{base}:TODO.md")]).ok();
            match on_origin(file.as_deref(), &pick.key, &pick.text) {
                OnOrigin::Open(description, tags) => (description, tags),
                OnOrigin::Done => {
                    return refuse(format!(
                        "`{}` is already done on origin/{default_branch}; pull the clone, then `pma scan`",
                        pick.text
                    ));
                }
                OnOrigin::Absent => {
                    return refuse(format!(
                        "`{}` is not in TODO.md on origin/{default_branch}; commit and push it first",
                        pick.text
                    ));
                }
            }
        }
    };

    // Before the worktree, so a task the user marked leaves nothing behind.
    // A campaign states its class: it has no item to read tags from.
    let class = pick.class.unwrap_or_else(|| Class::of(&pick.key, &tags));
    if !class.dispatchable() {
        return refuse(format!(
            "`{}` is class {} and is not dispatched; remove `#manual` to change that",
            pick.text,
            class.name()
        ));
    }

    let parent = home.join("worktrees").join(&pick.project);
    let stem = slug(&pick.text);
    let (slug, worktree) = (1..)
        .map(|n| match n {
            1 => stem.clone(),
            n => format!("{stem}-{n}"),
        })
        .map(|s| (s.clone(), parent.join(&s)))
        .find(|(s, path)| {
            // A remote branch is left by an earlier pull request; pushing
            // over it would be rejected.
            !path.exists()
                && [
                    format!("refs/heads/pma/{s}"),
                    format!("refs/remotes/origin/pma/{s}"),
                ]
                .iter()
                .all(|r| git(repo, &["rev-parse", "--verify", "--quiet", r]).is_err())
        })
        .expect("an unused slug exists");
    let branch = format!("pma/{slug}");
    std::fs::create_dir_all(&parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    git(
        repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            &branch,
            path_arg(&worktree)?,
            &base,
        ],
    )?;
    // Anything short of a queued run releases the worktree and its branch
    // here: no run row exists to release them later.
    let prepared = queue(
        store,
        home,
        cfg,
        over,
        pick,
        &worktree,
        &branch,
        class,
        default_branch,
        base,
        details,
    );
    if !matches!(prepared, Ok(Prepared::Queued(_))) {
        let _ = remove_worktree(repo, &worktree, &branch);
    }
    prepared
}

/// The part of `prepare` that runs once the worktree exists.
#[allow(clippy::too_many_arguments)]
fn queue(
    store: &Store,
    home: &Path,
    cfg: &Config,
    over: &Overrides,
    pick: &Pick,
    worktree: &Path,
    branch: &str,
    class: Class,
    default_branch: String,
    base: String,
    details: String,
) -> Result<Prepared> {
    let repo = &pick.repo;
    let refuse = |why: String| Ok(Prepared::Refused(format!("{}: {why}", pick.project)));
    // Before base verify, which runs the project's code and could install a
    // hook of its own.
    let git_snapshot = git_snapshot(&Run {
        repo: repo.clone(),
        worktree: worktree.to_path_buf(),
        ..Run::default()
    })?;
    let verify = verify_command(cfg, &pick.project, worktree);
    // The worktree is a clean checkout of the base, so this is the base
    // tree. One verify at the head alone cannot tell a regression from a
    // repository that was already failing.
    let (verify_base_ok, verify_base_seconds) = match &verify {
        Some(v) => base_verify(store, home, cfg, &pick.project, &base, v, worktree)?,
        None => (None, None),
    };
    let mut features = Features::of(&pick.text, &details);
    features.repo_files = tracked_files(worktree);
    features.verify_seconds = verify_base_seconds.filter(|_| verify.is_some());
    (features.prior_decided, features.prior_accepted) = store.decided_runs(&pick.project)?;
    let complexity = complexity::estimate(class, &features);

    // A policy applies only once someone activates it; in shadow it is
    // computed and recorded but the settings still decide.
    let subject = Subject {
        class,
        complexity,
        tier: pick.tier,
        // A task dispatched on its own names no node, which is what a route
        // stating no node condition serves; a workflow unit names its node.
        node: pick.workflow.as_ref().map(|u| u.node.as_str()),
        lap: pick.workflow.as_ref().map_or(0, |u| u.lap),
    };
    let active = store.active_route()?;
    let mut routed = (None, None, None, None, class.scope());
    if let Some(rev) = &active {
        let policy = rev.policy()?;
        let Some(route) = policy.route(&subject) else {
            return refuse(format!(
                "`{}` matches no route in policy revision {}; add one or widen the last",
                pick.text, rev.revision
            ));
        };
        let scope = route.scope.clone().unwrap_or_else(|| class.scope());
        routed = (
            Some(rev.revision),
            Some(route.name.clone()),
            Some(route.approval),
            (!rev.shadow).then(|| (route.agent.clone(), route.model.clone())),
            scope,
        );
    }
    let (route_revision, route_name, approval, applied, scope) = routed;
    let chosen = choose_worker(store, cfg, over, applied)?;
    let (agent, model) = (chosen.agent.clone(), chosen.model.clone());

    let mut run = Run {
        id: 0,
        project: pick.project.clone(),
        task_key: pick.key.clone(),
        text: pick.text.clone(),
        gh: pick.gh,
        quadrant: pick.quadrant.clone(),
        agent,
        repo: repo.clone(),
        branch: branch.to_string(),
        worktree: worktree.to_path_buf(),
        default_branch,
        base,
        prompt: String::new(),
        state: RunState::Queued,
        feedback: None,
        seconds: None,
        cost_usd: None,
        summary: None,
        commits: None,
        diffstat: None,
        verify: verify.clone(),
        verify_ok: None,
        error: None,
        outcome: None,
        dispatched_at: None,
        ready_at: None,
        decided_at: None,
        published_at: None,
        review_seconds: None,
        class: Some(class),
        scope,
        tier: pick.tier,
        description: (!details.trim().is_empty()).then(|| details.clone()),
        agent_budget: Some(cfg.agent_budget),
        timeout_minutes: Some(cfg.timeout),
        verify_base_ok,
        verify_base_seconds,
        changed_paths: None,
        scope_error: None,
        model,
        complexity: Some(complexity),
        features: Some(features),
        estimator: Some(complexity::ESTIMATOR.into()),
        route_revision,
        route: route_name,
        approval,
        approved_tree: None,
        approved_head: None,
        approved_by: None,
        // Set by the workflow pass; a task dispatched on its own has none.
        workflow_instance: pick.workflow.as_ref().map(|u| u.instance),
        node: pick.workflow.as_ref().map(|u| u.node.clone()),
        unit: pick.workflow.as_ref().map(|u| u.unit.clone()),
        lap: pick.workflow.as_ref().map_or(0, |u| u.lap),
        preset: chosen.preset.clone(),
        extra_args: chosen.args.clone(),
        git_snapshot: Some(git_snapshot),
    };
    run.prompt = prompt(&run, &details, verify.as_deref());
    store.insert_run(&mut run)?;
    Ok(Prepared::Queued(Box::new(run)))
}

fn path_arg(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()).into())
}

/// A task's item in the remote `TODO.md`.
#[derive(Debug, PartialEq)]
enum OnOrigin {
    /// Open, with its description lines and its tags.
    Open(String, Vec<String>),
    Done,
    /// Not in the file, or no file.
    Absent,
}

fn on_origin(file: Option<&str>, key: &str, text: &str) -> OnOrigin {
    let Some(file) = file else {
        return OnOrigin::Absent;
    };
    let items = todo::parse(file).items;
    let mut matching = items.iter().filter(|i| i.is_task(key, text));
    match matching.clone().find(|i| !i.done) {
        Some(item) => OnOrigin::Open(
            item.description
                .iter()
                .map(|l| format!("{}\n", l.trim_start()))
                .collect(),
            item.tags.clone(),
        ),
        None if matching.next().is_some() => OnOrigin::Done,
        None => OnOrigin::Absent,
    }
}

/// The failed-job log of each workflow's latest decisive run, the run scan
/// judged failing. The inner error refuses the task: a workflow passes now.
fn failed_ci_logs(
    repo: &Path,
    branch: &str,
    workflows: &[String],
) -> Result<std::result::Result<String, String>> {
    use crate::sync::gh;
    let url = git(repo, &["remote", "get-url", "origin"])?;
    let slug = scan::github_slug(&url).ok_or("origin is not on GitHub")?;
    let mut logs = String::new();
    for workflow in workflows {
        let runs = gh(&[
            "run",
            "list",
            "-R",
            &slug,
            "--branch",
            branch,
            "--workflow",
            workflow,
            "--limit",
            "20",
            "--json",
            "databaseId,status,conclusion",
        ])?;
        let Some(id) = scan::latest_failed_run(&runs)? else {
            return Ok(Err(format!(
                "`{workflow}` no longer fails on {branch}; run `pma scan`"
            )));
        };
        // A startup failure has no job log; the agent still gets the run id.
        let log = gh(&["run", "view", &id.to_string(), "-R", &slug, "--log-failed"])
            .map(|log| agent::tail(&log, CI_LOG_LINES))
            .unwrap_or_else(|e| format!("(no failed-job log: {e})"));
        logs.push_str(&format!(
            "Workflow `{workflow}`, run {id}, last {CI_LOG_LINES} lines of `gh run view --log-failed`:\n\n```\n{log}\n```\n\n"
        ));
    }
    Ok(Ok(logs))
}

fn prompt(run: &Run, details: &str, verify: Option<&str>) -> String {
    let mut p = format!(
        "You are working on the project `{}`, in a git worktree of it.\n\nTask: {}\n",
        run.project, run.text
    );
    if !details.trim().is_empty() {
        p.push('\n');
        p.push_str(details);
    }
    p.push_str(
        "\nRules:\n\
         - Leave your changes uncommitted. Do not commit, push, or switch branches.\n\
         - Do not edit TODO.md. pma marks the task done when the change is published.\n",
    );
    if let Some(v) = verify {
        p.push_str(&format!(
            "- Run `{v}` to check your change. pma runs it again after you finish.\n"
        ));
    }
    p.push_str("\nEnd with a short summary of what you changed and what is left undone.\n");
    p
}

/// What a dispatch settled on: a worker, a model, the arguments that configure
/// it, and the preset those came from.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Chosen {
    pub agent: String,
    pub model: Option<String>,
    pub args: Vec<String>,
    pub preset: Option<String>,
}

/// Which worker runs, at which model, with which configuration. Highest first:
/// the `-a` and `-m` flags, a preset named on the command line, an applied
/// route, then the default preset. A model named nowhere is a model this
/// dispatch does not state: the worker is run without one and uses its own,
/// which `runs.model` then records as null.
pub fn choose_worker(
    store: &Store,
    cfg: &Config,
    over: &Overrides,
    applied: Option<(Option<String>, Option<String>)>,
) -> Result<Chosen> {
    let (route_agent, route_model) = applied.unwrap_or((None, None));
    let default = match (&over.preset, &cfg.preset) {
        (Some(_), _) => None,
        (None, Some(name)) => Some(store.preset(name)?),
        (None, None) => None,
    };
    let named = over.preset.clone().or(default);
    let agent = over
        .agent
        .clone()
        .or_else(|| named.as_ref().map(|p| p.agent.clone()))
        .or(route_agent)
        .unwrap_or_else(|| cfg.agent.clone());
    let model = over
        .model
        .clone()
        .or_else(|| named.as_ref().and_then(|p| p.model.clone()))
        .or(route_model);
    // Arguments come with the preset that named them or not at all: a route
    // names a worker and a model, never a flag.
    let (args, preset) = match &named {
        Some(p) if p.agent == agent => (p.args.clone(), Some(p.name.clone())),
        _ => (Vec::new(), None),
    };
    Ok(Chosen {
        agent,
        model,
        args,
        preset,
    })
}

/// The project's `verify` setting, else a command detected in the worktree.
/// `none` disables verification.
pub fn verify_command(cfg: &Config, project: &str, worktree: &Path) -> Option<String> {
    match cfg.project(project).verify {
        Some(v) if v == "none" => None,
        Some(v) => Some(v),
        None => agent::detect_verify(worktree),
    }
}

/// How much sooner a worker that bounds itself is told to stop. A worker given
/// the same deadline as this process is killed by it first, and one that runs a
/// container then leaves the container for its own sweep to find.
pub const INNER_GRACE: Duration = Duration::from_secs(30);

/// Runs queued runs, at most `max_parallel` at once, starting a run only while
/// the batch stays within `batch_budget`. `done` sees each run as it finishes.
pub fn execute(
    store: &Store,
    home: &Path,
    cfg: &Config,
    over: &Overrides,
    runs: Vec<Run>,
    mut done: impl FnMut(&Run),
) -> Result<Vec<Run>> {
    let workers = store.agents()?;
    // Escalation follows the policy only when it is applied. A shadow
    // revision is recorded and not acted on, here as everywhere. A model
    // named on the command line also holds: escalation picks a stronger
    // model, and the caller has just picked one.
    let policy = match store.active_route()? {
        Some(rev) if !rev.shadow && over.model.is_none() => Some(rev.policy()?),
        _ => None,
    };
    struct Queue {
        waiting: VecDeque<Run>,
        spent: f64,
        running: usize,
    }
    let queue = Mutex::new(Queue {
        waiting: runs.into(),
        spent: 0.0,
        running: 0,
    });
    let threads = (cfg.max_parallel as usize).max(1);
    // An attempt rides with the run it belongs to: only this thread has the
    // store, and a row appended after the run's update would be lost on a
    // save error.
    let (tx, rx) = mpsc::channel::<(Run, Vec<Attempt>)>();
    let mut finished = Vec::new();
    let mut save_error = None;

    std::thread::scope(|s| {
        for _ in 0..threads {
            let tx = tx.clone();
            let queue = &queue;
            let workers = &workers;
            let policy = policy.as_ref();
            s.spawn(move || {
                loop {
                    let mut run = {
                        let mut q = queue.lock().unwrap();
                        let Some(mut run) = q.waiting.pop_front() else {
                            break;
                        };
                        // A route that may escalate reserves both attempts,
                        // so the second is not refused after the first spent
                        // the batch's room.
                        let tries = 1 + i64::from(escalation(policy, &run).is_some());
                        let committed = q.spent
                            + q.running as f64 * cfg.agent_budget
                            + tries as f64 * cfg.agent_budget;
                        if committed > cfg.batch_budget + 1e-9 {
                            run.state = RunState::Failed;
                            run.error =
                                Some(format!("{NOT_STARTED} ${} reached", cfg.batch_budget));
                            // Refused before the agent ran, so it consumes
                            // no attempt.
                            let _ = tx.send((run, Vec::new()));
                            continue;
                        }
                        q.running += 1;
                        run
                    };
                    run.state = RunState::Running;
                    let _ = tx.send((run.clone(), Vec::new()));
                    let mut made = vec![attempt(home, cfg, workers, &mut run, None)];
                    // One retry at a stronger model, only where the check
                    // itself refused the work.
                    if consumes(&run)
                        && let Some(model) = escalation(policy, &run)
                    {
                        run.model = Some(model);
                        made.push(attempt(home, cfg, workers, &mut run, Some(ESCALATION)));
                    }
                    {
                        let mut q = queue.lock().unwrap();
                        q.running -= 1;
                        q.spent += run.cost_usd.unwrap_or(0.0);
                    }
                    let _ = tx.send((run, made));
                }
            });
        }
        drop(tx);
        for (run, attempts) in rx {
            if let Err(e) = store.update_run(&run) {
                save_error.get_or_insert(e.to_string());
            }
            for a in &attempts {
                if let Err(e) = store.insert_attempt(a) {
                    save_error.get_or_insert(e.to_string());
                }
                // Escalation counts like any other attempt: each refused
                // check is one negative signal about the task.
                if a.verify_ok == Some(false)
                    && let Err(e) = store.consume_attempt(&run.project, &attempt_key(&run))
                {
                    save_error.get_or_insert(e.to_string());
                }
            }
            if run.state != RunState::Running {
                done(&run);
                finished.push(run);
            }
        }
    });
    match save_error {
        Some(e) => Err(e.into()),
        None => Ok(finished),
    }
}

/// One agent run in the run's worktree, then verification. With `feedback`,
/// the reviewer's notes follow the original prompt.
///
/// The run accumulates cost and time across attempts, and keeps only the
/// latest summary and verify result. The returned `Attempt` holds this
/// attempt's own values; the caller appends it, because the worker threads
/// have no store.
#[must_use]
fn attempt(
    home: &Path,
    cfg: &Config,
    workers: &[Worker],
    run: &mut Run,
    feedback: Option<&str>,
) -> Attempt {
    let mut a = Attempt {
        id: 0,
        run_id: run.id,
        n: 0,
        agent: run.agent.clone(),
        model: run.model.clone(),
        prompt: String::new(),
        feedback: feedback.map(str::to_string),
        started_at: crate::dates::now(),
        seconds: None,
        cost_usd: None,
        summary: None,
        verify: None,
        verify_ok: None,
        error: None,
        outcome: None,
    };
    let Some(worker) = workers.iter().find(|w| w.name == run.agent) else {
        fail(
            run,
            &mut a,
            format!("unknown agent `{}`; see `pma agent`", run.agent),
        );
        return a;
    };
    let logs = home.join("runs").join(run.id.to_string());
    let agent_env = home.join("agent-env");
    if let Err(e) = std::fs::create_dir_all(&logs) {
        fail(run, &mut a, format!("{}: {e}", logs.display()));
        return a;
    }
    let n = (1..)
        .find(|n| !logs.join(format!("agent-{n}.log")).exists())
        .unwrap_or(1);
    let timeout = Duration::from_secs(cfg.timeout as u64 * 60);

    a.prompt = match feedback {
        Some(f) => format!(
            "{}\nYour previous attempt is already in the working tree. The reviewer's feedback:\n\n{f}\n",
            run.prompt
        ),
        None => run.prompt.clone(),
    };
    a.verify = run.verify.clone();
    let mut cmd = worker.build(
        &a.prompt,
        &run.worktree,
        run.model.as_deref(),
        cfg.agent_budget,
        &run.extra_args,
        timeout.saturating_sub(INNER_GRACE),
    );
    worker.allow_verify(&mut cmd, run.verify.as_deref());
    cmd.current_dir(&run.worktree);
    if let Err(e) = agent::restrict(&mut cmd, &agent_env) {
        fail(run, &mut a, format!("{}: {e}", agent_env.display()));
        return a;
    }
    a.started_at = crate::dates::now();
    run.error = None;
    let log = logs.join(format!("agent-{n}.log"));
    let finished = match agent::run_limited(cmd, &log, timeout) {
        Ok(f) => f,
        Err(e) => {
            fail(run, &mut a, format!("{}: {e}", worker.command));
            return a;
        }
    };
    let report = worker.parse.report(
        &std::fs::read_to_string(&log).unwrap_or_default(),
        finished.success == Some(true),
    );
    a.seconds = Some(finished.seconds);
    run.seconds = Some(run.seconds.unwrap_or(0) + finished.seconds);
    // An agent that reports no cost leaves both null. Zero would read as free.
    if let Some(c) = report.cost_usd {
        a.cost_usd = Some(c);
        run.cost_usd = Some(run.cost_usd.unwrap_or(0.0) + c);
    }
    a.summary = Some(report.summary.clone());
    run.summary = Some(report.summary);
    // Before any git call in the tree: what follows would run under a config
    // the agent wrote.
    if let Err(e) = own_gitdir(run) {
        fail(run, &mut a, e.to_string());
        return a;
    }
    run.commits = wt_git(
        run,
        &["rev-list", "--count", &format!("{}..HEAD", run.base)],
    )
    .ok()
    .and_then(|s| s.parse().ok());
    run.diffstat = diffstat(run);
    match changed_paths(run) {
        Ok(paths) => {
            run.changed_paths = Some(paths);
            run.scope_error = None;
        }
        Err(e) => {
            // Not an empty set: that would read as a run that changed
            // nothing, and pass every scope check.
            run.changed_paths = None;
            run.scope_error = Some(e.to_string());
        }
    }

    let failure = match finished.success {
        None => Some(format!("timed out after {} minutes", cfg.timeout)),
        Some(_) if !report.ok => Some(format!("agent failed; log: {}", log.display())),
        _ => None,
    };
    if let Some(f) = failure {
        fail(run, &mut a, f);
        return a;
    }

    run.verify_ok = None;
    if let Some(v) = &run.verify {
        let vlog = logs.join(format!("verify-{n}.log"));
        match verify_once(v, &run.worktree, &agent_env, &vlog, timeout) {
            Ok((ok, seconds)) => {
                a.seconds = Some(a.seconds.unwrap_or(0) + seconds);
                run.seconds = Some(run.seconds.unwrap_or(0) + seconds);
                run.verify_ok = Some(ok == Some(true));
            }
            Err(e) => {
                run.verify_ok = Some(false);
                run.error = Some(format!("verify: {e}"));
                a.error = run.error.clone();
            }
        }
    }
    a.verify_ok = run.verify_ok;
    run.enter(RunState::Ready);
    a.outcome = Some(RunState::Ready.name().into());
    a
}

/// Files git tracks in the worktree, as a proxy for how far a change can
/// reach. Zero when it cannot be counted, which reads as a small repository
/// rather than refusing the dispatch.
fn tracked_files(worktree: &Path) -> i64 {
    git(worktree, &["ls-files"])
        .map(|out| out.lines().filter(|l| !l.is_empty()).count() as i64)
        .unwrap_or(0)
}

/// Runs `command` in `worktree` under the agent's stripped environment, the
/// same way the head check runs it. `Ok(None)` means it was killed at the
/// timeout.
pub fn verify_once(
    command: &str,
    worktree: &Path,
    agent_env: &Path,
    log: &Path,
    timeout: Duration,
) -> std::io::Result<(Option<bool>, i64)> {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", command]).current_dir(worktree);
    agent::restrict(&mut cmd, agent_env)?;
    let f = agent::run_limited(cmd, log, timeout)?;
    Ok((f.success, f.seconds))
}

/// `verify` at the base commit, measured once per project, base, command and
/// timeout. Every task of a project in one batch shares a base, so the batch
/// pays for one run. A failure to start is recorded as unknown rather than as
/// a failing base, which would read as a broken repository.
fn base_verify(
    store: &Store,
    home: &Path,
    cfg: &Config,
    project: &str,
    base: &str,
    command: &str,
    worktree: &Path,
) -> Result<(Option<bool>, Option<i64>)> {
    if let Some((ok, seconds)) = store.verify_base(project, base, command, cfg.timeout)? {
        return Ok((ok, Some(seconds)));
    }
    let (ok, seconds, _) = measure_base(store, home, cfg, project, base, command, worktree)?;
    Ok((ok, Some(seconds)))
}

/// Runs `command` in `worktree`, a checkout of `base`, under the agent's
/// environment, and records the result in the base cache. Returns the result,
/// the seconds it took and the log.
fn measure_base(
    store: &Store,
    home: &Path,
    cfg: &Config,
    project: &str,
    base: &str,
    command: &str,
    worktree: &Path,
) -> Result<(Option<bool>, i64, PathBuf)> {
    let agent_env = home.join("agent-env");
    let logs = home.join("verify-base");
    std::fs::create_dir_all(&logs).map_err(|e| format!("{}: {e}", logs.display()))?;
    let log = logs.join(format!("{project}-{}.log", &base[..base.len().min(12)]));
    let timeout = Duration::from_secs(cfg.timeout as u64 * 60);
    let (ok, seconds) = match verify_once(command, worktree, &agent_env, &log, timeout) {
        Ok((success, seconds)) => (success, seconds),
        Err(_) => (None, 0),
    };
    store.set_verify_base(project, base, command, cfg.timeout, ok, seconds)?;
    Ok((ok, seconds, log))
}

/// What `pma verify` found for one project.
#[derive(Debug)]
pub struct Preflight {
    pub base: String,
    /// `None` when the project has no verify command.
    pub command: Option<String>,
    /// As in the base cache: `None` when the check timed out or could not
    /// start.
    pub ok: Option<bool>,
    pub seconds: i64,
    pub log: Option<PathBuf>,
}

/// Runs the project's verify at the head of its remote default branch, in a
/// fresh worktree and under the agent's environment, as a dispatch's base
/// check does, and records the result in the base cache. A check that fails
/// for a reason of its own then shows before an agent is paid to find it.
/// Unlike dispatch, it measures again rather than reading the cache.
pub fn preflight(
    store: &Store,
    home: &Path,
    cfg: &Config,
    project: &str,
    repo: &Path,
) -> Result<Preflight> {
    git(repo, &["fetch", "--quiet", "origin"])?;
    let default_branch = scan::default_branch(repo).ok_or(scan::DEFAULT_BRANCH_UNKNOWN)?;
    let base = git(
        repo,
        &[
            "rev-parse",
            &format!("refs/remotes/origin/{default_branch}"),
        ],
    )?;
    let tree = home.join("verify-trees").join(project);
    // Left by a preflight that was interrupted.
    remove_worktree(repo, &tree, "")?;
    if let Some(parent) = tree.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            "--quiet",
            path_arg(&tree)?,
            &base,
        ],
    )?;
    let measured = match verify_command(cfg, project, &tree) {
        None => Ok((None, None, 0, None)),
        Some(command) => measure_base(store, home, cfg, project, &base, &command, &tree)
            .map(|(ok, seconds, log)| (Some(command), ok, seconds, Some(log))),
    };
    let removed = remove_worktree(repo, &tree, "");
    let (command, ok, seconds, log) = measured?;
    removed?;
    Ok(Preflight {
        base,
        command,
        ok,
        seconds,
        log,
    })
}

/// Added after the original prompt when a run is retried at a stronger
/// model. The worktree still holds the first attempt's work.
const ESCALATION: &str = "The project's check refused your previous attempt, which is still in \
                          the working tree. Reproduce the failure, then fix it.";

/// The model a failed run retries at, or `None` when its route says nothing,
/// when the policy is not applied, or when it has already escalated.
fn escalation(policy: Option<&Policy>, run: &Run) -> Option<String> {
    let name = run.route.as_deref()?;
    let route = policy?.routes.iter().find(|r| r.name == name)?;
    let escalate = route.escalate.as_ref()?;
    // `attempts` is the number beyond the first, and only one is honoured.
    if escalate.attempts < 1 || run.model.as_deref() == Some(escalate.model.as_str()) {
        return None;
    }
    Some(escalate.model.clone())
}

/// Fails the run and the attempt with one error, so neither records an
/// outcome the other contradicts.
fn fail(run: &mut Run, a: &mut Attempt, error: String) {
    run.state = RunState::Failed;
    run.error = Some(error.clone());
    a.error = Some(error);
    a.outcome = Some(RunState::Failed.name().into());
}

/// The last line of `git diff --stat` against the base, untracked files
/// included.
fn diffstat(run: &Run) -> Option<String> {
    wt_git(run, &["add", "--all", "--intent-to-add"]).ok()?;
    let stat = wt_git(run, &["diff", "--stat", &run.base]).ok()?;
    Some(
        stat.lines()
            .last()
            .unwrap_or("no changes")
            .trim()
            .to_string(),
    )
}

/// Every path the run changed against its base, untracked files included and
/// both sides of a rename listed. NUL-delimited, because a path may contain a
/// newline, and `--name-status` rather than `--name-only`, because a rename
/// names two paths and only the status says so.
fn changed_paths(run: &Run) -> Result<Vec<String>> {
    wt_git(run, &["add", "--all", "--intent-to-add"])?;
    let out = wt_git(
        run,
        &["diff", "--name-status", "-z", "--find-renames", &run.base],
    )?;
    let mut fields = out.split('\0').filter(|f| !f.is_empty());
    let mut paths = Vec::new();
    while let Some(status) = fields.next() {
        let wanted = match status.as_bytes().first() {
            // A rename or a copy names its source and its destination.
            Some(b'R' | b'C') => 2,
            Some(_) => 1,
            None => continue,
        };
        for _ in 0..wanted {
            match fields.next() {
                Some(p) => paths.push(p.to_string()),
                None => return Err(format!("`git diff` ended after status `{status}`").into()),
            }
        }
    }
    Ok(paths)
}

/// `git diff` of the worktree against the run's base.
pub fn diff(run: &Run) -> Result<String> {
    wt_git(run, &["add", "--all", "--intent-to-add"])?;
    wt_git(run, &["diff", &run.base])
}

/// The worktree's content as one object id: `git add --all`, which stages
/// without committing, then `git write-tree`. The same call when publishing
/// says whether anything changed since.
pub fn tree(run: &Run) -> Result<String> {
    wt_git(run, &["add", "--all"])?;
    wt_git(run, &["write-tree"])
}

pub fn approve(store: &Store, run: &mut Run, by: &str) -> Result<()> {
    // Approving an already approved run re-takes the evidence, which is how
    // a reviewer says the tree is fine after a publish refused a stale one.
    if !matches!(run.state, RunState::Ready | RunState::Approved) {
        return Err(format!(
            "run #{} is {}; only a ready or approved run can be approved",
            run.id,
            run.state.name()
        )
        .into());
    }
    if run.approval == Some(crate::route::Approval::Propose) {
        return Err(format!(
            "run #{} took route `{}`, which is `propose`: it produces a patch and a \
             summary, and nothing is published. Change the route to publish it.",
            run.id,
            run.route.as_deref().unwrap_or("-")
        )
        .into());
    }
    // Recorded now, so a publish can say whether it is publishing what was read.
    run.approved_tree = Some(tree(run)?);
    run.approved_head = wt_git(run, &["rev-parse", "HEAD"]).ok();
    run.approved_by = Some(by.to_string());
    run.enter(RunState::Approved);
    store.update_run(run)
}

pub fn reject(store: &Store, run: &mut Run) -> Result<()> {
    if run.state == RunState::PrOpen {
        return Err(format!(
            "run #{} has an open pull request; merge or close it: {}",
            run.id,
            run.outcome.as_deref().unwrap_or_default()
        )
        .into());
    }
    if run.state.is_final() || matches!(run.state, RunState::Queued | RunState::Running) {
        return Err(format!(
            "run #{} is {}; it cannot be rejected",
            run.id,
            run.state.name()
        )
        .into());
    }
    remove_worktree(&run.repo, &run.worktree, &run.branch)?;
    run.enter(RunState::Rejected);
    store.update_run(run)?;
    // A reviewer who refuses the work has judged the task, whatever verify
    // made of it.
    store.consume_attempt(&run.project, &attempt_key(run))?;
    Ok(())
}

/// Runs the agent again in the same worktree with the reviewer's feedback.
pub fn rework(
    store: &Store,
    home: &Path,
    cfg: &Config,
    run: &mut Run,
    feedback: &str,
) -> Result<()> {
    if !matches!(
        run.state,
        RunState::Ready | RunState::Failed | RunState::Approved
    ) {
        return Err(format!(
            "run #{} is {}; it cannot be reworked",
            run.id,
            run.state.name()
        )
        .into());
    }
    if !run.worktree.is_dir() {
        return Err(format!(
            "{} no longer exists; reject the run",
            run.worktree.display()
        )
        .into());
    }
    run.feedback = Some(feedback.to_string());
    // The approver read a tree that is about to change.
    run.approved_tree = None;
    run.approved_head = None;
    run.approved_by = None;
    run.state = RunState::Running;
    store.update_run(run)?;
    let a = attempt(home, cfg, &store.agents()?, run, Some(feedback));
    store.update_run(run)?;
    store.insert_attempt(&a)?;
    if consumes(run) {
        store.consume_attempt(&run.project, &attempt_key(run))?;
    }
    Ok(())
}

/// Removes the worktree and its branch. Either may already be gone.
pub fn remove_worktree(repo: &Path, worktree: &Path, branch: &str) -> Result<()> {
    let gitfile = std::fs::symlink_metadata(worktree.join(".git")).is_ok_and(|m| m.is_file());
    if worktree.exists() && gitfile {
        git(
            repo,
            &["worktree", "remove", "--force", path_arg(worktree)?],
        )?;
    } else if worktree.exists() {
        // Git refuses a worktree whose `.git` is not its file. The directory
        // is pma's, so it goes directly, and `prune` drops the entry.
        std::fs::remove_dir_all(worktree).map_err(|e| format!("{}: {e}", worktree.display()))?;
    }
    let _ = git(repo, &["worktree", "prune"]);
    if git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
    {
        git(repo, &["branch", "-D", branch])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_names_a_project_a_line_a_signal_a_heading_or_a_quadrant() {
        let parse = |s| {
            let (project, what) = target(s).unwrap();
            (project.to_string(), what)
        };
        assert_eq!(parse("cynn"), ("cynn".into(), Target::Project));
        assert_eq!(parse("cynn:31"), ("cynn".into(), Target::Line(31)));
        assert_eq!(
            parse("cynn:ci"),
            ("cynn".into(), Target::Signal("ci".into()))
        );
        assert_eq!(
            parse("cynn:critical"),
            ("cynn".into(), Target::Priority(Priority::Critical))
        );
        assert_eq!(
            parse("cynn:q1"),
            ("cynn".into(), Target::Quadrant(Quadrant::Q1))
        );
        assert_eq!(
            parse("cynn:Q4"),
            ("cynn".into(), Target::Quadrant(Quadrant::Q4))
        );
    }

    /// Only a target naming one task fails the batch when it cannot run.
    #[test]
    fn one_task_or_many() {
        assert!(Target::Line(3).is_one());
        assert!(Target::Signal("deps".into()).is_one());
        assert!(!Target::Priority(Priority::Low).is_one());
        assert!(!Target::Quadrant(Quadrant::Q2).is_one());
        assert!(!Target::Project.is_one());
    }

    #[test]
    fn an_unreadable_target_says_what_is_accepted() {
        let err = |s| target(s).unwrap_err().to_string();
        assert!(err("cynn:soon").contains("expected a TODO.md line, ci, deps, critical"));
        assert!(err("cynn:q5").contains("expected a TODO.md line"));
        assert!(err(":31").contains("no project before the colon"));
        assert!(err("").contains("name a project"));
    }

    #[test]
    fn slugs_are_short_ascii_words() {
        assert_eq!(
            slug("Fix the `Lexer` crash on Ctrl-C!"),
            "fix-the-lexer-crash-on-ctrl-c"
        );
        assert_eq!(slug("::"), "task");
        assert_eq!(
            slug("add a context manager for every network class and its trainer"),
            "add-a-context-manager-for-every-network"
        );
        assert_eq!(slug(&"x".repeat(60)), "task");
    }

    #[test]
    fn prompts_carry_the_task_rules_and_verify() {
        let run = Run {
            id: 1,
            project: "cynn".into(),
            task_key: "k".into(),
            text: "Add evaluate()".into(),
            gh: None,
            quadrant: None,
            agent: "claude".into(),
            repo: PathBuf::new(),
            branch: String::new(),
            worktree: PathBuf::new(),
            default_branch: "main".into(),
            base: String::new(),
            prompt: String::new(),
            state: RunState::Queued,
            feedback: None,
            seconds: None,
            cost_usd: None,
            summary: None,
            commits: None,
            diffstat: None,
            verify: None,
            verify_ok: None,
            error: None,
            outcome: None,
            dispatched_at: None,
            ready_at: None,
            decided_at: None,
            published_at: None,
            review_seconds: None,
            class: Some(Class::Specified),
            scope: Vec::new(),
            tier: Some(1),
            description: None,
            agent_budget: None,
            timeout_minutes: None,
            verify_base_ok: None,
            verify_base_seconds: None,
            changed_paths: None,
            scope_error: None,
            model: None,
            complexity: None,
            features: None,
            estimator: None,
            route_revision: None,
            route: None,
            approval: None,
            approved_tree: None,
            approved_head: None,
            approved_by: None,
            workflow_instance: None,
            node: None,
            unit: None,
            lap: 0,
            preset: None,
            extra_args: Vec::new(),
            git_snapshot: None,
        };
        let p = prompt(&run, "for all classes\n", Some("make test"));
        assert!(p.contains("`cynn`"));
        assert!(
            p.contains("Task: Add evaluate()\n\nfor all classes\n"),
            "{p}"
        );
        assert!(p.contains("Do not commit"));
        assert!(p.contains("Run `make test` to check your change."));
        assert!(!prompt(&run, "", None).contains("pma runs"));
    }

    /// Only a run that reached the agent and failed its check says anything
    /// about the task's suitability.
    #[test]
    fn an_attempt_counts_only_when_the_check_decided_against_it() {
        let ready = |verify_ok| Run {
            state: RunState::Ready,
            verify_ok,
            ..Run::blank()
        };
        assert!(consumes(&ready(Some(false))));
        assert!(!consumes(&ready(Some(true))), "an accepted check");
        assert!(!consumes(&ready(None)), "verify never ran");
        for error in [
            "not started: batch budget $5 reached",
            "timed out",
            "claude: no such file",
        ] {
            let run = Run {
                state: RunState::Failed,
                error: Some(error.into()),
                ..Run::blank()
            };
            assert!(!consumes(&run), "{error}");
        }
    }

    /// A workflow unit's attempts count against its lineage, so every node and
    /// lap spending on it draws from one counter; a task's count against its
    /// text.
    #[test]
    fn a_workflow_unit_counts_attempts_against_its_lineage() {
        let unit = Run {
            task_key: "workflow:3:u1".into(),
            text: "Fix the parser".into(),
            ..Run::default()
        };
        assert_eq!(attempt_key(&unit), "workflow:3:u1");
        let task = Run {
            task_key: "fix the parser".into(),
            text: "Fix  the Parser".into(),
            ..Run::default()
        };
        assert_eq!(attempt_key(&task), "fix the parser");
    }

    /// An item named like a signal, a campaign or a workflow unit is still an
    /// item: checked on origin before dispatch, and ticked when published.
    #[test]
    fn an_item_key_never_reads_as_another_kind_of_task() {
        let file = "# TODO\n\n## High\n\n- [ ] CI\n- [ ] deps\n- [ ] Workflow: migrate\n\
                    - [ ] campaign: x\n- [ ] item:ci\n- [ ] fix CI\n";
        let keys: Vec<String> = todo::parse(file).items.iter().map(|i| i.key()).collect();
        assert_eq!(
            keys,
            [
                "item:ci",
                "item:deps",
                "item:workflow: migrate",
                "item:campaign: x",
                "item:item:ci",
                "fix ci"
            ]
        );
        assert!(keys.iter().all(|k| !without_item(k)), "{keys:?}");
        assert_eq!(
            on_origin(Some(file), "item:ci", "CI"),
            OnOrigin::Open(String::new(), Vec::new())
        );
    }

    #[test]
    fn a_revision_ignores_wording_that_does_not_change_the_task() {
        assert_eq!(revision("Fix  the PARSER"), revision("fix the parser"));
        assert_ne!(revision("fix CI: build"), revision("fix CI: build, test"));
    }

    #[test]
    fn items_are_open_done_or_absent_on_origin() {
        let file = "# TODO\n\n## High\n\n- [ ] open one #manual gh:3\n  why\n- [x] finished\n";
        assert_eq!(
            on_origin(Some(file), "gh:3", "open one"),
            OnOrigin::Open("why\n".into(), vec!["manual".into()]),
            "the tags decide the class, so they travel with the item"
        );
        assert_eq!(
            on_origin(Some(file), "open one", "open one"),
            OnOrigin::Open("why\n".into(), vec!["manual".into()]),
            "a key that lost gh:N still matches by text"
        );
        assert_eq!(
            on_origin(Some(file), "finished", "finished"),
            OnOrigin::Done
        );
        assert_eq!(on_origin(Some(file), "other", "other"), OnOrigin::Absent);
        assert_eq!(on_origin(None, "open one", "open one"), OnOrigin::Absent);
    }
}
