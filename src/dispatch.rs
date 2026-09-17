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

/// Creates the worktree and branch for `pick` from the remote default branch,
/// and records a queued run.
pub fn prepare(store: &Store, home: &Path, cfg: &Config, pick: &Pick) -> Result<Run> {
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
    let ci_log = if pick.key == "ci" {
        Some(failed_ci_log(repo, &default_branch)?)
    } else {
        None
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
            !path.exists()
                && git(
                    repo,
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("refs/heads/pma/{s}"),
                    ],
                )
                .is_err()
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

    let details = match &ci_log {
        Some(log) => format!(
            "Failing log from `gh run view --log-failed`, last {CI_LOG_LINES} lines:\n\n```\n{log}\n```\n"
        ),
        None if pick.key == "deps" => {
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
        None => match item_description(&worktree, &pick.key, &pick.text) {
            Some(lines) => lines,
            None => {
                remove_worktree(repo, &worktree, &branch)?;
                return Err(format!(
                    "{}: `{}` is not an open item in TODO.md on origin/{default_branch}; commit and push it first",
                    pick.project, pick.text
                )
                .into());
            }
        },
    };
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
    Ok(run)
}

fn path_arg(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()).into())
}

/// The item's description lines, or `None` when it is not open in the file.
fn item_description(worktree: &Path, key: &str, text: &str) -> Option<String> {
    let file = std::fs::read_to_string(worktree.join("TODO.md")).ok()?;
    let item = todo::parse(&file)
        .items
        .into_iter()
        .find(|i| !i.done && i.is_task(key, text))?;
    Some(
        item.description
            .iter()
            .map(|l| format!("{}\n", l.trim_start()))
            .collect(),
    )
}

fn failed_ci_log(repo: &Path, branch: &str) -> Result<String> {
    let url = git(repo, &["remote", "get-url", "origin"])?;
    let slug = scan::github_slug(&url).ok_or("origin is not on GitHub")?;
    use crate::sync::gh;
    let id = gh(&[
        "run",
        "list",
        "-R",
        &slug,
        "--branch",
        branch,
        "--status",
        "failure",
        "--limit",
        "1",
        "--json",
        "databaseId",
        "--jq",
        ".[0].databaseId",
    ])?;
    let log = gh(&["run", "view", id.trim(), "-R", &slug, "--log-failed"])?;
    Ok(agent::tail(&log, CI_LOG_LINES))
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
            "- pma runs `{v}` after you finish, to check the change.\n"
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
    let mut cmd = agent::claude(&prompt, cfg.agent_budget);
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

    run.verify = verify_command(cfg, &run.project, &run.worktree);
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
        assert!(p.contains("pma runs `make test`"));
        assert!(!prompt(&run, "", None).contains("pma runs"));
    }
}
