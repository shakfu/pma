//! The portfolio database: roots, projects with tiers, settings, the results
//! of the last scan, agent runs, and portfolio notes.
//!
//! One user and one session at a time is assumed. The rollback journal
//! (`journal_mode=DELETE`) keeps the file complete between transactions, so
//! the directory can be tracked in git.

use std::collections::HashMap;
use std::error::Error;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, params};

use crate::class::Class;
use crate::complexity::Features;
use crate::route::{Approval, Policy};
use crate::scan::{Ci, Facts};
use crate::todo::Priority;
use crate::worker::{Parser, Worker};

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

const VERSION: i64 = 24;

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

/// Version 4. `pma/` branches that no open run owned at the last scan.
const LEFTOVER: &str = "
ALTER TABLE projects ADD COLUMN leftover INTEGER NOT NULL DEFAULT 0;
";

/// Version 5. Runs may be `pr-open`, which an older `pma` would read as
/// `failed`. Runs shipped as pull requests before it return to `pr-open`, so
/// the next settle checks whether they were merged.
const PR_OPEN: &str = "
UPDATE runs SET state = 'pr-open' WHERE state = 'shipped' AND outcome LIKE 'https://%/pull/%';
";

/// Version 6. `runs` holds one mutable row per task, so a rework overwrites
/// the previous attempt's summary, cost, verify result and start time. An
/// attempt is now its own append-only row, and `runs` keeps the lifecycle
/// summary plus a timestamp per transition. Existing runs contribute their
/// last attempt, whose outcome was not recorded and stays null.
const ATTEMPTS: &str = "
CREATE TABLE attempts (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES runs(id),
    n INTEGER NOT NULL,
    agent TEXT NOT NULL,
    prompt TEXT NOT NULL,
    feedback TEXT,
    started_at INTEGER NOT NULL,
    seconds INTEGER,
    cost_usd REAL,
    summary TEXT,
    verify TEXT,
    verify_ok INTEGER,
    error TEXT,
    outcome TEXT,
    UNIQUE (run_id, n)
);
ALTER TABLE runs ADD COLUMN dispatched_at INTEGER;
ALTER TABLE runs ADD COLUMN ready_at INTEGER;
ALTER TABLE runs ADD COLUMN decided_at INTEGER;
ALTER TABLE runs ADD COLUMN published_at INTEGER;
ALTER TABLE runs ADD COLUMN review_seconds INTEGER;
INSERT INTO attempts (run_id, n, agent, prompt, feedback, started_at, seconds,
                      cost_usd, summary, verify, verify_ok, error, outcome)
SELECT id, 1, agent, prompt, feedback, started_at, seconds, cost_usd, summary,
       verify, verify_ok, error, NULL
FROM runs WHERE started_at IS NOT NULL;
UPDATE runs SET dispatched_at = started_at WHERE started_at IS NOT NULL;
ALTER TABLE runs DROP COLUMN started_at;
";

/// Version 7. The decision a dispatch made, snapshotted on the run and never
/// updated. A rescan or a reworded item changes the scanned task, and the
/// project's tier and the budgets change under `pma config`, so none of them
/// can be read back later from the task. Runs dispatched before this version
/// have no snapshot and stay null.
const SNAPSHOT: &str = "
ALTER TABLE runs ADD COLUMN class TEXT;
ALTER TABLE runs ADD COLUMN scope TEXT;
ALTER TABLE runs ADD COLUMN tier INTEGER;
ALTER TABLE runs ADD COLUMN description TEXT;
ALTER TABLE runs ADD COLUMN agent_budget REAL;
ALTER TABLE runs ADD COLUMN timeout_minutes INTEGER;
";

/// Version 8. One verify run at the head proves the tree is green now. The
/// same command at the base separates three cases: already broken, broken by
/// the agent, fixed by the agent. The result is cached because every task of
/// a project in one batch shares a base. Timeout is part of the key: a pass
/// under a longer limit says nothing about a shorter one.
const VERIFY_BASE: &str = "
CREATE TABLE verify_base (
    project TEXT NOT NULL,
    base TEXT NOT NULL,
    command TEXT NOT NULL,
    timeout_minutes INTEGER NOT NULL,
    ok INTEGER,
    seconds INTEGER NOT NULL,
    measured_at INTEGER NOT NULL,
    PRIMARY KEY (project, base, command, timeout_minutes)
);
ALTER TABLE runs ADD COLUMN verify_base_ok INTEGER;
ALTER TABLE runs ADD COLUMN verify_base_seconds INTEGER;
";

/// Version 9. The paths a run changed, as a JSON array. Stored rather than
/// the violations derived from them, so a later change to the class rules
/// re-reads the evidence instead of trusting a verdict recorded under rules
/// nobody can name any more. Null means the paths could not be enumerated,
/// which `scope_error` explains; an empty array means the run changed
/// nothing.
const CHANGED_PATHS: &str = "
ALTER TABLE runs ADD COLUMN changed_paths TEXT;
ALTER TABLE runs ADD COLUMN scope_error TEXT;
";

/// Version 10. Attempts consumed per task revision, so `--auto` stops
/// choosing a task an agent cannot close. Keyed by the normalised task text
/// rather than by the run, because a counter on a run resets when the run
/// ends, and keyed by text rather than by a permanent id, because a reworded
/// task is a different specification and a recurring `ci` incident names its
/// failing workflows.
const EXHAUSTION: &str = "
CREATE TABLE exhaustion (
    project TEXT NOT NULL,
    revision TEXT NOT NULL,
    attempts INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (project, revision)
);
";

/// Version 11. A worker is a record, not a code path. `config` is a flat
/// table of scalars with a closed key list, and an agent record is an
/// open-ended name holding a list, so it gets its own table rather than a
/// dotted key. `claude` is seeded as one entry with exactly the behaviour it
/// had when it was compiled in.
const AGENTS: &str = "
CREATE TABLE agents (
    name TEXT PRIMARY KEY,
    command TEXT NOT NULL,
    args TEXT NOT NULL,
    allow TEXT,
    parse TEXT NOT NULL,
    reports_cost INTEGER NOT NULL,
    enforces_budget INTEGER NOT NULL,
    sandbox INTEGER NOT NULL,
    resumes INTEGER NOT NULL
);
ALTER TABLE runs ADD COLUMN model TEXT;
ALTER TABLE attempts ADD COLUMN model TEXT;
";

/// Version 12. Drops the settings phase 3 retired. A stored row for an
/// unknown key makes every command fail, so retiring one is a migration, not
/// only a code change. The names stay in `config::RETIRED`, which explains
/// them to `pma config`.
const RETIRE_KEYS: &str = "
DELETE FROM config WHERE key IN (
    'dispatch_quadrants', 'overflow_quadrants',
    'stale_after.1', 'stale_after.2', 'stale_after.3', 'stale_after.4', 'stale_after.5'
);
";

/// Version 13. The complexity estimate and the features it read, on the run
/// that used them, with the version of the rule that produced it. Routing
/// matches on the value, so replaying a decision means re-running the rule
/// named here rather than today's.
const COMPLEXITY: &str = "
ALTER TABLE runs ADD COLUMN complexity INTEGER;
ALTER TABLE runs ADD COLUMN features TEXT;
ALTER TABLE runs ADD COLUMN estimator TEXT;
";

/// Version 14. Routing policy as a versioned artifact with provenance.
/// Editing a revision and putting it into effect are separate acts: a
/// revision is a draft until someone activates it, and the run records the
/// revision it was dispatched under, so a later activation does not rewrite
/// what a past dispatch decided.
const ROUTES: &str = "
CREATE TABLE routes (
    revision INTEGER PRIMARY KEY,
    document TEXT NOT NULL,
    proposed_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    activated_at INTEGER,
    activated_by TEXT,
    shadow INTEGER NOT NULL DEFAULT 0
);
ALTER TABLE runs ADD COLUMN route_revision INTEGER;
ALTER TABLE runs ADD COLUMN route TEXT;
ALTER TABLE runs ADD COLUMN approval TEXT;
";

/// Version 15. What an approval was given for. Ship rebases, edits
/// `TODO.md`, amends and publishes without rerunning anything, so an
/// approval that names no tree can publish content no check ever saw. The
/// base, the verify command and the scope are already immutable on the run;
/// the tree and the approver are what was missing.
const APPROVAL_EVIDENCE: &str = "
ALTER TABLE runs ADD COLUMN approved_tree TEXT;
ALTER TABLE runs ADD COLUMN approved_head TEXT;
ALTER TABLE runs ADD COLUMN approved_by TEXT;
";

/// Version 16. One task definition across many repositories. Membership is
/// stored rather than recomputed, so a rescan cannot change the set under a
/// campaign that is already running, and a restart can tell which members
/// already have a run instead of opening a second pull request for them.
const CAMPAIGNS: &str = "
CREATE TABLE campaigns (
    name TEXT PRIMARY KEY,
    text TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    class TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE campaign_members (
    campaign TEXT NOT NULL REFERENCES campaigns(name) ON DELETE CASCADE,
    project TEXT NOT NULL,
    run_id INTEGER,
    PRIMARY KEY (campaign, project)
);
";

/// Version 24. Retires a worker's own default model in favour of presets. A
/// preset says the same thing and more -- a worker, a model and the arguments
/// that configure it -- and two ways to name a default model is one too many.
/// Each worker that named one gets a preset of its own name, which becomes the
/// default when that worker was the configured one. The column is dropped
/// rather than left unread: a retirement that leaves the old place writable is
/// not one.
const PRESETS_REPLACE_WORKER_MODEL: &str = "
INSERT OR IGNORE INTO presets (name, agent, model, args, created_at)
 SELECT name, name, model, '[]', strftime('%s', 'now') FROM agents
  WHERE model IS NOT NULL AND model <> '';
INSERT OR REPLACE INTO config (key, value)
 SELECT 'preset', a.name FROM agents a
  WHERE a.model IS NOT NULL AND a.model <> ''
    AND a.name = COALESCE((SELECT value FROM config WHERE key = 'agent'), 'claude')
    AND NOT EXISTS (SELECT 1 FROM config WHERE key = 'preset' AND value <> '');
ALTER TABLE agents DROP COLUMN model;
";

/// Version 23. Presets: a named worker, model and configuration. Effort is not
/// a field, because what expresses it differs per agent -- `--thinking` for
/// one, `--variant` for another -- so a preset carries arguments and the worker
/// record says where they go. A preset takes part in no matching, so it
/// competes with no route; what it buys is a name for a combination worth
/// returning to, and a list of the ones in use.
///
/// The name and the arguments are recorded on each run, because a replay must
/// read back what ran rather than what the preset says today.
const PRESETS: &str = "
CREATE TABLE presets (
    name TEXT PRIMARY KEY,
    agent TEXT NOT NULL,
    model TEXT,
    args TEXT NOT NULL DEFAULT '[]',
    created_at INTEGER NOT NULL
);
ALTER TABLE runs ADD COLUMN preset TEXT;
ALTER TABLE runs ADD COLUMN extra_args TEXT;
";

/// Version 22. A worker's default model. A model name is only meaningful to
/// the worker that understands it, so storing it beside the choice of worker
/// meant naming another worker had to drop it. The value the `model` setting
/// held is carried onto the configured worker, and the setting is retired.
const WORKER_MODEL: &str = "
ALTER TABLE agents ADD COLUMN model TEXT;
UPDATE agents SET model = (SELECT value FROM config WHERE key = 'model')
 WHERE name = COALESCE((SELECT value FROM config WHERE key = 'agent'), 'claude')
   AND EXISTS (SELECT 1 FROM config WHERE key = 'model' AND value <> '');
DELETE FROM config WHERE key = 'model';
";

/// Version 21. Environment a worker needs, so reaching OpenAI, OpenRouter or
/// any OpenAI-compatible endpoint is a record rather than a code path: a base
/// URL or a config location belongs to the worker, and the key comes from the
/// session. Seeds the two other workers `pma` ships a template for, without
/// replacing one the user has already edited.
const WORKER_ENV: &str = "
ALTER TABLE agents ADD COLUMN env TEXT NOT NULL DEFAULT '{}';
";

/// Version 20. A workflow is a typed function over bags of units, stored as a
/// revision and activated like a routing policy. Units are immutable and carry
/// their lineage, so a lap, an annotation and a decomposition each leave a
/// readable chain; a node's bag is derived from the units it wrote and the
/// frontier from the moves recorded, so nothing holds a cursor a crash could
/// lose. The worst case is stored with the revision, because what `activate`
/// weighs against the budget must be what `propose` printed.
const WORKFLOWS: &str = "
CREATE TABLE workflows (
    revision INTEGER PRIMARY KEY,
    document TEXT NOT NULL,
    source TEXT,
    proposed_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    activated_at INTEGER,
    activated_by TEXT,
    worst_case_runs INTEGER,
    worst_case_cost REAL
);
CREATE TABLE workflow_instances (
    id INTEGER PRIMARY KEY,
    workflow TEXT NOT NULL,
    revision INTEGER NOT NULL REFERENCES workflows(revision),
    args TEXT NOT NULL,
    target TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    outcome TEXT
);
CREATE TABLE workflow_units (
    instance INTEGER NOT NULL REFERENCES workflow_instances(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    type TEXT NOT NULL,
    node TEXT NOT NULL,
    parent TEXT,
    root TEXT NOT NULL,
    depth INTEGER NOT NULL DEFAULT 0,
    lap INTEGER NOT NULL DEFAULT 0,
    project TEXT,
    data TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (instance, id)
);
CREATE INDEX idx_workflow_units_root ON workflow_units(instance, root);
CREATE TABLE workflow_moves (
    instance INTEGER NOT NULL REFERENCES workflow_instances(id) ON DELETE CASCADE,
    unit TEXT NOT NULL,
    edge INTEGER NOT NULL,
    taken INTEGER NOT NULL,
    reason TEXT,
    at INTEGER NOT NULL,
    PRIMARY KEY (instance, unit, edge)
);
CREATE TABLE workflow_verdicts (
    instance INTEGER NOT NULL REFERENCES workflow_instances(id) ON DELETE CASCADE,
    unit TEXT NOT NULL,
    check_name TEXT NOT NULL,
    verdict TEXT NOT NULL,
    detail TEXT,
    at INTEGER NOT NULL,
    PRIMARY KEY (instance, unit, check_name)
);
ALTER TABLE runs ADD COLUMN workflow_instance INTEGER;
ALTER TABLE runs ADD COLUMN node TEXT;
ALTER TABLE runs ADD COLUMN unit TEXT;
ALTER TABLE runs ADD COLUMN lap INTEGER NOT NULL DEFAULT 0;
";

/// Version 19. A project's private tags, for grouping and for selecting a set
/// to act on. A project carries several, so the tags cannot live in a column
/// on its row. Kept out of `projects` for a second reason: a tag is the user's
/// own, and every column there is overwritten by the next scan.
const PROJECT_TAGS: &str = "
CREATE TABLE project_tags (
    project TEXT NOT NULL,
    tag TEXT NOT NULL,
    PRIMARY KEY (project, tag)
);
CREATE INDEX idx_project_tags_tag ON project_tags(tag);
";

/// Version 18. When a full scan stopped finding a project under any root.
/// The row used to be deleted, which also dropped its tier and every task's
/// `first_seen` -- neither of which a later scan can rebuild, so a project
/// that left a root and came back lost the age of all its work.
const ABSENCE: &str = "
ALTER TABLE projects ADD COLUMN absent_since INTEGER;
";

/// Version 17. `owner/name` on GitHub, recorded by the scan. `sync` derived it
/// per invocation from the origin URL and no other command could reach it, so
/// an item's `gh:N` was a number with no repository to resolve it against.
const SLUG: &str = "
ALTER TABLE projects ADD COLUMN slug TEXT;
";

pub struct Store {
    conn: Connection,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectRow {
    pub name: String,
    pub path: PathBuf,
    /// `owner/name` on GitHub; `None` without a GitHub origin, or until the
    /// project is scanned again.
    pub slug: Option<String>,
    /// When a full scan stopped finding the project under any root; `None`
    /// while it is present. `path` is then where it was last seen.
    pub absent_since: Option<i64>,
    pub tier: Option<u8>,
    pub scanned_at: Option<i64>,
    pub has_todo: bool,
    pub lint_errors: i64,
    pub dirty: i64,
    /// `pma/` branches that no open run owned at the last scan.
    pub leftover: i64,
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

/// What a project's record consists of, for `pma forget`.
#[derive(Debug, Clone, PartialEq)]
pub struct Footprint {
    pub tasks: i64,
    pub tags: i64,
    pub campaigns: i64,
    /// Runs are kept, so this is reported rather than deleted.
    pub runs: i64,
    /// Runs that are not shipped or rejected; each may own a worktree.
    pub open_runs: Vec<i64>,
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

/// Exclusive right to run agents and change worktrees. `flock` releases it
/// when the process exits, so a crash leaves no stale lock.
pub struct Session {
    _file: File,
}

impl Session {
    /// `None` when another process holds the lock.
    pub fn try_acquire(dir: &Path) -> Result<Option<Session>> {
        let path = dir.join("session.lock");
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
        Ok(Some(Session { _file: file }))
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
                "another pma session{holder} is dispatching, reworking, rejecting or shipping; wait for it to finish"
            )
            .into()
        })
    }
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
        // Other commands may write while a session runs agents.
        conn.busy_timeout(Duration::from_secs(5))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        match version {
            0..VERSION => {
                let steps = [
                    SCHEMA,
                    RUNS,
                    DEPS_AND_NOTES,
                    LEFTOVER,
                    PR_OPEN,
                    ATTEMPTS,
                    SNAPSHOT,
                    VERIFY_BASE,
                    CHANGED_PATHS,
                    EXHAUSTION,
                    AGENTS,
                    RETIRE_KEYS,
                    COMPLEXITY,
                    ROUTES,
                    APPROVAL_EVIDENCE,
                    CAMPAIGNS,
                    SLUG,
                    ABSENCE,
                    PROJECT_TAGS,
                    WORKFLOWS,
                    WORKER_ENV,
                    WORKER_MODEL,
                    PRESETS,
                    PRESETS_REPLACE_WORKER_MODEL,
                ];
                let tx = conn.unchecked_transaction()?;
                for step in &steps[version as usize..] {
                    tx.execute_batch(step)?;
                }
                if version < 11 {
                    seed_agent(&tx, &crate::worker::Worker::claude())?;
                }
                if version < 21 {
                    // Not replacing an edited row: a template is a starting
                    // point, and the store is the record.
                    for w in [
                        crate::worker::Worker::opencode(),
                        crate::worker::Worker::omp(),
                    ] {
                        if tx.query_row(
                            "SELECT count(*) FROM agents WHERE name = ?1",
                            [&w.name],
                            |r| r.get::<_, i64>(0),
                        )? == 0
                        {
                            seed_agent(&tx, &w)?;
                        }
                    }
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
                    last_activity, ci, ci_detail, scan_error, deps, deps_detail, deps_at,
                    leftover, slug, absent_since
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
                leftover: r.get(15)?,
                slug: r.get(16)?,
                absent_since: r.get(17)?,
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
    /// Records the scan. A full scan marks every project it did not find as
    /// absent and returns their names; the rows stay, since a project that
    /// leaves a root is usually a move, not a deletion.
    pub fn save_scan(&mut self, facts: &[Facts], full: bool, now: i64) -> Result<Vec<String>> {
        let open_runs: Vec<Run> = self
            .runs()?
            .into_iter()
            .filter(|r| !r.state.is_final())
            .collect();
        let tx = self.conn.transaction()?;
        for f in facts {
            let (ci, ci_detail) = f.ci.to_columns();
            let leftover = f
                .pma_branches
                .iter()
                .filter(|b| {
                    !open_runs
                        .iter()
                        .any(|r| r.project == f.name && &r.branch == *b)
                })
                .count() as i64;
            tx.execute(
                "INSERT INTO projects (name, path, scanned_at, has_todo, lint_errors, dirty, ahead,
                                       last_activity, ci, ci_detail, scan_error, leftover, slug)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT (name) DO UPDATE SET
                    path = excluded.path, scanned_at = excluded.scanned_at,
                    has_todo = excluded.has_todo, lint_errors = excluded.lint_errors,
                    dirty = excluded.dirty, ahead = excluded.ahead,
                    last_activity = excluded.last_activity, ci = excluded.ci,
                    ci_detail = excluded.ci_detail, scan_error = excluded.scan_error,
                    leftover = excluded.leftover, slug = excluded.slug,
                    absent_since = NULL",
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
                    leftover,
                    f.slug,
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

        let mut absent = Vec::new();
        if full {
            let scanned: std::collections::HashSet<&str> =
                facts.iter().map(|f| f.name.as_str()).collect();
            let mut stmt = tx.prepare("SELECT name FROM projects WHERE absent_since IS NULL")?;
            let present: Vec<String> = stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            for name in present {
                if !scanned.contains(name.as_str()) {
                    tx.execute(
                        "UPDATE projects SET absent_since = ?2 WHERE name = ?1",
                        params![name, now],
                    )?;
                    absent.push(name);
                }
            }
        }
        tx.commit()?;
        Ok(absent)
    }

    /// What forgetting a project would delete, and what would hold it back.
    pub fn project_footprint(&self, name: &str) -> Result<Footprint> {
        let count =
            |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [name], |r| r.get(0))?) };
        let open_runs = self
            .runs()?
            .into_iter()
            .filter(|r| r.project == name && !r.state.is_final())
            .map(|r| r.id)
            .collect();
        Ok(Footprint {
            tasks: count("SELECT count(*) FROM tasks WHERE project = ?1")?,
            tags: count("SELECT count(*) FROM project_tags WHERE project = ?1")?,
            campaigns: count("SELECT count(*) FROM campaign_members WHERE project = ?1")?,
            runs: count("SELECT count(*) FROM runs WHERE project = ?1")?,
            open_runs,
        })
    }

    /// Deletes a project's record and everything keyed to it but its runs,
    /// whose worktrees may still exist and which the design keeps beyond a
    /// project's removal. Caller checks that nothing holds it back.
    pub fn forget_project(&mut self, name: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        for sql in [
            "DELETE FROM project_tags WHERE project = ?1",
            "DELETE FROM campaign_members WHERE project = ?1",
            "DELETE FROM exhaustion WHERE project = ?1",
            "DELETE FROM verify_base WHERE project = ?1",
            // `tasks` follows by ON DELETE CASCADE.
            "DELETE FROM projects WHERE name = ?1",
        ] {
            tx.execute(sql, [name])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Every `(project, tag)` pair, ordered by project then tag.
    pub fn project_tags(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT project, tag FROM project_tags ORDER BY project, tag")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Returns false when the project already had the tag.
    pub fn add_project_tag(&self, project: &str, tag: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "INSERT OR IGNORE INTO project_tags (project, tag) VALUES (?1, ?2)",
            params![project, tag],
        )? > 0)
    }

    /// Returns false when the project did not have the tag.
    pub fn remove_project_tag(&self, project: &str, tag: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM project_tags WHERE project = ?1 AND tag = ?2",
            params![project, tag],
        )? > 0)
    }

    /// The projects carrying any of `tags`, in name order. A project in
    /// several of them appears once.
    pub fn projects_tagged(&self, tags: &[String]) -> Result<Vec<String>> {
        let mut found: Vec<String> = Vec::new();
        for (project, tag) in self.project_tags()? {
            if tags.iter().any(|t| t == &tag) && !found.contains(&project) {
                found.push(project);
            }
        }
        found.sort();
        Ok(found)
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

    /// Assigns the run's id and stamps `dispatched_at`, in the row and in
    /// `run`.
    pub fn insert_run(&self, run: &mut Run) -> Result<i64> {
        run.dispatched_at = Some(crate::dates::now());
        self.conn.execute(
            "INSERT INTO runs (project, task_key, text, gh, quadrant, agent, branch, worktree,
                               default_branch, base, prompt, state, repo, dispatched_at,
                               class, scope, tier, description, agent_budget, timeout_minutes,
                               verify, verify_base_ok, verify_base_seconds, model,
                               complexity, features, estimator,
                               route_revision, route, approval,
                               workflow_instance, node, unit, lap, preset, extra_args)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                     ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                     ?31, ?32, ?33, ?34, ?35, ?36)",
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
                run.dispatched_at,
                run.class.map(Class::name),
                run.scope.join(","),
                run.tier,
                run.description,
                run.agent_budget,
                run.timeout_minutes,
                run.verify,
                run.verify_base_ok,
                run.verify_base_seconds,
                run.model,
                run.complexity,
                run.features.map(Features::to_json),
                run.complexity.map(|_| crate::complexity::ESTIMATOR),
                run.route_revision,
                run.route,
                run.approval.map(Approval::name),
                run.workflow_instance,
                run.node,
                run.unit,
                run.lap,
                run.preset,
                serde_json::to_string(&run.extra_args).unwrap_or_else(|_| "[]".into()),
            ],
        )?;
        run.id = self.conn.last_insert_rowid();
        Ok(run.id)
    }

    /// Writes every mutable column of the run with `run.id`.
    pub fn update_run(&self, run: &Run) -> Result<()> {
        self.conn.execute(
            // `verify` is frozen at dispatch: a command re-detected at rework
            // could differ from the one the base was checked with.
            "UPDATE runs SET state = ?2, feedback = ?3, seconds = ?4, cost_usd = ?5,
                summary = ?6, commits = ?7, diffstat = ?8, verify_ok = ?9,
                error = ?10, outcome = ?11, prompt = ?12, ready_at = ?13,
                decided_at = ?14, published_at = ?15, review_seconds = ?16,
                changed_paths = ?17, scope_error = ?18, approved_tree = ?19,
                approved_head = ?20, approved_by = ?21
             WHERE id = ?1",
            params![
                run.id,
                run.state.name(),
                run.feedback,
                run.seconds,
                run.cost_usd,
                run.summary,
                run.commits,
                run.diffstat,
                run.verify_ok,
                run.error,
                run.outcome,
                run.prompt,
                run.ready_at,
                run.decided_at,
                run.published_at,
                run.review_seconds,
                run.changed_paths
                    .as_ref()
                    .map(|p| serde_json::to_string(p).unwrap_or_default()),
                run.scope_error,
                run.approved_tree,
                run.approved_head,
                run.approved_by,
            ],
        )?;
        Ok(())
    }

    pub fn runs(&self) -> Result<Vec<Run>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project, task_key, text, gh, quadrant, agent, branch, worktree,
                    default_branch, base, prompt, state, feedback, seconds,
                    cost_usd, summary, commits, diffstat, verify, verify_ok, error, outcome, repo,
                    dispatched_at, ready_at, decided_at, published_at, review_seconds,
                    class, scope, tier, description, agent_budget, timeout_minutes,
                    verify_base_ok, verify_base_seconds, changed_paths, scope_error, model,
                    complexity, features, estimator, route_revision, route, approval,
                    approved_tree, approved_head, approved_by,
                    workflow_instance, node, unit, lap, preset, extra_args
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
                seconds: r.get(14)?,
                cost_usd: r.get(15)?,
                summary: r.get(16)?,
                commits: r.get(17)?,
                diffstat: r.get(18)?,
                verify: r.get(19)?,
                verify_ok: r.get(20)?,
                error: r.get(21)?,
                outcome: r.get(22)?,
                repo: PathBuf::from(r.get::<_, String>(23)?),
                dispatched_at: r.get(24)?,
                ready_at: r.get(25)?,
                decided_at: r.get(26)?,
                published_at: r.get(27)?,
                review_seconds: r.get(28)?,
                class: r
                    .get::<_, Option<String>>(29)?
                    .as_deref()
                    .and_then(Class::parse),
                scope: r
                    .get::<_, Option<String>>(30)?
                    .unwrap_or_default()
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
                tier: r.get(31)?,
                description: r.get(32)?,
                agent_budget: r.get(33)?,
                timeout_minutes: r.get(34)?,
                verify_base_ok: r.get(35)?,
                verify_base_seconds: r.get(36)?,
                changed_paths: r
                    .get::<_, Option<String>>(37)?
                    .and_then(|s| serde_json::from_str(&s).ok()),
                scope_error: r.get(38)?,
                model: r.get(39)?,
                complexity: r.get(40)?,
                features: r
                    .get::<_, Option<String>>(41)?
                    .as_deref()
                    .and_then(Features::from_json),
                estimator: r.get(42)?,
                route_revision: r.get(43)?,
                route: r.get(44)?,
                approval: r
                    .get::<_, Option<String>>(45)?
                    .as_deref()
                    .and_then(Approval::parse),
                approved_tree: r.get(46)?,
                approved_head: r.get(47)?,
                approved_by: r.get(48)?,
                workflow_instance: r.get(49)?,
                node: r.get(50)?,
                unit: r.get(51)?,
                lap: r.get(52)?,
                preset: r.get(53)?,
                extra_args: r
                    .get::<_, Option<String>>(54)?
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
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

    /// Creates a campaign with a fixed membership. The set does not change
    /// afterwards: a rescan must not move work under a campaign in flight.
    pub fn add_campaign(&self, c: &Campaign) -> Result<()> {
        if c.members.is_empty() {
            return Err(format!("campaign `{}` selects no project", c.name).into());
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO campaigns (name, text, description, class, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![c.name, c.text, c.description, c.class.name(), c.created_at],
        )
        .map_err(|e| format!("campaign `{}`: {e}", c.name))?;
        for (project, _) in &c.members {
            tx.execute(
                "INSERT INTO campaign_members (campaign, project) VALUES (?1, ?2)",
                params![c.name, project],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn campaigns(&self) -> Result<Vec<Campaign>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, text, description, class, created_at FROM campaigns ORDER BY name",
        )?;
        let rows: Vec<Campaign> = stmt
            .query_map([], |r| {
                let class: String = r.get(3)?;
                Ok(Campaign {
                    name: r.get(0)?,
                    text: r.get(1)?,
                    description: r.get(2)?,
                    class: Class::parse(&class).unwrap_or(Class::Specified),
                    created_at: r.get(4)?,
                    members: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut stmt = self
            .conn
            .prepare("SELECT campaign, project, run_id FROM campaign_members ORDER BY project")?;
        let members: Vec<(String, String, Option<i64>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .map(|mut c| {
                c.members = members
                    .iter()
                    .filter(|(name, ..)| *name == c.name)
                    .map(|(_, p, run)| (p.clone(), *run))
                    .collect();
                c
            })
            .collect())
    }

    pub fn campaign(&self, name: &str) -> Result<Campaign> {
        self.campaigns()?
            .into_iter()
            .find(|c| c.name == name)
            .ok_or_else(|| format!("unknown campaign `{name}`; see `pma campaign`").into())
    }

    /// Records which run took a member, so a restart does not dispatch it
    /// again and open a second pull request.
    pub fn set_campaign_run(&self, campaign: &str, project: &str, run: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE campaign_members SET run_id = ?3 WHERE campaign = ?1 AND project = ?2",
            params![campaign, project, run],
        )?;
        Ok(())
    }

    /// Returns false when no campaign has the name.
    pub fn remove_campaign(&self, name: &str) -> Result<bool> {
        self.conn
            .execute("DELETE FROM campaign_members WHERE campaign = ?1", [name])?;
        Ok(self
            .conn
            .execute("DELETE FROM campaigns WHERE name = ?1", [name])?
            > 0)
    }

    /// Stores a policy revision as a draft and returns its number. Storing
    /// is not activating: nothing routes by it until someone says so.
    pub fn add_route_revision(&self, document: &str, by: &str) -> Result<i64> {
        // Refused here, so an unreadable document never reaches the table.
        // Stored as `pma` read it, with every default written out, so two
        // revisions diff by what they mean rather than by how they were
        // typed, and `pma route show` states the route names a reader will
        // see in a replay.
        let document = Policy::parse(document)?.to_json();
        let next: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(revision), 0) + 1 FROM routes",
            [],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO routes (revision, document, proposed_by, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![next, document, by, crate::dates::now()],
        )?;
        Ok(next)
    }

    /// Puts a revision into effect, in shadow or for real, and retires any
    /// other. `by` is recorded: raising autonomy is somebody's decision.
    pub fn activate_route(&self, revision: i64, by: &str, shadow: bool) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let found = tx.execute(
            "UPDATE routes SET activated_at = ?2, activated_by = ?3, shadow = ?4
             WHERE revision = ?1",
            params![revision, crate::dates::now(), by, shadow],
        )?;
        if found == 0 {
            return Err(format!("no policy revision {revision}").into());
        }
        tx.execute(
            "UPDATE routes SET activated_at = NULL, activated_by = NULL WHERE revision <> ?1",
            [revision],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every revision, oldest first.
    pub fn route_revisions(&self) -> Result<Vec<RouteRevision>> {
        let mut stmt = self.conn.prepare(
            "SELECT revision, document, proposed_by, created_at, activated_at, activated_by,
                    shadow
             FROM routes ORDER BY revision",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(RouteRevision {
                revision: r.get(0)?,
                document: r.get(1)?,
                proposed_by: r.get(2)?,
                created_at: r.get(3)?,
                activated_at: r.get(4)?,
                activated_by: r.get(5)?,
                shadow: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The revision in effect, if any.
    pub fn active_route(&self) -> Result<Option<RouteRevision>> {
        Ok(self
            .route_revisions()?
            .into_iter()
            .find(|r| r.activated_at.is_some()))
    }

    /// Stores a document as a draft. Refused here, so an unreadable one never
    /// reaches the table, and stored normalised, so two revisions diff by what
    /// they mean rather than by how they were typed -- or by whether a script
    /// or a person wrote them. `source` keeps the script for provenance.
    pub fn add_workflow_revision(
        &self,
        doc: &crate::workflow::Document,
        source: Option<&str>,
        by: &str,
        worst_case_runs: i64,
        worst_case_cost: f64,
    ) -> Result<i64> {
        let next: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(revision), 0) + 1 FROM workflows",
            [],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO workflows
                (revision, document, source, proposed_by, created_at,
                 worst_case_runs, worst_case_cost)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                next,
                doc.to_json(),
                source,
                by,
                crate::dates::now(),
                worst_case_runs,
                worst_case_cost
            ],
        )?;
        Ok(next)
    }

    /// Puts a revision into effect and retires any other. `by` is recorded:
    /// what a workflow may spend is somebody's decision.
    pub fn activate_workflow(&self, revision: i64, by: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let found = tx.execute(
            "UPDATE workflows SET activated_at = ?2, activated_by = ?3 WHERE revision = ?1",
            params![revision, crate::dates::now(), by],
        )?;
        if found == 0 {
            return Err(format!("no workflow revision {revision}").into());
        }
        tx.execute(
            "UPDATE workflows SET activated_at = NULL, activated_by = NULL WHERE revision <> ?1",
            [revision],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every revision, oldest first.
    pub fn workflow_revisions(&self) -> Result<Vec<WorkflowRevision>> {
        let mut stmt = self.conn.prepare(
            "SELECT revision, document, source, proposed_by, created_at, activated_at,
                    activated_by, worst_case_runs, worst_case_cost
             FROM workflows ORDER BY revision",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(WorkflowRevision {
                revision: r.get(0)?,
                document: r.get(1)?,
                source: r.get(2)?,
                proposed_by: r.get(3)?,
                created_at: r.get(4)?,
                activated_at: r.get(5)?,
                activated_by: r.get(6)?,
                worst_case_runs: r.get(7)?,
                worst_case_cost: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The revision in effect, if any.
    pub fn active_workflow(&self) -> Result<Option<WorkflowRevision>> {
        Ok(self
            .workflow_revisions()?
            .into_iter()
            .find(|r| r.activated_at.is_some()))
    }

    /// Starts an instance: the frozen argument bag and what it was run over.
    pub fn add_workflow_instance(
        &self,
        workflow: &str,
        revision: i64,
        args: &str,
        target: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO workflow_instances (workflow, revision, args, target, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![workflow, revision, args, target, crate::dates::now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn workflow_instances(&self) -> Result<Vec<WorkflowInstance>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workflow, revision, args, target, started_at, finished_at, outcome
             FROM workflow_instances ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(WorkflowInstance {
                id: r.get(0)?,
                workflow: r.get(1)?,
                revision: r.get(2)?,
                args: r.get(3)?,
                target: r.get(4)?,
                started_at: r.get(5)?,
                finished_at: r.get(6)?,
                outcome: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn finish_instance(&self, instance: i64, outcome: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_instances SET finished_at = ?2, outcome = ?3 WHERE id = ?1",
            params![instance, crate::dates::now(), outcome],
        )?;
        Ok(())
    }

    /// Units are immutable, so this only ever inserts.
    pub fn add_workflow_unit(&self, instance: i64, u: &WorkflowUnit) -> Result<()> {
        self.conn.execute(
            "INSERT INTO workflow_units
                (instance, id, type, node, parent, root, depth, lap, project, data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                instance,
                u.id,
                u.ty,
                u.node,
                u.parent,
                u.root,
                u.depth,
                u.lap,
                u.project,
                u.data,
                crate::dates::now()
            ],
        )?;
        Ok(())
    }

    pub fn workflow_units(&self, instance: i64) -> Result<Vec<WorkflowUnit>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, type, node, parent, root, depth, lap, project, data
             FROM workflow_units WHERE instance = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map([instance], |r| {
            Ok(WorkflowUnit {
                id: r.get(0)?,
                ty: r.get(1)?,
                node: r.get(2)?,
                parent: r.get(3)?,
                root: r.get(4)?,
                depth: r.get(5)?,
                lap: r.get(6)?,
                project: r.get(7)?,
                data: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One row per unit and edge: which way it went, or the guard that
    /// refused it. A unit that stopped is therefore answerable for.
    pub fn add_workflow_move(
        &self,
        instance: i64,
        unit: &str,
        edge: usize,
        taken: bool,
        reason: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO workflow_moves (instance, unit, edge, taken, reason, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                instance,
                unit,
                edge as i64,
                taken,
                reason,
                crate::dates::now()
            ],
        )?;
        Ok(())
    }

    /// Every move recorded, as `(unit, edge, taken)`.
    pub fn workflow_moves(&self, instance: i64) -> Result<Vec<(String, usize, bool)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT unit, edge, taken FROM workflow_moves WHERE instance = ?1")?;
        let rows = stmt.query_map([instance], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)? as usize,
                r.get::<_, bool>(2)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_workflow_verdict(
        &self,
        instance: i64,
        unit: &str,
        check: &str,
        verdict: &str,
        detail: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO workflow_verdicts
                (instance, unit, check_name, verdict, detail, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![instance, unit, check, verdict, detail, crate::dates::now()],
        )?;
        Ok(())
    }

    /// Verdicts by unit and check name.
    pub fn workflow_verdicts(&self, instance: i64) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT unit, check_name, verdict FROM workflow_verdicts WHERE instance = ?1",
        )?;
        let rows = stmt.query_map([instance], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Names a worker, a model and the arguments that configure it. Replaces
    /// one of the same name.
    pub fn set_preset(&self, p: &Preset) -> Result<()> {
        if self.agent(&p.agent).is_err() {
            return Err(format!("no worker `{}`; `pma agent` lists them", p.agent).into());
        }
        self.conn.execute(
            "INSERT OR REPLACE INTO presets (name, agent, model, args, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                p.name,
                p.agent,
                p.model,
                serde_json::to_string(&p.args).unwrap_or_else(|_| "[]".into()),
                crate::dates::now()
            ],
        )?;
        Ok(())
    }

    pub fn presets(&self) -> Result<Vec<Preset>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, agent, model, args FROM presets ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            Ok(Preset {
                name: r.get(0)?,
                agent: r.get(1)?,
                model: r.get::<_, Option<String>>(2)?.filter(|m| !m.is_empty()),
                args: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn preset(&self, name: &str) -> Result<Preset> {
        self.presets()?
            .into_iter()
            .find(|p| p.name == name)
            .ok_or_else(|| format!("no preset `{name}`; `pma preset` lists them").into())
    }

    /// Returns false when no preset has the name.
    pub fn remove_preset(&self, name: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM presets WHERE name = ?1", [name])?
            > 0)
    }

    /// Every worker, by name.
    pub fn agents(&self) -> Result<Vec<Worker>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, command, args, allow, parse, reports_cost, enforces_budget,
                    sandbox, resumes, env
             FROM agents ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| {
            let args: String = r.get(2)?;
            let parse: String = r.get(4)?;
            Ok(Worker {
                name: r.get(0)?,
                command: r.get(1)?,
                args: serde_json::from_str(&args).unwrap_or_default(),
                allow: r.get::<_, Option<String>>(3)?.filter(|a| !a.is_empty()),
                parse: Parser::parse(&parse).unwrap_or(Parser::TextTail),
                reports_cost: r.get(5)?,
                enforces_budget: r.get(6)?,
                sandbox: r.get(7)?,
                resumes: r.get(8)?,
                env: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn agent(&self, name: &str) -> Result<Worker> {
        self.agents()?
            .into_iter()
            .find(|w| w.name == name)
            .ok_or_else(|| format!("unknown agent `{name}`; see `pma agent`").into())
    }

    pub fn set_agent(&self, w: &Worker) -> Result<()> {
        seed_agent(&self.conn, w)
    }

    /// Returns false when no agent has the name.
    pub fn remove_agent(&self, name: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM agents WHERE name = ?1", [name])?
            > 0)
    }

    /// Decided runs in a project and how many were accepted. An open pull
    /// request is in neither: no one has judged it.
    pub fn decided_runs(&self, project: &str) -> Result<(i64, i64)> {
        let mut decided = 0;
        let mut accepted = 0;
        for run in self.runs()?.iter().filter(|r| r.project == project) {
            match run.state {
                RunState::Approved | RunState::Shipped => {
                    decided += 1;
                    accepted += 1;
                }
                RunState::Rejected => decided += 1,
                _ => {}
            }
        }
        Ok((decided, accepted))
    }

    /// Attempts consumed against a task revision, across every run of it.
    pub fn consumed_attempts(&self, project: &str, revision: &str) -> Result<i64> {
        Ok(self
            .conn
            .query_row(
                "SELECT attempts FROM exhaustion WHERE project = ?1 AND revision = ?2",
                params![project, revision],
                |r| r.get(0),
            )
            .unwrap_or(0))
    }

    /// Records one consumed attempt and returns the new total.
    pub fn consume_attempt(&self, project: &str, revision: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO exhaustion (project, revision, attempts, updated_at)
             VALUES (?1, ?2, 1, ?3)
             ON CONFLICT (project, revision)
             DO UPDATE SET attempts = attempts + 1, updated_at = ?3",
            params![project, revision, crate::dates::now()],
        )?;
        self.consumed_attempts(project, revision)
    }

    /// Returns false when the revision had consumed none.
    pub fn reset_attempts(&self, project: &str, revision: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM exhaustion WHERE project = ?1 AND revision = ?2",
            params![project, revision],
        )? > 0)
    }

    /// A base verification of this exact repository, commit, command and
    /// timeout, or `None` when it has not been measured.
    pub fn verify_base(
        &self,
        project: &str,
        base: &str,
        command: &str,
        timeout_minutes: i64,
    ) -> Result<Option<(Option<bool>, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT ok, seconds FROM verify_base
             WHERE project = ?1 AND base = ?2 AND command = ?3 AND timeout_minutes = ?4",
        )?;
        let mut rows = stmt.query_map(params![project, base, command, timeout_minutes], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn set_verify_base(
        &self,
        project: &str,
        base: &str,
        command: &str,
        timeout_minutes: i64,
        ok: Option<bool>,
        seconds: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO verify_base
                (project, base, command, timeout_minutes, ok, seconds, measured_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                project,
                base,
                command,
                timeout_minutes,
                ok,
                seconds,
                crate::dates::now()
            ],
        )?;
        Ok(())
    }

    /// Appends an attempt. `n` is assigned here, so two reworks cannot race
    /// to the same number.
    pub fn insert_attempt(&self, a: &Attempt) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO attempts (run_id, n, agent, prompt, feedback, started_at, seconds,
                                   cost_usd, summary, verify, verify_ok, error, outcome, model)
             VALUES (?1, (SELECT COALESCE(MAX(n), 0) + 1 FROM attempts WHERE run_id = ?1),
                     ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                a.run_id,
                a.agent,
                a.prompt,
                a.feedback,
                a.started_at,
                a.seconds,
                a.cost_usd,
                a.summary,
                a.verify,
                a.verify_ok,
                a.error,
                a.outcome,
                a.model,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Every attempt, oldest first. `run` limits it to one run.
    pub fn attempts(&self, run: Option<i64>) -> Result<Vec<Attempt>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, n, agent, prompt, feedback, started_at, seconds, cost_usd,
                    summary, verify, verify_ok, error, outcome, model
             FROM attempts WHERE (?1 IS NULL OR run_id = ?1) ORDER BY run_id, n",
        )?;
        let rows = stmt.query_map([run], |r| {
            Ok(Attempt {
                id: r.get(0)?,
                run_id: r.get(1)?,
                n: r.get(2)?,
                agent: r.get(3)?,
                prompt: r.get(4)?,
                feedback: r.get(5)?,
                started_at: r.get(6)?,
                seconds: r.get(7)?,
                cost_usd: r.get(8)?,
                summary: r.get(9)?,
                verify: r.get(10)?,
                verify_ok: r.get(11)?,
                error: r.get(12)?,
                outcome: r.get(13)?,
                model: r.get(14)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Marks runs left queued or running by a session that ended as failed.
    /// Only a `Session` holder runs agents, so while it is held none is live.
    pub fn fail_interrupted_runs(&self, _held: &Session) -> Result<usize> {
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
    /// Published as a pull request that is neither merged nor closed.
    PrOpen,
    Rejected,
    Shipped,
}

impl RunState {
    const ALL: [(&'static str, RunState); 8] = [
        ("queued", RunState::Queued),
        ("running", RunState::Running),
        ("ready", RunState::Ready),
        ("failed", RunState::Failed),
        ("approved", RunState::Approved),
        ("pr-open", RunState::PrOpen),
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
    /// What shipping did: a pushed commit, or a pull request URL.
    pub outcome: Option<String>,
    /// When the run row was created. Never updated, so a per-day count and a
    /// digest window have a stable date. Per-attempt start times are in
    /// `attempts`.
    pub dispatched_at: Option<i64>,
    /// When the run last became ready. A rework moves it forward.
    pub ready_at: Option<i64>,
    /// The first approval or rejection. A pull request closed later does not
    /// replace the approval that published it.
    pub decided_at: Option<i64>,
    /// When `pma` published: the push, or the pull request opening. A merge
    /// days later does not move it.
    pub published_at: Option<i64>,
    /// Review time the user reported with `pma review --minutes`, summed over
    /// reviews of this run.
    pub review_seconds: Option<i64>,
    /// The class predicted at dispatch. `None` for a run dispatched before
    /// classes existed.
    pub class: Option<Class>,
    /// Globs the class allowed, resolved at dispatch. Empty means the class
    /// stated no bound; the privileged paths still apply.
    pub scope: Vec<String>,
    /// The project's tier at dispatch. Tiers change, and routes match on them.
    pub tier: Option<u8>,
    /// The item's description lines as dispatched.
    pub description: Option<String>,
    /// `agent_budget` at dispatch, so an attempt's cost can be read against
    /// the limit that applied to it.
    pub agent_budget: Option<f64>,
    pub timeout_minutes: Option<i64>,
    /// `verify` at the base commit, before the agent ran. `None` when there
    /// is no command, or when the run predates the check; `Some(false)` also
    /// covers a base run that timed out.
    pub verify_base_ok: Option<bool>,
    pub verify_base_seconds: Option<i64>,
    /// Every path the run changed against its base, both sides of a rename
    /// included. `None` when they could not be enumerated: an empty list
    /// would read as a clean run.
    pub changed_paths: Option<Vec<String>>,
    /// Why the paths could not be enumerated.
    pub scope_error: Option<String>,
    /// The model chosen at dispatch, or `None` for the worker's own default.
    pub model: Option<String>,
    /// 1 to 5, from the rule named in `estimator`.
    pub complexity: Option<i64>,
    /// What that rule read.
    pub features: Option<Features>,
    pub estimator: Option<String>,
    /// The policy revision in effect at dispatch, and the route it matched.
    /// A later activation does not change either.
    pub route_revision: Option<i64>,
    pub route: Option<String>,
    pub approval: Option<Approval>,
    /// The tree the approver saw, as `git write-tree` names it. Ship refuses
    /// to publish a worktree that no longer matches.
    pub approved_tree: Option<String>,
    /// The commit the worktree was on. Ship commits before it publishes, so
    /// the tree check applies only while the head is still this one; past
    /// that, a resumed ship is recognised by its own pushed commits.
    pub approved_head: Option<String>,
    pub approved_by: Option<String>,
    /// The workflow instance that dispatched this run, or `None` for a task
    /// dispatched on its own.
    pub workflow_instance: Option<i64>,
    /// The qualified node, which a route matches on and a report groups by.
    pub node: Option<String>,
    /// The unit that drove it.
    pub unit: Option<String>,
    /// Laps the unit had completed. 0 outside a workflow.
    pub lap: i64,
    /// The preset this run was dispatched under, where one named it.
    pub preset: Option<String>,
    /// Arguments the preset added, recorded so a replay reads back what ran.
    pub extra_args: Vec<String>,
}

impl Run {
    /// A run with every field empty, for tests in this crate to override.
    #[cfg(test)]
    pub fn blank() -> Run {
        Run {
            id: 0,
            project: "p".into(),
            task_key: "fix it".into(),
            text: "Fix it".into(),
            gh: None,
            quadrant: None,
            agent: "claude".into(),
            repo: PathBuf::from("/r/p"),
            branch: "pma/fix-it".into(),
            worktree: PathBuf::from("/w/p/fix-it"),
            default_branch: "main".into(),
            base: "abc".into(),
            prompt: "do it".into(),
            state: RunState::Queued,
            feedback: None,
            seconds: None,
            cost_usd: None,
            summary: None,
            commits: None,
            diffstat: None,
            verify: None,
            verify_ok: None,
            error: None,
            outcome: None,
            dispatched_at: None,
            ready_at: None,
            decided_at: None,
            published_at: None,
            review_seconds: None,
            class: None,
            scope: Vec::new(),
            tier: None,
            description: None,
            agent_budget: None,
            timeout_minutes: None,
            verify_base_ok: None,
            verify_base_seconds: None,
            changed_paths: None,
            scope_error: None,
            model: None,
            complexity: None,
            features: None,
            estimator: None,
            route_revision: None,
            route: None,
            approval: None,
            approved_tree: None,
            approved_head: None,
            approved_by: None,
            workflow_instance: None,
            node: None,
            unit: None,
            lap: 0,
            preset: None,
            extra_args: Vec::new(),
        }
    }

    /// Moves the run to `state` and stamps the transition. Call it instead of
    /// assigning `state`, so no transition goes unrecorded.
    pub fn enter(&mut self, state: RunState) {
        let now = crate::dates::now();
        match state {
            RunState::Ready => self.ready_at = Some(now),
            RunState::Approved | RunState::Rejected => {
                self.decided_at.get_or_insert(now);
            }
            RunState::Shipped | RunState::PrOpen => {
                self.published_at.get_or_insert(now);
            }
            _ => {}
        }
        self.state = state;
    }
}

/// One agent invocation and the verification that followed it. Rows are
/// appended and never updated, so a rework cannot overwrite what the previous
/// attempt did.
#[derive(Debug, Clone, PartialEq)]
pub struct Attempt {
    pub id: i64,
    pub run_id: i64,
    /// 1 for the first attempt, then one per rework.
    pub n: i64,
    pub agent: String,
    pub prompt: String,
    /// The reviewer feedback this attempt was given, if any.
    pub feedback: Option<String>,
    pub started_at: i64,
    /// Agent and verify time for this attempt alone.
    pub seconds: Option<i64>,
    /// `None` when the agent reports no cost. Never zero for unknown.
    pub cost_usd: Option<f64>,
    pub summary: Option<String>,
    pub verify: Option<String>,
    pub verify_ok: Option<bool>,
    pub error: Option<String>,
    /// The state the run reached: `ready` or `failed`. Null for the attempt
    /// reconstructed from a run that predates this table.
    pub outcome: Option<String>,
    /// The model this attempt ran, or `None` for the worker's own default.
    pub model: Option<String>,
}

/// One task definition applied across a fixed set of repositories.
#[derive(Debug, Clone, PartialEq)]
pub struct Campaign {
    pub name: String,
    pub text: String,
    pub description: String,
    pub class: Class,
    pub created_at: i64,
    /// Project, and the run that took it, if any.
    pub members: Vec<(String, Option<i64>)>,
}

/// One stored policy document and who put it where it is.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteRevision {
    pub revision: i64,
    pub document: String,
    pub proposed_by: String,
    pub created_at: i64,
    pub activated_at: Option<i64>,
    pub activated_by: Option<String>,
    /// Computed and recorded on each run, but not applied.
    pub shadow: bool,
}

/// A named worker, model and configuration. `model` unset means the worker's
/// own default, and `args` are inserted where its record puts `{extra}`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Preset {
    pub name: String,
    pub agent: String,
    pub model: Option<String>,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowInstance {
    pub id: i64,
    pub workflow: String,
    pub revision: i64,
    /// The arguments the pass was given, as JSON, so a replay reads the same
    /// instantiation rather than the document's defaults.
    pub args: String,
    /// What the argument bag was taken from.
    pub target: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub outcome: Option<String>,
}

/// A unit as stored: immutable, carrying its lineage and its data.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowUnit {
    pub id: String,
    pub ty: String,
    /// The node that wrote it; `@input` for a root unit.
    pub node: String,
    pub parent: Option<String>,
    pub root: String,
    pub depth: i64,
    pub lap: i64,
    pub project: Option<String>,
    /// The declared fields, as JSON.
    pub data: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowRevision {
    pub revision: i64,
    /// The document as `pma` read it.
    pub document: String,
    /// The script that built it, where one did.
    pub source: Option<String>,
    pub proposed_by: String,
    pub created_at: i64,
    pub activated_at: Option<i64>,
    pub activated_by: Option<String>,
    /// Over one unit of input, so what a pass may spend is this times the
    /// argument bag.
    pub worst_case_runs: Option<i64>,
    pub worst_case_cost: Option<f64>,
}

impl WorkflowRevision {
    pub fn document(&self) -> Result<crate::workflow::Document> {
        Ok(crate::workflow::Document::parse(&self.document)?)
    }
}

impl RouteRevision {
    pub fn policy(&self) -> Result<Policy> {
        Ok(Policy::parse(&self.document)?)
    }
}

fn seed_agent(conn: &Connection, w: &Worker) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO agents
            (name, command, args, allow, parse, reports_cost, enforces_budget, sandbox,
             resumes, env)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            w.name,
            w.command,
            serde_json::to_string(&w.args).unwrap_or_default(),
            w.allow,
            w.parse.name(),
            w.reports_cost,
            w.enforces_budget,
            w.sandbox,
            w.resumes,
            serde_json::to_string(&w.env).unwrap_or_else(|_| "{}".into()),
        ],
    )?;
    Ok(())
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
            slug: Some(format!("shakfu/{name}")),
            todo: Some(TodoFacts {
                lint_errors: 1,
                items,
            }),
            dirty: 3,
            pma_branches: vec![],
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
        for (version, steps) in [
            (1, &[SCHEMA][..]),
            (2, &[SCHEMA, RUNS][..]),
            (3, &[SCHEMA, RUNS, DEPS_AND_NOTES][..]),
            (4, &[SCHEMA, RUNS, DEPS_AND_NOTES, LEFTOVER][..]),
        ] {
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
            if version >= 2 {
                for outcome in ["https://github.com/o/r/pull/3", "pushed abc to main"] {
                    conn.execute(
                        "INSERT INTO runs (project, task_key, text, agent, repo, branch, worktree,
                                           default_branch, base, prompt, state, outcome)
                         VALUES ('kept', 'k', 't', 'claude', '/k', 'pma/t', '/w', 'main', 'b',
                                 'p', 'shipped', ?1)",
                        [outcome],
                    )
                    .unwrap();
                }
            }
            drop(conn);
            let store = Store::open(&db).unwrap();
            let p = store.project("kept").unwrap().unwrap();
            assert_eq!(
                (p.deps, p.deps_detail.as_str(), p.leftover),
                (None, "", 0),
                "v{version}"
            );
            let states: Vec<_> = store.runs().unwrap().iter().map(|r| r.state).collect();
            if version >= 2 {
                assert_eq!(
                    states,
                    [RunState::PrOpen, RunState::Shipped],
                    "v{version}: a shipped pull request is open again"
                );
            }
            assert!(store.notes().unwrap().is_empty());
            drop(store);
            assert!(Store::open(&db).is_ok(), "v{version} reopens");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Migration 6 turns each existing run's single recorded attempt into an
    /// `attempts` row, so upgrading loses no history, and moves the run's
    /// start time to `dispatched_at`. Migration 7 adds the decision snapshot,
    /// which an existing run cannot supply.
    #[test]
    fn a_v5_database_gains_an_attempt_per_run() {
        let dir = scratch("attempts-migrate");
        let db = dir.join("v5.db");
        let conn = Connection::open(&db).unwrap();
        for step in [SCHEMA, RUNS, DEPS_AND_NOTES, LEFTOVER, PR_OPEN] {
            conn.execute_batch(step).unwrap();
        }
        conn.pragma_update(None, "user_version", 5).unwrap();
        conn.execute_batch(
            "INSERT INTO runs (project, task_key, text, agent, repo, branch, worktree,
                               default_branch, base, prompt, state, started_at, seconds,
                               cost_usd, summary, verify, verify_ok)
             VALUES ('p', 'k', 't', 'claude', '/r', 'pma/t', '/w', 'main', 'b', 'do it',
                     'ready', 100, 42, 0.5, 'first summary', 'make test', 1);
             INSERT INTO runs (project, task_key, text, agent, repo, branch, worktree,
                               default_branch, base, prompt, state)
             VALUES ('p', 'k2', 't2', 'claude', '/r', 'pma/t2', '/w2', 'main', 'b', 'do it',
                     'queued');",
        )
        .unwrap();
        drop(conn);

        let store = Store::open(&db).unwrap();
        let runs = store.runs().unwrap();
        assert_eq!(runs[0].dispatched_at, Some(100));
        // Never started, so it contributes no attempt and no dispatch time.
        assert_eq!(runs[1].dispatched_at, None);

        let attempts = store.attempts(None).unwrap();
        assert_eq!(attempts.len(), 1, "only the run that started");
        let a = &attempts[0];
        assert_eq!((a.run_id, a.n, a.started_at), (runs[0].id, 1, 100));
        assert_eq!(a.summary.as_deref(), Some("first summary"));
        assert_eq!(
            (a.seconds, a.cost_usd, a.verify_ok),
            (Some(42), Some(0.5), Some(true))
        );
        assert_eq!(a.outcome, None, "the old row did not record one");
        // Migration 7 cannot reconstruct a decision that was never recorded.
        assert_eq!(runs[0].class, None);
        assert_eq!(runs[0].tier, None);
        assert!(runs[0].scope.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Upgrading seeds `claude` with exactly the behaviour it had when it
    /// was compiled in, so a database made before the adapter keeps working.
    #[test]
    fn upgrading_seeds_the_worker_that_was_compiled_in() {
        let dir = scratch("agents");
        let store = Store::open(&dir.join("p.db")).unwrap();
        // Three templates, alphabetically: the one that was compiled in, and
        // the two installed separately.
        assert_eq!(
            store.agents().unwrap(),
            [Worker::claude(), Worker::omp(), Worker::opencode()]
        );

        let mut w = Worker::claude();
        w.name = "codex".into();
        w.command = "codex".into();
        w.args = vec![
            "exec".into(),
            "-C".into(),
            "{dir}".into(),
            "{prompt}".into(),
        ];
        w.allow = None;
        w.parse = Parser::TextTail;
        w.reports_cost = false;
        w.sandbox = true;
        store.set_agent(&w).unwrap();
        assert_eq!(
            store.agents().unwrap(),
            [
                Worker::claude(),
                w.clone(),
                Worker::omp(),
                Worker::opencode()
            ]
        );
        assert_eq!(store.agent("codex").unwrap(), w);
        assert!(store.agent("cursor").is_err());

        assert!(store.remove_agent("codex").unwrap());
        assert!(!store.remove_agent("codex").unwrap());
        assert_eq!(
            store.agents().unwrap(),
            [Worker::claude(), Worker::omp(), Worker::opencode()]
        );

        // A template is a starting point: an edited row survives reopening,
        // which is what seeding only an absent name buys.
        let mut edited = Worker::opencode();
        edited.env = std::collections::BTreeMap::from([(
            "OPENAI_BASE_URL".to_string(),
            "http://localhost:11434/v1".to_string(),
        )]);
        store.set_agent(&edited).unwrap();
        drop(store);
        let store = Store::open(&dir.join("p.db")).unwrap();
        assert_eq!(store.agent("opencode").unwrap(), edited);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A worker that named a model before presets existed keeps that pairing:
    /// it becomes a preset of the worker's name, and the default when that
    /// worker was the configured one.
    #[test]
    fn retiring_a_workers_model_leaves_a_preset_in_its_place() {
        let dir = scratch("worker-model");
        let path = dir.join("p.db");
        // A store as schema 23 left it: the column exists and holds a model.
        {
            let store = Store::open(&path).unwrap();
            store
                .conn
                .execute_batch("ALTER TABLE agents ADD COLUMN model TEXT")
                .unwrap();
            store
                .conn
                .execute("UPDATE agents SET model = 'opus' WHERE name = 'claude'", [])
                .unwrap();
            store.set_config("agent", "claude").unwrap();
            store.conn.pragma_update(None, "user_version", 23).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.preset("claude").unwrap(),
            Preset {
                name: "claude".into(),
                agent: "claude".into(),
                model: Some("opus".into()),
                args: Vec::new(),
            }
        );
        assert_eq!(
            store
                .config_rows()
                .unwrap()
                .iter()
                .find(|(k, _)| k == "preset"),
            Some(&("preset".to_string(), "claude".to_string())),
            "and it is the default, because that worker was the configured one"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The counter survives the run that raised it, so rejecting a task and
    /// dispatching it again does not start from zero. Rewording it does.
    #[test]
    fn consumed_attempts_are_kept_per_task_revision() {
        let dir = scratch("exhaustion");
        let store = Store::open(&dir.join("p.db")).unwrap();
        assert_eq!(store.consumed_attempts("p", "fix the parser").unwrap(), 0);

        assert_eq!(store.consume_attempt("p", "fix the parser").unwrap(), 1);
        assert_eq!(store.consume_attempt("p", "fix the parser").unwrap(), 2);
        assert_eq!(store.consumed_attempts("p", "fix the parser").unwrap(), 2);
        // A reworded task, another project, and a `ci` incident naming other
        // workflows are each their own revision.
        assert_eq!(
            store
                .consumed_attempts("p", "fix the parser on empty input")
                .unwrap(),
            0
        );
        assert_eq!(store.consumed_attempts("q", "fix the parser").unwrap(), 0);

        assert!(store.reset_attempts("p", "fix the parser").unwrap());
        assert_eq!(store.consumed_attempts("p", "fix the parser").unwrap(), 0);
        assert!(!store.reset_attempts("p", "fix the parser").unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The cache answers only for the exact repository, commit, command and
    /// timeout it measured. A pass under a longer limit says nothing about a
    /// shorter one, and a changed command says nothing at all.
    #[test]
    fn base_verification_is_cached_per_command_and_timeout() {
        let dir = scratch("verify-base");
        let store = Store::open(&dir.join("p.db")).unwrap();
        assert_eq!(
            store.verify_base("p", "abc", "make test", 30).unwrap(),
            None
        );

        store
            .set_verify_base("p", "abc", "make test", 30, Some(false), 12)
            .unwrap();
        assert_eq!(
            store.verify_base("p", "abc", "make test", 30).unwrap(),
            Some((Some(false), 12))
        );
        for (base, command, timeout) in [
            ("def", "make test", 30),
            ("abc", "cargo test", 30),
            ("abc", "make test", 10),
        ] {
            assert_eq!(
                store.verify_base("p", base, command, timeout).unwrap(),
                None,
                "{base} {command} {timeout}"
            );
        }
        // A base that could not be measured is unknown, not failing.
        store
            .set_verify_base("p", "def", "make test", 30, None, 0)
            .unwrap();
        assert_eq!(
            store.verify_base("p", "def", "make test", 30).unwrap(),
            Some((None, 0))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A rework must not overwrite what the previous attempt did.
    #[test]
    fn attempts_are_appended_never_replaced() {
        let dir = scratch("attempts");
        let store = Store::open(&dir.join("p.db")).unwrap();
        let mut run = Run::blank();
        store.insert_run(&mut run).unwrap();
        assert!(run.dispatched_at.is_some(), "stamped on insert");

        let mut a = Attempt {
            id: 0,
            run_id: run.id,
            n: 0,
            agent: "claude".into(),
            prompt: "do it".into(),
            feedback: None,
            started_at: 10,
            seconds: Some(30),
            cost_usd: Some(0.2),
            summary: Some("first".into()),
            verify: Some("make test".into()),
            verify_ok: Some(false),
            error: None,
            outcome: Some("ready".into()),
            model: Some("haiku".into()),
        };
        store.insert_attempt(&a).unwrap();
        a.feedback = Some("try again".into());
        a.summary = Some("second".into());
        a.verify_ok = Some(true);
        store.insert_attempt(&a).unwrap();

        let rows = store.attempts(Some(run.id)).unwrap();
        assert_eq!(rows.iter().map(|r| r.n).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(rows[0].summary.as_deref(), Some("first"));
        assert_eq!(rows[0].verify_ok, Some(false));
        assert_eq!(rows[1].feedback.as_deref(), Some("try again"));
        assert!(store.attempts(None).unwrap().len() == 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Each transition stamps its own time, and neither a merge nor a closure
    /// moves the time the run was published.
    #[test]
    fn transitions_are_stamped_once() {
        let mut run = Run::blank();
        run.enter(RunState::Ready);
        let ready = run.ready_at.expect("ready stamped");
        run.enter(RunState::Approved);
        let decided = run.decided_at.expect("approval stamped");
        run.enter(RunState::PrOpen);
        let published = run.published_at.expect("publication stamped");

        run.enter(RunState::Shipped);
        assert_eq!(
            run.published_at,
            Some(published),
            "a merge is not a publish"
        );
        run.enter(RunState::Rejected);
        assert_eq!(run.decided_at, Some(decided), "the approval stands");
        assert_eq!(run.ready_at, Some(ready));
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
        let mut store = Store::open(&dir.join("p.db")).unwrap();
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
            seconds: None,
            cost_usd: None,
            summary: None,
            commits: None,
            diffstat: None,
            verify: Some("make test".into()),
            verify_ok: None,
            error: None,
            outcome: None,
            dispatched_at: None,
            ready_at: None,
            decided_at: None,
            published_at: None,
            review_seconds: None,
            class: Some(Class::Mechanical),
            scope: vec!["Cargo.lock".into(), "Cargo.toml".into()],
            tier: Some(2),
            description: Some("within the constraints\n".into()),
            agent_budget: Some(1.0),
            timeout_minutes: Some(30),
            verify_base_ok: Some(true),
            verify_base_seconds: Some(12),
            changed_paths: None,
            scope_error: None,
            model: Some("sonnet".into()),
            complexity: Some(2),
            features: Some(Features {
                text_words: 3,
                ..Features::default()
            }),
            estimator: Some(crate::complexity::ESTIMATOR.into()),
            route_revision: Some(2),
            route: Some("chores".into()),
            approval: Some(Approval::Batch),
            approved_tree: None,
            approved_head: None,
            approved_by: None,
            workflow_instance: None,
            node: None,
            unit: None,
            lap: 0,
            preset: None,
            extra_args: Vec::new(),
        };
        store.insert_run(&mut run).unwrap();
        assert_eq!(store.run(run.id).unwrap(), run);

        run.state = RunState::Ready;
        run.cost_usd = Some(0.25);
        run.changed_paths = Some(vec!["Cargo.lock".into(), "a b/c\nd".into()]);
        run.verify_ok = Some(false);
        run.summary = Some("done".into());
        store.update_run(&run).unwrap();
        assert_eq!(store.run(run.id).unwrap(), run);

        // The command the base was checked with cannot change under the run.
        run.verify = Some("make something-else".into());
        store.update_run(&run).unwrap();
        assert_eq!(
            store.run(run.id).unwrap().verify.as_deref(),
            Some("make test")
        );
        run.verify = Some("make test".into());

        run.state = RunState::PrOpen;
        store.update_run(&run).unwrap();
        assert_eq!(store.run(run.id).unwrap().state, RunState::PrOpen);
        assert!(!run.state.is_final(), "an open pull request holds its task");

        run.state = RunState::Running;
        store.update_run(&run).unwrap();
        let session = Session::acquire(&dir).unwrap();
        assert_eq!(store.fail_interrupted_runs(&session).unwrap(), 1);
        let failed = store.run(run.id).unwrap();
        assert_eq!(failed.state, RunState::Failed);
        assert!(failed.error.unwrap().starts_with("interrupted"));
        assert!(store.run(99).is_err());

        // A branch is leftover unless an open run of the same project owns it.
        let mut scanned = facts("p", vec![]);
        scanned.pma_branches = vec!["pma/fix-it".into(), "pma/stray".into()];
        let mut other = facts("q", vec![]);
        other.pma_branches = vec!["pma/fix-it".into()];
        let leftover = |store: &mut Store, facts: &[Facts]| {
            store.save_scan(facts, false, 1).unwrap();
            let projects = store.projects().unwrap();
            projects.iter().map(|p| p.leftover).collect::<Vec<_>>()
        };
        assert_eq!(
            leftover(&mut store, &[scanned.clone(), other.clone()]),
            [1, 1]
        );
        run.state = RunState::Shipped;
        store.update_run(&run).unwrap();
        assert_eq!(leftover(&mut store, &[scanned, other]), [2, 1]);
        let _ = std::fs::remove_dir_all(dir);
    }

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

    /// Unlike `deps`, the slug is read on every scan, so the last scan wins
    /// and a project whose GitHub origin is gone loses it.
    #[test]
    fn the_last_scan_owns_the_slug() {
        let dir = scratch("slug");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        store
            .save_scan(&[facts("one", vec![])], true, 1000)
            .unwrap();
        assert_eq!(
            store.project("one").unwrap().unwrap().slug.as_deref(),
            Some("shakfu/one")
        );

        let mut moved = facts("one", vec![]);
        moved.slug = None;
        store.save_scan(&[moved], true, 2000).unwrap();
        assert_eq!(store.project("one").unwrap().unwrap().slug, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Forgetting clears the record but not the runs, whose worktrees may
    /// still exist.
    #[test]
    fn forgetting_a_project_keeps_its_runs() {
        let dir = scratch("forget");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        store
            .save_scan(
                &[facts("a", vec![item("t", 1)]), facts("b", vec![])],
                true,
                1,
            )
            .unwrap();
        store.set_tier("a", Path::new("/r/a"), Some(1)).unwrap();
        store.add_project_tag("a", "ai").unwrap();
        store.add_project_tag("b", "ai").unwrap();
        let mut run = Run::blank();
        run.project = "a".into();
        store.insert_run(&mut run).unwrap();
        store.consume_attempt("a", "t").unwrap();

        let f = store.project_footprint("a").unwrap();
        assert_eq!((f.tasks, f.tags, f.runs), (1, 1, 1));
        assert_eq!(f.open_runs, [run.id], "an open run holds a worktree");

        let mut open = store.run(run.id).unwrap();
        open.state = RunState::Shipped;
        store.update_run(&open).unwrap();
        assert!(store.project_footprint("a").unwrap().open_runs.is_empty());

        store.forget_project("a").unwrap();
        assert!(store.project("a").unwrap().is_none());
        assert!(store.tasks().unwrap().is_empty(), "its tasks go");
        assert_eq!(
            store.projects_tagged(&["ai".into()]).unwrap(),
            ["b"],
            "another project keeps the tag"
        );
        assert_eq!(store.consumed_attempts("a", "t").unwrap(), 0);
        assert_eq!(store.run(run.id).unwrap().project, "a", "the run is kept");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_project_carries_several_tags() {
        let dir = scratch("tags");
        let store = Store::open(&dir.join("p.db")).unwrap();
        assert!(store.add_project_tag("cyllama", "ai").unwrap());
        assert!(
            !store.add_project_tag("cyllama", "ai").unwrap(),
            "idempotent"
        );
        store.add_project_tag("cyllama", "audio").unwrap();
        store.add_project_tag("cysox", "audio").unwrap();

        assert_eq!(
            store.project_tags().unwrap(),
            [
                ("cyllama".to_string(), "ai".to_string()),
                ("cyllama".to_string(), "audio".to_string()),
                ("cysox".to_string(), "audio".to_string()),
            ]
        );
        assert_eq!(
            store.projects_tagged(&["audio".into()]).unwrap(),
            ["cyllama", "cysox"]
        );
        assert_eq!(
            store
                .projects_tagged(&["ai".into(), "audio".into()])
                .unwrap(),
            ["cyllama", "cysox"],
            "a project in both tags is listed once"
        );

        assert!(store.remove_project_tag("cyllama", "audio").unwrap());
        assert!(!store.remove_project_tag("cyllama", "audio").unwrap());
        assert_eq!(store.projects_tagged(&["audio".into()]).unwrap(), ["cysox"]);
        assert_eq!(
            store.projects_tagged(&["ai".into()]).unwrap(),
            ["cyllama"],
            "removing one tag leaves the other"
        );
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
        assert_eq!(p.slug.as_deref(), Some("shakfu/one"));

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
    fn full_scan_marks_projects_no_longer_found() {
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

        assert!(
            store
                .save_scan(&[facts("b", vec![])], false, 2)
                .unwrap()
                .is_empty(),
            "a partial scan marks nothing"
        );
        assert_eq!(
            store.projects().unwrap().len(),
            2,
            "a partial scan removes nothing"
        );

        let absent = store.save_scan(&[facts("b", vec![])], true, 3).unwrap();
        assert_eq!(absent, ["a"]);
        let a = store.project("a").unwrap().unwrap();
        assert_eq!(a.absent_since, Some(3));
        assert_eq!(a.tier, Some(1), "an absent project keeps its tier");
        assert_eq!(
            store.tasks().unwrap().len(),
            1,
            "its tasks and their first_seen are kept"
        );

        // Reported once, not on every later scan.
        assert!(
            store
                .save_scan(&[facts("b", vec![])], true, 4)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.project("a").unwrap().unwrap().absent_since, Some(3));

        // Finding it again makes it present, without a second first_seen.
        store
            .save_scan(&[facts("a", vec![item("t", 1)])], false, 5)
            .unwrap();
        assert_eq!(store.project("a").unwrap().unwrap().absent_since, None);
        assert_eq!(store.tasks().unwrap()[0].first_seen, 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
