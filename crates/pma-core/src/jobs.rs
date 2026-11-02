//! Jobs: a `pma` command queued to run detached. `--detach` stores the
//! command's arguments and starts a runner unless one holds `runner.lock`.
//! The runner runs each job as a child `pma`, oldest first and side by side
//! where their projects differ, and exits once the queue is empty. Design:
//! `docs/dev/async_dispatch.md`, `docs/dev/project-locks.md`.
//!
//! A job runs under the agent watchdog, so the runner exiting kills the job,
//! and the job exiting kills its agents: nothing outlives the process that
//! recorded it as running.

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::store::{self, Job, Result, Session, Store};

const RUNNER_LOCK: &str = "runner.lock";

/// Set by the runner on the `pma` it starts for a job.
const JOB_VAR: &str = "PMA_JOB";

/// The job this process runs for, if a runner started it.
pub fn current() -> Option<i64> {
    std::env::var(JOB_VAR).ok()?.parse().ok()
}

/// Queues this invocation, less `--detach`, and makes sure a runner will
/// take it. `targets` are the command's project arguments; one names the
/// job's project, for that project's view.
pub fn enqueue(store: &Store, targets: &[String]) -> Result<()> {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--detach")
        .collect();
    let project = crate::api::one_project(targets);
    let id = queue(store, &args, &std::env::current_dir()?, project.as_deref())?;
    println!("job {id} queued; `pma job` lists jobs and `pma job log {id}` shows its output");
    Ok(())
}

/// `project` is the one the command is about, for the project's view.
pub fn queue(
    store: &Store,
    args: &[String],
    cwd: &std::path::Path,
    project: Option<&str>,
) -> Result<i64> {
    let id = store.add_job(args, cwd, project)?;
    spawn_runner()?;
    Ok(id)
}

/// Queues the command that approves the plan a job stopped at. The plan is
/// checked when that job runs: one that changed since is refused, as it is
/// in the foreground.
pub fn approve(store: &Store, id: i64) -> Result<String> {
    let job = store.job(id)?;
    let Some(args) = job.awaiting.filter(|_| job.state == "awaiting-approval") else {
        return Err(format!("job {id} is {}, not awaiting approval", job.state).into());
    };
    if !store.approve_job(id)? {
        return Err(format!("job {id} was approved already").into());
    }
    let next = queue(store, &args, &job.cwd, job.project.as_deref())?;
    Ok(format!("job {next} queued: pma {}", args.join(" ")))
}

/// Starts `pma runner` in a process group of its own, with no terminal, so
/// closing the terminal does not end it. One that finds the lock held exits.
fn spawn_runner() -> Result<()> {
    use std::os::unix::process::CommandExt;

    let state = store::state_home()?;
    std::fs::create_dir_all(&state).map_err(|e| format!("{}: {e}", state.display()))?;
    let path = state.join("runner.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Command::new(std::env::current_exe()?)
        .arg("runner")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()?;
    Ok(())
}

/// What a job needs to itself while it runs.
#[derive(Debug, PartialEq)]
enum Needs {
    /// Reads and writes no worktree a run holds.
    Nothing,
    Project(String),
    Everything,
}

/// A command takes a project's lock only if it is listed here, and only a job
/// about one project can use it. Anything else takes every project, as a
/// workflow, whose nodes may touch any of them, must.
fn needs(job: &Job) -> Needs {
    let command = job.args.first().map_or("", String::as_str);
    match (command, &job.project) {
        // A person in a shell: it holds no lock, so other work goes on.
        ("scan" | "shell", _) => Needs::Nothing,
        (
            "dispatch" | "review" | "push" | "pr" | "stage" | "commit" | "merge" | "verify"
            | "deps",
            Some(project),
        ) => Needs::Project(project.clone()),
        _ => Needs::Everything,
    }
}

fn interactive(job: &Job) -> bool {
    job.args.iter().any(|a| a == "--interactive")
}

/// Whether a dispatch must wait for its clone: one run holds a clone until it
/// is published or rejected. It stays queued rather than failing, and a later
/// job in the project, which may be the push that frees the clone, goes first.
fn waits_for_clone(store: &Store, job: &Job, project: &str) -> Result<bool> {
    if job.args.first().is_none_or(|c| c != "dispatch")
        || job.args.iter().any(|a| a == "--worktree" || a == "--local")
        || crate::api::config(store)?.workspace != crate::config::Workspace::Clone
    {
        return Ok(false);
    }
    let Some(row) = store.project(project)? else {
        return Ok(false);
    };
    Ok(crate::dispatch::clone_busy(&store.runs()?, &row.path).is_some())
}

/// A job taken, on its own thread.
struct Running {
    needs: Needs,
    /// A person is driving it, so it takes none of `max_parallel`.
    interactive: bool,
    thread: std::thread::JoinHandle<()>,
}

/// The oldest queued job that can start now. Jobs in a project keep their order,
/// and one that needs everything stops later ones from overtaking it, so it
/// cannot wait forever.
fn next_job(store: &Store, running: &[Running], slots: usize) -> Result<Option<Job>> {
    let everything = running.iter().any(|r| r.needs == Needs::Everything);
    let used = running
        .iter()
        .filter(|r| !r.interactive && matches!(r.needs, Needs::Project(_)))
        .count();
    let mut held: Vec<&str> = running
        .iter()
        .filter_map(|r| match &r.needs {
            Needs::Project(p) => Some(p.as_str()),
            _ => None,
        })
        .collect();
    let queued = store.queued_jobs()?;
    for job in &queued {
        match needs(job) {
            Needs::Nothing => return Ok(Some(job.clone())),
            Needs::Everything => {
                if running.is_empty() {
                    return Ok(Some(job.clone()));
                }
                return Ok(None);
            }
            Needs::Project(_) if everything => return Ok(None),
            Needs::Project(project) => {
                if waits_for_clone(store, job, &project)? {
                    continue;
                }
                let free = !held.contains(&project.as_str());
                if free && (interactive(job) || used < slots) {
                    return Ok(Some(job.clone()));
                }
                // A later job in this project must not overtake this one.
                held.push(job.project.as_deref().unwrap_or_default());
            }
        }
    }
    Ok(None)
}

/// Runs queued jobs until none can ever start: none left, and none running. A
/// dispatch waiting for its clone keeps this alive, polling. The lock is
/// released before the last check, so a job queued while it was held is not
/// stranded: its command's runner found the lock held and exited.
pub fn run() -> Result<()> {
    let home = store::home()?;
    loop {
        let Some(lock) = Session::try_lock(&home, RUNNER_LOCK)? else {
            return Ok(());
        };
        let store = Store::open_default()?;
        store.fail_running_jobs()?;
        let mut running: Vec<Running> = Vec::new();
        loop {
            running.retain(|r| !r.thread.is_finished());
            let slots = crate::api::config(&store)?.max_parallel.max(1) as usize;
            if let Some(job) = next_job(&store, &running, slots)? {
                let Some(job) = store.claim_job(job.id)? else {
                    continue;
                };
                let (needs, interactive) = (needs(&job), interactive(&job));
                let scope = match &needs {
                    Needs::Nothing => Scope::None,
                    Needs::Project(p) => Scope::Project(p.clone()),
                    Needs::Everything => Scope::All,
                };
                let home = home.clone();
                let thread = std::thread::spawn(move || {
                    // Each thread its own connection: a `Store` is not shared.
                    let Ok(store) = Store::open_default() else {
                        return;
                    };
                    if let Err(e) = run_one(&store, &home, &job, &scope) {
                        eprintln!("job {}: {e}", job.id);
                        let _ = store.finish_job(job.id, None);
                    }
                    if let Err(e) = notify(&store, job.id) {
                        eprintln!("job {}: notify: {e}", job.id);
                    }
                });
                running.push(Running {
                    needs,
                    interactive,
                    thread,
                });
                continue;
            }
            if running.is_empty() && !store.has_queued_jobs()? {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        drop(lock);
        if !store.has_queued_jobs()? {
            return Ok(());
        }
    }
}

/// What a job's command will lock when it starts.
enum Scope {
    None,
    Project(String),
    All,
}

fn run_one(store: &Store, home: &std::path::Path, job: &Job, scope: &Scope) -> Result<()> {
    // A command run in the foreground may hold the lock. Waiting here rather
    // than in the job keeps the job from failing on it.
    loop {
        let free = match scope {
            Scope::None => true,
            Scope::Project(p) => Session::try_projects(home, std::slice::from_ref(p))?.is_some(),
            Scope::All => Session::try_acquire(home)?.is_some(),
        };
        if free {
            break;
        }
        if store.job(job.id)?.state == "stopped" {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    let log = log_path(job.id)?;
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(&job.args)
        .current_dir(&job.cwd)
        .env(JOB_VAR, job.id.to_string());
    let (mut child, lifeline) = crate::agent::spawn_guarded(cmd, None, &log)?;
    store.set_job_pid(job.id, child.id())?;
    // `stop` read no pid if it ran before the line above; it kills here.
    if store.job(job.id)?.state == "stopped" {
        kill_group(child.id().into());
    }
    let status = child.wait()?;
    // The watchdog's reader is still waiting on the pipe; closing it ends
    // the group.
    drop(lifeline);
    store.finish_job(job.id, status.code())?;
    Ok(())
}

/// Runs `notify` once a job has ended. Bounded and logged like an agent, so
/// a hung command cannot hold the queue.
fn notify(store: &Store, id: i64) -> Result<()> {
    let rows = store.config_rows()?;
    let cfg =
        crate::config::Config::with_overrides(rows.iter().map(|(k, v)| (k.as_str(), v.as_str())))?;
    let Some(command) = cfg.notify.filter(|c| !c.trim().is_empty()) else {
        return Ok(());
    };
    let job = store.job(id)?;
    let mut summary = format!("pma job {id} {}: pma {}", job.state, job.args.join(" "));
    if job.state == "awaiting-approval" {
        summary.push_str(&format!("; `pma job approve {id}` runs its plan"));
    }
    let mut cmd = Command::new("sh");
    cmd.args(["-c", &command])
        .env("PMA_JOB_ID", id.to_string())
        .env("PMA_JOB_STATE", &job.state)
        .env("PMA_JOB_SUMMARY", &summary);
    let log = log_path(id)?.with_extension("notify.log");
    crate::agent::run_limited(cmd, None, &log, Duration::from_secs(30))?;
    Ok(())
}

pub fn log_path(id: i64) -> Result<PathBuf> {
    let dir = store::state_home()?.join("jobs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir.join(format!("{id}.log")))
}

/// Stops a queued job, or kills a running one's process group. Its agents
/// die with it; its runs are marked failed when the next session starts.
pub fn stop(store: &Store, id: i64) -> Result<String> {
    let job = store.job(id)?;
    let was = store.stop_job(id)?;
    Ok(match was.as_str() {
        "queued" => format!("job {id} stopped before it ran"),
        "running" => {
            if let Some(pid) = job.pid {
                kill_group(pid);
            }
            format!("job {id} stopped; its agents were killed")
        }
        other => format!("job {id} is {other}; nothing to stop"),
    })
}

fn kill_group(pid: i64) {
    let _ = Command::new("kill")
        .args(["-TERM", "--", &format!("-{pid}")])
        .status();
}

/// One line per job: id, state, exit code, and the command.
pub fn list(jobs: &[Job]) -> String {
    if jobs.is_empty() {
        return "no jobs. `--detach` on `pma dispatch` or `pma workflow run` queues one\n".into();
    }
    let mut out = String::new();
    for j in jobs {
        let exit = j.exit_code.map_or("-".into(), |c| c.to_string());
        out.push_str(&format!(
            "#{:<4} {:<17} exit {:<3} {}  pma {}\n",
            j.id,
            j.state,
            exit,
            crate::dates::format(crate::dates::day(j.created_at)),
            j.args.join(" ")
        ));
        if j.state == "awaiting-approval" {
            out.push_str(&format!(
                "      `pma job log {}` shows the plan; `pma job approve {}` runs it\n",
                j.id, j.id
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(args: &[&str], project: Option<&str>) -> Job {
        Job {
            id: 1,
            args: args.iter().map(|a| a.to_string()).collect(),
            cwd: "/".into(),
            state: "queued".into(),
            created_at: 0,
            started_at: None,
            finished_at: None,
            pid: None,
            exit_code: None,
            awaiting: None,
            project: project.map(String::from),
        }
    }

    /// A shell waits on a person for as long as they like, so it holds no
    /// lock: the project's runs and other jobs go on beside it.
    #[test]
    fn a_shell_holds_no_lock() {
        assert!(matches!(
            needs(&job(&["shell", "p"], Some("p"))),
            Needs::Nothing
        ));
        assert!(matches!(
            needs(&job(&["dispatch", "p:1"], Some("p"))),
            Needs::Project(_)
        ));
    }
}
