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
use crate::config::{Config, Workspace};
use crate::rank::Quadrant;
use crate::route::{Policy, Subject};
use crate::scan;
use crate::store::{Attempt, Result, Run, RunItem, RunState, Store};
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
    /// Start from the clone's `HEAD` rather than the fetched default branch.
    /// A local run never reaches the remote.
    pub local: bool,
    /// A person drives the agent in a herdr pane.
    pub interactive: bool,
    /// Where the agent edits. `Clone` ignores `local`: a clone run always
    /// starts from the working tree, and waits there for review.
    pub workspace: Workspace,
    /// A bundle's items, when `key` is `BUNDLE`; empty otherwise.
    pub items: Vec<RunItem>,
}

/// The key of a bundle: one run over several TODO.md items of a project
/// (`docs/dev/bundled-dispatch.md`).
pub const BUNDLE: &str = "bundle";

/// The key of a run over a prompt the developer typed, which names no
/// TODO.md item.
pub const PROMPT: &str = "prompt";

/// What a prompt's run is called: its first line, at most 80 characters.
pub fn prompt_title(prompt: &str) -> String {
    let line = prompt.trim().lines().next().unwrap_or("");
    match line.char_indices().nth(80) {
        Some((at, _)) => format!("{}...", line[..at].trim_end()),
        None => line.to_string(),
    }
}

/// Items one bundle may hold. Its budget and timeout are those of one task.
pub const BUNDLE_MAX: usize = 8;

/// What a bundle's run is called: its items' texts.
pub fn bundle_text(items: &[RunItem]) -> String {
    items
        .iter()
        .map(|i| i.text.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

/// The TODO.md items a run marks done when published, as key, text and
/// issue: a bundle's items the approver did not drop, or the run's own item.
pub fn closes(run: &Run) -> Vec<(&str, &str, Option<i64>)> {
    if run.task_key == BUNDLE {
        return run
            .items
            .iter()
            .filter(|i| !i.dropped)
            .map(|i| (i.key.as_str(), i.text.as_str(), i.gh))
            .collect();
    }
    match without_item(&run.task_key) {
        true => Vec::new(),
        false => vec![(run.task_key.as_str(), run.text.as_str(), run.gh)],
    }
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
    is_signal(key)
        || key == BUNDLE
        || key == PROMPT
        || key.starts_with("campaign:")
        || key.starts_with("workflow:")
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
        // A refused refresh says an update broke the check, which is the
        // agent's task, not that an agent cannot do it.
        false if run.agent == crate::worker::REFRESH => format!("{} #refresh", revision(&run.text)),
        false => revision(&run.text),
    }
}

/// Whether this attempt counts against the limit. A run that never reached
/// the agent, or whose agent could not start or was killed at the timeout,
/// says nothing about the task's suitability.
fn consumes(run: &Run) -> bool {
    run.state == RunState::Ready && run.verify_ok == Some(false)
}

/// The tail of the check's output from the project's last `refresh` run,
/// when that check failed. `None` when the last refresh passed, or there was
/// none.
fn failed_refresh(store: &Store, home: &Path, project: &str) -> Result<Option<String>> {
    let last = store
        .runs()?
        .into_iter()
        .filter(|r| {
            r.project == project && r.task_key == "deps" && r.agent == crate::worker::REFRESH
        })
        .max_by_key(|r| r.id);
    let Some(run) = last.filter(|r| r.verify_ok == Some(false)) else {
        return Ok(None);
    };
    let logs = home.join("runs").join(run.id.to_string());
    let newest = (1..)
        .map(|n| logs.join(format!("verify-{n}.log")))
        .take_while(|p| p.is_file())
        .last();
    Ok(newest
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| crate::agent::tail(&text, CI_LOG_LINES)))
}

/// Lines of failed CI log carried in a fix-CI prompt.
const CI_LOG_LINES: usize = 200;

/// Runs `git -C dir args`, returning trimmed stdout or an error naming the
/// command and its stderr. With no filesystem monitor: one names a program,
/// and these calls carry the user's credentials. Bounded by `GIT_TIMEOUT`, so
/// a stalled fetch or push cannot hold the session lock.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "core.fsmonitor=false"])
        .args(args);
    let what = args.join(" ");
    let out = crate::scan::bounded(cmd, GIT_TIMEOUT).map_err(|e| format!("git {what}: {e}"))?;
    if !out.success {
        return Err(format!("git {what}: {}", out.stderr.trim()).into());
    }
    Ok(out.stdout.trim().to_string())
}

/// How long one `git` call outside a scan may take. Longer than a scan's
/// bound: a push, a fetch or a worktree checkout of a large repository is slow.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(600);

/// `git` in the run's worktree, once its `.git` is still the one `git
/// worktree add` wrote and its hooks and settings are the ones taken at
/// dispatch. An agent that replaced either would hand these calls, which
/// carry the user's credentials, a program of its own to run.
pub fn wt_git(run: &Run, args: &[&str]) -> Result<String> {
    unchanged_git(run)?;
    git(&run.worktree, args)
}

/// How a snapshot change reads in an error, which is also how a run that
/// found one is recognised afterwards.
const GIT_CHANGED: &str = "the repository's hooks or git settings changed since run #";

/// Refuses a run whose repository gained or changed a hook, or a setting
/// that runs a program or redirects a push, since dispatch. pma's git calls
/// carry the user's credentials and environment, and none of that is in the
/// diff the reviewer read. Also refuses a replaced `.git`, which is all a run
/// from before the snapshot existed can be checked for. A run whose
/// worktree is gone, once staged, is checked in its clone.
pub fn unchanged_git(run: &Run) -> Result<()> {
    let gone = !run.worktree.exists();
    let Some(then) = &run.git_snapshot else {
        return match gone || run.workspace == Workspace::Clone {
            true => Ok(()),
            false => own_gitdir(run),
        };
    };
    let now = match gone {
        true => snapshot_in(&run.repo, &run.repo)?,
        false => git_snapshot(run)?,
    };
    match changes(then, &now) {
        None => Ok(()),
        Some(changes) => Err(format!(
            "{GIT_CHANGED}{} was dispatched:\n{changes}\n\
             pma runs no git in {} until they are restored, since its calls carry your \
             credentials. If you made these changes, `pma review {} --reject \
             --keep-git-changes`.",
            run.id,
            run.repo.display(),
            run.id,
        )
        .into()),
    }
}

/// The lines each snapshot has and the other lacks, or `None` when equal.
fn changes(then: &str, now: &str) -> Option<String> {
    if then == now {
        return None;
    }
    let (then, now): (Vec<&str>, Vec<&str>) = (then.lines().collect(), now.lines().collect());
    let lines: Vec<String> = then
        .iter()
        .filter(|l| !now.contains(l))
        .map(|l| format!("  - {l}"))
        .chain(
            now.iter()
                .filter(|l| !then.contains(l))
                .map(|l| format!("  + {l}")),
        )
        .collect();
    Some(lines.join("\n"))
}

/// Refuses to run git in `repo` while a run there that is not settled found
/// its snapshot changed, until the clone matches that snapshot again. A
/// setting an agent added stays in the clone after its run fails; the next
/// fetch or `worktree add` would run it, and the next snapshot would take it
/// as its baseline.
pub fn clone_unchanged(store: &Store, repo: &Path) -> Result<()> {
    clone_held(&store.runs()?, repo)
}

/// `clone_unchanged` over runs already read, for a caller checking many
/// repositories.
pub fn clone_held(runs: &[Run], repo: &Path) -> Result<()> {
    let found: Vec<&Run> = runs
        .iter()
        .filter(|r| {
            r.repo == repo
                && !r.state.is_final()
                && r.git_snapshot.is_some()
                && r.error.as_deref().is_some_and(|e| e.contains(GIT_CHANGED))
        })
        .collect();
    if found.is_empty() {
        return Ok(());
    }
    let now = snapshot_in(repo, repo)?;
    for run in found {
        if let Some(changes) = changes(run.git_snapshot.as_deref().unwrap_or_default(), &now) {
            return Err(format!(
                "{GIT_CHANGED}{} was dispatched, and they are still in {}:\n{changes}\n\
                 pma runs no git there until they are restored. If you made these changes, \
                 `pma review {} --reject --keep-git-changes`.",
                run.id,
                repo.display(),
                run.id,
            )
            .into());
        }
    }
    Ok(())
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
    if run.workspace == Workspace::Clone {
        return snapshot_in(&run.repo, &run.repo);
    }
    own_gitdir(run)?;
    snapshot_in(&run.repo, &run.worktree)
}

/// The snapshot as git reads it in `dir`, a worktree of `repo` or the clone
/// itself.
fn snapshot_in(repo: &Path, dir: &Path) -> Result<String> {
    use std::os::unix::fs::PermissionsExt;

    let mut lines = Vec::new();
    let hooks = git(
        dir,
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
        let hashes = git(repo, &args)?;
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
    let listing = git(dir, &["config", "--list", "--show-scope", "-z"])?;
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
        lines.push(format!("config {key} {}", hash_text(repo, &value)?));
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

/// Why `run` must be approved on its own before it is published: a ready
/// bundle, whose approver has not yet said which items it finished.
/// Publishing it as it is would tick every item.
pub fn needs_approval(run: &Run) -> Option<String> {
    (run.state == RunState::Ready && !run.items.is_empty()).then(|| {
        format!(
            "run #{} is a bundle; approve it first, leaving open the items it did \
             not finish (`pma review {} --approve --drop N`)",
            run.id, run.id
        )
    })
}

/// Refuses a list of bundle items to leave open, numbered from 1, that
/// names an item the run lacks or every item it has.
pub fn check_drops(run: &Run, drop: &[usize]) -> Result<()> {
    if drop.is_empty() {
        return Ok(());
    }
    let n = run.items.len();
    if n == 0 {
        return Err(format!("run #{} is not a bundle; it has no items to drop", run.id).into());
    }
    if let Some(bad) = drop.iter().find(|i| **i == 0 || **i > n) {
        return Err(format!("run #{} has items 1 to {n}; there is no item {bad}", run.id).into());
    }
    if (1..=n).all(|i| drop.contains(&i)) {
        return Err(format!(
            "every item of run #{} is dropped; reject the run instead",
            run.id
        )
        .into());
    }
    Ok(())
}

/// What `prepare` made of a pick.
/// How a run refused before it started says so, in `Run::error`. A run that
/// never reached the agent is not a run that failed: the caller leaves its
/// work where it was rather than recording a result for it.
pub const NOT_STARTED: &str = "not started: batch budget";

#[derive(Debug)]
pub enum Prepared {
    /// With a warning for each bundle item left out.
    Queued(Box<Run>, Vec<String>),
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
    // A bundle loses the items origin no longer holds open.
    let mut pick = pick.clone();
    let mut dropped = Vec::new();
    let project = pick.project.clone();
    let repo = &pick.repo.clone();
    // Before the fetch, which reads `url.*` and `core.sshCommand`.
    clone_unchanged(store, repo)?;
    let in_clone = pick.workspace == Workspace::Clone;
    if in_clone {
        crate::publish::settle_committed(store)?;
    }
    if in_clone && let Some(held) = clone_busy(&store.runs()?, repo) {
        let state = store.run(held)?.state;
        return Ok(Prepared::Refused(format!(
            "{project}: its clone holds run #{held} ({}); {}, or dispatch with --worktree",
            state.name(),
            clears_clone(state, held)
        )));
    }
    // A clone run's `base` is `HEAD`, which publishing requires unmoved; its
    // `start` is the working tree, which may differ.
    let start = in_clone.then(|| working_tree_commit(repo)).transpose()?;
    let (default_branch, base) = if in_clone {
        (
            git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?,
            git(repo, &["rev-parse", "HEAD"])?,
        )
    } else if pick.local {
        (
            git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?,
            working_tree_commit(repo)?,
        )
    } else {
        git(repo, &["fetch", "--quiet", "origin"])?;
        let default_branch = scan::default_branch(repo).ok_or(scan::DEFAULT_BRANCH_UNKNOWN)?;
        let base = git(
            repo,
            &[
                "rev-parse",
                &format!("refs/remotes/origin/{default_branch}"),
            ],
        )?;
        (default_branch, base)
    };
    let refuse = |why: String| Ok(Prepared::Refused(format!("{project}: {why}")));

    let key = pick.key.clone();
    let (details, tags) = match key.as_str() {
        "ci" => {
            let Some(scan::Ci::Failing(workflows)) = store.project(&pick.project)?.map(|p| p.ci)
            else {
                return refuse("CI was not failing at the last scan".into());
            };
            // CI failed on the default branch, whatever a local clone has
            // checked out.
            let branch = match pick.local || in_clone {
                true => scan::default_branch(repo).ok_or(scan::DEFAULT_BRANCH_UNKNOWN)?,
                false => default_branch.clone(),
            };
            match failed_ci_logs(repo, &branch, &workflows)? {
                Ok(logs) => (logs, Vec::new()),
                Err(why) => return refuse(why),
            }
        }
        "deps" => {
            let detail = store
                .project(&pick.project)?
                .map(|p| p.deps_detail)
                .unwrap_or_default();
            let mut prompt = format!(
                "Outdated dependencies at the last `pma scan --deps`:\n\n```\n{detail}\n```\n\n\
                 Update them within the project's version constraints first. Change a \
                 constraint only where the tests still pass, and list any dependency you \
                 left behind, with the reason.\n"
            );
            if let Some(log) = failed_refresh(store, home, &pick.project)? {
                prompt.push_str(&format!(
                    "\n`pma deps refresh` took every update the constraints allow, and the \
                     project's check then failed. Find the update that broke it. Fix the code, \
                     or hold that package back with a constraint and say why. The check's \
                     output:\n\n```\n{log}\n```\n"
                ));
            }
            (prompt, Vec::new())
        }
        BUNDLE => {
            let from = start.as_deref().unwrap_or(&base);
            let file = git(repo, &["show", &format!("{from}:TODO.md")]).ok();
            let mut kept = Vec::new();
            for item in &pick.items {
                let why = match on_origin(file.as_deref(), &item.key, &item.text) {
                    OnOrigin::Open(description, tags) => {
                        if Class::of(&item.key, &tags).dispatchable() {
                            kept.push((item.clone(), description));
                            continue;
                        }
                        "is tagged #manual and is not dispatched".to_string()
                    }
                    OnOrigin::Done => format!("is already done on origin/{default_branch}"),
                    OnOrigin::Absent => format!(
                        "is not in TODO.md on origin/{default_branch}; commit and push it first"
                    ),
                };
                dropped.push(format!("{project}: `{}` {why}; left out", item.text));
            }
            if kept.is_empty() {
                return refuse(format!(
                    "no item of the bundle can be dispatched: {}",
                    dropped.join("; ")
                ));
            }
            if let [(item, description)] = &kept[..] {
                pick.key = item.key.clone();
                pick.text = item.text.clone();
                pick.gh = item.gh;
                pick.items.clear();
                (description.clone(), Vec::new())
            } else {
                let mut details = String::new();
                for (n, (item, description)) in kept.iter().enumerate() {
                    details.push_str(&format!("{}. {}\n", n + 1, item.text));
                    for line in description.lines() {
                        details.push_str(&format!("   {line}\n"));
                    }
                    details.push('\n');
                }
                pick.items = kept.into_iter().map(|(item, _)| item).collect();
                pick.text = bundle_text(&pick.items);
                (details, Vec::new())
            }
        }
        key if without_item(key) && !is_signal(key) => {
            (pick.details.clone().unwrap_or_default(), Vec::new())
        }
        _ => {
            // The tree the agent starts from: the working tree for a clone run.
            let from = start.as_deref().unwrap_or(&base);
            let file = git(repo, &["show", &format!("{from}:TODO.md")]).ok();
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

    if in_clone {
        // The run's `worktree` is the clone; `branch` is what it has checked
        // out, which is where a push goes.
        let prepared = queue(
            store,
            home,
            cfg,
            over,
            &pick,
            repo,
            &default_branch.clone(),
            class,
            default_branch,
            base,
            start,
            details,
        );
        return noting(prepared, dropped);
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
        &pick,
        &worktree,
        &branch,
        class,
        default_branch,
        base,
        None,
        details,
    );
    if !matches!(prepared, Ok(Prepared::Queued(..))) {
        let _ = remove_worktree(repo, &worktree, &branch);
    }
    noting(prepared, dropped)
}

/// A queued run with the bundle items `prepare` left out.
fn noting(prepared: Result<Prepared>, dropped: Vec<String>) -> Result<Prepared> {
    prepared.map(|p| match p {
        Prepared::Queued(run, _) => Prepared::Queued(run, dropped),
        p => p,
    })
}

/// The clone run that holds `repo`, if one does: one that is queued, under
/// way, or waiting in review or in its clone's index. A failed one too, since
/// its agent may have left edits that only a reject reverts.
/// What ends the hold of a run in `state` on its clone.
fn clears_clone(state: RunState, id: i64) -> String {
    match state {
        RunState::Queued | RunState::Running => "wait for it to finish".into(),
        RunState::Approved => format!("push it with `pma push {id}`, or reject it"),
        RunState::Staged => format!("commit it with `pma commit {id}`, or reject it"),
        RunState::Committed => format!("push it with `pma push {id}`"),
        RunState::PrOpen => format!("merge it with `pma merge {id}`"),
        _ => "approve or reject it first".into(),
    }
}

pub fn clone_busy(runs: &[Run], repo: &Path) -> Option<i64> {
    runs.iter()
        .filter(|r| r.repo == repo && r.workspace == Workspace::Clone && !r.state.is_final())
        .map(|r| r.id)
        .next()
}

/// A commit of the clone's working tree, untracked files included and ignored
/// ones not, on top of `HEAD`. A local edit starts from it, so it sees the
/// developer's uncommitted work and the edits applied before it, and its patch
/// applies to the tree it was made from. Built in a temporary index, so the
/// clone's index, refs and files are untouched; `HEAD` when nothing differs.
pub fn working_tree_commit(repo: &Path) -> Result<String> {
    let head = git(repo, &["rev-parse", "HEAD"])?;
    let index = std::env::temp_dir().join(format!(
        "pma-index-{}-{}",
        std::process::id(),
        crate::dates::now()
    ));
    let run = |args: &[&str]| -> Result<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["-c", "core.fsmonitor=false"])
            .args(args)
            .env("GIT_INDEX_FILE", &index)
            .env("GIT_AUTHOR_NAME", "pma")
            .env("GIT_AUTHOR_EMAIL", "pma@localhost")
            .env("GIT_COMMITTER_NAME", "pma")
            .env("GIT_COMMITTER_EMAIL", "pma@localhost")
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
    };
    let built = (|| {
        run(&["read-tree", &head])?;
        run(&["add", "--all"])?;
        let tree = run(&["write-tree"])?;
        if tree == run(&["rev-parse", &format!("{head}^{{tree}}")])? {
            return Ok(head.clone());
        }
        run(&["commit-tree", &tree, "-p", &head, "-m", "pma: working tree"])
    })();
    let _ = std::fs::remove_file(&index);
    built
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
    start: Option<String>,
    details: String,
) -> Result<Prepared> {
    let repo = &pick.repo;
    let refuse = |why: String| Ok(Prepared::Refused(format!("{}: {why}", pick.project)));
    // Before base verify, which runs the project's code and could install a
    // hook of its own.
    let git_snapshot = git_snapshot(&Run {
        repo: repo.clone(),
        worktree: worktree.to_path_buf(),
        workspace: pick.workspace,
        ..Run::default()
    })?;
    let verify = verify_command(cfg, &pick.project, worktree);
    // The worktree is a clean checkout of the base, so this is the base
    // tree; a clone run's is its working tree, keyed by `start`. One verify
    // at the head alone cannot tell a regression from a repository that was
    // already failing.
    let (verify_base_ok, verify_base_seconds) = match &verify {
        Some(v) => {
            let at = start.as_deref().unwrap_or(&base);
            base_verify(store, home, cfg, &pick.project, at, v, worktree)?
        }
        None => (None, None),
    };
    let mut features = Features::of(&pick.text, &details);
    features.repo_files = tracked_files(worktree);
    features.verify_seconds = verify_base_seconds.filter(|_| verify.is_some());
    (features.prior_decided, features.prior_accepted) = store.decided_runs(&pick.project)?;
    let complexity = complexity::estimate(class, &features);

    // A policy applies only once someone activates it; in shadow it is
    // computed and recorded but the settings still decide.
    // An untiered project is ranked at `default_tier`, so it is routed there.
    let tier = cfg.tier_or_default(pick.tier);
    let subject = Subject {
        class,
        complexity,
        tier,
        // A task dispatched on its own names no node, which is what a route
        // stating no node condition serves; a workflow unit names its node.
        node: pick.workflow.as_ref().map(|u| u.node.as_str()),
        lap: pick.workflow.as_ref().map_or(0, |u| u.lap),
    };
    let active = store.active_route()?;
    let mut routed = (None, None, None, None, class.scope());
    if let Some(rev) = &active
        && rev.shadow
    {
        // Recorded for `pma route replay`; it refuses, scopes and approves
        // nothing.
        let policy = rev.policy()?;
        routed.0 = Some(rev.revision);
        routed.1 = policy.route(&subject).map(|r| r.name.clone());
    } else if let Some(rev) = &active {
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
            Some((route.agent.clone(), route.model.clone())),
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
        tier,
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
        reviewed_tree: None,
        infra: false,
        workspace: pick.workspace,
        start,
        denied: Vec::new(),
        review_measured: None,
        pane: None,
        interactive: pick.interactive,
        agent_state: None,
        items: pick.items.clone(),
    };
    run.prompt = prompt(&run, &details, verify.as_deref());
    store.insert_run(&mut run)?;
    Ok(Prepared::Queued(Box::new(run), Vec::new()))
}

pub fn path_arg(path: &Path) -> Result<&str> {
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

/// Where an agent drafts changelog entries, relative to its worktree. pma
/// moves the file out before any diff, so it is never part of the change.
const CHANGELOG_DRAFT: &str = ".pma/changelog.md";

/// Whether a run's agent is asked for changelog entries: its project keeps a
/// `CHANGELOG.md`.
pub fn wants_changelog(run: &Run) -> bool {
    run.repo.join("CHANGELOG.md").is_file()
}

/// Where a run's drafted changelog entries are kept, outside its tree.
pub fn changelog_draft(home: &Path, id: i64) -> PathBuf {
    home.join("runs").join(id.to_string()).join("changelog.md")
}

fn prompt(run: &Run, details: &str, verify: Option<&str>) -> String {
    let where_ = match run.workspace {
        Workspace::Clone => {
            "in the developer's own clone. Files you did not change are theirs; leave them as they are."
        }
        Workspace::Worktree => "in a git worktree of it.",
    };
    let head = format!(
        "You are working on the project `{}`, {where_}\n\n",
        run.project
    );
    // A prompt's title is its first line, which the details repeat.
    let mut p = if run.task_key == PROMPT {
        format!("{head}Task, from the developer:\n")
    } else if run.items.is_empty() {
        format!("{head}Task: {}\n", run.text)
    } else {
        format!("{head}Tasks, numbered:\n")
    };
    if !details.trim().is_empty() {
        p.push('\n');
        p.push_str(details);
    }
    p.push_str(
        "\nRules:\n\
         - Leave your changes uncommitted. Do not commit, push, or switch branches.\n\
         - Do not edit TODO.md. pma marks the task done when the change is published.\n",
    );
    if wants_changelog(run) {
        p.push_str(&format!(
            "- Do not edit CHANGELOG.md. Write one entry per separable change to \
             `{CHANGELOG_DRAFT}`: a bold one-line title, then what went wrong and what \
             changed. pma keeps it out of your change and shows it to the reviewer.\n"
        ));
    }
    if let Some(v) = verify {
        p.push_str(&format!(
            "- Run `{v}` to check your change. pma runs it again after you finish.\n"
        ));
    }
    p.push_str(match run.items.is_empty() {
        true => "\nEnd with a short summary of what you changed and what is left undone.\n",
        false => {
            "\nEnd with a short summary, by task number, of what you changed and what is \
             left undone.\n"
        }
    });
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
    let agent = over
        .agent
        .clone()
        .or_else(|| over.preset.as_ref().map(|p| p.agent.clone()))
        .or(route_agent)
        .or_else(|| default.as_ref().map(|p| p.agent.clone()))
        .unwrap_or_else(|| cfg.agent.clone());
    // The default preset's model is for its own worker; a route that named
    // another worker does not inherit it.
    let model = over
        .model
        .clone()
        .or_else(|| over.preset.as_ref().and_then(|p| p.model.clone()))
        .or(route_model)
        .or_else(|| {
            default
                .as_ref()
                .filter(|p| p.agent == agent)
                .and_then(|p| p.model.clone())
        });
    let named = over.preset.clone().or(default);
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
/// With `retry`, a run's consumed attempts are reset once its agent starts.
pub fn execute(
    store: &Store,
    home: &Path,
    cfg: &Config,
    over: &Overrides,
    runs: Vec<Run>,
    retry: bool,
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
        /// What the runs under way may still spend.
        reserved: f64,
    }
    let queue = Mutex::new(Queue {
        waiting: runs.into(),
        spent: 0.0,
        reserved: 0.0,
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
                        let committed =
                            q.spent + q.reserved + tries as f64 * attempt_price(cfg, &run);
                        if committed > cfg.batch_budget + 1e-9 {
                            run.state = RunState::Failed;
                            run.error =
                                Some(format!("{NOT_STARTED} ${} reached", cfg.batch_budget));
                            // Refused before the agent ran, so it consumes
                            // no attempt.
                            let _ = tx.send((run, Vec::new()));
                            continue;
                        }
                        q.reserved += attempt_price(cfg, &run);
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
                        q.reserved -= attempt_price(cfg, &run);
                        q.spent += run
                            .cost_usd
                            .unwrap_or(made.len() as f64 * attempt_price(cfg, &run));
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
            // A run is sent once as it starts, before any attempt is charged.
            if retry
                && run.state == RunState::Running
                && attempts.is_empty()
                && let Err(e) = store.reset_attempts(&run.project, &attempt_key(&run))
            {
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

/// What one attempt of `run` is charged against the batch when its worker
/// reports no cost: its whole budget, so it cannot run the batch past its
/// limit unseen. `refresh` runs no model and costs nothing.
fn attempt_price(cfg: &Config, run: &Run) -> f64 {
    match run.agent == crate::worker::REFRESH {
        true => 0.0,
        false => cfg.agent_budget,
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
/// One attempt, then, for a clone run, a record of what it changed: the working
/// tree is the developer's from here on, so nothing later can say which edits
/// were the agent's.
fn attempt(
    home: &Path,
    cfg: &Config,
    workers: &[Worker],
    run: &mut Run,
    feedback: Option<&str>,
) -> Attempt {
    let a = attempt_once(home, cfg, workers, run, feedback);
    if run.workspace == Workspace::Clone
        && let Err(e) = record_change(home, run)
    {
        eprintln!("warning: #{}: its change was not recorded: {e}", run.id);
    }
    a
}

/// Where a clone run's change is kept, as a patch: a file rather than a commit,
/// which `git gc` would in time remove.
fn change_record(home: &Path, run: &Run) -> PathBuf {
    home.join("runs")
        .join(run.id.to_string())
        .join("ready.patch")
}

fn record_change(home: &Path, run: &Run) -> Result<()> {
    let record = change_record(home, run);
    let _ = std::fs::remove_file(&record);
    let tree = working_tree_commit(&run.repo)?;
    let patch = crate::publish::write_patch(home, run, &tree)?;
    // A copy: `change.patch` is what a publish or a reject last wrote, and stays.
    std::fs::copy(&patch, &record).map_err(|e| format!("{}: {e}", record.display()))?;
    Ok(())
}

/// Whether a clone run's `HEAD` has left its base: the working tree then holds
/// later commits as well as the run's change.
fn clone_moved(run: &Run) -> bool {
    run.workspace == Workspace::Clone
        && git(&run.repo, &["rev-parse", "HEAD"]).is_ok_and(|head| head != run.base)
}

fn attempt_once(
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
    let title = format!("#{} {}", run.id, run.text);
    let id = run.id;
    let hosted = crate::herdr::host(
        cfg,
        crate::herdr::Host {
            project: &run.project,
            title: &title,
            interactive: run.interactive,
            close_after: false,
        },
        cmd,
        &log,
        timeout,
        // On a worker thread, with no store. At once rather than at the end,
        // so a running agent's pane can be opened.
        |pane| {
            if let Err(e) = Store::open_default().and_then(|s| s.set_pane(id, pane)) {
                eprintln!("warning: #{id}: {e}");
            }
        },
        move |state| {
            if let Err(e) = Store::open_default().and_then(|s| s.set_agent_state(id, state)) {
                eprintln!("warning: #{id}: {e}");
            }
        },
    );
    let finished = match hosted {
        Ok(f) => f,
        Err(e) => {
            fail(run, &mut a, format!("{}: {e}", worker.command));
            return a;
        }
    };
    // A person drove it in a terminal, so there is no event stream to read.
    let report = match run.interactive {
        true => agent::Report {
            ok: finished.success == Some(true),
            summary: "interactive session in herdr; it reports no summary or cost".into(),
            cost_usd: None,
            denied: Vec::new(),
        },
        false => worker.parse.report(
            &agent::read_log(&log).unwrap_or_default(),
            finished.success == Some(true),
        ),
    };
    a.seconds = Some(finished.seconds);
    run.seconds = Some(run.seconds.unwrap_or(0) + finished.seconds);
    // An agent that reports no cost leaves both null. Zero would read as free.
    if let Some(c) = report.cost_usd {
        a.cost_usd = Some(c);
        run.cost_usd = Some(run.cost_usd.unwrap_or(0.0) + c);
    }
    a.summary = Some(report.summary.clone());
    run.summary = Some(report.summary);
    run.denied = report.denied;
    // A regular file only: a symlink could copy any file pma can read into
    // the review.
    let drafted = run.worktree.join(CHANGELOG_DRAFT);
    if std::fs::symlink_metadata(&drafted).is_ok_and(|m| m.is_file())
        && std::fs::copy(&drafted, logs.join("changelog.md")).is_ok()
    {
        let _ = std::fs::remove_file(&drafted);
        let _ = std::fs::remove_dir(run.worktree.join(".pma"));
    }
    // Before any git call in the tree: what follows would run under a config
    // or hooks the agent wrote.
    if let Err(e) = unchanged_git(run) {
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
        let sandbox = cfg.project(&run.project).sandbox;
        match verify_once(v, &run.worktree, sandbox, &agent_env, &vlog, timeout) {
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
    // After verify, so it is the tree `pma review` will show.
    run.reviewed_tree = tree(run).ok();
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
///
/// With `sandbox`, writes are confined to the worktree, the temp directory and
/// the toolchain caches, so a build file the agent edited cannot write
/// elsewhere on the host. Reads and the network stay open. Nothing under the
/// repository's `.git` is granted: a deleted object loses history nothing can
/// rebuild. A sandbox that cannot start is an error, not an unconfined run.
pub fn verify_once(
    command: &str,
    worktree: &Path,
    sandbox: bool,
    agent_env: &Path,
    log: &Path,
    timeout: Duration,
) -> std::io::Result<(Option<bool>, i64)> {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", command]).current_dir(worktree);
    agent::restrict(&mut cmd, agent_env)?;
    let policy = match sandbox {
        true => Some(confine(worktree)?),
        false => None,
    };
    let f = agent::run_limited(cmd, policy.as_ref(), log, timeout)?;
    Ok((f.success, f.seconds))
}

/// The verify sandbox for `worktree`, checked by running one confined command.
fn confine(worktree: &Path) -> std::io::Result<sanduk_sandbox::Policy> {
    // `/tmp` as well as `$TMPDIR`, which on macOS is under `/var/folders`: a
    // test suite that names `/tmp` itself, for a short Unix socket path,
    // fails under the policy otherwise.
    let policy = sanduk_sandbox::Policy::new(worktree)?.writable("/tmp")?;
    policy.preflight().map_err(|e| {
        std::io::Error::other(format!(
            "the verify sandbox did not start ({e}); \
             `pma config projects.<name>.sandbox off` runs verify unconfined"
        ))
    })?;
    Ok(policy)
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
    let sandbox = cfg.project(project).sandbox;
    if let Some((ok, seconds)) = store.verify_base(project, base, command, cfg.timeout, sandbox)? {
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
    let sandbox = cfg.project(project).sandbox;
    let (ok, seconds) = match verify_once(command, worktree, sandbox, &agent_env, &log, timeout) {
        Ok((success, seconds)) => (success, seconds),
        Err(_) => (None, 0),
    };
    store.set_verify_base(project, base, command, cfg.timeout, sandbox, ok, seconds)?;
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
    clone_unchanged(store, repo)?;
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
/// What the run's change is read against: `start` for a clone run, else the
/// base.
pub fn from(run: &Run) -> &str {
    run.start.as_deref().unwrap_or(&run.base)
}

/// The side of the diff the reviewer sees. In a worktree, the working tree
/// itself, after `git add --intent-to-add` has made untracked files visible
/// to `git diff`. In a clone, a commit of the working tree built in a
/// temporary index, so the clone's own index is never touched: `git diff`
/// then takes it as any other revision. For a run whose tree is gone, the
/// tree the reviewer was last shown, which is also what a rejected or
/// published run shows.
fn to(run: &Run) -> Result<Option<String>> {
    let reviewable = matches!(
        run.state,
        RunState::Running | RunState::Ready | RunState::Approved | RunState::Failed
    );
    if !reviewable || !run.worktree.is_dir() {
        return Ok(run.reviewed_tree.clone());
    }
    match run.workspace {
        Workspace::Clone => Ok(Some(working_tree_commit(&run.repo)?)),
        Workspace::Worktree => {
            wt_git(run, &["add", "--all", "--intent-to-add"])?;
            Ok(None)
        }
    }
}

/// `git diff` arguments for the run's change: `from` against `to`, or
/// against the working tree when `to` is `None`.
fn range(run: &Run, to: Option<&str>) -> Vec<String> {
    let mut v = vec![from(run).to_string()];
    v.extend(to.map(str::to_string));
    v
}

fn diffstat(run: &Run) -> Option<String> {
    let to = to(run).ok()?;
    let mut args = vec!["diff".to_string(), "--stat".into()];
    args.extend(range(run, to.as_deref()));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let stat = wt_git(run, &args).ok()?;
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
    let to = to(run)?;
    let mut args = vec![
        "diff".to_string(),
        "--name-status".into(),
        "-z".into(),
        "--find-renames".into(),
    ];
    args.extend(range(run, to.as_deref()));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = wt_git(run, &args)?;
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

/// `git diff` of the run's change: the worktree against the base, or a
/// clone's working tree against `start`.
pub fn diff(run: &Run) -> Result<String> {
    // Not `start` against the tree: that would show every later commit as the
    // run's. While `HEAD` holds, the live tree stays right: a reviewer's tweak
    // belongs in what they read and publish.
    if clone_moved(run)
        && matches!(
            run.state,
            RunState::Ready | RunState::Approved | RunState::Failed
        )
    {
        let record = change_record(&crate::store::state_home()?, run);
        return Ok(std::fs::read_to_string(record).unwrap_or_else(|_| {
            "(none recorded: the clone has moved since this run, so its change \
             can no longer be told from later work)"
                .to_string()
        }));
    }
    let to = to(run)?;
    if to.is_none() && !run.worktree.is_dir() {
        return Err(format!(
            "{} no longer exists and no tree was recorded",
            run.worktree.display()
        )
        .into());
    }
    let mut args = vec!["diff".to_string()];
    args.extend(range(run, to.as_deref()));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // Two objects diff anywhere in the repository, so a run whose worktree
    // is gone still reads.
    unchanged_git(run)?;
    let dir = match run.worktree.is_dir() {
        true => &run.worktree,
        false => &run.repo,
    };
    git(dir, &args)
}

/// The worktree's content as one object id: `git add --all`, which stages
/// without committing, then `git write-tree`. The same call when publishing
/// says whether anything changed since. A clone's content is read through a
/// temporary index, so nothing is staged in the user's.
pub fn tree(run: &Run) -> Result<String> {
    if run.workspace == Workspace::Clone {
        unchanged_git(run)?;
        let commit = working_tree_commit(&run.repo)?;
        return git(&run.repo, &["rev-parse", &format!("{commit}^{{tree}}")]);
    }
    wt_git(run, &["add", "--all"])?;
    wt_git(run, &["write-tree"])
}

/// The most one reading adds to a run's review time. A run left open over
/// lunch is not half a day of review.
const REVIEW_CAP_SECONDS: i64 = 30 * 60;

/// A decision ends what its pane was kept open for.
fn settle_review(store: &Store, run: &Run) -> Result<()> {
    if let Some(pane) = &run.pane {
        crate::herdr::close(pane);
    }
    store.settle_review(run.id, crate::dates::now(), REVIEW_CAP_SECONDS)
}

pub fn approve(store: &Store, run: &mut Run, by: &str) -> Result<()> {
    let read = run.state;
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
    // What is approved is what the reviewer was last shown. Anything written
    // since, by an agent that outlived its run or by anyone, is read first.
    let now = tree(run)?;
    if run
        .reviewed_tree
        .as_ref()
        .is_some_and(|shown| shown != &now)
    {
        return Err(format!(
            "run #{} changed after it was last shown; read `pma review {}` again, \
             then approve it",
            run.id, run.id
        )
        .into());
    }
    run.approved_tree = Some(now);
    run.approved_head = wt_git(run, &["rev-parse", "HEAD"]).ok();
    run.approved_by = Some(by.to_string());
    run.enter(RunState::Approved);
    settle_review(store, run)?;
    // No session lock is held: a publish may have run since `run` was read.
    store.update_run_from(run, read)
}

/// `keep` accepts hooks or settings the run found changed as the user's own.
/// Without it, such a run is rejected only once the clone matches its
/// snapshot again, so rejecting does not quietly lift what `clone_unchanged`
/// holds. Returns how many files the run's change held.
///
/// `leave_files` closes the run and touches nothing: for a change that has
/// already landed, or a clone that has moved on.
pub fn reject(
    store: &Store,
    run: &mut Run,
    keep: bool,
    infra: bool,
    leave_files: bool,
) -> Result<usize> {
    let fix = run.fixing_pr().is_some();
    let files = discard(store, run, keep, leave_files)?;
    settle_review(store, run)?;
    // Only the fix was refused. The pull request still holds the task.
    if fix {
        return Ok(files);
    }
    run.infra = infra;
    store.update_run(run)?;
    let attempts = store.attempts(Some(run.id))?;
    if infra {
        // The checks this run failed judged the infrastructure, not the task.
        let failed = attempts
            .iter()
            .filter(|a| a.verify_ok == Some(false))
            .count() as i64;
        store.refund_attempts(&run.project, &attempt_key(run), failed)?;
    } else if !attempts.is_empty() {
        // A reviewer who refuses the work has judged the task, whatever
        // verify made of it. A run that never started has no work to judge.
        store.consume_attempt(&run.project, &attempt_key(run))?;
    }
    Ok(files)
}

/// Removes a run's worktree and marks it rejected, consuming no attempt: what
/// `reject` does before a reviewer's judgement is counted. A local edit that
/// failed verify was counted by verify; one that did not apply was judged by
/// no one. Returns how many files the change held.
pub fn discard(store: &Store, run: &mut Run, keep: bool, leave_files: bool) -> Result<usize> {
    let read = run.state;
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
    if !keep
        && let Some(then) = &run.git_snapshot
        && run
            .error
            .as_deref()
            .is_some_and(|e| e.contains(GIT_CHANGED))
        && let Some(changes) = changes(then, &snapshot_in(&run.repo, &run.repo)?)
    {
        return Err(format!(
            "run #{} found the hooks or git settings of {} changed, and they still are:\n\
             {changes}\nRestore them first, or pass --keep-git-changes if you made them.",
            run.id,
            run.repo.display()
        )
        .into());
    }
    if run.workspace == Workspace::Clone {
        // Only the recorded change. A patch of the tree as it is now would hold
        // later work too, and reversing it would remove that without a word.
        let record = change_record(&crate::store::state_home()?, run);
        let files = patch_files(&record);
        if !leave_files {
            let close = format!(
                "`pma close {}`, or `X`, ends the run and leaves your files alone",
                run.id
            );
            if clone_moved(run) {
                return Err(format!(
                    "{} has moved past run #{}'s base, so taking its change out could \
                     remove later work. {close}",
                    run.repo.display(),
                    run.id
                )
                .into());
            }
            if !record.exists() {
                return Err(format!(
                    "run #{} has no record of its change, so it cannot be told from \
                     your own edits. Take it out yourself if you want it gone. {close}",
                    run.id
                )
                .into());
            }
            revert(run, Some(&record))?;
        }
        if leave_files {
            run.outcome = Some("closed; its files were left as they are".into());
        }
        run.enter(RunState::Rejected);
        store.update_run_from(run, read)?;
        return Ok(files);
    }
    // The patch stays in the run's log directory either way.
    let patch = keep_patch(run);
    let files = patch.as_deref().map_or(0, patch_files);
    // Written first, and only over the state that was read: a run another
    // front end approved meanwhile is refused with its worktree intact.
    if run.fixing_pr().is_some() {
        run.state = RunState::PrOpen;
        run.error = None;
        run.approved_tree = None;
        run.approved_head = None;
        run.approved_by = None;
    } else {
        run.enter(RunState::Rejected);
    }
    store.update_run_from(run, read)?;
    if run.state == RunState::PrOpen {
        git(&run.repo, &["update-ref", "-d", &pr_tip_ref(run)])?;
    }
    remove_worktree(&run.repo, &run.worktree, &run.branch)
        .map_err(|e| format!("run #{} is rejected, but its worktree stays: {e}", run.id))?;
    Ok(files)
}

/// How many files a patch from `git diff` touches; none when it cannot be
/// read.
fn patch_files(patch: &Path) -> usize {
    std::fs::read(patch).map_or(0, |b| {
        b.split(|&c| c == b'\n')
            .filter(|l| l.starts_with(b"diff --git "))
            .count()
    })
}

/// Writes the run's change to `runs/<id>/change.patch`, best effort: a run
/// that never ran has nothing to keep.
fn keep_patch(run: &Run) -> Option<PathBuf> {
    let home = crate::store::state_home().ok()?;
    // A worktree's `to` is the working tree itself; the patch needs an
    // object, so it is written from the tree.
    let to = match to(run).ok()? {
        Some(to) => to,
        None => tree(run).ok()?,
    };
    crate::publish::write_patch(&home, run, &to).ok()
}

/// Takes a clone run's change out of the working tree: the patch in reverse,
/// which refuses when the files moved on since. Only the working tree: the
/// index is the user's. Nothing to revert is fine.
fn revert(run: &Run, patch: Option<&Path>) -> Result<()> {
    let Some(patch) = patch else {
        return Ok(());
    };
    if std::fs::metadata(patch).map_or(0, |m| m.len()) == 0 {
        return Ok(());
    }
    git(
        &run.repo,
        &[
            "apply",
            "--reverse",
            "--binary",
            "--check",
            path_arg(patch)?,
        ],
    )
    .and_then(|_| {
        git(
            &run.repo,
            &["apply", "--reverse", "--binary", path_arg(patch)?],
        )
    })
    .map(|_| ())
    .map_err(|e| {
        format!(
            "run #{}'s change no longer reverts cleanly from {}: {e}\n\
             Take it out yourself, then reject again. The patch: {}",
            run.id,
            run.repo.display(),
            patch.display()
        )
        .into()
    })
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
    settle_review(store, run)?;
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

/// Where a conflict fix records the pull request branch's tip, as the lease
/// its publish pushes over.
pub fn pr_tip_ref(run: &Run) -> String {
    format!("refs/pma/pr-tip/{}", run.id)
}

/// Sends a `pr-open` run back to review with its pull request's conflicts
/// resolved by the agent. The run's worktree is rebuilt from the branch, with
/// the default branch merged in and the markers left for the agent. Publishing
/// then updates the same pull request.
pub fn fix_conflicts(store: &Store, home: &Path, cfg: &Config, run: &mut Run) -> Result<()> {
    if run.state != RunState::PrOpen {
        return Err(format!(
            "run #{} is {}; it has no open pull request",
            run.id,
            run.state.name()
        )
        .into());
    }
    let url = run.outcome.clone().unwrap_or_default();
    if !crate::publish::conflicting(&url)? {
        return Err(format!("pull request has no conflicts: {url}").into());
    }
    clone_unchanged(store, &run.repo)?;
    git(&run.repo, &["fetch", "--quiet", "origin"])?;
    let onto = format!("refs/remotes/origin/{}", run.default_branch);
    let base = git(&run.repo, &["rev-parse", &onto])?;
    let tip = git(
        &run.repo,
        &["rev-parse", &format!("refs/remotes/origin/{}", run.branch)],
    )?;
    git(
        &run.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-B",
            &run.branch,
            path_arg(&run.worktree)?,
            &tip,
        ],
    )?;
    git(&run.repo, &["update-ref", &pr_tip_ref(run), &tip])?;
    // Fails on the conflicts it leaves; the list below is what matters.
    let _ = wt_git(run, &["merge", "--quiet", "--no-commit", "--no-ff", &onto]);
    let conflicted = wt_git(run, &["diff", "--name-only", "--diff-filter=U"])?;
    // The branch's tick is on a stale copy. Publishing ticks the new one.
    let _ = wt_git(run, &["checkout", &onto, "--", "TODO.md"]);
    let feedback = format!(
        "This change's pull request conflicts with `{}`, now merged into the working tree. \
         Resolve the conflict markers in these files, keeping both sides' intent, and run \
         the tests. Do not commit:\n{conflicted}",
        run.default_branch
    );
    run.base = base;
    run.feedback = Some(feedback.clone());
    run.approved_tree = None;
    run.approved_head = None;
    run.approved_by = None;
    run.state = RunState::Running;
    store.update_run(run)?;
    let mut a = attempt(home, cfg, &store.agents()?, run, Some(&feedback));
    let marked = conflicted.lines().find(|f| {
        std::fs::read_to_string(run.worktree.join(f))
            .is_ok_and(|text| text.lines().any(|l| l.starts_with("<<<<<<< ")))
    });
    if run.state == RunState::Ready
        && let Some(file) = marked
    {
        fail(run, &mut a, format!("conflict markers remain in {file}"));
    }
    store.update_run(run)?;
    store.insert_attempt(&a)?;
    if consumes(run) {
        store.consume_attempt(&run.project, &attempt_key(run))?;
    }
    Ok(())
}

/// Removes the worktree and its branch. Either may already be gone.
pub fn remove_worktree(repo: &Path, worktree: &Path, branch: &str) -> Result<()> {
    // A clone run's `worktree` is the repository itself.
    let same = || match (worktree.canonicalize(), repo.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => worktree == repo,
    };
    if same() {
        return Err(format!("{} is the repository, not a worktree", worktree.display()).into());
    }
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
            reviewed_tree: None,
            infra: false,
            workspace: Workspace::Worktree,
            start: None,
            denied: Vec::new(),
            review_measured: None,
            pane: None,
            interactive: false,
            agent_state: None,
            items: Vec::new(),
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

        let repo = std::env::temp_dir().join(format!("pma-changelog-{}", std::process::id()));
        std::fs::create_dir_all(&repo).unwrap();
        let run = Run {
            repo: repo.clone(),
            ..run
        };
        assert!(
            !prompt(&run, "", None).contains("CHANGELOG.md"),
            "the project keeps none"
        );
        std::fs::write(repo.join("CHANGELOG.md"), "# Changelog\n").unwrap();
        let p = prompt(&run, "", None);
        assert!(p.contains("Do not edit CHANGELOG.md"), "{p}");
        assert!(p.contains("`.pma/changelog.md`"), "{p}");
        let _ = std::fs::remove_dir_all(repo);
    }

    fn bundle(dropped: &[usize]) -> Run {
        let item = |n: usize, key: &str, gh| RunItem {
            key: key.into(),
            text: format!("task {key}"),
            gh,
            dropped: dropped.contains(&n),
        };
        Run {
            id: 4,
            task_key: BUNDLE.into(),
            text: "task a; task b; task c".into(),
            items: vec![
                item(1, "a", Some(7)),
                item(2, "b", None),
                item(3, "c", Some(9)),
            ],
            ..Run::blank()
        }
    }

    /// A prompt's run is named by its first line, cut at 80 characters; the
    /// agent reads the whole prompt once.
    #[test]
    fn a_prompt_run_carries_the_whole_prompt() {
        assert_eq!(prompt_title("  fix it\nand more"), "fix it");
        let long = "word ".repeat(30);
        let title = prompt_title(&long);
        assert!(
            title.ends_with("...") && title.chars().count() <= 83,
            "{title}"
        );
        let run = Run {
            task_key: PROMPT.into(),
            text: "fix it".into(),
            ..Run::blank()
        };
        let p = prompt(&run, "fix it\nand more\n", None);
        assert!(
            p.contains("Task, from the developer:\n\nfix it\nand more\n"),
            "{p}"
        );
        assert_eq!(p.matches("fix it").count(), 1, "{p}");
    }

    /// A bundle's prompt numbers its tasks, and asks for a summary by number,
    /// which is how the approver says which are done.
    #[test]
    fn a_bundle_prompt_numbers_its_tasks() {
        let p = prompt(&bundle(&[]), "1. task a\n\n2. task b\n", None);
        assert!(
            p.contains("Tasks, numbered:\n\n1. task a\n\n2. task b\n"),
            "{p}"
        );
        assert!(!p.contains("Task: "), "{p}");
        assert!(p.contains("summary, by task number"), "{p}");
    }

    #[test]
    fn a_bundle_closes_what_its_approver_kept() {
        let run = bundle(&[2]);
        assert_eq!(
            closes(&run),
            [("a", "task a", Some(7)), ("c", "task c", Some(9))]
        );
        let one = Run {
            gh: Some(3),
            ..Run::blank()
        };
        assert_eq!(closes(&one), [("fix it", "Fix it", Some(3))]);
        let signal = Run {
            task_key: "ci".into(),
            ..Run::blank()
        };
        assert!(closes(&signal).is_empty());
    }

    /// A ready bundle is published only after an approval that says which
    /// items it finished; an approved one, or a plain ready run, goes on.
    #[test]
    fn a_ready_bundle_needs_its_own_approval() {
        let ready = |run: Run| Run {
            state: RunState::Ready,
            ..run
        };
        assert!(needs_approval(&ready(bundle(&[]))).is_some());
        assert!(needs_approval(&ready(Run::blank())).is_none());
        let approved = Run {
            state: RunState::Approved,
            ..bundle(&[])
        };
        assert!(needs_approval(&approved).is_none());
    }

    #[test]
    fn drops_name_items_the_bundle_has_and_leave_one() {
        let run = bundle(&[]);
        assert!(check_drops(&run, &[]).is_ok());
        assert!(check_drops(&run, &[1, 3]).is_ok());
        let err = check_drops(&run, &[4]).unwrap_err().to_string();
        assert_eq!(err, "run #4 has items 1 to 3; there is no item 4");
        assert!(check_drops(&run, &[0]).is_err());
        let err = check_drops(&run, &[1, 2, 3]).unwrap_err().to_string();
        assert!(err.contains("reject the run instead"), "{err}");
        let err = check_drops(&Run::blank(), &[1]).unwrap_err().to_string();
        assert!(err.contains("is not a bundle"), "{err}");
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
        let refresh = Run {
            agent: crate::worker::REFRESH.into(),
            ..task.clone()
        };
        assert_eq!(attempt_key(&refresh), "fix the parser #refresh");
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
