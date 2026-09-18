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
use crate::config::Config;
use crate::scan;
use crate::store::{Attempt, Result, Run, RunState, Store};
use crate::todo;

/// A task chosen for dispatch.
#[derive(Debug, Clone)]
pub struct Pick {
    pub project: String,
    pub repo: PathBuf,
    /// The item's key, or a signal: `ci` or `deps`.
    pub key: String,
    pub text: String,
    pub gh: Option<i64>,
    /// The project's tier, recorded on the run because tiers change.
    pub tier: Option<u8>,
    pub quadrant: Option<String>,
}

/// Whether a task key names a signal rather than a TODO.md item.
pub fn is_signal(key: &str) -> bool {
    matches!(key, "ci" | "deps")
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

/// Whether this attempt counts against the limit. A run that never reached
/// the agent, or whose agent could not start or was killed at the timeout,
/// says nothing about the task's suitability.
fn consumes(run: &Run) -> bool {
    run.state == RunState::Ready && run.verify_ok == Some(false)
}

/// Lines of failed CI log carried in a fix-CI prompt.
const CI_LOG_LINES: usize = 200;

/// Runs `git -C dir args`, returning trimmed stdout or an error naming the
/// command and its stderr.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
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
pub fn prepare(store: &Store, home: &Path, cfg: &Config, pick: &Pick) -> Result<Prepared> {
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
    let class = Class::of(&pick.key, &tags);
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

    let verify = verify_command(cfg, &pick.project, &worktree);
    // The worktree is a clean checkout of the base, so this is the base
    // tree. One verify at the head alone cannot tell a regression from a
    // repository that was already failing.
    let (verify_base_ok, verify_base_seconds) = match &verify {
        Some(v) => base_verify(store, home, cfg, &pick.project, &base, v, &worktree)?,
        None => (None, None),
    };
    let mut run = Run {
        id: 0,
        project: pick.project.clone(),
        task_key: pick.key.clone(),
        text: pick.text.clone(),
        gh: pick.gh,
        quadrant: pick.quadrant.clone(),
        agent: agent::AGENT.into(),
        repo: repo.clone(),
        branch,
        worktree,
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
        scope: class.scope(),
        tier: pick.tier,
        description: (!details.trim().is_empty()).then(|| details.clone()),
        agent_budget: Some(cfg.agent_budget),
        timeout_minutes: Some(cfg.timeout),
        verify_base_ok,
        verify_base_seconds,
        changed_paths: None,
        scope_error: None,
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
         - Do not edit TODO.md. pma marks the task done when the change ships.\n",
    );
    if let Some(v) = verify {
        p.push_str(&format!(
            "- Run `{v}` to check your change. pma runs it again after you finish.\n"
        ));
    }
    p.push_str("\nEnd with a short summary of what you changed and what is left undone.\n");
    p
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

/// Runs queued runs, at most `max_parallel` at once, starting a run only while
/// the batch stays within `batch_budget`. `done` sees each run as it finishes.
pub fn execute(
    store: &Store,
    home: &Path,
    cfg: &Config,
    runs: Vec<Run>,
    mut done: impl FnMut(&Run),
) -> Result<Vec<Run>> {
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
    let workers = (cfg.max_parallel as usize).max(1);
    // An attempt rides with the run it belongs to: only this thread has the
    // store, and a row appended after the run's update would be lost on a
    // save error.
    let (tx, rx) = mpsc::channel::<(Run, Option<Attempt>)>();
    let mut finished = Vec::new();
    let mut save_error = None;

    std::thread::scope(|s| {
        for _ in 0..workers {
            let tx = tx.clone();
            let queue = &queue;
            s.spawn(move || {
                loop {
                    let mut run = {
                        let mut q = queue.lock().unwrap();
                        let Some(mut run) = q.waiting.pop_front() else {
                            break;
                        };
                        let committed = q.spent + (q.running + 1) as f64 * cfg.agent_budget;
                        if committed > cfg.batch_budget + 1e-9 {
                            run.state = RunState::Failed;
                            run.error = Some(format!(
                                "not started: batch budget ${} reached",
                                cfg.batch_budget
                            ));
                            // Refused before the agent ran, so it consumes
                            // no attempt.
                            let _ = tx.send((run, None));
                            continue;
                        }
                        q.running += 1;
                        run
                    };
                    run.state = RunState::Running;
                    let _ = tx.send((run.clone(), None));
                    let a = attempt(home, cfg, &mut run, None);
                    {
                        let mut q = queue.lock().unwrap();
                        q.running -= 1;
                        q.spent += run.cost_usd.unwrap_or(0.0);
                    }
                    let _ = tx.send((run, Some(a)));
                }
            });
        }
        drop(tx);
        for (run, attempt) in rx {
            if let Err(e) = store.update_run(&run) {
                save_error.get_or_insert(e.to_string());
            }
            if let Some(a) = attempt {
                if let Err(e) = store.insert_attempt(&a) {
                    save_error.get_or_insert(e.to_string());
                }
                if consumes(&run)
                    && let Err(e) = store.consume_attempt(&run.project, &revision(&run.text))
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
fn attempt(home: &Path, cfg: &Config, run: &mut Run, feedback: Option<&str>) -> Attempt {
    let mut a = Attempt {
        id: 0,
        run_id: run.id,
        n: 0,
        agent: run.agent.clone(),
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
    let logs = home.join("runs").join(run.id.to_string());
    let empty = home.join("empty");
    if let Err(e) = std::fs::create_dir_all(&logs).and_then(|_| std::fs::create_dir_all(&empty)) {
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
    let mut cmd = agent::claude(&a.prompt, cfg.agent_budget, run.verify.as_deref());
    cmd.current_dir(&run.worktree);
    agent::restrict(&mut cmd, &empty);
    a.started_at = crate::dates::now();
    run.error = None;
    let log = logs.join(format!("agent-{n}.log"));
    let finished = match agent::run_limited(cmd, &log, timeout) {
        Ok(f) => f,
        Err(e) => {
            fail(run, &mut a, format!("{}: {e}", agent::AGENT));
            return a;
        }
    };
    let report = agent::parse_claude(&std::fs::read_to_string(&log).unwrap_or_default());
    a.seconds = Some(finished.seconds);
    run.seconds = Some(run.seconds.unwrap_or(0) + finished.seconds);
    // An agent that reports no cost leaves both null. Zero would read as free.
    if let Some(c) = report.cost_usd {
        a.cost_usd = Some(c);
        run.cost_usd = Some(run.cost_usd.unwrap_or(0.0) + c);
    }
    a.summary = Some(report.summary.clone());
    run.summary = Some(report.summary);
    run.commits = git(
        &run.worktree,
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
        match verify_once(v, &run.worktree, &empty, &vlog, timeout) {
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

/// Runs `command` in `worktree` under the agent's stripped environment, the
/// same way the head check runs it. `Ok(None)` means it was killed at the
/// timeout.
fn verify_once(
    command: &str,
    worktree: &Path,
    empty: &Path,
    log: &Path,
    timeout: Duration,
) -> std::io::Result<(Option<bool>, i64)> {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", command]).current_dir(worktree);
    agent::restrict(&mut cmd, empty);
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
    let empty = home.join("empty");
    let logs = home.join("verify-base");
    std::fs::create_dir_all(&empty).map_err(|e| format!("{}: {e}", empty.display()))?;
    std::fs::create_dir_all(&logs).map_err(|e| format!("{}: {e}", logs.display()))?;
    let log = logs.join(format!("{project}-{}.log", &base[..base.len().min(12)]));
    let timeout = Duration::from_secs(cfg.timeout as u64 * 60);
    let (ok, seconds) = match verify_once(command, worktree, &empty, &log, timeout) {
        Ok((success, seconds)) => (success, seconds),
        Err(_) => (None, 0),
    };
    store.set_verify_base(project, base, command, cfg.timeout, ok, seconds)?;
    Ok((ok, Some(seconds)))
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
    git(&run.worktree, &["add", "--all", "--intent-to-add"]).ok()?;
    let stat = git(&run.worktree, &["diff", "--stat", &run.base]).ok()?;
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
    git(&run.worktree, &["add", "--all", "--intent-to-add"])?;
    let out = git(
        &run.worktree,
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
    git(&run.worktree, &["add", "--all", "--intent-to-add"])?;
    git(&run.worktree, &["diff", &run.base])
}

pub fn approve(store: &Store, run: &mut Run) -> Result<()> {
    if run.state != RunState::Ready {
        return Err(format!(
            "run #{} is {}; only a ready run can be approved",
            run.id,
            run.state.name()
        )
        .into());
    }
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
    store.consume_attempt(&run.project, &revision(&run.text))?;
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
    run.state = RunState::Running;
    store.update_run(run)?;
    let a = attempt(home, cfg, run, Some(feedback));
    store.update_run(run)?;
    store.insert_attempt(&a)?;
    if consumes(run) {
        store.consume_attempt(&run.project, &revision(&run.text))?;
    }
    Ok(())
}

/// Removes the worktree and its branch. Either may already be gone.
pub fn remove_worktree(repo: &Path, worktree: &Path, branch: &str) -> Result<()> {
    if worktree.exists() {
        git(
            repo,
            &["worktree", "remove", "--force", path_arg(worktree)?],
        )?;
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
