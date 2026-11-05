use super::*;

/// Files git tracks in the worktree, as a proxy for how far a change can
/// reach. Zero when it cannot be counted, which reads as a small repository
/// rather than refusing the dispatch.
pub(super) fn tracked_files(worktree: &Path) -> i64 {
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
pub(super) fn base_verify(
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

pub(super) fn base_log(home: &Path, project: &str, base: &str) -> PathBuf {
    home.join("verify-base")
        .join(format!("{project}-{}.log", &base[..base.len().min(12)]))
}

/// Runs `command` in `worktree`, a checkout of `base`, under the agent's
/// environment, and records the result in the base cache. Returns the result,
/// the seconds it took and the log.
pub(super) fn measure_base(
    store: &Store,
    home: &Path,
    cfg: &Config,
    project: &str,
    base: &str,
    command: &str,
    worktree: &Path,
) -> Result<(Option<bool>, i64, PathBuf)> {
    let agent_env = home.join("agent-env");
    let log = base_log(home, project, base);
    if let Some(logs) = log.parent() {
        std::fs::create_dir_all(logs).map_err(|e| format!("{}: {e}", logs.display()))?;
    }
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
