//! Dispatch and review: a worktree per task, agent runs within the batch
//! budget, verification by `pma` itself, and the review actions.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use crate::agent;
use crate::config::Config;
use crate::scan;
use crate::store::{Result, Run, RunState, Store};
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
    pub quadrant: Option<String>,
}

/// Whether a task key names a signal rather than a TODO.md item.
pub fn is_signal(key: &str) -> bool {
    matches!(key, "ci" | "deps")
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

    let details = match pick.key.as_str() {
        "ci" => {
            let Some(scan::Ci::Failing(workflows)) = store.project(&pick.project)?.map(|p| p.ci)
            else {
                return refuse("CI was not failing at the last scan".into());
            };
            match failed_ci_logs(repo, &default_branch, &workflows)? {
                Ok(logs) => logs,
                Err(why) => return refuse(why),
            }
        }
        "deps" => {
            let detail = store
                .project(&pick.project)?
                .map(|p| p.deps_detail)
                .unwrap_or_default();
            format!(
                "Outdated dependencies at the last `pma scan --deps`:\n\n```\n{detail}\n```\n\n\
                 Update them within the project's version constraints first. Change a \
                 constraint only where the tests still pass, and list any dependency you \
                 left behind, with the reason.\n"
            )
        }
        _ => {
            let file = git(repo, &["show", &format!("{base}:TODO.md")]).ok();
            match on_origin(file.as_deref(), &pick.key, &pick.text) {
                OnOrigin::Open(description) => description,
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
        started_at: None,
        seconds: None,
        cost_usd: None,
        summary: None,
        commits: None,
        diffstat: None,
        verify: None,
        verify_ok: None,
        error: None,
        outcome: None,
    };
    run.prompt = prompt(&run, &details, verify.as_deref());
    run.id = store.insert_run(&run)?;
    Ok(Prepared::Queued(Box::new(run)))
}

fn path_arg(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()).into())
}

/// A task's item in the remote `TODO.md`.
#[derive(Debug, PartialEq)]
enum OnOrigin {
    /// Open, with its description lines.
    Open(String),
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
    let (tx, rx) = mpsc::channel::<Run>();
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
                            let _ = tx.send(run);
                            continue;
                        }
                        q.running += 1;
                        run
                    };
                    run.state = RunState::Running;
                    let _ = tx.send(run.clone());
                    attempt(home, cfg, &mut run, None);
                    {
                        let mut q = queue.lock().unwrap();
                        q.running -= 1;
                        q.spent += run.cost_usd.unwrap_or(0.0);
                    }
                    let _ = tx.send(run);
                }
            });
        }
        drop(tx);
        for run in rx {
            if let Err(e) = store.update_run(&run) {
                save_error.get_or_insert(e.to_string());
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
fn attempt(home: &Path, cfg: &Config, run: &mut Run, feedback: Option<&str>) {
    let logs = home.join("runs").join(run.id.to_string());
    let empty = home.join("empty");
    if let Err(e) = std::fs::create_dir_all(&logs).and_then(|_| std::fs::create_dir_all(&empty)) {
        run.state = RunState::Failed;
        run.error = Some(format!("{}: {e}", logs.display()));
        return;
    }
    let n = (1..)
        .find(|n| !logs.join(format!("agent-{n}.log")).exists())
        .unwrap_or(1);
    let timeout = Duration::from_secs(cfg.timeout as u64 * 60);

    let prompt = match feedback {
        Some(f) => format!(
            "{}\nYour previous attempt is already in the working tree. The reviewer's feedback:\n\n{f}\n",
            run.prompt
        ),
        None => run.prompt.clone(),
    };
    run.verify = verify_command(cfg, &run.project, &run.worktree);
    let mut cmd = agent::claude(&prompt, cfg.agent_budget, run.verify.as_deref());
    cmd.current_dir(&run.worktree);
    agent::restrict(&mut cmd, &empty);
    run.started_at = Some(crate::dates::now());
    run.error = None;
    let log = logs.join(format!("agent-{n}.log"));
    let finished = match agent::run_limited(cmd, &log, timeout) {
        Ok(f) => f,
        Err(e) => {
            run.state = RunState::Failed;
            run.error = Some(format!("{}: {e}", agent::AGENT));
            return;
        }
    };
    let report = agent::parse_claude(&std::fs::read_to_string(&log).unwrap_or_default());
    run.seconds = Some(run.seconds.unwrap_or(0) + finished.seconds);
    if let Some(c) = report.cost_usd {
        run.cost_usd = Some(run.cost_usd.unwrap_or(0.0) + c);
    }
    run.summary = Some(report.summary);
    run.commits = git(
        &run.worktree,
        &["rev-list", "--count", &format!("{}..HEAD", run.base)],
    )
    .ok()
    .and_then(|s| s.parse().ok());
    run.diffstat = diffstat(run);

    let failure = match finished.success {
        None => Some(format!("timed out after {} minutes", cfg.timeout)),
        Some(_) if !report.ok => Some(format!("agent failed; log: {}", log.display())),
        _ => None,
    };
    if let Some(f) = failure {
        run.state = RunState::Failed;
        run.error = Some(f);
        return;
    }

    run.verify_ok = None;
    if let Some(v) = &run.verify {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", v]).current_dir(&run.worktree);
        agent::restrict(&mut cmd, &empty);
        let vlog = logs.join(format!("verify-{n}.log"));
        match agent::run_limited(cmd, &vlog, timeout) {
            Ok(f) => {
                run.seconds = Some(run.seconds.unwrap_or(0) + f.seconds);
                run.verify_ok = Some(f.success == Some(true));
            }
            Err(e) => {
                run.verify_ok = Some(false);
                run.error = Some(format!("verify: {e}"));
            }
        }
    }
    run.state = RunState::Ready;
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
    run.state = RunState::Approved;
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
    run.state = RunState::Rejected;
    store.update_run(run)
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
    attempt(home, cfg, run, Some(feedback));
    store.update_run(run)
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
            started_at: None,
            seconds: None,
            cost_usd: None,
            summary: None,
            commits: None,
            diffstat: None,
            verify: None,
            verify_ok: None,
            error: None,
            outcome: None,
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

    #[test]
    fn items_are_open_done_or_absent_on_origin() {
        let file = "# TODO\n\n## High\n\n- [ ] open one gh:3\n  why\n- [x] finished\n";
        assert_eq!(
            on_origin(Some(file), "gh:3", "open one"),
            OnOrigin::Open("why\n".into())
        );
        assert_eq!(
            on_origin(Some(file), "open one", "open one"),
            OnOrigin::Open("why\n".into()),
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
