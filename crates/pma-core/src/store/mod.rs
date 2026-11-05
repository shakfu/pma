//! The portfolio database: roots, projects with tiers, settings, the results
//! of the last scan, agent runs, and portfolio notes.
//!
//! One user is assumed, and one session per project (`Session`). The rollback journal
//! (`journal_mode=DELETE`) keeps the file complete between transactions, so
//! a copy of it is a usable backup.

use std::collections::HashMap;
use std::error::Error;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::class::Class;
use crate::complexity::Features;
use crate::config::Workspace;
use crate::route::{Approval, Policy};
use crate::scan::{Ci, Facts, Fault};
use crate::todo::Priority;
use crate::worker::{Parser, Worker};

mod attempts;
mod campaigns;
mod jobs;
mod lock;
mod migrations;
use migrations::migrate;
mod notes;
mod projects;
mod routes;
mod runs;
mod settings;
mod workflows;

pub use attempts::*;
pub use campaigns::*;
pub use jobs::*;
pub use lock::*;
pub use notes::*;
pub use projects::*;
pub use routes::*;
pub use runs::*;
pub use settings::*;
pub use workflows::*;

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub struct Store {
    conn: Connection,
    /// `settings.toml`, beside the database. The configuration tables mirror
    /// it; every change to them goes through it first.
    settings: PathBuf,
}

/// A directory named by a variable, `None` when unset or empty. A relative
/// one is refused: the database and session lock would follow the working
/// directory.
fn dir_var(name: &str) -> Result<Option<PathBuf>> {
    match std::env::var_os(name).filter(|v| !v.is_empty()) {
        Some(v) if Path::new(&v).is_relative() => {
            Err(format!("{name} must be an absolute path, not `{}`", v.display()).into())
        }
        v => Ok(v.map(PathBuf::from)),
    }
}

/// `~/.config/pma`, or `PMA_HOME` when set.
pub fn home() -> Result<PathBuf> {
    if let Some(dir) = dir_var("PMA_HOME")? {
        return Ok(dir);
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config").join("pma"))
}

/// Where worktrees, artifacts and logs go: `PMA_STATE`, else `$PMA_HOME/state`
/// when `PMA_HOME` is set, else `$XDG_STATE_HOME/pma` or `~/.local/state/pma`.
/// Kept apart from the database, whose directory holds nothing else.
pub fn state_home() -> Result<PathBuf> {
    if let Some(dir) = dir_var("PMA_STATE")? {
        return Ok(dir);
    }
    if let Some(dir) = dir_var("PMA_HOME")? {
        return Ok(dir.join("state"));
    }
    // The XDG specification says a relative path is ignored.
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME").filter(|v| Path::new(v).is_absolute()) {
        return Ok(PathBuf::from(dir).join("pma"));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local").join("state").join("pma"))
}

impl Store {
    pub fn open_default() -> Result<Store> {
        let dir = home()?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        Store::open(&dir.join("projects.db"))
    }

    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
        conn.query_row("PRAGMA journal_mode = DELETE", [], |r| {
            r.get::<_, String>(0)
        })?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Other commands may write while a session runs agents.
        conn.busy_timeout(Duration::from_secs(5))?;
        let version = migrate(&conn, path)?;
        let store = Store {
            conn,
            settings: path.with_file_name(crate::settings::FILE),
        };
        store.load_settings()?;
        store.migrate_settings(version)?;
        Ok(store)
    }

    /// Records the tree `pma review <id>` showed. One column, so a reading
    /// command does not write back a copy of the whole run.
    /// SQLite's data version: it moves whenever another connection commits.
    /// A front end polls it to learn that anything changed, without reading
    /// the tables to find out.
    pub fn data_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .pragma_query_value(None, "data_version", |r| r.get(0))?)
    }

    /// Runs `work` in one transaction, committing only where it succeeds.
    ///
    /// A workflow node writes several rows per unit -- the units it produced,
    /// the moves that route them, the marker saying it is done with its
    /// input -- and a failure part way through would leave a unit routed by a
    /// node that had not finished with it. The boundary is one unit, not one
    /// node: a sink writes a file, and a unit whose file is written and whose
    /// move is not would have its sink run twice.
    pub fn atomically<T>(&self, work: impl FnOnce() -> Result<T>) -> Result<T> {
        // Immediate takes the write lock first, so `busy_timeout` waits for
        // it. A deferred read upgraded to a write fails at once when another
        // writer holds the lock.
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let out = work()?;
        tx.commit()?;
        Ok(out)
    }

    #[cfg(test)]
    fn journal_mode(&self) -> String {
        self.conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap()
    }
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{ScannedItem, TodoFacts};

    pub(super) fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pma-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(super) fn item(key: &str, line: i64) -> ScannedItem {
        ScannedItem {
            key: key.into(),
            line,
            priority: Priority::High,
            text: key.into(),
            tags: vec!["a".into(), "b".into()],
            due: Some("2026-10-01".into()),
            gh: Some(4),
            group: Some("Security".into()),
            added_at: Some(100),
        }
    }

    pub(super) fn facts(name: &str, items: Vec<ScannedItem>) -> Facts {
        Facts {
            name: name.into(),
            path: PathBuf::from(format!("/r/{name}")),
            slug: Some(format!("shakfu/{name}")),
            todo: Some(TodoFacts {
                lint_errors: 1,
                first_lint: Some("line 1: the file must start with `# TODO`".into()),
                items,
            }),
            dirty: 3,
            pma_branches: vec![],
            ahead: None,
            last_activity: Some(42),
            ci: Ci::Failing(vec!["test".into(), "wheels".into()]),
            ci_runs: vec![(
                "test".into(),
                "https://github.com/shakfu/x/actions/runs/7".into(),
            )],
            lock_at: None,
            deps: None,
            faults: vec![(
                Fault::TodoLint,
                "line 1: the file must start with `# TODO`".into(),
            )],
            ci_read: true,
            todo_unread: false,
            held: false,
            stamp: String::new(),
        }
    }

    #[test]
    fn opens_with_rollback_journal_and_reopens() {
        let dir = scratch("open");
        let db = dir.join("p.db");
        let store = Store::open(&db).unwrap();
        assert_eq!(store.journal_mode(), "delete");
        store.set_config("tiers.1", "0.9").unwrap();
        drop(store);
        let store = Store::open(&db).unwrap();
        assert_eq!(
            store.config_rows().unwrap(),
            [("tiers.1".to_string(), "0.9".to_string())]
        );
        assert!(store.reset_config("tiers.1").unwrap());
        assert!(!store.reset_config("tiers.1").unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A transaction that reads and then writes waits for another writer.
    /// Deferred, the read lock it took could not be upgraded, and SQLite
    /// fails such an upgrade at once rather than waiting.
    #[test]
    fn atomically_waits_for_another_writer() {
        let dir = scratch("immediate");
        let path = dir.join("p.db");
        let store = Store::open(&path).unwrap();
        let (held, holding) = std::sync::mpsc::channel();
        let other = path.clone();
        let writer = std::thread::spawn(move || {
            let conn = Connection::open(&other).unwrap();
            conn.execute_batch("BEGIN IMMEDIATE; INSERT INTO config VALUES ('a', '1');")
                .unwrap();
            held.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            conn.execute_batch("COMMIT").unwrap();
        });
        holding.recv().unwrap();
        store
            .atomically(|| {
                let n: i64 = store
                    .conn
                    .query_row("SELECT count(*) FROM config", [], |r| r.get(0))?;
                store
                    .conn
                    .execute("INSERT INTO config VALUES ('b', ?1)", [n.to_string()])?;
                Ok(())
            })
            .unwrap();
        writer.join().unwrap();
        assert_eq!(store.config_rows().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
