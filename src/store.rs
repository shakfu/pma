//! The portfolio database: roots, projects with tiers, settings, and the
//! results of the last scan.
//!
//! One user and one session at a time is assumed. The rollback journal
//! (`journal_mode=DELETE`) keeps the file complete between transactions, so
//! the directory can be tracked in git.

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

use crate::scan::{Ci, Facts};
use crate::todo::Priority;

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

const VERSION: i64 = 2;

const SCHEMA: &str = "
CREATE TABLE config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE roots (path TEXT PRIMARY KEY);
CREATE TABLE projects (
    name TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    tier INTEGER CHECK (tier BETWEEN 1 AND 5),
    scanned_at INTEGER,
    has_todo INTEGER NOT NULL DEFAULT 0,
    lint_errors INTEGER NOT NULL DEFAULT 0,
    dirty INTEGER NOT NULL DEFAULT 0,
    ahead INTEGER,
    last_activity INTEGER,
    ci TEXT NOT NULL DEFAULT 'unknown',
    ci_detail TEXT NOT NULL DEFAULT '',
    scan_error TEXT
);
CREATE TABLE tasks (
    project TEXT NOT NULL REFERENCES projects(name) ON DELETE CASCADE,
    key TEXT NOT NULL,
    line INTEGER NOT NULL,
    priority TEXT NOT NULL,
    text TEXT NOT NULL,
    tags TEXT NOT NULL,
    due TEXT,
    gh INTEGER,
    heading TEXT,
    added_at INTEGER,
    first_seen INTEGER NOT NULL,
    PRIMARY KEY (project, key)
);
";

pub struct Store {
    conn: Connection,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectRow {
    pub name: String,
    pub path: PathBuf,
    pub tier: Option<u8>,
    pub scanned_at: Option<i64>,
    pub has_todo: bool,
    pub lint_errors: i64,
    pub dirty: i64,
    pub ahead: Option<i64>,
    pub last_activity: Option<i64>,
    pub ci: Ci,
    pub scan_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskRow {
    pub project: String,
    pub line: i64,
    pub priority: Priority,
    pub text: String,
    pub tags: Vec<String>,
    pub due: Option<String>,
    pub gh: Option<i64>,
    pub group: Option<String>,
    pub added_at: Option<i64>,
    pub first_seen: i64,
}

/// `~/.config/pma`, or `PMA_HOME` when set.
pub fn home() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("PMA_HOME") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config").join("pma"))
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
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        match version {
            0 => {
                conn.execute_batch(SCHEMA)?;
                conn.pragma_update(None, "user_version", VERSION)?;
            }
            1 => {
                conn.execute_batch("ALTER TABLE tasks ADD COLUMN heading TEXT")?;
                conn.pragma_update(None, "user_version", VERSION)?;
            }
            VERSION => {}
            v => {
                return Err(format!(
                    "{} has schema version {v}; this pma reads {VERSION}",
                    path.display()
                )
                .into());
            }
        }
        Ok(Store { conn })
    }

    pub fn config_rows(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM config ORDER BY key")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO config (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Returns whether a stored value was removed.
    pub fn reset_config(&self, key: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM config WHERE key = ?1", [key])?
            > 0)
    }

    pub fn roots(&self) -> Result<Vec<PathBuf>> {
        let mut stmt = self.conn.prepare("SELECT path FROM roots ORDER BY path")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows
            .map(|r| r.map(PathBuf::from))
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Returns false when the root was already present.
    pub fn add_root(&self, path: &Path) -> Result<bool> {
        Ok(self.conn.execute(
            "INSERT OR IGNORE INTO roots (path) VALUES (?1)",
            [path_str(path)?],
        )? > 0)
    }

    pub fn remove_root(&self, path: &Path) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM roots WHERE path = ?1", [path_str(path)?])?
            > 0)
    }

    pub fn set_tier(&self, name: &str, path: &Path, tier: Option<u8>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO projects (name, path, tier) VALUES (?1, ?2, ?3)
             ON CONFLICT (name) DO UPDATE SET tier = excluded.tier",
            params![name, path_str(path)?, tier],
        )?;
        Ok(())
    }

    pub fn last_scan(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT MAX(scanned_at) FROM projects", [], |r| r.get(0))?)
    }

    pub fn project(&self, name: &str) -> Result<Option<ProjectRow>> {
        Ok(self.projects()?.into_iter().find(|p| p.name == name))
    }

    pub fn projects(&self) -> Result<Vec<ProjectRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, path, tier, scanned_at, has_todo, lint_errors, dirty, ahead,
                    last_activity, ci, ci_detail, scan_error
             FROM projects ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ProjectRow {
                name: r.get(0)?,
                path: PathBuf::from(r.get::<_, String>(1)?),
                tier: r.get(2)?,
                scanned_at: r.get(3)?,
                has_todo: r.get(4)?,
                lint_errors: r.get(5)?,
                dirty: r.get(6)?,
                ahead: r.get(7)?,
                last_activity: r.get(8)?,
                ci: Ci::from_columns(&r.get::<_, String>(9)?, r.get::<_, String>(10)?),
                scan_error: r.get(11)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn tasks(&self) -> Result<Vec<TaskRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT project, line, priority, text, tags, due, gh, added_at, first_seen, heading
             FROM tasks ORDER BY project, line",
        )?;
        let rows = stmt.query_map([], |r| {
            let priority: String = r.get(2)?;
            let tags: String = r.get(4)?;
            Ok(TaskRow {
                project: r.get(0)?,
                line: r.get(1)?,
                priority: Priority::parse(&priority).unwrap_or(Priority::Low),
                text: r.get(3)?,
                tags: tags.split_whitespace().map(String::from).collect(),
                due: r.get(5)?,
                gh: r.get(6)?,
                added_at: r.get(7)?,
                first_seen: r.get(8)?,
                group: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records scan results. Each task keeps the `first_seen` it had in the
    /// previous scan. With `full`, projects not in `facts` are deleted and
    /// returned with the tier they had.
    pub fn save_scan(
        &mut self,
        facts: &[Facts],
        full: bool,
        now: i64,
    ) -> Result<Vec<(String, Option<u8>)>> {
        let tx = self.conn.transaction()?;
        for f in facts {
            let (ci, ci_detail) = f.ci.to_columns();
            tx.execute(
                "INSERT INTO projects (name, path, scanned_at, has_todo, lint_errors, dirty, ahead,
                                       last_activity, ci, ci_detail, scan_error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT (name) DO UPDATE SET
                    path = excluded.path, scanned_at = excluded.scanned_at,
                    has_todo = excluded.has_todo, lint_errors = excluded.lint_errors,
                    dirty = excluded.dirty, ahead = excluded.ahead,
                    last_activity = excluded.last_activity, ci = excluded.ci,
                    ci_detail = excluded.ci_detail, scan_error = excluded.scan_error",
                params![
                    f.name,
                    path_str(&f.path)?,
                    now,
                    f.todo.is_some(),
                    f.todo.as_ref().map_or(0, |t| t.lint_errors),
                    f.dirty,
                    f.ahead,
                    f.last_activity,
                    ci,
                    ci_detail,
                    f.error,
                ],
            )?;

            let first_seen: HashMap<String, i64> = {
                let mut stmt =
                    tx.prepare("SELECT key, first_seen FROM tasks WHERE project = ?1")?;
                let rows = stmt.query_map([&f.name], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            tx.execute("DELETE FROM tasks WHERE project = ?1", [&f.name])?;
            for item in f.todo.iter().flat_map(|t| &t.items) {
                tx.execute(
                    "INSERT INTO tasks (project, key, line, priority, text, tags, due, gh, added_at, first_seen, heading)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        f.name,
                        item.key,
                        item.line,
                        item.priority.name(),
                        item.text,
                        item.tags.join(" "),
                        item.due,
                        item.gh,
                        item.added_at,
                        first_seen.get(&item.key).copied().unwrap_or(now),
                        item.group,
                    ],
                )?;
            }
        }

        let mut removed = Vec::new();
        if full {
            let scanned: std::collections::HashSet<&str> =
                facts.iter().map(|f| f.name.as_str()).collect();
            let mut stmt = tx.prepare("SELECT name, tier FROM projects")?;
            let all: Vec<(String, Option<u8>)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            for (name, tier) in all {
                if !scanned.contains(name.as_str()) {
                    tx.execute("DELETE FROM projects WHERE name = ?1", [&name])?;
                    removed.push((name, tier));
                }
            }
        }
        tx.commit()?;
        Ok(removed)
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

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pma-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn item(key: &str, line: i64) -> ScannedItem {
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

    fn facts(name: &str, items: Vec<ScannedItem>) -> Facts {
        Facts {
            name: name.into(),
            path: PathBuf::from(format!("/r/{name}")),
            todo: Some(TodoFacts {
                lint_errors: 1,
                items,
            }),
            dirty: 3,
            ahead: None,
            last_activity: Some(42),
            ci: Ci::Failing(vec!["test".into(), "wheels".into()]),
            error: None,
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

    #[test]
    fn version_1_databases_gain_the_heading_column() {
        let dir = scratch("migrate");
        let db = dir.join("p.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch("ALTER TABLE tasks DROP COLUMN heading")
            .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        drop(conn);

        let mut store = Store::open(&db).unwrap();
        store
            .save_scan(&[facts("one", vec![item("x", 1)])], true, 5)
            .unwrap();
        assert_eq!(store.tasks().unwrap()[0].group.as_deref(), Some("Security"));
        drop(store);
        let conn = Connection::open(&db).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, VERSION);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn roots_add_and_remove() {
        let dir = scratch("roots");
        let store = Store::open(&dir.join("p.db")).unwrap();
        assert!(store.add_root(Path::new("/a")).unwrap());
        assert!(!store.add_root(Path::new("/a")).unwrap());
        store.add_root(Path::new("/b")).unwrap();
        assert_eq!(
            store.roots().unwrap(),
            [PathBuf::from("/a"), PathBuf::from("/b")]
        );
        assert!(store.remove_root(Path::new("/a")).unwrap());
        assert_eq!(store.roots().unwrap(), [PathBuf::from("/b")]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn scan_round_trips_and_keeps_tier_and_first_seen() {
        let dir = scratch("scan");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        store.set_tier("one", Path::new("/r/one"), Some(2)).unwrap();

        store
            .save_scan(
                &[facts("one", vec![item("x", 5), item("y", 6)])],
                true,
                1000,
            )
            .unwrap();
        store
            .save_scan(
                &[facts("one", vec![item("y", 7), item("z", 8)])],
                true,
                2000,
            )
            .unwrap();

        let p = store.project("one").unwrap().unwrap();
        assert_eq!(p.tier, Some(2), "a scan keeps the tier");
        assert_eq!(
            (p.scanned_at, p.has_todo, p.lint_errors, p.dirty),
            (Some(2000), true, 1, 3)
        );
        assert_eq!((p.ahead, p.last_activity), (None, Some(42)));
        assert_eq!(p.ci, Ci::Failing(vec!["test".into(), "wheels".into()]));

        let tasks = store.tasks().unwrap();
        let seen: Vec<_> = tasks
            .iter()
            .map(|t| (t.text.as_str(), t.line, t.first_seen))
            .collect();
        assert_eq!(
            seen,
            [("y", 7, 1000), ("z", 8, 2000)],
            "x is gone, y keeps its first scan"
        );
        assert_eq!(tasks[0].tags, ["a", "b"]);
        assert_eq!(tasks[0].group.as_deref(), Some("Security"));
        assert_eq!(
            (tasks[0].due.as_deref(), tasks[0].gh, tasks[0].added_at),
            (Some("2026-10-01"), Some(4), Some(100))
        );
        assert_eq!(store.last_scan().unwrap(), Some(2000));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn full_scan_removes_projects_no_longer_found() {
        let dir = scratch("full");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        store
            .save_scan(
                &[facts("a", vec![item("t", 1)]), facts("b", vec![])],
                true,
                1,
            )
            .unwrap();
        store.set_tier("a", Path::new("/r/a"), Some(1)).unwrap();

        assert_eq!(
            store.save_scan(&[facts("b", vec![])], false, 2).unwrap(),
            []
        );
        assert_eq!(
            store.projects().unwrap().len(),
            2,
            "a partial scan removes nothing"
        );

        let removed = store.save_scan(&[facts("b", vec![])], true, 3).unwrap();
        assert_eq!(removed, [("a".to_string(), Some(1))]);
        assert!(
            store.tasks().unwrap().is_empty(),
            "tasks go with their project"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
