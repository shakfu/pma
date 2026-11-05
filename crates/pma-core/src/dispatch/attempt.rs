use super::*;
use crate::minos::{Link, Sidecar};

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

/// One attempt, then, for a clone run, a record of what it changed: the working
/// tree is the developer's from here on, so nothing later can say which edits
/// were the agent's.
#[must_use]
pub(super) fn attempt(
    home: &Path,
    cfg: &Config,
    workers: &[Worker],
    run: &mut Run,
    feedback: Option<&str>,
) -> Attempt {
    // `prepare` left this to run beside the agent. Not on a rework, which has
    // the first attempt's result.
    let beside = (cfg.verify_base_alongside
        && !run.interactive
        && feedback.is_none()
        && run.verify_base_ok.is_none()
        && run.verify_base_seconds.is_none())
    .then_some(())
    .and(run.verify.clone())
    .map(|command| {
        let (home, cfg) = (home.to_path_buf(), cfg.clone());
        let (project, repo) = (run.project.clone(), run.repo.clone());
        let at = run.start.clone().unwrap_or_else(|| run.base.clone());
        std::thread::spawn(move || {
            let measured = base_tree(&home, &project, &repo, &at).and_then(|tree| {
                let store = Store::open_default()?;
                measure_base(&store, &home, &cfg, &project, &at, &command, &tree)
            });
            measured
                .map(|(ok, seconds, _)| (ok, seconds))
                .map_err(|e| e.to_string())
        })
    });
    let a = attempt_once(home, cfg, workers, run, feedback);
    if let Some(thread) = beside {
        match thread.join() {
            Ok(Ok((ok, seconds))) => {
                run.verify_base_ok = ok;
                run.verify_base_seconds = Some(seconds);
            }
            // Unknown, as a base that could not start is: review says so.
            Ok(Err(e)) => eprintln!("warning: #{}: base not verified: {e}", run.id),
            Err(_) => eprintln!("warning: #{}: base not verified", run.id),
        }
    }
    if run.workspace == Workspace::Clone
        && let Err(e) = record_change(home, run)
    {
        eprintln!("warning: #{}: its change was not recorded: {e}", run.id);
    }
    a
}

/// A checkout of `at` for measuring a base beside an agent. Kept between runs,
/// with its ignored files, so a build there is not cold each time; the project's
/// lock keeps it to one run.
fn base_tree(home: &Path, project: &str, repo: &Path, at: &str) -> Result<PathBuf> {
    let tree = home.join("verify-trees").join(format!("{project}.base"));
    if tree.join(".git").exists() {
        git(&tree, &["checkout", "--quiet", "--detach", "--force", at])?;
        git(&tree, &["clean", "-fdq"])?;
    } else {
        if let Some(parent) = tree.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        // A directory git lost track of is not reused.
        let _ = git(repo, &["worktree", "prune"]);
        git(
            repo,
            &[
                "worktree",
                "add",
                "--detach",
                "--force",
                "--quiet",
                path_arg(&tree)?,
                at,
            ],
        )?;
    }
    Ok(tree)
}

/// Where a clone run's change is kept, as a patch: a file rather than a commit,
/// which `git gc` would in time remove.
pub(super) fn change_record(home: &Path, run: &Run) -> PathBuf {
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
pub(super) fn clone_moved(run: &Run) -> bool {
    run.workspace == Workspace::Clone
        && git(&run.repo, &["rev-parse", "HEAD"]).is_ok_and(|head| head != run.base)
}

/// One agent run in the run's worktree, then verification. With `feedback`,
/// the reviewer's notes follow the original prompt.
///
/// The run accumulates cost and time across attempts, and keeps only the
/// latest summary and verify result. The returned `Attempt` holds this
/// attempt's own values; the caller appends it, because the worker threads
/// have no store.
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
    if let Err(e) = attempt_steps(home, cfg, workers, run, &mut a, feedback) {
        fail(run, &mut a, e);
    }
    a
}

/// What the steps of one attempt share.
struct Session<'a> {
    cfg: &'a Config,
    worker: &'a Worker,
    /// The run's log directory.
    logs: PathBuf,
    agent_env: PathBuf,
    /// The attempt's number, which names its logs.
    n: usize,
    timeout: Duration,
}

impl Session<'_> {
    fn log(&self, kind: &str) -> PathBuf {
        self.logs.join(format!("{kind}-{}.log", self.n))
    }
}

/// How the agent starts: its command, and how it reaches its room, if any.
struct Launch<'l> {
    cmd: Command,
    /// The agent under a broker that holds its pipes.
    wrapped: Option<(&'l Link, Command)>,
    /// A broker beside the agent that serves the room's socket.
    side: Option<(Sidecar, &'l Link)>,
    room: Option<String>,
}

/// The steps of `attempt_once`. An error fails the run and the attempt.
fn attempt_steps(
    home: &Path,
    cfg: &Config,
    workers: &[Worker],
    run: &mut Run,
    a: &mut Attempt,
    feedback: Option<&str>,
) -> std::result::Result<(), String> {
    let worker = workers
        .iter()
        .find(|w| w.name == run.agent)
        .ok_or_else(|| format!("unknown agent `{}`; see `pma agent`", run.agent))?;
    let logs = home.join("runs").join(run.id.to_string());
    std::fs::create_dir_all(&logs).map_err(|e| format!("{}: {e}", logs.display()))?;
    let n = (1..)
        .find(|n| !logs.join(format!("agent-{n}.log")).exists())
        .unwrap_or(1);
    let s = Session {
        cfg,
        worker,
        logs,
        agent_env: home.join("agent-env"),
        n,
        timeout: Duration::from_secs(cfg.timeout as u64 * 60),
    };

    a.prompt = match feedback {
        Some(f) => format!(
            "{}\nYour previous attempt is already in the working tree. The reviewer's feedback:\n\n{f}\n",
            run.prompt
        ),
        None => run.prompt.clone(),
    };
    a.verify = run.verify.clone();
    // Not for a run a person drives: they are at its terminal already.
    let link = match cfg.minos.enabled && !run.interactive {
        true => crate::minos::open(&cfg.minos, home, run, n)
            .inspect_err(|e| eprintln!("warning: #{}: no minos room: {e}", run.id))
            .ok(),
        false => None,
    };
    let launch = launch(&s, run, a, link.as_ref())?;
    let failure = host_agent(&s, run, a, launch)?;
    finish(&s, run, a, failure)
}

/// Builds the agent's confined command and connects it to the room. The
/// worker's template starts the broker, the broker holds a relay's pipes, or
/// a broker beside the agent serves the socket.
fn launch<'l>(
    s: &Session,
    run: &Run,
    a: &mut Attempt,
    link: Option<&'l Link>,
) -> std::result::Result<Launch<'l>, String> {
    let (cfg, worker) = (s.cfg, s.worker);
    let inner = s.timeout.saturating_sub(INNER_GRACE);
    let own = link.filter(|_| worker.links_itself());
    let relay = link.filter(|_| own.is_none()).and_then(|l| {
        let cmd = worker.build_relay(
            &run.worktree,
            run.model.as_deref(),
            cfg.agent_budget,
            &run.extra_args,
            inner,
            l,
        )?;
        Some((l, cmd))
    });
    let mut side = None;
    if let Some(l) = link.filter(|_| own.is_none() && relay.is_none()) {
        match crate::minos::serve(&cfg.minos, l, &s.log("minosb")) {
            Ok(broker) => {
                broker.say(&format!("Run #{} started: {}", run.id, run.text));
                a.prompt.push_str(crate::minos::SIDECAR_NOTE);
                side = Some((broker, l));
            }
            Err(e) => eprintln!("warning: #{}: no minos room: {e}", run.id),
        }
    }
    let built = match run.interactive {
        true => worker.build_interactive(
            &a.prompt,
            &run.worktree,
            run.model.as_deref(),
            &run.extra_args,
        ),
        false => Some(worker.build_linked(
            &a.prompt,
            &run.worktree,
            run.model.as_deref(),
            cfg.agent_budget,
            &run.extra_args,
            inner,
            own,
        )),
    };
    let cmd = built.ok_or_else(|| no_interactive(&worker.name))?;
    let mut cmd = confine_agent(worker, run, &s.agent_env, cmd)?;
    if let Some((_, l)) = &side {
        cmd.env("MINOS_SOCKET", &l.socket);
    }
    let wrapped = relay.and_then(|(l, relay_cmd)| {
        let adapter = worker.relay.as_deref().unwrap_or_default();
        confine_agent(worker, run, &s.agent_env, relay_cmd)
            .and_then(|c| crate::minos::wrap(&cfg.minos, l, adapter, &a.prompt, &c))
            .inspect_err(|e| eprintln!("warning: #{}: no minos room: {e}", run.id))
            .ok()
            .map(|w| (l, w))
    });
    let room = own
        .or(wrapped.as_ref().map(|(l, _)| *l))
        .or(side.as_ref().map(|(_, l)| *l))
        .map(|l| l.room.clone());
    Ok(Launch {
        cmd,
        wrapped,
        side,
        room,
    })
}

/// Runs the agent to its end and records its report on the run and the
/// attempt. Returns why the agent failed the run, if it did, for `finish` to
/// apply once the tree is read.
fn host_agent(
    s: &Session,
    run: &mut Run,
    a: &mut Attempt,
    launch: Launch,
) -> std::result::Result<Option<String>, String> {
    let Launch {
        cmd,
        wrapped,
        side,
        mut room,
    } = launch;
    let (cfg, worker, id) = (s.cfg, s.worker, run.id);
    let set_room = |room: Option<&str>| {
        if let Err(e) = Store::open_default().and_then(|s| s.set_room(id, room)) {
            eprintln!("warning: #{id}: {e}");
        }
    };
    if cfg.minos.enabled {
        set_room(room.as_deref());
    }
    a.started_at = crate::dates::now();
    run.error = None;
    let log = s.log("agent");
    let title = format!("#{} {}", run.id, run.text);
    let (project, interactive) = (run.project.clone(), run.interactive);
    let host = |cmd: Command| {
        crate::herdr::host(
            cfg,
            crate::herdr::Host {
                project: &project,
                title: &title,
                interactive,
                close_after: false,
            },
            cmd,
            &log,
            s.timeout,
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
        )
    };
    // A broker that never entered its room ran no agent: the run goes on
    // without the room rather than failing for it.
    let mut relayed = false;
    let hosted = match wrapped {
        Some((l, w)) => {
            let hosted = host(w);
            if crate::minos::ready(&l.events) {
                relayed = true;
                hosted
            } else {
                let why = agent::read_log(&log).map_or_else(String::new, |t| agent::tail(&t, 1));
                eprintln!(
                    "warning: #{id}: minosb did not enter room {}: {why}; running without it",
                    l.room
                );
                room = None;
                set_room(None);
                host(cmd)
            }
        }
        None => host(cmd),
    };
    run.room = room;
    let finished = hosted.map_err(|e| format!("{}: {e}", worker.command))?;
    let output = || agent::read_log(&log).unwrap_or_default();
    // A person drove it in a terminal, so there is no event stream to read.
    let report = match run.interactive {
        true => agent::Report {
            ok: finished.success == Some(true),
            summary: "interactive session in herdr; it reports no summary or cost".into(),
            cost_usd: None,
            denied: Vec::new(),
        },
        // A person's correction starts another turn of the same session.
        false if relayed => agent::parse_relayed(&output()),
        false => worker
            .parse
            .report(&output(), finished.success == Some(true)),
    };
    if let Some((broker, _)) = &side {
        let first = report.summary.lines().next().unwrap_or_default();
        broker.say(&format!("Run #{id} ended: {first}"));
    }
    drop(side);
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
    Ok(agent_failure(
        finished.success,
        report.ok,
        cfg.timeout,
        &log,
    ))
}

/// Reads what the agent left in the tree, then verifies it. `failure`, from
/// `host_agent`, fails the run after the tree is read, so a failed run still
/// records its commits, diffstat and changed paths.
fn finish(
    s: &Session,
    run: &mut Run,
    a: &mut Attempt,
    failure: Option<String>,
) -> std::result::Result<(), String> {
    // A regular file only: a symlink could copy any file pma can read into
    // the review.
    let drafted = run.worktree.join(CHANGELOG_DRAFT);
    if std::fs::symlink_metadata(&drafted).is_ok_and(|m| m.is_file())
        && std::fs::copy(&drafted, s.logs.join("changelog.md")).is_ok()
    {
        let _ = std::fs::remove_file(&drafted);
        let _ = std::fs::remove_dir(run.worktree.join(".pma"));
    }
    // Before any git call in the tree: what follows would run under a config
    // or hooks the agent wrote.
    unchanged_git(run).map_err(|e| e.to_string())?;
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
    if let Some(f) = failure {
        return Err(f);
    }

    run.verify_ok = None;
    if let Some(v) = &run.verify {
        let sandbox = s.cfg.project(&run.project).sandbox;
        let vlog = s.log("verify");
        match verify_once(v, &run.worktree, sandbox, &s.agent_env, &vlog, s.timeout) {
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
    Ok(())
}

/// Why an agent that exited fails its run: `success` is `None` when it was
/// killed at the timeout, and `report_ok` is the worker's own verdict.
fn agent_failure(
    success: Option<bool>,
    report_ok: bool,
    minutes: i64,
    log: &Path,
) -> Option<String> {
    match success {
        None => Some(format!("timed out after {minutes} minutes")),
        Some(_) if !report_ok => Some(format!("agent failed; log: {}", log.display())),
        _ => None,
    }
}

/// `cmd` as an agent may run: the verify allowlist, the run's directory, and
/// no push credential.
fn confine_agent(
    worker: &Worker,
    run: &Run,
    agent_env: &Path,
    mut cmd: Command,
) -> std::result::Result<Command, String> {
    worker.allow_verify(&mut cmd, run.verify.as_deref());
    cmd.current_dir(&run.worktree);
    agent::restrict(&mut cmd, agent_env).map_err(|e| format!("{}: {e}", agent_env.display()))?;
    Ok(cmd)
}

/// Why a worker cannot start a session a person drives.
pub fn no_interactive(agent: &str) -> String {
    format!(
        "agent `{agent}` has no `interactive_args`; set them in settings.toml or with \
         `pma agent set {agent} interactive-args '[...]'`"
    )
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
pub(super) fn fail(run: &mut Run, a: &mut Attempt, error: String) {
    run.state = RunState::Failed;
    run.error = Some(error.clone());
    a.error = Some(error);
    a.outcome = Some(RunState::Failed.name().into());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A timeout reads as one whatever the worker reported; otherwise the
    /// worker's verdict decides, and its exit status alone does not.
    #[test]
    fn an_agent_fails_at_the_timeout_or_by_its_own_report() {
        let log = Path::new("/runs/1/agent-1.log");
        let fails = |success, ok| agent_failure(success, ok, 30, log);
        assert_eq!(
            fails(None, true).as_deref(),
            Some("timed out after 30 minutes")
        );
        assert_eq!(
            fails(None, false).as_deref(),
            Some("timed out after 30 minutes")
        );
        let failed = Some("agent failed; log: /runs/1/agent-1.log");
        assert_eq!(fails(Some(true), false).as_deref(), failed);
        assert_eq!(fails(Some(false), false).as_deref(), failed);
        assert_eq!(fails(Some(true), true), None);
        assert_eq!(fails(Some(false), true), None, "the report decides");
    }
}
