//! The portfolio database: roots, projects with tiers, settings, the results
//! of the last scan, agent runs, and portfolio notes.
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

const VERSION: i64 = 3;

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

/// Version 2. Runs outlive a project's removal from the scan: their worktrees
/// still exist and must be shipped or rejected.
const RUNS: &str = "
CREATE TABLE runs (
    id INTEGER PRIMARY KEY,
    project TEXT NOT NULL,
    task_key TEXT NOT NULL,
    text TEXT NOT NULL,
    gh INTEGER,
    quadrant TEXT,
    agent TEXT NOT NULL,
    repo TEXT NOT NULL,
    branch TEXT NOT NULL,
    worktree TEXT NOT NULL,
    default_branch TEXT NOT NULL,
    base TEXT NOT NULL,
    prompt TEXT NOT NULL,
    state TEXT NOT NULL,
    feedback TEXT,
    started_at INTEGER,
    seconds INTEGER,
    cost_usd REAL,
    summary TEXT,
    commits INTEGER,
    diffstat TEXT,
    verify TEXT,
    verify_ok INTEGER,
    error TEXT,
    outcome TEXT
);
";

/// Version 3. `deps` is `NULL` until measured, and keeps its value across
/// scans that do not measure it.
const DEPS_AND_NOTES: &str = "
ALTER TABLE projects ADD COLUMN deps INTEGER;
ALTER TABLE projects ADD COLUMN deps_detail TEXT NOT NULL DEFAULT '';
ALTER TABLE projects ADD COLUMN deps_at INTEGER;
CREATE TABLE notes (
    id INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    text TEXT NOT NULL
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
    /// Outdated dependencies at the last measurement; `None` if never measured.
    pub deps: Option<i64>,
    /// One `ecosystem: name current -> latest` per line.
    pub deps_detail: String,
    pub deps_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskRow {
    pub project: String,
    pub key: String,
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
            0..VERSION => {
                let steps = [SCHEMA, RUNS, DEPS_AND_NOTES];
                let tx = conn.unchecked_transaction()?;
                for step in &steps[version as usize..] {
                    tx.execute_batch(step)?;
                }
                tx.pragma_update(None, "user_version", VERSION)?;
                tx.commit()?;
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
                    last_activity, ci, ci_detail, scan_error, deps, deps_detail, deps_at
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
                deps: r.get(12)?,
                deps_detail: r.get(13)?,
                deps_at: r.get(14)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn tasks(&self) -> Result<Vec<TaskRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT project, line, priority, text, tags, due, gh, added_at, first_seen, heading, key
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
                key: r.get(10)?,
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

            if let Some(d) = &f.deps {
                tx.execute(
                    "UPDATE projects SET deps = ?2, deps_detail = ?3, deps_at = ?4 WHERE name = ?1",
                    params![f.name, d.outdated, d.detail, now],
                )?;
            }

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

    pub fn notes(&self) -> Result<Vec<Note>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, created_at, updated_at, text FROM notes ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok(Note {
                id: r.get(0)?,
                created_at: r.get(1)?,
                updated_at: r.get(2)?,
                text: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_note(&self, text: &str, now: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO notes (created_at, updated_at, text) VALUES (?1, ?1, ?2)",
            params![now, text],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Returns false when no note has the id.
    pub fn edit_note(&self, id: i64, text: &str, now: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE notes SET text = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, text, now],
        )? > 0)
    }

    /// Returns false when no note has the id.
    pub fn remove_note(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM notes WHERE id = ?1", [id])? > 0)
    }

    pub fn insert_run(&self, run: &Run) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO runs (project, task_key, text, gh, quadrant, agent, branch, worktree,
                               default_branch, base, prompt, state, repo)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                run.project,
                run.task_key,
                run.text,
                run.gh,
                run.quadrant,
                run.agent,
                run.branch,
                path_str(&run.worktree)?,
                run.default_branch,
                run.base,
                run.prompt,
                run.state.name(),
                path_str(&run.repo)?,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Writes every mutable column of the run with `run.id`.
    pub fn update_run(&self, run: &Run) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET state = ?2, feedback = ?3, started_at = ?4, seconds = ?5,
                cost_usd = ?6, summary = ?7, commits = ?8, diffstat = ?9, verify = ?10,
                verify_ok = ?11, error = ?12, outcome = ?13, prompt = ?14
             WHERE id = ?1",
            params![
                run.id,
                run.state.name(),
                run.feedback,
                run.started_at,
                run.seconds,
                run.cost_usd,
                run.summary,
                run.commits,
                run.diffstat,
                run.verify,
                run.verify_ok,
                run.error,
                run.outcome,
                run.prompt,
            ],
        )?;
        Ok(())
    }

    pub fn runs(&self) -> Result<Vec<Run>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project, task_key, text, gh, quadrant, agent, branch, worktree,
                    default_branch, base, prompt, state, feedback, started_at, seconds,
                    cost_usd, summary, commits, diffstat, verify, verify_ok, error, outcome, repo
             FROM runs ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            let state: String = r.get(12)?;
            Ok(Run {
                id: r.get(0)?,
                project: r.get(1)?,
                task_key: r.get(2)?,
                text: r.get(3)?,
                gh: r.get(4)?,
                quadrant: r.get(5)?,
                agent: r.get(6)?,
                branch: r.get(7)?,
                worktree: PathBuf::from(r.get::<_, String>(8)?),
                default_branch: r.get(9)?,
                base: r.get(10)?,
                prompt: r.get(11)?,
                state: RunState::parse(&state).unwrap_or(RunState::Failed),
                feedback: r.get(13)?,
                started_at: r.get(14)?,
                seconds: r.get(15)?,
                cost_usd: r.get(16)?,
                summary: r.get(17)?,
                commits: r.get(18)?,
                diffstat: r.get(19)?,
                verify: r.get(20)?,
                verify_ok: r.get(21)?,
                error: r.get(22)?,
                outcome: r.get(23)?,
                repo: PathBuf::from(r.get::<_, String>(24)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn run(&self, id: i64) -> Result<Run> {
        self.runs()?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("no run #{id}").into())
    }

    /// Marks runs left queued or running by a session that ended as failed. One
    /// session at a time is assumed, so none of them is still running.
    pub fn fail_interrupted_runs(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE runs SET state = 'failed', error = 'interrupted: pma exited during the run'
             WHERE state IN ('queued', 'running')",
            [],
        )?)
    }

    #[cfg(test)]
    fn journal_mode(&self) -> String {
        self.conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub id: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Queued,
    Running,
    Ready,
    Failed,
    Approved,
    Rejected,
    Shipped,
}

impl RunState {
    const ALL: [(&'static str, RunState); 7] = [
        ("queued", RunState::Queued),
        ("running", RunState::Running),
        ("ready", RunState::Ready),
        ("failed", RunState::Failed),
        ("approved", RunState::Approved),
        ("rejected", RunState::Rejected),
        ("shipped", RunState::Shipped),
    ];

    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, s)| *s == self)
            .map_or("", |(n, _)| n)
    }

    fn parse(s: &str) -> Option<RunState> {
        Self::ALL.iter().find(|(n, _)| *n == s).map(|(_, v)| *v)
    }

    /// A run in a final state no longer holds its task or its worktree.
    pub fn is_final(self) -> bool {
        matches!(self, RunState::Rejected | RunState::Shipped)
    }
}

/// One task dispatched to an agent, from its worktree to shipping.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub id: i64,
    pub project: String,
    /// The item's key, or a signal: `ci` or `deps`.
    pub task_key: String,
    pub text: String,
    pub gh: Option<i64>,
    /// Quadrant at dispatch; `None` for an untiered project.
    pub quadrant: Option<String>,
    pub agent: String,
    /// The user's clone the worktree belongs to.
    pub repo: PathBuf,
    pub branch: String,
    pub worktree: PathBuf,
    pub default_branch: String,
    /// Commit the worktree started from.
    pub base: String,
    pub prompt: String,
    pub state: RunState,
    /// Reviewer feedback for the latest rework.
    pub feedback: Option<String>,
    pub started_at: Option<i64>,
    /// Agent and verify time, summed over reworks.
    pub seconds: Option<i64>,
    /// Summed over reworks; `None` when the agent reports no cost.
    pub cost_usd: Option<f64>,
    pub summary: Option<String>,
    /// Commits the agent made beyond `base`; it was told to make none.
    pub commits: Option<i64>,
    pub diffstat: Option<String>,
    /// The verify command, or `None` when there is none.
    pub verify: Option<String>,
    pub verify_ok: Option<bool>,
    pub error: Option<String>,
    /// What shipping did: a pushed commit or a pull request URL.
    pub outcome: Option<String>,
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{DepsFacts, ScannedItem, TodoFacts};

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
            deps: None,
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
    fn older_databases_are_upgraded_in_place() {
        let dir = scratch("migrate");
        for (version, steps) in [(1, &[SCHEMA][..]), (2, &[SCHEMA, RUNS][..])] {
            let db = dir.join(format!("v{version}.db"));
            let conn = Connection::open(&db).unwrap();
            for step in steps {
                conn.execute_batch(step).unwrap();
            }
            conn.pragma_update(None, "user_version", version).unwrap();
            conn.execute(
                "INSERT INTO projects (name, path) VALUES ('kept', '/k')",
                [],
            )
            .unwrap();
            drop(conn);
            let store = Store::open(&db).unwrap();
            let p = store.project("kept").unwrap().unwrap();
            assert_eq!((p.deps, p.deps_detail.as_str()), (None, ""), "v{version}");
            assert!(store.runs().unwrap().is_empty());
            assert!(store.notes().unwrap().is_empty());
            drop(store);
            assert!(Store::open(&db).is_ok(), "v{version} reopens");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn notes_add_edit_and_remove() {
        let dir = scratch("notes");
        let store = Store::open(&dir.join("p.db")).unwrap();
        let a = store
            .add_note("move CI to one reusable workflow", 10)
            .unwrap();
        let b = store.add_note("second", 20).unwrap();
        assert!(
            store
                .edit_note(a, "move CI to reusable workflows", 30)
                .unwrap()
        );
        assert!(!store.edit_note(99, "x", 30).unwrap());
        assert!(store.remove_note(b).unwrap());
        assert!(!store.remove_note(b).unwrap());
        assert_eq!(
            store.notes().unwrap(),
            [Note {
                id: a,
                created_at: 10,
                updated_at: 30,
                text: "move CI to reusable workflows".into()
            }]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn runs_round_trip_and_interrupted_runs_fail() {
        let dir = scratch("runs");
        let store = Store::open(&dir.join("p.db")).unwrap();
        let mut run = Run {
            id: 0,
            project: "p".into(),
            task_key: "fix it".into(),
            text: "Fix it".into(),
            gh: Some(3),
            quadrant: Some("Q1".into()),
            agent: "claude".into(),
            repo: PathBuf::from("/r/p"),
            branch: "pma/fix-it".into(),
            worktree: PathBuf::from("/w/p/fix-it"),
            default_branch: "main".into(),
            base: "abc".into(),
            prompt: "do it".into(),
            state: RunState::Running,
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
        run.id = store.insert_run(&run).unwrap();
        assert_eq!(store.run(run.id).unwrap(), run);

        run.state = RunState::Ready;
        run.cost_usd = Some(0.25);
        run.verify = Some("make test".into());
        run.verify_ok = Some(false);
        run.summary = Some("done".into());
        store.update_run(&run).unwrap();
        assert_eq!(store.run(run.id).unwrap(), run);

        run.state = RunState::Running;
        store.update_run(&run).unwrap();
        assert_eq!(store.fail_interrupted_runs().unwrap(), 1);
        let failed = store.run(run.id).unwrap();
        assert_eq!(failed.state, RunState::Failed);
        assert!(failed.error.unwrap().starts_with("interrupted"));
        assert!(store.run(99).is_err());
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
        assert_eq!((p.deps, p.deps_at), (None, None));

        let mut measured = facts("one", vec![item("y", 7), item("z", 8)]);
        measured.deps = Some(DepsFacts {
            outdated: Some(2),
            detail: "uv: ruff 0.14 -> 0.16".into(),
        });
        store.save_scan(&[measured], true, 3000).unwrap();
        store
            .save_scan(
                &[facts("one", vec![item("y", 7), item("z", 8)])],
                true,
                4000,
            )
            .unwrap();
        let p = store.project("one").unwrap().unwrap();
        assert_eq!(
            (p.deps, p.deps_detail.as_str(), p.deps_at),
            (Some(2), "uv: ruff 0.14 -> 0.16", Some(3000)),
            "a scan that does not measure deps keeps the last measurement"
        );
        assert_eq!(
            (p.scanned_at, p.has_todo, p.lint_errors, p.dirty),
            (Some(4000), true, 1, 3)
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
        assert_eq!(store.last_scan().unwrap(), Some(4000));
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
