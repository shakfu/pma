use super::*;

/// The right to run agents and change worktrees: over every project, or over
/// the ones named, so work on one does not block work on another. `flock`
/// releases it when the process exits, so a crash leaves no stale lock, and
/// that makes a lock held the proof that a run is live. Design:
/// docs/dev/project-locks.md.
pub struct Session {
    _files: Vec<File>,
    /// `None` for every project.
    pub(super) projects: Option<Vec<String>>,
}

impl Session {
    /// `None` when another process holds the lock.
    pub fn try_acquire(dir: &Path) -> Result<Option<Session>> {
        Session::try_lock(dir, "session.lock")
    }

    /// The same exclusive lock on another file in `dir`, such as the job
    /// runner's.
    pub fn try_lock(dir: &Path, name: &str) -> Result<Option<Session>> {
        let path = dir.join(name);
        let io = |e: std::io::Error| format!("{}: {e}", path.display());
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(io)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => return Err(io(e).into()),
        }
        file.set_len(0).map_err(io)?;
        write!(file, "{}", std::process::id()).map_err(io)?;
        Ok(Some(Session {
            _files: vec![file],
            projects: None,
        }))
    }

    /// The named projects, or `None` when something holds one of them or every
    /// project. Every project is held by taking `session.lock` exclusively, and
    /// one by holding it shared, so the two exclude each other.
    pub fn try_projects(dir: &Path, projects: &[String]) -> Result<Option<Session>> {
        let mut projects = projects.to_vec();
        projects.sort();
        projects.dedup();
        let path = dir.join("session.lock");
        let io = |e: std::io::Error| format!("{}: {e}", path.display());
        let shared = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(io)?;
        match shared.try_lock_shared() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => return Err(io(e).into()),
        }
        let mut files = vec![shared];
        for project in &projects {
            // A name from the command line, not yet checked against the store.
            let file = format!("project-{}.lock", project.replace('/', "_"));
            match Session::try_lock(dir, &file)? {
                Some(mut held) => files.append(&mut held._files),
                None => return Ok(None),
            }
        }
        Ok(Some(Session {
            _files: files,
            projects: Some(projects),
        }))
    }

    /// `try_projects`, waiting out a brief hold as `acquire` does.
    pub fn acquire_projects(dir: &Path, projects: &[String]) -> Result<Session> {
        const WAIT: Duration = Duration::from_secs(2);
        let started = std::time::Instant::now();
        loop {
            if let Some(session) = Session::try_projects(dir, projects)? {
                return Ok(session);
            }
            if started.elapsed() >= WAIT {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let holder = projects
            .iter()
            .filter_map(|p| std::fs::read_to_string(dir.join(format!("project-{p}.lock"))).ok())
            .map(|pid| pid.trim().to_string())
            .find(|pid| !pid.is_empty())
            .or_else(|| {
                std::fs::read_to_string(dir.join("session.lock"))
                    .ok()
                    .map(|pid| pid.trim().to_string())
            })
            .filter(|pid| !pid.is_empty())
            .map_or(String::new(), |pid| format!(" (pid {pid})"));
        Err(format!(
            "another pma session{holder} is working on {}; wait for it to finish",
            projects.join(", ")
        )
        .into())
    }

    /// Like `try_acquire`, but waits out a brief hold, such as `pma review`
    /// checking for a live session. A lock still held is an error naming its
    /// holder.
    pub fn acquire(dir: &Path) -> Result<Session> {
        const WAIT: Duration = Duration::from_secs(2);
        let started = std::time::Instant::now();
        loop {
            if let Some(session) = Session::try_acquire(dir)? {
                return Ok(session);
            }
            if started.elapsed() >= WAIT {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err({
            let pid = std::fs::read_to_string(dir.join("session.lock")).unwrap_or_default();
            let holder = match pid.trim() {
                "" => String::new(),
                pid => format!(" (pid {pid})"),
            };
            format!(
                "another pma session{holder} is dispatching, reworking, rejecting or publishing; wait for it to finish"
            )
            .into()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::*;

    #[test]
    fn one_session_holds_the_lock_until_dropped() {
        let dir = scratch("session");
        let first = Session::acquire(&dir).unwrap();
        assert!(Session::try_acquire(&dir).unwrap().is_none());
        let err = Session::acquire(&dir).err().unwrap().to_string();
        assert!(
            err.contains(&format!("(pid {})", std::process::id())),
            "{err}"
        );
        drop(first);
        assert!(Session::try_acquire(&dir).unwrap().is_some());

        let brief = Session::acquire(&dir).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(brief);
        });
        assert!(Session::acquire(&dir).is_ok(), "a brief hold is waited out");
        release.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Work on one project leaves another free, and work over every project
    /// excludes both. Failing interrupted runs stays within what is held.
    #[test]
    fn a_project_lock_holds_only_its_project() {
        let dir = scratch("projectlocks");
        let store = Store::open(&dir.join("p.db")).unwrap();
        let names = |n: &str| vec![n.to_string()];
        let live = |project: &str| {
            let mut run = Run {
                project: project.into(),
                state: RunState::Running,
                ..Default::default()
            };
            store.insert_run(&mut run).unwrap();
            run.id
        };
        let (a_run, b_run) = (live("a"), live("b"));

        // `acquire`: a test thread forking a child can hold a lock for a moment.
        let a = Session::acquire_projects(&dir, &names("a")).unwrap();
        assert!(Session::try_projects(&dir, &names("a")).unwrap().is_none());
        assert!(Session::try_acquire(&dir).unwrap().is_none());
        let b = Session::acquire_projects(&dir, &names("b")).unwrap();

        assert_eq!(store.fail_interrupted_runs(&a).unwrap(), 1);
        assert_eq!(store.run(a_run).unwrap().state, RunState::Failed);
        assert_eq!(store.run(b_run).unwrap().state, RunState::Running);

        drop((a, b));
        let all = Session::acquire(&dir).unwrap();
        assert!(Session::try_projects(&dir, &names("a")).unwrap().is_none());
        assert_eq!(store.fail_interrupted_runs(&all).unwrap(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
