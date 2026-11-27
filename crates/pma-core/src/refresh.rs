//! Keeps the store current after the first `pma scan`. Local state is re-read
//! when a project's files change; CI, which needs the network, on a timer while
//! a front end is open. Design: docs/dev/refresh.md.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::api;
use crate::dates;
use crate::dispatch;
use crate::scan::{self, Facts};
use crate::store::{Result, RunState, Store};

/// How often a front end looks for changed files. 112 projects take 61 ms.
const TICK: Duration = Duration::from_secs(5);

/// How often, in seconds, a front end reads the CI of a project it watches.
const CI_POLL: i64 = 60;

/// How long, in seconds, after a run lands its project's CI is watched while
/// CI has not judged the commit. A run starts within seconds of a push, and
/// `ci_pending` watches it from then on.
const LANDED_WATCH: i64 = 15 * 60;

/// Reads `selected`, leaving unread a clone whose git would run a setting a
/// failed run added.
pub fn read(
    store: &Store,
    selected: Vec<(String, PathBuf)>,
    offline: bool,
    deps: bool,
    finished: &(dyn Fn(&str) + Sync),
) -> Result<Vec<Facts>> {
    let cfg = api::config(store)?;
    let runs = store.runs()?;
    let mut held = Vec::new();
    let mut free = Vec::new();
    for (name, path) in selected {
        match dispatch::clone_held(&runs, &path) {
            Ok(()) => free.push((name, path)),
            Err(e) => held.push(Facts::held(&name, &path, e.to_string())),
        }
    }
    let prior = match offline {
        true => Default::default(),
        false => store.ci_alone()?,
    };
    let mut facts = scan::scan_all(&free, &cfg.activity_ignore, offline, deps, &prior, finished);
    facts.extend(held);
    Ok(facts)
}

/// An offline read that keeps each project's last CI and run links, which it
/// does not ask for and would otherwise record as unknown.
fn offline(store: &Store, projects: &[(String, PathBuf)]) -> Result<Vec<Facts>> {
    let cfg = api::config(store)?;
    let runs = store.runs()?;
    let free: Vec<(String, PathBuf)> = projects
        .iter()
        .filter(|(_, path)| dispatch::clone_held(&runs, path).is_ok())
        .cloned()
        .collect();
    let mut facts = scan::scan_all(
        &free,
        &cfg.activity_ignore,
        true,
        false,
        &Default::default(),
        &|_| {},
    );
    for f in &mut facts {
        if let Some(row) = store.project(&f.name)? {
            (f.ci, f.ci_runs, f.ci_pending) = (row.ci, row.ci_runs, row.ci_pending);
        }
    }
    Ok(facts)
}

/// Rereads `projects`, as after a publish ticks an item.
pub fn rescan(store: &mut Store, projects: &[(String, PathBuf)]) -> Result<()> {
    if projects.is_empty() {
        return Ok(());
    }
    let facts = offline(store, projects)?;
    store.save_scan(&facts, false, dates::now())?;
    Ok(())
}

/// Rereads `projects` with their CI, before a CI task is dispatched, so the
/// task and every front end's row match CI now. A project whose CI cannot be
/// read keeps the last one; the warning says why.
pub fn ci(store: &mut Store, projects: &[(String, PathBuf)]) -> Result<Vec<String>> {
    let mut facts = offline(store, projects)?;
    let prior = store.ci_alone()?;
    let mut warnings = Vec::new();
    for f in &mut facts {
        let kept = prior.get(&f.name).map_or(&[][..], Vec::as_slice);
        match scan::ci_state(&f.path, kept, dates::now()) {
            Ok(read) => {
                (
                    f.ci,
                    f.ci_runs,
                    f.ci_alone,
                    f.ci_seen,
                    f.ci_pending,
                    f.ci_read,
                ) = (
                    read.ci,
                    read.runs,
                    read.alone,
                    read.seen,
                    read.pending,
                    true,
                );
            }
            Err(why) => warnings.push(format!(
                "{}: CI not read, using the last scan's: {why}",
                f.name
            )),
        }
    }
    store.save_quiet_scan(&facts, false, dates::now())?;
    Ok(warnings)
}

/// Rereads `projects` as the timer does, leaving no row in `pma history`: after
/// an edit by hand, whose unstaged changes no stamp sees.
pub fn reread(store: &mut Store, projects: &[(String, PathBuf)]) -> Result<()> {
    let facts = offline(store, projects)?;
    store.save_quiet_scan(&facts, false, dates::now())?;
    Ok(())
}

/// Rereads each project under a root whose files changed, and any new one.
/// Nothing before the first scan, which a person starts. A clone an agent is
/// working in waits until the run ends.
pub fn changed(store: &mut Store) -> Result<usize> {
    if store.last_scan()?.is_none() {
        return Ok(0);
    }
    crate::publish::settle_committed(store)?;
    let (found, _) = scan::discover(&store.roots()?);
    let stamps = store.project_stamps()?;
    let runs = store.runs()?;
    let due: Vec<(String, PathBuf)> = found
        .into_iter()
        .filter(|(name, path)| {
            stamps.get(name) != Some(&scan::stamp(path))
                && !runs
                    .iter()
                    .any(|r| r.state == RunState::Running && r.worktree == *path)
        })
        .collect();
    if due.is_empty() {
        return Ok(0);
    }
    let facts = offline(store, &due)?;
    store.save_quiet_scan(&facts, false, dates::now())?;
    Ok(facts.len())
}

/// Whether CI was last read over `scan.every` minutes ago. Never before the
/// first scan, and never with `scan.every` at 0.
fn ci_due(store: &Store) -> Result<bool> {
    let every = api::config(store)?.scan_every;
    Ok(every > 0
        && store
            .last_full_scan()?
            .is_some_and(|at| dates::now() - at >= every * 60))
}

/// A full scan with CI, as `pma scan` does, that leaves no row in `pma history`
/// unless it found a change.
fn everything(store: &mut Store) -> Result<()> {
    let (found, _) = scan::discover(&store.roots()?);
    let facts = read(store, found, false, false, &|_| {})?;
    store.save_quiet_scan(&facts, true, dates::now())?;
    Ok(())
}

/// Projects whose CI a front end reads every `CI_POLL`: one failing with a
/// newer run still going, and one a run landed on within `LANDED_WATCH` that
/// CI has not judged. A clone an agent is running in waits, as in `changed`.
fn ci_watched(store: &Store, now: i64) -> Result<Vec<(String, PathBuf)>> {
    let runs = store.runs()?;
    Ok(store
        .projects()?
        .into_iter()
        .filter(|p| p.absent_since.is_none())
        .filter(|p| {
            let pending = match &p.ci {
                scan::Ci::Failing(w) => w.iter().any(|w| p.ci_pending.contains(w)),
                _ => false,
            };
            pending
                || runs.iter().any(|r| {
                    r.project == p.name
                        && r.ci_verdict.is_none()
                        && r.landed_at.is_some_and(|at| now - at < LANDED_WATCH)
                })
        })
        .filter(|p| {
            !runs
                .iter()
                .any(|r| r.state == RunState::Running && r.worktree == p.path)
        })
        .map(|p| (p.name, p.path))
        .collect())
}

/// Reads the CI of each watched project not read in the last `CI_POLL`;
/// `read_at` holds when each was last read.
fn watch(store: &mut Store, read_at: &mut HashMap<String, i64>) -> Result<()> {
    let now = dates::now();
    let due: Vec<(String, PathBuf)> = ci_watched(store, now)?
        .into_iter()
        .filter(|(name, _)| read_at.get(name).is_none_or(|at| now - at >= CI_POLL))
        .collect();
    if due.is_empty() {
        return Ok(());
    }
    for (name, _) in &due {
        read_at.insert(name.clone(), now);
    }
    ci(store, &due)?;
    Ok(())
}

/// Keeps the store current while a front end is open. Failures are dropped: a
/// terminal UI has nowhere to print them, and `c` scans by hand.
pub fn start(open: impl Fn() -> Result<Store> + Send + 'static) {
    std::thread::spawn(move || {
        let Ok(mut store) = open() else {
            return;
        };
        let mut read_at = HashMap::new();
        loop {
            std::thread::sleep(TICK);
            let _ = changed(&mut store);
            if ci_due(&store).unwrap_or(false) {
                let _ = everything(&mut store);
            } else {
                let _ = watch(&mut store, &mut read_at);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// The timer watches a failing project whose CI is going, and one a run
    /// landed on until CI judges it or `LANDED_WATCH` passes; never a clone an
    /// agent is running in.
    #[test]
    fn ci_is_watched_while_it_can_change_a_task() {
        let dir = std::env::temp_dir().join(format!("pma-ci-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        let blank = scan::Facts::held("x", &dir, String::new());
        let project = |name: &str, ci: scan::Ci, pending: &[&str]| scan::Facts {
            name: name.into(),
            path: dir.join(name),
            held: false,
            faults: Vec::new(),
            ci,
            ci_pending: pending.iter().map(|w| w.to_string()).collect(),
            ci_read: true,
            ..blank.clone()
        };
        let failing = || scan::Ci::Failing(vec!["test".into()]);
        let facts = [
            project("going", failing(), &["test"]),
            project("other", failing(), &["docs"]),
            project("passing", scan::Ci::Passing, &["test"]),
            project("landed", scan::Ci::Passing, &[]),
            project("judged", scan::Ci::Passing, &[]),
            project("busy", failing(), &["test"]),
        ];
        store.save_scan(&facts, true, dates::now()).unwrap();
        let add = |name: &str, state: RunState, landed: bool, judged: bool| {
            let mut run = crate::store::Run {
                project: name.into(),
                state,
                worktree: dir.join(name),
                ..crate::store::Run::default()
            };
            store.insert_run(&mut run).unwrap();
            if landed {
                store.set_landed(run.id, "abc").unwrap();
            }
            // Only a scan writes a verdict, so the test writes it directly.
            if judged {
                rusqlite::Connection::open(dir.join("p.db"))
                    .unwrap()
                    .execute(
                        "UPDATE runs SET ci_verdict = 'fixed' WHERE id = ?1",
                        [run.id],
                    )
                    .unwrap();
            }
        };
        add("landed", RunState::Pushed, true, false);
        add("judged", RunState::Pushed, true, true);
        add("busy", RunState::Running, false, false);
        let names = |now| -> Vec<String> {
            ci_watched(&store, now)
                .unwrap()
                .into_iter()
                .map(|(n, _)| n)
                .collect()
        };
        let now = dates::now();
        assert_eq!(names(now), ["going", "landed"]);
        assert_eq!(names(now + LANDED_WATCH), ["going"], "the watch ends");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unstaged edit changes no stamp, so the timer misses it; a reread
    /// after the editor counts it, and adds no row to `pma history`.
    #[test]
    fn a_reread_sees_an_unstaged_edit() {
        let dir = std::env::temp_dir().join(format!("pma-reread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("p");
        std::fs::create_dir_all(&repo).unwrap();
        let sh = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        sh(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("TODO.md"), "# TODO\n\n## High\n\n- [ ] x\n").unwrap();
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        sh(&["add", "."]);
        sh(&["commit", "-qm", "first"]);

        let mut store = Store::open(&dir.join("p.db")).unwrap();
        let projects = vec![("p".to_string(), repo.clone())];
        let facts = scan::scan_all(&projects, &[], true, false, &Default::default(), &|_| {});
        store.save_scan(&facts, true, dates::now()).unwrap();
        let dirty = |s: &Store| s.project("p").unwrap().unwrap().dirty;
        assert_eq!(dirty(&store), 0);
        let scans = store.scans(100).unwrap().len();

        let before = scan::stamp(&repo);
        std::fs::write(repo.join("a.txt"), "fixed by hand\n").unwrap();
        assert_eq!(scan::stamp(&repo), before, "no stamp sees it");
        reread(&mut store, &projects).unwrap();
        assert_eq!(dirty(&store), 1);
        assert_eq!(store.scans(100).unwrap().len(), scans, "no history row");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
