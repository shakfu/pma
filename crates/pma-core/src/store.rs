//! The portfolio database: roots, projects with tiers, settings, the results
//! of the last scan, agent runs, and portfolio notes.
//!
//! One user and one session at a time is assumed. The rollback journal
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

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

const VERSION: i64 = 44;

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
/// still exist and must be published or rejected.
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

/// Version 37. Each failing workflow's latest failed run, so a view can link
/// to it. A table rather than a column on `projects`: `ci_detail` keeps the
/// names alone, which every reader of a failing state splits by line, and
/// tests replay migrations, which `ADD COLUMN` cannot survive.
const CI_RUNS: &str = "
CREATE TABLE IF NOT EXISTS ci_runs (
    project TEXT NOT NULL REFERENCES projects(name) ON DELETE CASCADE,
    workflow TEXT NOT NULL,
    url TEXT NOT NULL,
    PRIMARY KEY (project, workflow)
);
";

/// Version 40. Each fault a scan found, by kind, in place of `scan_error`:
/// one free-text column could not say whether git, the file or a timeout
/// failed. Rows from before are classified by the text the scan wrote; a
/// `mended` change names the fault it mends, taken from the change that
/// found it, and one with none to name is dropped.
const SCAN_FAULTS: &str = "
CREATE TABLE IF NOT EXISTS project_faults (
    project TEXT NOT NULL REFERENCES projects(name) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    detail TEXT NOT NULL,
    PRIMARY KEY (project, kind)
);
CREATE TABLE IF NOT EXISTS scan_faults (
    scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
    project TEXT NOT NULL,
    kind TEXT NOT NULL,
    detail TEXT NOT NULL,
    PRIMARY KEY (scan, project, kind)
);
CREATE TEMP VIEW fault_kind (ord, prefix, kind) AS
 VALUES (1, 'the repository''s hooks or git settings changed%', 'git_settings_changed'),
        (2, 'git status:%', 'git_unreadable'),
        (3, 'TODO.md:%', 'todo_unreadable'),
        (4, '%lint error%', 'todo_lint'),
        (5, '%git rev-list:%', 'unpublished_unknown'),
        (6, '%git for-each-ref:%', 'unpublished_unknown');
INSERT OR IGNORE INTO project_faults (project, kind, detail)
 SELECT name, coalesce((SELECT kind FROM fault_kind WHERE scan_error LIKE prefix ORDER BY ord LIMIT 1),
                       'scan_partial'), scan_error
 FROM projects WHERE scan_error IS NOT NULL;
INSERT OR IGNORE INTO project_faults (project, kind, detail)
 SELECT name, 'todo_lint', lint_errors || iif(lint_errors = 1, ' lint error', ' lint errors')
 FROM projects WHERE lint_errors > 0;
INSERT OR IGNORE INTO scan_faults (scan, project, kind, detail)
 SELECT scan, project, coalesce((SELECT kind FROM fault_kind WHERE scan_error LIKE prefix ORDER BY ord LIMIT 1),
                                'scan_partial'), scan_error
 FROM scan_projects WHERE scan_error IS NOT NULL;
INSERT OR IGNORE INTO scan_faults (scan, project, kind, detail)
 SELECT scan, project, 'todo_lint', lint_errors || iif(lint_errors = 1, ' lint error', ' lint errors')
 FROM scan_projects WHERE lint_errors > 0;
UPDATE scan_changes
 SET kind = coalesce((SELECT kind FROM fault_kind WHERE detail LIKE prefix ORDER BY ord LIMIT 1), 'scan_partial')
 WHERE kind = 'broken';
UPDATE scan_changes SET detail = coalesce(
    (SELECT b.kind FROM scan_changes b
      WHERE b.project = scan_changes.project AND b.scan < scan_changes.scan
        AND b.kind IN (SELECT kind FROM fault_kind UNION SELECT 'scan_partial')
      ORDER BY b.scan DESC LIMIT 1), '')
 WHERE kind = 'mended' AND detail = '';
DELETE FROM scan_changes WHERE kind = 'mended' AND detail = '';
DROP VIEW fault_kind;
ALTER TABLE projects DROP COLUMN scan_error;
ALTER TABLE scan_projects DROP COLUMN scan_error;
";

/// Version 41. Where the agent edited: `clone`, the user's repository from
/// its working tree, or `worktree`. `start` is the commit of that working
/// tree at dispatch, which a clone run's diff, scope and revert read
/// against; `base` stays `HEAD`, which publishing requires unmoved. Rows from
/// before are worktree runs.
const WORKSPACE: &str = "
ALTER TABLE runs ADD COLUMN workspace TEXT NOT NULL DEFAULT 'worktree';
ALTER TABLE runs ADD COLUMN start TEXT;
";

/// Version 39. Seeds `refresh`, the worker that runs `pma deps refresh`.
/// Seeded through `settings.toml`, which every later edit applies whole.
/// Version 42. The project a queued command is about, when it names one,
/// so a project's view lists its jobs beside its runs.
const JOB_PROJECT: &str = "
ALTER TABLE jobs ADD COLUMN project TEXT;
";

const REFRESH_WORKER: &str = "-- no schema change; the seed is in Rust";

/// Version 43. The items of a bundle, one run over several TODO.md items,
/// numbered from 1 as the prompt numbers them. A run with one item has none.
const RUN_ITEMS: &str = "
CREATE TABLE run_items (
    run INTEGER NOT NULL REFERENCES runs(id),
    n INTEGER NOT NULL,
    key TEXT NOT NULL,
    text TEXT NOT NULL,
    gh INTEGER,
    dropped INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (run, n)
);
";

/// Version 38. When the stalest lock last changed, read on every scan, and
/// the crates cargo's constraints hold back, read with `--deps`. A table for
/// the reason `ci_runs` is one.
const PROJECT_DEPS: &str = "
CREATE TABLE IF NOT EXISTS project_deps (
    project TEXT PRIMARY KEY REFERENCES projects(name) ON DELETE CASCADE,
    lock_at INTEGER,
    held_back INTEGER
);
";

/// Version 36. Each scan, each project's state at it, and what changed since
/// the scan before. The project and task rows hold only the last scan, so
/// "what changed" and "failing since when" had nothing to read.
const SCAN_HISTORY: &str = "
CREATE TABLE IF NOT EXISTS scans (
    id INTEGER PRIMARY KEY,
    at INTEGER NOT NULL,
    full INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS scan_projects (
    scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
    project TEXT NOT NULL,
    ci TEXT NOT NULL,
    ci_detail TEXT NOT NULL,
    lint_errors INTEGER NOT NULL,
    scan_error TEXT,
    open INTEGER NOT NULL,
    PRIMARY KEY (scan, project)
);
CREATE INDEX IF NOT EXISTS idx_scan_projects_project ON scan_projects(project, scan);
CREATE TABLE IF NOT EXISTS scan_changes (
    scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
    project TEXT NOT NULL,
    kind TEXT NOT NULL,
    priority TEXT,
    detail TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_scan_changes_scan ON scan_changes(scan);
";

/// Version 35. The `settings.toml` text the configuration tables were last
/// filled from, so an unchanged file is not applied again. `IF NOT EXISTS`
/// because tests replay migrations over a store that has it.
const SETTINGS_SYNC: &str = "
CREATE TABLE IF NOT EXISTS settings_sync (text TEXT NOT NULL);
";

/// Version 34. A workflow node that is not an `edit` changes no files, so a
/// run of one that finished is `done`, not `ready` for a review it cannot have.
const READ_RUNS_DONE: &str = "
UPDATE runs SET state = 'done'
WHERE state = 'ready' AND branch = '' AND workflow_instance IS NOT NULL;
";

/// Version 44. Every worker that runs `claude` moves from `acceptEdits` to
/// `auto`. Only that pair is rewritten, so an otherwise edited row keeps its
/// edits and one on another mode stays there.
const CLAUDE_AUTO_MODE: &str = r#"
UPDATE agents
SET args = replace(args, '"--permission-mode","acceptEdits"', '"--permission-mode","auto"')
WHERE command = 'claude';
"#;

/// Version 33. The command a job that stopped at a workflow plan would run
/// to approve it, as a JSON array. And the `claude` worker streams its
/// events, unless its row was edited: `pma watch` reads them.
const JOB_APPROVAL_AND_STREAM: &str = r#"
ALTER TABLE jobs ADD COLUMN awaiting TEXT;
UPDATE agents
SET args = '["-p","{prompt}","--output-format","stream-json","--verbose","--permission-mode","acceptEdits","--model","{model}","--max-budget-usd","{budget}","{extra}"]'
WHERE name = 'claude'
  AND args = '["-p","{prompt}","--output-format","json","--permission-mode","acceptEdits","--model","{model}","--max-budget-usd","{budget}","{extra}"]';
"#;

/// Version 32. A `pma` command queued to run detached, with what became of
/// it. `args` is a JSON array, run as `pma <args>` in `cwd`.
const JOBS: &str = "
CREATE TABLE jobs (
    id INTEGER PRIMARY KEY,
    args TEXT NOT NULL,
    cwd TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    finished_at INTEGER,
    pid INTEGER,
    exit_code INTEGER
);
";

/// Version 31. A rejection the reviewer put down to infrastructure: the
/// check or the machine failed, not the agent. Such a run is in no share.
const INFRA_REJECTION: &str = "
ALTER TABLE runs ADD COLUMN infra INTEGER NOT NULL DEFAULT 0;
";

/// Version 30. Whether verify ran confined is part of the base cache's key. A
/// base measured unconfined says nothing about a confined head: a check that
/// fails only under the sandbox would read as the agent's regression. The
/// cached rows are dropped and measured again.
const VERIFY_BASE_SANDBOX: &str = "
DROP TABLE verify_base;
CREATE TABLE verify_base (
    project TEXT NOT NULL,
    base TEXT NOT NULL,
    command TEXT NOT NULL,
    timeout_minutes INTEGER NOT NULL,
    sandbox INTEGER NOT NULL,
    ok INTEGER,
    seconds INTEGER NOT NULL,
    measured_at INTEGER NOT NULL,
    PRIMARY KEY (project, base, command, timeout_minutes, sandbox)
);
";

/// Version 29. The tree a reviewer was last shown, which an approval must
/// match: what the agent left, or what `pma review <id>` printed since.
const REVIEWED_TREE: &str = "
ALTER TABLE runs ADD COLUMN reviewed_tree TEXT;
";

/// Version 28. `shipped` named two different things: a push to the default
/// branch, and a pull request merged. `pma push` and `pma pr` are separate
/// commands now, and a run's state says which happened, in GitHub's words. A
/// pull request closed without merging is `closed`, not `rejected`, which is
/// kept for a run rejected in review and never published. The `publish`
/// settings are dropped: the command says where a run goes.
const PUSH_OR_PR: &str = "
UPDATE runs SET state = 'merged' WHERE state = 'shipped' AND outcome LIKE '%/pull/%';
UPDATE runs SET state = 'pushed' WHERE state = 'shipped';
UPDATE runs SET state = 'closed' WHERE state = 'rejected' AND outcome LIKE '%/pull/%';
DELETE FROM config WHERE key = 'publish' OR (key LIKE 'projects.%' AND key LIKE '%.publish');
";

/// Version 27. Drops the health score's weights. `pma status` sorts by the
/// worst state a project is in, which needs no weight, and a stored row for a
/// key nothing reads is one `pma config` would have to explain forever.
const RETIRE_WEIGHTS: &str = "
DELETE FROM config WHERE key LIKE 'weights.%';
";

/// Version 26. What makes git run a program in a run's repository, or push
/// elsewhere, as it stood at dispatch: hooks and the settings that name a
/// command or rewrite a URL. Publishing refuses when it has changed, since neither
/// is in the diff a reviewer reads.
const GIT_SNAPSHOT: &str = "
ALTER TABLE runs ADD COLUMN git_snapshot TEXT;
";

/// Version 25. Seeds `sanduk`, the worker that runs the same agent in a
/// disposable container. No schema: a worker is a record, and this one exists
/// so the flags that have to be right are right without being typed. An
/// existing row of that name is left alone, as the other templates are.
const SANDUK_WORKER: &str = "-- no schema change; the seed is in Rust";

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

/// A project's state as the history compares it.
struct ScanState {
    ci: Ci,
    faults: Vec<(Fault, String)>,
}

/// Every scanned project's state, by name. A row a tier made before any
/// scan read the project has no state to compare against.
fn scan_state(conn: &Connection) -> Result<HashMap<String, ScanState>> {
    let mut stmt =
        conn.prepare("SELECT name, ci, ci_detail FROM projects WHERE scanned_at IS NOT NULL")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            ScanState {
                ci: Ci::from_columns(&r.get::<_, String>(1)?, r.get(2)?),
                faults: Vec::new(),
            },
        ))
    })?;
    let mut states: HashMap<String, ScanState> = rows.collect::<rusqlite::Result<_>>()?;
    for (project, faults) in project_faults(conn)? {
        if let Some(s) = states.get_mut(&project) {
            s.faults = faults;
        }
    }
    Ok(states)
}

/// Each project's faults, worst first.
fn project_faults(conn: &Connection) -> Result<HashMap<String, Vec<(Fault, String)>>> {
    let mut stmt = conn.prepare("SELECT project, kind, detail FROM project_faults")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out: HashMap<String, Vec<(Fault, String)>> = HashMap::new();
    for row in rows {
        let (project, kind, detail) = row?;
        if let Some(f) = Fault::from_column(&kind) {
            out.entry(project).or_default().push((f, detail));
        }
    }
    for faults in out.values_mut() {
        faults.sort_by_key(|(f, _)| *f);
    }
    Ok(out)
}

/// The last CI state an earlier scan knew for a project.
fn last_known_ci(conn: &Connection, project: &str, before: i64) -> Result<Option<Ci>> {
    Ok(conn
        .query_row(
            "SELECT ci, ci_detail FROM scan_projects
             WHERE project = ?1 AND scan < ?2 AND ci != 'unknown'
             ORDER BY scan DESC LIMIT 1",
            params![project, before],
            |r| Ok(Ci::from_columns(&r.get::<_, String>(0)?, r.get(1)?)),
        )
        .optional()?)
}

pub struct Store {
    conn: Connection,
    /// `settings.toml`, beside the database. The configuration tables mirror
    /// it; every change to them goes through it first.
    settings: PathBuf,
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
    /// Each failing workflow's latest failed run, as `(workflow, url)`.
    pub ci_runs: Vec<(String, String)>,
    /// Each fault the last scan found, worst first, with its cause.
    pub faults: Vec<(Fault, String)>,
    /// Outdated dependencies at the last measurement; `None` if never measured.
    pub deps: Option<i64>,
    /// One `ecosystem: name current -> latest` per line.
    pub deps_detail: String,
    pub deps_at: Option<i64>,
    /// Crates cargo's constraints held back at the last measurement.
    pub held_back: Option<i64>,
    /// When the stalest lock last changed, at the last scan.
    pub lock_at: Option<i64>,
}

/// What a project's record consists of, for `pma project forget`.
#[derive(Debug, Clone, PartialEq)]
pub struct Footprint {
    pub tasks: i64,
    pub tags: i64,
    pub campaigns: i64,
    /// Runs are kept, so this is reported rather than deleted.
    pub runs: i64,
    /// Runs that are not final; each may own a worktree.
    pub open_runs: Vec<i64>,
    /// Its `projects.<name>.*` settings, such as `verify`.
    pub settings: Vec<String>,
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
                "another pma session{holder} is dispatching, reworking, rejecting or publishing; wait for it to finish"
            )
            .into()
        })
    }
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
                    SANDUK_WORKER,
                    GIT_SNAPSHOT,
                    RETIRE_WEIGHTS,
                    PUSH_OR_PR,
                    REVIEWED_TREE,
                    VERIFY_BASE_SANDBOX,
                    INFRA_REJECTION,
                    JOBS,
                    JOB_APPROVAL_AND_STREAM,
                    READ_RUNS_DONE,
                    SETTINGS_SYNC,
                    SCAN_HISTORY,
                    CI_RUNS,
                    PROJECT_DEPS,
                    REFRESH_WORKER,
                    SCAN_FAULTS,
                    WORKSPACE,
                    JOB_PROJECT,
                    RUN_ITEMS,
                    CLAUDE_AUTO_MODE,
                ];
                let tx = conn.unchecked_transaction()?;
                for step in &steps[version as usize..] {
                    tx.execute_batch(step)?;
                }
                if version < 11 {
                    seed_agent(&tx, &crate::worker::Worker::claude())?;
                }
                // Not replacing an edited row: a template is a starting
                // point, and the store is the record.
                let mut seeded = Vec::new();
                if version < 21 {
                    seeded.extend([
                        crate::worker::Worker::opencode(),
                        crate::worker::Worker::omp(),
                    ]);
                }
                if version < 25 {
                    seeded.push(crate::worker::Worker::sanduk());
                }
                for w in seeded {
                    if tx.query_row(
                        "SELECT count(*) FROM agents WHERE name = ?1",
                        [&w.name],
                        |r| r.get::<_, i64>(0),
                    )? == 0
                    {
                        seed_agent(&tx, &w)?;
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
        let store = Store {
            conn,
            settings: path.with_file_name(crate::settings::FILE),
        };
        store.load_settings()?;
        // After the file is read, so the seed is written to it and survives
        // the next edit. A worker already of that name is left alone.
        let named = |w: &Worker| w.name == crate::worker::REFRESH;
        if version < 39 && !store.agents()?.iter().any(named) {
            store.change_settings(|doc| {
                crate::settings::set_agent(doc, &crate::worker::Worker::refresh())
            })?;
        }
        Ok(store)
    }

    /// Fills the configuration tables from `settings.toml` when its text
    /// changed since they were last filled. With no file, writes one from the
    /// tables: the first run after they held the configuration. A migration
    /// that changes a configuration row must change the file too, or the next
    /// edit of the file undoes it.
    fn load_settings(&self) -> Result<()> {
        if !self.settings.exists() {
            crate::settings::create(&self.settings, &self.settings_from_tables()?)?;
        }
        let (text, settings) = crate::settings::read(&self.settings)?;
        let synced: Option<String> = self
            .conn
            .query_row("SELECT text FROM settings_sync", [], |r| r.get(0))
            .optional()?;
        if synced.as_deref() != Some(text.as_str()) {
            self.apply_settings(&text, &settings)?;
        }
        Ok(())
    }

    fn settings_from_tables(&self) -> Result<crate::settings::Settings> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, tier FROM projects WHERE tier IS NOT NULL ORDER BY name")?;
        let tiers = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(crate::settings::Settings {
            config: self.config_rows()?,
            roots: self.roots()?,
            tiers,
            tags: self.project_tags()?,
            agents: self.agents()?,
            presets: self.presets()?,
        })
    }

    /// Replaces the configuration tables with what the file says. A tier for a
    /// project no scan has recorded waits for the scan.
    fn apply_settings(&self, text: &str, s: &crate::settings::Settings) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute_batch(
            "DELETE FROM config; DELETE FROM roots; DELETE FROM agents; DELETE FROM presets;
             DELETE FROM project_tags; DELETE FROM settings_sync;
             UPDATE projects SET tier = NULL;",
        )?;
        for (key, value) in &s.config {
            tx.execute(
                "INSERT OR REPLACE INTO config (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
        }
        for root in &s.roots {
            tx.execute(
                "INSERT OR IGNORE INTO roots (path) VALUES (?1)",
                [path_str(root)?],
            )?;
        }
        for (name, tier) in &s.tiers {
            tx.execute(
                "UPDATE projects SET tier = ?2 WHERE name = ?1",
                params![name, tier],
            )?;
        }
        for (project, tag) in &s.tags {
            tx.execute(
                "INSERT OR IGNORE INTO project_tags (project, tag) VALUES (?1, ?2)",
                params![project, tag],
            )?;
        }
        for w in &s.agents {
            seed_agent(&tx, w)?;
        }
        for p in &s.presets {
            tx.execute(
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
        }
        tx.execute("INSERT INTO settings_sync (text) VALUES (?1)", [text])?;
        tx.commit()?;
        Ok(())
    }

    /// Edits `settings.toml` with `f`, then refills the tables from it.
    fn change_settings<T>(&self, f: impl FnOnce(&mut toml_edit::DocumentMut) -> T) -> Result<T> {
        let out = crate::settings::edit(&self.settings, f)?;
        let (text, settings) = crate::settings::read(&self.settings)?;
        self.apply_settings(&text, &settings)?;
        Ok(out)
    }

    /// Re-applies the file's tiers and tags, for projects a scan just added.
    fn reapply_settings(&self) -> Result<()> {
        let (text, settings) = crate::settings::read(&self.settings)?;
        self.apply_settings(&text, &settings)
    }

    pub fn config_rows(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM config ORDER BY key")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.change_settings(|doc| crate::settings::set_config(doc, key, value))
    }

    /// Returns whether a stored value was removed.
    pub fn reset_config(&self, key: &str) -> Result<bool> {
        self.change_settings(|doc| crate::settings::reset_config(doc, key))
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
        let path = path_str(path)?;
        self.change_settings(|doc| crate::settings::add_root(doc, path))
    }

    pub fn remove_root(&self, path: &Path) -> Result<bool> {
        let path = path_str(path)?;
        self.change_settings(|doc| crate::settings::remove_root(doc, path))
    }

    /// Records the project first if no scan has, so the tier has a row.
    pub fn set_tier(&self, name: &str, path: &Path, tier: Option<u8>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO projects (name, path, tier) VALUES (?1, ?2, ?3)
             ON CONFLICT (name) DO UPDATE SET tier = excluded.tier",
            params![name, path_str(path)?, tier],
        )?;
        self.change_settings(|doc| crate::settings::set_tier(doc, name, tier))
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
                    last_activity, ci, ci_detail, deps, deps_detail, deps_at,
                    leftover, slug, absent_since, d.held_back, d.lock_at
             FROM projects LEFT JOIN project_deps d ON d.project = projects.name
             ORDER BY name",
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
                deps: r.get(11)?,
                deps_detail: r.get(12)?,
                deps_at: r.get(13)?,
                leftover: r.get(14)?,
                slug: r.get(15)?,
                absent_since: r.get(16)?,
                held_back: r.get(17)?,
                lock_at: r.get(18)?,
                ci_runs: Vec::new(),
                faults: Vec::new(),
            })
        })?;
        let mut rows: Vec<ProjectRow> = rows.collect::<rusqlite::Result<_>>()?;
        let mut stmt = self
            .conn
            .prepare("SELECT project, workflow, url FROM ci_runs ORDER BY project, workflow")?;
        let runs = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get(2)?))
        })?;
        for run in runs {
            let (project, workflow, url) = run?;
            if let Some(row) = rows.iter_mut().find(|r| r.name == project) {
                row.ci_runs.push((workflow, url));
            }
        }
        let mut faults = project_faults(&self.conn)?;
        for row in &mut rows {
            row.faults = faults.remove(&row.name).unwrap_or_default();
        }
        Ok(rows)
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

    /// Records the scan, with each project's state and what changed since the
    /// scan before (`crate::history`). Each task keeps the `first_seen` it had
    /// in the previous scan. A full scan marks every project it did not find
    /// as absent and returns their names; the rows stay, since a project that
    /// leaves a root is usually a move, not a deletion.
    pub fn save_scan(&mut self, facts: &[Facts], full: bool, now: i64) -> Result<Vec<String>> {
        use crate::history::{self, Change};
        let open_runs: Vec<Run> = self
            .runs()?
            .into_iter()
            .filter(|r| !r.state.is_final())
            .collect();
        let tx = self.conn.transaction()?;
        let before = scan_state(&tx)?;
        let returned: std::collections::HashSet<String> = {
            let mut stmt =
                tx.prepare("SELECT name FROM projects WHERE absent_since IS NOT NULL")?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        tx.execute(
            "INSERT INTO scans (at, full) VALUES (?1, ?2)",
            params![now, full],
        )?;
        let scan = tx.last_insert_rowid();
        let mut changes: Vec<(String, Change)> = Vec::new();
        for f in facts {
            match (before.get(&f.name), returned.contains(&f.name)) {
                // A first scan finds every project; none of them is news.
                (None, _) if !before.is_empty() => changes.push((f.name.clone(), Change::New)),
                (Some(_), true) => changes.push((f.name.clone(), Change::Returned)),
                _ => {}
            }
            // What this scan did not read keeps the last scan's fault: an
            // unread file its lint errors, as its tasks; CI not asked, its
            // `CiUnknown`.
            let keep_lint = f.held || f.todo_unread;
            let keep_ci = !f.ci_read;
            tx.execute(
                "DELETE FROM project_faults WHERE project = ?1
                    AND NOT (kind = ?2 AND ?3) AND NOT (kind = ?4 AND ?5)",
                params![
                    f.name,
                    Fault::TodoLint.column(),
                    keep_lint,
                    Fault::CiUnknown.column(),
                    keep_ci
                ],
            )?;
            let save_faults = |tx: &rusqlite::Transaction| -> rusqlite::Result<()> {
                for (fault, why) in &f.faults {
                    tx.execute(
                        "INSERT OR REPLACE INTO project_faults (project, kind, detail)
                         VALUES (?1, ?2, ?3)",
                        params![f.name, fault.column(), why],
                    )?;
                }
                Ok(())
            };
            if f.held {
                tx.execute(
                    "UPDATE projects SET absent_since = NULL WHERE name = ?1",
                    [&f.name],
                )?;
                save_faults(&tx)?;
                continue;
            }
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
                                       last_activity, ci, ci_detail, leftover, slug)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT (name) DO UPDATE SET
                    path = excluded.path, scanned_at = excluded.scanned_at,
                    has_todo = CASE WHEN ?13 THEN projects.has_todo ELSE excluded.has_todo END,
                    lint_errors = CASE WHEN ?13 THEN projects.lint_errors
                                       ELSE excluded.lint_errors END,
                    dirty = excluded.dirty, ahead = excluded.ahead,
                    last_activity = excluded.last_activity, ci = excluded.ci,
                    ci_detail = excluded.ci_detail,
                    leftover = excluded.leftover, slug = excluded.slug,
                    absent_since = NULL",
                params![
                    f.name,
                    path_str(&f.path)?,
                    now,
                    f.todo.is_some() || f.todo_unread,
                    f.todo.as_ref().map_or(0, |t| t.lint_errors),
                    f.dirty,
                    f.ahead,
                    f.last_activity,
                    ci,
                    ci_detail,
                    leftover,
                    f.slug,
                    f.todo_unread,
                ],
            )?;
            save_faults(&tx)?;
            tx.execute("DELETE FROM ci_runs WHERE project = ?1", [&f.name])?;
            for (workflow, url) in &f.ci_runs {
                tx.execute(
                    "INSERT OR REPLACE INTO ci_runs (project, workflow, url) VALUES (?1, ?2, ?3)",
                    params![f.name, workflow, url],
                )?;
            }

            tx.execute(
                "INSERT INTO project_deps (project, lock_at) VALUES (?1, ?2)
                 ON CONFLICT (project) DO UPDATE SET lock_at = excluded.lock_at",
                params![f.name, f.lock_at],
            )?;
            if let Some(d) = &f.deps {
                tx.execute(
                    "UPDATE projects SET deps = ?2, deps_detail = ?3, deps_at = ?4 WHERE name = ?1",
                    params![f.name, d.outdated, d.detail, now],
                )?;
                tx.execute(
                    "UPDATE project_deps SET held_back = ?2 WHERE project = ?1",
                    params![f.name, d.held_back],
                )?;
            }

            // An unreadable file says nothing about the tasks; the last good
            // scan's rows and their ages stand.
            if f.todo_unread {
                continue;
            }
            // By key, else by text: `sync` adding `gh:N` changes an item's key
            // but not its text, and an `id:` keeps an item's key when its text
            // is reworded. Either way the item keeps its age: when it was first
            // seen, and when its text first appeared in git, which a reworded
            // text no longer finds in the history.
            // An item matched neither way is opened; a row no item matched
            // is closed.
            let rows: Vec<(String, String, String, i64, Option<i64>)> = {
                let mut stmt = tx.prepare(
                    "SELECT key, text, priority, first_seen, added_at FROM tasks WHERE project = ?1",
                )?;
                stmt.query_map([&f.name], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<rusqlite::Result<_>>()?
            };
            let by_key: HashMap<&str, usize> = rows
                .iter()
                .enumerate()
                .map(|(i, r)| (r.0.as_str(), i))
                .collect();
            let by_text: HashMap<String, usize> = rows
                .iter()
                .enumerate()
                .map(|(i, r)| (crate::todo::normal_text(&r.1), i))
                .collect();
            let mut matched = std::collections::HashSet::new();
            let known = before.contains_key(&f.name);
            tx.execute("DELETE FROM tasks WHERE project = ?1", [&f.name])?;
            for item in f.todo.iter().flat_map(|t| &t.items) {
                let found = by_key
                    .get(item.key.as_str())
                    .or_else(|| by_text.get(&crate::todo::normal_text(&item.text)))
                    .copied();
                match found {
                    Some(i) => {
                        matched.insert(i);
                    }
                    None if known => changes.push((
                        f.name.clone(),
                        Change::Opened {
                            priority: item.priority.name().into(),
                            text: item.text.clone(),
                        },
                    )),
                    None => {}
                }
                let before = found.map(|i| (rows[i].3, rows[i].4));
                let first_seen = before.map_or(now, |(seen, _)| seen);
                let added_at = match (item.added_at, before.and_then(|(_, added)| added)) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
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
                        added_at,
                        first_seen,
                        item.group,
                    ],
                )?;
            }
            for (i, (_, text, priority, ..)) in rows.iter().enumerate() {
                if !matched.contains(&i) {
                    changes.push((
                        f.name.clone(),
                        Change::Closed {
                            priority: priority.clone(),
                            text: text.clone(),
                        },
                    ));
                }
            }
        }

        // Each project as the scan left it, against the state before.
        let after = scan_state(&tx)?;
        for f in facts {
            let Some(state) = after.get(&f.name) else {
                continue;
            };
            let (ci, detail) = state.ci.to_columns();
            tx.execute(
                "INSERT INTO scan_projects (scan, project, ci, ci_detail, lint_errors, open)
                 VALUES (?1, ?2, ?3, ?4,
                         (SELECT lint_errors FROM projects WHERE name = ?2),
                         (SELECT count(*) FROM tasks WHERE project = ?2))",
                params![scan, f.name, ci, detail],
            )?;
            for (fault, why) in &state.faults {
                tx.execute(
                    "INSERT INTO scan_faults (scan, project, kind, detail) VALUES (?1, ?2, ?3, ?4)",
                    params![scan, f.name, fault.column(), why],
                )?;
            }
            let Some(was) = before.get(&f.name) else {
                continue;
            };
            let known_ci = match &was.ci {
                Ci::Unknown(_) => last_known_ci(&tx, &f.name, scan)?,
                ci => Some(ci.clone()),
            };
            let faults = history::fault_changes(&was.faults, &state.faults);
            for c in history::ci_change(known_ci.as_ref(), &state.ci)
                .into_iter()
                .chain(faults)
            {
                changes.push((f.name.clone(), c));
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
        for name in &absent {
            changes.push((name.clone(), Change::Absent));
        }
        for (project, change) in &changes {
            let (kind, priority, detail) = change.to_columns();
            tx.execute(
                "INSERT INTO scan_changes (scan, project, kind, priority, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![scan, project, kind, priority, detail],
            )?;
        }
        tx.execute("DELETE FROM scans WHERE id <= ?1", [scan - history::KEPT])?;
        tx.commit()?;
        // A project this scan added may have a tier or tags in the file.
        self.reapply_settings()?;
        Ok(absent)
    }

    /// The latest `limit` scans, newest first, with their totals.
    pub fn scans(&self, limit: usize) -> Result<Vec<crate::history::Scan>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.at, s.full,
                    (SELECT count(*) FROM scan_projects p WHERE p.scan = s.id),
                    (SELECT count(*) FROM scan_projects p WHERE p.scan = s.id AND p.ci = 'failing'),
                    (SELECT coalesce(sum(p.open), 0) FROM scan_projects p WHERE p.scan = s.id),
                    (SELECT count(*) FROM scan_changes c WHERE c.scan = s.id AND c.kind = 'opened'),
                    (SELECT count(*) FROM scan_changes c WHERE c.scan = s.id AND c.kind = 'closed'),
                    (SELECT count(*) FROM scan_changes c WHERE c.scan = s.id
                        AND c.kind NOT IN ('opened', 'closed'))
             FROM scans s ORDER BY s.id DESC LIMIT ?1",
        )?;
        let mut by_fault = self
            .conn
            .prepare("SELECT kind, count(*) FROM scan_faults WHERE scan = ?1 GROUP BY kind")?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(crate::history::Scan {
                id: r.get(0)?,
                at: r.get(1)?,
                full: r.get(2)?,
                projects: r.get(3)?,
                failing: r.get(4)?,
                faults: [0; 7],
                open: r.get(5)?,
                opened: r.get(6)?,
                closed: r.get(7)?,
                other: r.get(8)?,
            })
        })?;
        let mut scans: Vec<crate::history::Scan> = rows.collect::<rusqlite::Result<_>>()?;
        for s in &mut scans {
            let counts = by_fault.query_map([s.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?;
            for c in counts {
                let (kind, n) = c?;
                if let Some(i) = Fault::ALL.iter().position(|f| f.column() == kind) {
                    s.faults[i] = n;
                }
            }
        }
        Ok(scans)
    }

    /// What a scan recorded as changed, by project, in the order recorded.
    pub fn scan_changes(&self, scan: i64) -> Result<Vec<crate::history::ProjectChange>> {
        let mut stmt = self.conn.prepare(
            "SELECT project, kind, priority, detail FROM scan_changes
             WHERE scan = ?1 ORDER BY project, rowid",
        )?;
        let rows = stmt.query_map([scan], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (project, kind, priority, detail) = row?;
            if let Some(change) = crate::history::Change::from_columns(&kind, priority, detail) {
                out.push(crate::history::ProjectChange { project, change });
            }
        }
        Ok(out)
    }

    /// For each project whose CI fails at its latest recorded scan, when that
    /// run of failing scans began.
    pub fn failing_since(&self) -> Result<HashMap<String, crate::history::Since>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.project, s.at, p.ci, p.ci_detail FROM scan_projects p
             JOIN scans s ON s.id = p.scan ORDER BY p.project, p.scan DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                Ci::from_columns(&r.get::<_, String>(2)?, r.get(3)?),
            ))
        })?;
        let mut by_project: HashMap<String, Vec<(i64, Ci)>> = HashMap::new();
        for row in rows {
            let (project, at, ci) = row?;
            by_project.entry(project).or_default().push((at, ci));
        }
        Ok(by_project
            .into_iter()
            .filter(|(_, states)| matches!(states.first(), Some((_, Ci::Failing(_)))))
            .filter_map(|(p, states)| crate::history::failing_since(&states).map(|s| (p, s)))
            .collect())
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
            settings: self
                .config_rows()?
                .into_iter()
                .filter_map(|(k, _)| {
                    k.strip_prefix(&format!("projects.{name}."))
                        .map(String::from)
                })
                .collect(),
        })
    }

    /// Deletes a project's record, its settings, and everything keyed to it
    /// but its runs, whose worktrees may still exist and which the design
    /// keeps beyond a project's removal. Caller checks that nothing holds it
    /// back.
    pub fn forget_project(&mut self, name: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        for sql in [
            "DELETE FROM project_tags WHERE project = ?1",
            "DELETE FROM campaign_members WHERE project = ?1",
            "DELETE FROM exhaustion WHERE project = ?1",
            "DELETE FROM verify_base WHERE project = ?1",
            "DELETE FROM scan_projects WHERE project = ?1",
            "DELETE FROM scan_changes WHERE project = ?1",
            // `tasks` follows by ON DELETE CASCADE.
            "DELETE FROM projects WHERE name = ?1",
        ] {
            tx.execute(sql, [name])?;
        }
        tx.commit()?;
        self.change_settings(|doc| crate::settings::forget_project(doc, name))
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
        self.change_settings(|doc| crate::settings::tag_project(doc, project, tag, true))
    }

    /// Returns false when the project did not have the tag.
    pub fn remove_project_tag(&self, project: &str, tag: &str) -> Result<bool> {
        self.change_settings(|doc| crate::settings::tag_project(doc, project, tag, false))
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

    /// Refuses a command already queued or running from the same directory,
    /// in the same statement, so two clicks cannot both queue it.
    pub fn add_job(&self, args: &[String], cwd: &Path, project: Option<&str>) -> Result<i64> {
        let json = serde_json::to_string(args)?;
        let cwd = cwd.to_string_lossy();
        let added = self.conn.execute(
            "INSERT INTO jobs (args, cwd, state, created_at, project)
             SELECT ?1, ?2, 'queued', ?3, ?4
             WHERE NOT EXISTS (SELECT 1 FROM jobs WHERE args = ?1 AND cwd = ?2
                               AND state IN ('queued', 'running'))",
            params![json, cwd, crate::dates::now(), project],
        )?;
        if added == 0 {
            let (id, state): (i64, String) = self.conn.query_row(
                "SELECT id, state FROM jobs WHERE args = ?1 AND cwd = ?2
                 AND state IN ('queued', 'running') ORDER BY id LIMIT 1",
                params![json, cwd],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            return Err(format!("job {id} is {state} already: pma {}", args.join(" ")).into());
        }
        Ok(self.conn.last_insert_rowid())
    }

    /// The newest `limit` jobs, newest first.
    pub fn jobs(&self, limit: i64) -> Result<Vec<Job>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {JOB_COLUMNS} FROM jobs ORDER BY id DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map([limit], job_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn job(&self, id: i64) -> Result<Job> {
        self.conn
            .query_row(
                &format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = ?1"),
                [id],
                job_row,
            )
            .optional()?
            .ok_or_else(|| format!("no job {id}; `pma job` lists them").into())
    }

    /// Marks the oldest queued job running and returns it.
    pub fn claim_job(&self) -> Result<Option<Job>> {
        let Some(id) = self
            .conn
            .query_row(
                "SELECT id FROM jobs WHERE state = 'queued' ORDER BY id LIMIT 1",
                [],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        self.conn.execute(
            "UPDATE jobs SET state = 'running', started_at = ?2 WHERE id = ?1 AND state = 'queued'",
            params![id, crate::dates::now()],
        )?;
        self.job(id).map(Some)
    }

    /// Records what would approve the plan a job stopped at.
    pub fn set_job_awaiting(&self, id: i64, args: &[String]) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET awaiting = ?2 WHERE id = ?1",
            params![id, serde_json::to_string(args)?],
        )?;
        Ok(())
    }

    /// Marks an awaiting job approved. False if it was not awaiting.
    pub fn approve_job(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE jobs SET state = 'approved' WHERE id = ?1 AND state = 'awaiting-approval'",
            [id],
        )? > 0)
    }

    /// Marks every job awaiting `plan` approved, once that plan has run by
    /// any route. Plan ids are hex, so the quoted match is exact.
    pub fn settle_jobs_awaiting(&self, plan: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET state = 'approved'
             WHERE state = 'awaiting-approval' AND instr(awaiting, '\"' || ?1 || '\"') > 0",
            [plan],
        )?;
        Ok(())
    }

    pub fn set_job_pid(&self, id: i64, pid: u32) -> Result<()> {
        self.conn
            .execute("UPDATE jobs SET pid = ?2 WHERE id = ?1", params![id, pid])?;
        Ok(())
    }

    /// Records how a job ended. A job stopped while it ran stays `stopped`.
    pub fn finish_job(&self, id: i64, exit_code: Option<i32>) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET finished_at = ?2, exit_code = ?3,
                 state = CASE WHEN state = 'stopped' THEN 'stopped'
                              WHEN ?3 = 0 AND awaiting IS NOT NULL THEN 'awaiting-approval'
                              WHEN ?3 = 0 THEN 'done' ELSE 'failed' END
             WHERE id = ?1",
            params![id, crate::dates::now(), exit_code],
        )?;
        Ok(())
    }

    /// Returns the state the job was in. Only a queued or running job stops.
    pub fn stop_job(&self, id: i64) -> Result<String> {
        let job = self.job(id)?;
        if matches!(job.state.as_str(), "queued" | "running") {
            self.conn.execute(
                "UPDATE jobs SET state = 'stopped', finished_at = ?2 WHERE id = ?1",
                params![id, crate::dates::now()],
            )?;
        }
        Ok(job.state)
    }

    /// Jobs left `running` by a runner that exited. Called under the runner
    /// lock, so no live runner holds them.
    pub fn fail_running_jobs(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE jobs SET state = 'failed', finished_at = ?1 WHERE state = 'running'",
            [crate::dates::now()],
        )?)
    }

    pub fn has_queued_jobs(&self) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM jobs WHERE state = 'queued'",
            [],
            |r| r.get::<_, i64>(0),
        )? > 0)
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
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO runs (project, task_key, text, gh, quadrant, agent, branch, worktree,
                               default_branch, base, prompt, state, repo, dispatched_at,
                               class, scope, tier, description, agent_budget, timeout_minutes,
                               verify, verify_base_ok, verify_base_seconds, model,
                               complexity, features, estimator,
                               route_revision, route, approval,
                               workflow_instance, node, unit, lap, preset, extra_args,
                               git_snapshot, workspace, start)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                     ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                     ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39)",
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
                run.git_snapshot,
                run.workspace.name(),
                run.start,
            ],
        )?;
        run.id = tx.last_insert_rowid();
        for (n, item) in run.items.iter().enumerate() {
            tx.execute(
                "INSERT INTO run_items (run, n, key, text, gh, dropped)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    run.id,
                    n as i64 + 1,
                    item.key,
                    item.text,
                    item.gh,
                    item.dropped
                ],
            )?;
        }
        tx.commit()?;
        Ok(run.id)
    }

    /// Marks bundle items 1-based `dropped` as left open, and every other
    /// item as done.
    pub fn set_dropped(&self, id: i64, dropped: &[usize]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("UPDATE run_items SET dropped = 0 WHERE run = ?1", [id])?;
        for n in dropped {
            tx.execute(
                "UPDATE run_items SET dropped = 1 WHERE run = ?1 AND n = ?2",
                params![id, *n as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Writes every mutable column of the run with `run.id`.
    pub fn update_run(&self, run: &Run) -> Result<()> {
        self.write_run(run, None)
    }

    /// `update_run`, only while the row is still in `read`, the state the copy
    /// was read in. For a command that holds no session lock, so its stale
    /// copy cannot undo a publish that ran meanwhile.
    pub fn update_run_from(&self, run: &Run, read: RunState) -> Result<()> {
        self.write_run(run, Some(read))
    }

    fn write_run(&self, run: &Run, read: Option<RunState>) -> Result<()> {
        let written = self.conn.execute(
            // `verify` is frozen at dispatch: a command re-detected at rework
            // could differ from the one the base was checked with.
            "UPDATE runs SET state = ?2, feedback = ?3, seconds = ?4, cost_usd = ?5,
                summary = ?6, commits = ?7, diffstat = ?8, verify_ok = ?9,
                error = ?10, outcome = ?11, prompt = ?12, ready_at = ?13,
                decided_at = ?14, published_at = ?15, review_seconds = ?16,
                changed_paths = ?17, scope_error = ?18, approved_tree = ?19,
                approved_head = ?20, approved_by = ?21, reviewed_tree = ?22, infra = ?23,
                base = ?25
             WHERE id = ?1 AND (?24 IS NULL OR state = ?24)",
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
                run.reviewed_tree,
                run.infra,
                read.map(RunState::name),
                run.base,
            ],
        )?;
        if written == 0 {
            let now = self.run(run.id)?.state;
            return Err(format!(
                "run #{} is now {}; read it again with `pma review {}`",
                run.id,
                now.name(),
                run.id
            )
            .into());
        }
        Ok(())
    }

    /// Adds review time in one column, so a command that holds no session
    /// lock writes nothing else.
    pub fn add_review_seconds(&self, id: i64, seconds: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET review_seconds = coalesce(review_seconds, 0) + ?2 WHERE id = ?1",
            params![id, seconds],
        )?;
        Ok(())
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

    pub fn set_reviewed_tree(&self, id: i64, tree: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET reviewed_tree = ?2 WHERE id = ?1",
            params![id, tree],
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
                    workflow_instance, node, unit, lap, preset, extra_args, git_snapshot,
                    reviewed_tree, infra, workspace, start
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
                git_snapshot: r.get(55)?,
                reviewed_tree: r.get(56)?,
                infra: r.get(57)?,
                workspace: Workspace::parse(&r.get::<_, String>(58)?).unwrap_or_default(),
                start: r.get(59)?,
                items: Vec::new(),
            })
        })?;
        let mut runs: Vec<Run> = rows.collect::<rusqlite::Result<_>>()?;
        let mut stmt = self
            .conn
            .prepare("SELECT run, key, text, gh, dropped FROM run_items ORDER BY run, n")?;
        let mut items: HashMap<i64, Vec<RunItem>> = HashMap::new();
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                RunItem {
                    key: r.get(1)?,
                    text: r.get(2)?,
                    gh: r.get(3)?,
                    dropped: r.get(4)?,
                },
            ))
        })? {
            let (run, item) = row?;
            items.entry(run).or_default().push(item);
        }
        for run in &mut runs {
            run.items = items.remove(&run.id).unwrap_or_default();
        }
        Ok(runs)
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

    /// One revision by number. What an instance resumes under, which is the
    /// revision it recorded rather than whichever is in effect now.
    pub fn workflow_revision(&self, revision: i64) -> Result<Option<WorkflowRevision>> {
        Ok(self
            .workflow_revisions()?
            .into_iter()
            .find(|r| r.revision == revision))
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

    /// One instance by id.
    pub fn workflow_instance(&self, instance: i64) -> Result<Option<WorkflowInstance>> {
        Ok(self
            .workflow_instances()?
            .into_iter()
            .find(|i| i.id == instance))
    }

    /// Records why an instance cannot go on, without closing it. A pass that
    /// hit a cap has work recorded and a reason it stopped, and neither is a
    /// finished workflow.
    pub fn note_instance_outcome(&self, instance: i64, outcome: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_instances SET outcome = ?2 WHERE id = ?1",
            params![instance, outcome],
        )?;
        Ok(())
    }

    /// Records new arguments on an instance and clears the outcome that
    /// stopped it, so a raised cap resumes it.
    pub fn reopen_instance(&self, instance: i64, args: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_instances SET args = ?2, outcome = NULL WHERE id = ?1",
            params![instance, args],
        )?;
        Ok(())
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
        self.change_settings(|doc| crate::settings::set_preset(doc, p))
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
        self.change_settings(|doc| crate::settings::remove_preset(doc, name))
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
        self.change_settings(|doc| crate::settings::set_agent(doc, w))
    }

    /// Returns false when no agent has the name.
    pub fn remove_agent(&self, name: &str) -> Result<bool> {
        self.change_settings(|doc| crate::settings::remove_agent(doc, name))
    }

    /// Decided runs in a project and how many were accepted. An open pull
    /// request is in neither: no one has judged it.
    pub fn decided_runs(&self, project: &str) -> Result<(i64, i64)> {
        let mut decided = 0;
        let mut accepted = 0;
        for run in self.runs()?.iter().filter(|r| r.project == project) {
            match run.state {
                RunState::Approved | RunState::Committed | RunState::Pushed | RunState::Merged => {
                    decided += 1;
                    accepted += 1;
                }
                RunState::Rejected if run.infra => {}
                RunState::Rejected | RunState::Closed => decided += 1,
                _ => {}
            }
        }
        Ok((decided, accepted))
    }

    /// Moves the runs still holding a task onto its new key, once `pma` has
    /// written an id into its line: those keyed by its old key or by its text,
    /// and the matching items of open bundles.
    /// A finished run keeps the key it was dispatched under.
    pub fn rekey_runs(&self, project: &str, old: &str, text: &str, new: &str) -> Result<()> {
        let text = crate::todo::normal_text(text);
        for run in self.runs()? {
            if run.project == project
                && !run.state.is_final()
                && (run.task_key == old || crate::todo::normal_text(&run.text) == text)
            {
                self.conn.execute(
                    "UPDATE runs SET task_key = ?2 WHERE id = ?1",
                    params![run.id, new],
                )?;
            }
            // A bundle's own key stays `bundle`; its items move.
            if run.project == project && !run.state.is_final() {
                for (n, item) in run.items.iter().enumerate() {
                    if item.key == old || crate::todo::normal_text(&item.text) == text {
                        self.conn.execute(
                            "UPDATE run_items SET key = ?3 WHERE run = ?1 AND n = ?2",
                            params![run.id, n as i64 + 1, new],
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    /// What a workflow instance's runs have cost, a run that reported no
    /// cost counted at `unknown`.
    pub fn instance_spend(&self, instance: i64, unknown: f64) -> Result<f64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(COALESCE(cost_usd, ?2)), 0) FROM runs
             WHERE workflow_instance = ?1",
            params![instance, unknown],
            |r| r.get(0),
        )?)
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
            // No row is no attempt; any other error is an error, since the
            // limit it guards would otherwise be lifted.
            .optional()?
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

    /// Takes back `n` consumed attempts, never below none.
    pub fn refund_attempts(&self, project: &str, revision: &str, n: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE exhaustion SET attempts = max(attempts - ?3, 0), updated_at = ?4
             WHERE project = ?1 AND revision = ?2",
            params![project, revision, n, crate::dates::now()],
        )?;
        Ok(())
    }

    /// Returns false when the revision had consumed none.
    pub fn reset_attempts(&self, project: &str, revision: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM exhaustion WHERE project = ?1 AND revision = ?2",
            params![project, revision],
        )? > 0)
    }

    /// A base verification of this exact repository, commit, command,
    /// timeout and sandbox, or `None` when it has not been measured.
    pub fn verify_base(
        &self,
        project: &str,
        base: &str,
        command: &str,
        timeout_minutes: i64,
        sandbox: bool,
    ) -> Result<Option<(Option<bool>, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT ok, seconds FROM verify_base
             WHERE project = ?1 AND base = ?2 AND command = ?3 AND timeout_minutes = ?4
               AND sandbox = ?5",
        )?;
        let mut rows = stmt.query_map(
            params![project, base, command, timeout_minutes, sandbox],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(rows.next().transpose()?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_verify_base(
        &self,
        project: &str,
        base: &str,
        command: &str,
        timeout_minutes: i64,
        sandbox: bool,
        ok: Option<bool>,
        seconds: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO verify_base
                (project, base, command, timeout_minutes, sandbox, ok, seconds, measured_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                project,
                base,
                command,
                timeout_minutes,
                sandbox,
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

/// A `pma` command queued to run detached.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: i64,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// `queued`, `running`, `done`, `failed` or `stopped`.
    pub state: String,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    /// The process group leader of a running job.
    pub pid: Option<i64>,
    pub exit_code: Option<i64>,
    /// For a job that stopped at a workflow plan: the arguments that approve
    /// it, which `pma job approve` queues.
    pub awaiting: Option<Vec<String>>,
    /// The project the command is about, when it names one.
    pub project: Option<String>,
}

const JOB_COLUMNS: &str = "id, args, cwd, state, created_at, started_at, finished_at, pid, \
                           exit_code, awaiting, project";

fn job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let args: String = r.get(1)?;
    Ok(Job {
        id: r.get(0)?,
        args: serde_json::from_str(&args).unwrap_or_default(),
        cwd: PathBuf::from(r.get::<_, String>(2)?),
        state: r.get(3)?,
        created_at: r.get(4)?,
        started_at: r.get(5)?,
        finished_at: r.get(6)?,
        pid: r.get(7)?,
        exit_code: r.get(8)?,
        awaiting: r
            .get::<_, Option<String>>(9)?
            .and_then(|a| serde_json::from_str(&a).ok()),
        project: r.get(10)?,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub id: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunState {
    #[default]
    Queued,
    Running,
    Ready,
    Failed,
    Approved,
    /// `pma pr` opened a pull request that is neither merged nor closed.
    PrOpen,
    /// Rejected in `pma review`: never published.
    Rejected,
    /// `pma stage` wrote it into its clone's index. It holds its task until
    /// it is committed and pushed, or rejected.
    Staged,
    /// `pma commit` committed it in its clone.
    Committed,
    /// `pma push` pushed it to the default branch.
    Pushed,
    /// Its pull request was merged.
    Merged,
    /// Its pull request was closed without merging.
    Closed,
    /// Written into its clone as unstaged changes, for the developer to
    /// stage and commit, or discard.
    Applied,
    /// A workflow node that is not an `edit` finished; its output became
    /// units. It changed no files, so there is nothing to review.
    Done,
}

impl RunState {
    const ALL: [(&'static str, RunState); 14] = [
        ("queued", RunState::Queued),
        ("running", RunState::Running),
        ("ready", RunState::Ready),
        ("failed", RunState::Failed),
        ("approved", RunState::Approved),
        ("pr-open", RunState::PrOpen),
        ("rejected", RunState::Rejected),
        ("staged", RunState::Staged),
        ("committed", RunState::Committed),
        ("pushed", RunState::Pushed),
        ("merged", RunState::Merged),
        ("closed", RunState::Closed),
        ("applied", RunState::Applied),
        ("done", RunState::Done),
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
        matches!(
            self,
            RunState::Rejected
                | RunState::Pushed
                | RunState::Merged
                | RunState::Closed
                | RunState::Applied
                | RunState::Done
        )
    }

    /// Whether its change reached the default branch: pushed, or merged.
    pub fn landed(self) -> bool {
        matches!(self, RunState::Pushed | RunState::Merged)
    }
}

/// One task dispatched to an agent, from its worktree to publishing. `Default`
/// is every field empty, which a workflow node fills differently from a
/// dispatch: it has no branch to push and no item to tick.
#[derive(Debug, Clone, PartialEq, Default)]
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
    /// What publishing did: a pushed commit, or a pull request URL.
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
    /// The tree the approver saw, as `git write-tree` names it. Publishing refuses
    /// to publish a worktree that no longer matches.
    pub approved_tree: Option<String>,
    /// The commit the worktree was on. Each publish attempt starts again from
    /// it and `approved_tree`.
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
    /// `dispatch::git_snapshot` at dispatch. `None` for a run from before it
    /// was taken.
    pub git_snapshot: Option<String>,
    /// The tree the reviewer was last shown: what the agent left, or what
    /// `pma review <id>` printed since. An approval must match it. `None` for
    /// a run from before it was recorded.
    pub reviewed_tree: Option<String>,
    /// Rejected for a fault of the check or the machine, not the agent.
    pub infra: bool,
    /// Where the agent edited. A worktree run's `worktree` is its own
    /// directory; a clone run's is `repo`.
    pub workspace: Workspace,
    /// A clone run's working tree at dispatch, as a commit; `HEAD` itself
    /// when nothing differed. What its diff and its revert read against.
    pub start: Option<String>,
    /// A bundle's items, in prompt order; empty for any other run.
    pub items: Vec<RunItem>,
}

/// One TODO.md item of a bundle.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RunItem {
    pub key: String,
    pub text: String,
    pub gh: Option<i64>,
    /// The approver left it open: not ticked, not closed.
    pub dropped: bool,
}

impl Run {
    /// The open pull request a conflict fix under review will update. A
    /// publish to the clone would leave it open and untracked.
    pub fn fixing_pr(&self) -> Option<&str> {
        matches!(
            self.state,
            RunState::Ready | RunState::Failed | RunState::Approved
        )
        .then_some(self.outcome.as_deref())
        .flatten()
        .filter(|o| o.starts_with("http"))
    }

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
            git_snapshot: None,
            reviewed_tree: None,
            infra: false,
            workspace: Workspace::Worktree,
            start: None,
            items: Vec::new(),
        }
    }

    /// Moves the run to `state` and stamps the transition. Call it instead of
    /// assigning `state`, so no transition goes unrecorded.
    pub fn enter(&mut self, state: RunState) {
        let now = crate::dates::now();
        match state {
            RunState::Ready => self.ready_at = Some(now),
            RunState::Approved | RunState::Committed | RunState::Rejected => {
                self.decided_at.get_or_insert(now);
            }
            RunState::Pushed | RunState::PrOpen => {
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

    /// An id written into an item's line moves an open bundle's item onto
    /// it, by key or by text; the bundle's own key stays.
    #[test]
    fn a_rekey_moves_an_open_bundles_items() {
        let dir = scratch("rekey-bundle");
        let store = Store::open(&dir.join("p.db")).unwrap();
        let item = |key: &str, text: &str| RunItem {
            key: key.into(),
            text: text.into(),
            ..RunItem::default()
        };
        let mut run = Run {
            task_key: crate::dispatch::BUNDLE.into(),
            text: "task a; task b".into(),
            items: vec![item("a", "task a"), item("old-b", "Task  B")],
            ..Run::blank()
        };
        store.insert_run(&mut run).unwrap();
        store.rekey_runs("p", "a", "task a", "id:1").unwrap();
        store.rekey_runs("p", "b", "task b", "id:2").unwrap();
        let got = store.run(run.id).unwrap();
        assert_eq!(got.task_key, crate::dispatch::BUNDLE);
        let keys: Vec<&str> = got.items.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, ["id:1", "id:2"]);
        let _ = std::fs::remove_dir_all(dir);
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
        }
    }

    /// A command not yet finished is not queued twice from one directory.
    #[test]
    fn a_job_is_not_queued_twice() {
        let dir = scratch("job-twice");
        let store = Store::open(&dir.join("p.db")).unwrap();
        let args = ["verify".to_string(), "a".into()];
        let first = store.add_job(&args, &dir, Some("a")).unwrap();
        let err = store.add_job(&args, &dir, Some("a")).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("job {first} is queued already: pma verify a")
        );
        store.add_job(&args, &dir.join("x"), Some("a")).unwrap();
        store.finish_job(first, Some(0)).unwrap();
        store.add_job(&args, &dir, Some("a")).unwrap();
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

    /// `shipped` said two things. A run with a pull request became `merged`,
    /// a push became `pushed`, and a pull request closed without merging is
    /// `closed`, where it was `rejected`. The `publish` settings go.
    #[test]
    fn shipped_splits_into_pushed_merged_and_closed() {
        let dir = scratch("push-or-pr");
        let db = dir.join("v27.db");
        let conn = Connection::open(&db).unwrap();
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
            SANDUK_WORKER,
            GIT_SNAPSHOT,
            RETIRE_WEIGHTS,
        ];
        for step in steps {
            conn.execute_batch(step).unwrap();
        }
        conn.pragma_update(None, "user_version", 27).unwrap();
        for (state, outcome) in [
            ("shipped", "https://github.com/o/r/pull/3"),
            ("shipped", "pushed abc to main"),
            ("rejected", "https://github.com/o/r/pull/4"),
            ("rejected", ""),
        ] {
            conn.execute(
                "INSERT INTO runs (project, task_key, text, agent, repo, branch, worktree,
                                   default_branch, base, prompt, state, outcome)
                 VALUES ('p', 'k', 't', 'claude', '/k', 'pma/t', '/w', 'main', 'b', 'p', ?1,
                         NULLIF(?2, ''))",
                params![state, outcome],
            )
            .unwrap();
        }
        for (key, value) in [
            ("publish", "push"),
            ("projects.a.publish", "pr"),
            ("timeout", "5"),
        ] {
            conn.execute(
                "INSERT INTO config (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .unwrap();
        }
        drop(conn);
        let store = Store::open(&db).unwrap();
        let states: Vec<_> = store.runs().unwrap().iter().map(|r| r.state).collect();
        assert_eq!(
            states,
            [
                RunState::Merged,
                RunState::Pushed,
                RunState::Closed,
                RunState::Rejected
            ]
        );
        let keys: Vec<String> = store
            .config_rows()
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys, ["timeout"]);
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
                    [RunState::PrOpen, RunState::Pushed],
                    "v{version}: a published pull request is open again, and a push is `pushed`"
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
        // Five templates, alphabetically: the one that was compiled in, the
        // two installed separately, the lock refresh, and the same agent in a
        // container.
        assert_eq!(
            store.agents().unwrap(),
            [
                Worker::claude(),
                Worker::omp(),
                Worker::opencode(),
                Worker::refresh(),
                Worker::sanduk()
            ]
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
                Worker::opencode(),
                Worker::refresh(),
                Worker::sanduk()
            ]
        );
        assert_eq!(store.agent("codex").unwrap(), w);
        assert!(store.agent("cursor").is_err());

        assert!(store.remove_agent("codex").unwrap());
        assert!(!store.remove_agent("codex").unwrap());
        assert_eq!(
            store.agents().unwrap(),
            [
                Worker::claude(),
                Worker::omp(),
                Worker::opencode(),
                Worker::refresh(),
                Worker::sanduk()
            ]
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

        // Seeded through the file, so an edit of it keeps the worker, and
        // removing it is not undone by reopening.
        let text = std::fs::read_to_string(dir.join(crate::settings::FILE)).unwrap();
        assert!(text.contains("[agents.refresh]"), "{text}");
        assert!(store.remove_agent("refresh").unwrap());
        drop(store);
        let store = Store::open(&dir.join("p.db")).unwrap();
        assert!(store.agent("refresh").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Schema 33 moves a `claude` row still on the shipped template to
    /// `stream-json`, which `pma watch` reads, and leaves an edited row alone.
    #[test]
    fn the_claude_template_moves_to_stream_json_unless_edited() {
        let old = r#"["-p","{prompt}","--output-format","json","--permission-mode","acceptEdits","--model","{model}","--max-budget-usd","{budget}","{extra}"]"#;
        for (args, streams) in [(old, true), (r#"["-p","{prompt}"]"#, false)] {
            let dir = scratch("stream-json");
            let path = dir.join("p.db");
            {
                let store = Store::open(&path).unwrap();
                store
                    .conn
                    .execute_batch("ALTER TABLE jobs DROP COLUMN awaiting;")
                    .unwrap();
                store
                    .conn
                    .execute("UPDATE agents SET args = ?1 WHERE name = 'claude'", [args])
                    .unwrap();
                store.conn.execute_batch(BEFORE_SCAN_FAULTS).unwrap();
                store.conn.pragma_update(None, "user_version", 32).unwrap();
            }
            let store = Store::open(&path).unwrap();
            let got = store.agent("claude").unwrap().args;
            assert_eq!(got.contains(&"stream-json".to_string()), streams, "{got:?}");
            if streams {
                assert_eq!(got, Worker::claude().args);
            }
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Schema 44 moves each `claude` row off `acceptEdits`, whatever else was
    /// edited in it, and leaves another mode or another command alone.
    #[test]
    fn claude_rows_move_to_auto_mode_and_keep_their_edits() {
        let accept = r#"["-p","{prompt}","--permission-mode","acceptEdits","--budget"]"#;
        let plan = r#"["-p","{prompt}","--permission-mode","plan"]"#;
        for (command, args, want) in [
            (
                "claude",
                accept,
                r#"["-p","{prompt}","--permission-mode","auto","--budget"]"#,
            ),
            ("claude", plan, plan),
            ("other", accept, accept),
        ] {
            let dir = scratch("auto-mode");
            let path = dir.join("p.db");
            {
                let store = Store::open(&path).unwrap();
                store
                    .conn
                    .execute(
                        "UPDATE agents SET command = ?1, args = ?2 WHERE name = 'claude'",
                        [command, args],
                    )
                    .unwrap();
                store.conn.pragma_update(None, "user_version", 43).unwrap();
            }
            let store = Store::open(&path).unwrap();
            let got: String = store
                .conn
                .query_row("SELECT args FROM agents WHERE name = 'claude'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(got, want);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Undoes versions 40 to 43, for a test that rewinds the schema past
    /// them.
    const BEFORE_SCAN_FAULTS: &str = "
        DROP TABLE run_items;
        ALTER TABLE jobs DROP COLUMN project;
        ALTER TABLE runs DROP COLUMN workspace;
        ALTER TABLE runs DROP COLUMN start;
        ALTER TABLE projects ADD COLUMN scan_error TEXT;
        ALTER TABLE scan_projects ADD COLUMN scan_error TEXT;
    ";

    /// Version 40 sorts each old `scan_error` and `broken` change by the
    /// prefix the scan wrote, and names the fault a `mended` change mends.
    #[test]
    fn scan_errors_become_faults_after_the_migration() {
        let dir = scratch("faults");
        let path = dir.join("p.db");
        {
            let store = Store::open(&path).unwrap();
            store.conn.execute_batch(BEFORE_SCAN_FAULTS).unwrap();
            store
                .conn
                .execute_batch(
                    "DROP TABLE project_faults; DROP TABLE scan_faults;
                     INSERT INTO projects (name, path, lint_errors, scan_error) VALUES
                        ('held', '/h', 0, 'the repository''s hooks or git settings changed since run #3'),
                        ('git', '/g', 2, 'git status: timed out'),
                        ('todo', '/t', 0, 'TODO.md: permission denied'),
                        ('slow', '/s', 0, 'git log: timed out'),
                        ('ahead', '/a', 0, 'git remote: timed out; git rev-list: timed out');
                     INSERT INTO scans (id, at, full) VALUES (1, 10, 1), (2, 20, 1);
                     INSERT INTO scan_projects (scan, project, ci, ci_detail, lint_errors, scan_error, open)
                        VALUES (1, 'git', 'unknown', '', 2, NULL, 0);
                     INSERT INTO scan_changes (scan, project, kind, priority, detail) VALUES
                        (1, 'git', 'broken', NULL, '2 lint errors'),
                        (2, 'git', 'mended', NULL, ''),
                        (2, 'todo', 'mended', NULL, '');",
                )
                .unwrap();
            store.conn.pragma_update(None, "user_version", 39).unwrap();
        }
        let store = Store::open(&path).unwrap();
        let faults: Vec<_> = store
            .projects()
            .unwrap()
            .into_iter()
            .map(|p| {
                (
                    p.name,
                    p.faults.into_iter().map(|(f, _)| f).collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            faults,
            [
                ("ahead".into(), vec![Fault::UnpublishedUnknown]),
                ("git".into(), vec![Fault::GitUnreadable, Fault::TodoLint]),
                ("held".into(), vec![Fault::GitSettingsChanged]),
                ("slow".into(), vec![Fault::ScanPartial]),
                ("todo".into(), vec![Fault::TodoUnreadable]),
            ]
        );
        assert_eq!(store.scans(10).unwrap()[1].faults, [0, 0, 0, 0, 0, 1, 0]);
        use crate::history::Change;
        let changes = |scan| -> Vec<Change> {
            store
                .scan_changes(scan)
                .unwrap()
                .into_iter()
                .map(|c| c.change)
                .collect()
        };
        assert_eq!(
            changes(1),
            [Change::Fault(Fault::TodoLint, "2 lint errors".into())]
        );
        assert_eq!(
            changes(2),
            [Change::Mended(Fault::TodoLint)],
            "a mended change with no fault to name is dropped"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Only a workflow run with no branch, which a non-`edit` node made, is
    /// moved from `ready` to `done`; a dispatched run still waits for review.
    #[test]
    fn a_ready_read_run_is_done_after_the_migration() {
        let dir = scratch("read-runs");
        let path = dir.join("p.db");
        {
            let store = Store::open(&path).unwrap();
            for (branch, instance) in [("", Some(1)), ("pma/t", Some(1)), ("pma/u", None)] {
                store
                    .conn
                    .execute(
                        "INSERT INTO runs (project, task_key, text, agent, repo, branch, worktree,
                                           default_branch, base, prompt, state, workflow_instance)
                         VALUES ('p', 'k', 't', 'claude', '/k', ?1, '/w', 'main', 'b', 'p',
                                 'ready', ?2)",
                        params![branch, instance],
                    )
                    .unwrap();
            }
            store.conn.execute_batch(BEFORE_SCAN_FAULTS).unwrap();
            store.conn.pragma_update(None, "user_version", 33).unwrap();
        }
        let store = Store::open(&path).unwrap();
        let states: Vec<_> = store.runs().unwrap().iter().map(|r| r.state).collect();
        assert_eq!(states, [RunState::Done, RunState::Ready, RunState::Ready]);
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
            // First: it refills the tables from settings.toml, which knows no
            // `model` column.
            store.set_config("agent", "claude").unwrap();
            store.conn.execute_batch(BEFORE_SCAN_FAULTS).unwrap();
            store
                .conn
                .execute_batch(
                    "ALTER TABLE agents ADD COLUMN model TEXT;
                     ALTER TABLE runs DROP COLUMN git_snapshot;
                     ALTER TABLE runs DROP COLUMN reviewed_tree;
                     ALTER TABLE runs DROP COLUMN infra;
                     DROP TABLE jobs;",
                )
                .unwrap();
            store
                .conn
                .execute("UPDATE agents SET model = 'opus' WHERE name = 'claude'", [])
                .unwrap();
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

        // A refund never takes the counter below none.
        store.refund_attempts("p", "fix the parser", 1).unwrap();
        assert_eq!(store.consumed_attempts("p", "fix the parser").unwrap(), 1);
        store.refund_attempts("p", "fix the parser", 3).unwrap();
        assert_eq!(store.consumed_attempts("p", "fix the parser").unwrap(), 0);
        store.consume_attempt("p", "fix the parser").unwrap();

        assert!(store.reset_attempts("p", "fix the parser").unwrap());
        assert_eq!(store.consumed_attempts("p", "fix the parser").unwrap(), 0);
        assert!(!store.reset_attempts("p", "fix the parser").unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The cache answers only for the exact repository, commit, command,
    /// timeout and sandbox it measured. A pass under a longer limit says
    /// nothing about a shorter one, a confined result nothing about an
    /// unconfined one, and a changed command nothing at all.
    #[test]
    fn base_verification_is_cached_per_command_and_timeout() {
        let dir = scratch("verify-base");
        let store = Store::open(&dir.join("p.db")).unwrap();
        assert_eq!(
            store
                .verify_base("p", "abc", "make test", 30, true)
                .unwrap(),
            None
        );

        store
            .set_verify_base("p", "abc", "make test", 30, true, Some(false), 12)
            .unwrap();
        assert_eq!(
            store
                .verify_base("p", "abc", "make test", 30, true)
                .unwrap(),
            Some((Some(false), 12))
        );
        for (base, command, timeout, sandbox) in [
            ("def", "make test", 30, true),
            ("abc", "cargo test", 30, true),
            ("abc", "make test", 10, true),
            ("abc", "make test", 30, false),
        ] {
            assert_eq!(
                store
                    .verify_base("p", base, command, timeout, sandbox)
                    .unwrap(),
                None,
                "{base} {command} {timeout} {sandbox}"
            );
        }
        // A base that could not be measured is unknown, not failing.
        store
            .set_verify_base("p", "def", "make test", 30, true, None, 0)
            .unwrap();
        assert_eq!(
            store
                .verify_base("p", "def", "make test", 30, true)
                .unwrap(),
            Some((None, 0))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A copy read before a publish must not write its older state over it.
    #[test]
    fn a_guarded_write_refuses_a_run_that_moved_on() {
        let dir = scratch("guarded");
        let store = Store::open(&dir.join("p.db")).unwrap();
        let mut run = Run::blank();
        run.state = RunState::Ready;
        store.insert_run(&mut run).unwrap();
        let mut stale = store.run(run.id).unwrap();

        let mut published = store.run(run.id).unwrap();
        published.state = RunState::Pushed;
        store.update_run(&published).unwrap();

        stale.state = RunState::Approved;
        let err = store
            .update_run_from(&stale, RunState::Ready)
            .unwrap_err()
            .to_string();
        assert!(err.contains("is now pushed"), "{err}");
        assert_eq!(store.run(run.id).unwrap().state, RunState::Pushed);
        store.update_run_from(&stale, RunState::Pushed).unwrap();

        store.add_review_seconds(run.id, 60).unwrap();
        store.add_review_seconds(run.id, 60).unwrap();
        assert_eq!(store.run(run.id).unwrap().review_seconds, Some(120));
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

    /// A rejection put down to infrastructure says nothing about the agent, so
    /// the project's record that routing reads leaves it out.
    #[test]
    fn an_infrastructure_rejection_is_not_decided() {
        let dir = scratch("decided");
        let store = Store::open(&dir.join("p.db")).unwrap();
        for (state, infra) in [
            (RunState::Merged, false),
            (RunState::Rejected, false),
            (RunState::Rejected, true),
        ] {
            let mut run = Run::blank();
            run.project = "p".into();
            store.insert_run(&mut run).unwrap();
            run.state = state;
            run.infra = infra;
            store.update_run(&run).unwrap();
        }
        assert_eq!(store.decided_runs("p").unwrap(), (2, 1));
        assert!(store.runs().unwrap()[2].infra, "stored and read back");
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

        run.enter(RunState::Merged);
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
            git_snapshot: Some("hook pre-commit abc executable".into()),
            reviewed_tree: None,
            infra: false,
            workspace: Workspace::Worktree,
            start: None,
            items: vec![
                RunItem {
                    key: "a".into(),
                    text: "first".into(),
                    gh: Some(7),
                    dropped: false,
                },
                RunItem {
                    key: "b".into(),
                    text: "second".into(),
                    gh: None,
                    dropped: true,
                },
            ],
        };
        store.insert_run(&mut run).unwrap();
        assert_eq!(store.run(run.id).unwrap(), run);
        store.set_dropped(run.id, &[1]).unwrap();
        (run.items[0].dropped, run.items[1].dropped) = (true, false);
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
        run.state = RunState::Pushed;
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
        store.set_config("projects.a.verify", "make test").unwrap();
        store.set_config("projects.a.sandbox", "off").unwrap();

        let f = store.project_footprint("a").unwrap();
        assert_eq!((f.tasks, f.tags, f.runs), (1, 1, 1));
        assert_eq!(f.settings, ["sandbox", "verify"]);
        assert_eq!(f.open_runs, [run.id], "an open run holds a worktree");

        let mut open = store.run(run.id).unwrap();
        open.state = RunState::Merged;
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
        assert!(
            store.project_footprint("a").unwrap().settings.is_empty(),
            "a later repository of the name inherits no settings"
        );
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

    /// The first open after the tables held the configuration writes them to
    /// `settings.toml`; from then on the file is what the tables say.
    #[test]
    fn settings_toml_takes_over_the_configuration_tables() {
        let dir = scratch("settings");
        let path = dir.join("p.db");
        {
            let store = Store::open(&path).unwrap();
            store
                .conn
                .execute_batch(
                    "INSERT INTO config (key, value) VALUES ('agent_budget', '3');
                 INSERT INTO roots (path) VALUES ('/r');
                 INSERT INTO project_tags (project, tag) VALUES ('a', 'py');",
                )
                .unwrap();
            store
                .conn
                .execute_batch("DELETE FROM settings_sync")
                .unwrap();
        }
        std::fs::remove_file(dir.join(crate::settings::FILE)).unwrap();
        let mut store = Store::open(&path).unwrap();
        let text = std::fs::read_to_string(dir.join(crate::settings::FILE)).unwrap();
        assert!(
            text.contains("agent_budget = 3") && text.contains("roots = [\"/r\"]"),
            "{text}"
        );
        assert!(
            text.contains("[agents.claude]"),
            "the seeded workers: {text}"
        );

        // A hand edit, applied on the next open; a tier waits for its scan.
        std::fs::write(
            dir.join(crate::settings::FILE),
            text.replace("agent_budget = 3", "agent_budget = 4") + "\n[projects.b]\ntier = 2\n",
        )
        .unwrap();
        drop(store);
        store = Store::open(&path).unwrap();
        assert!(
            store
                .config_rows()
                .unwrap()
                .contains(&("agent_budget".into(), "4".into()))
        );
        assert!(store.project("b").unwrap().is_none());
        store.save_scan(&[facts("b", vec![])], true, 1).unwrap();
        assert_eq!(store.project("b").unwrap().unwrap().tier, Some(2));

        // An invalid file stops the store from opening, naming the key.
        std::fs::write(
            dir.join(crate::settings::FILE),
            "[config]\nmax_parallel = 0\n",
        )
        .unwrap();
        drop(store);
        let e = Store::open(&path).err().expect("refused").to_string();
        assert!(e.contains("config.max_parallel"), "{e}");
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
            held_back: Some(5),
            detail: "uv: ruff 0.14 -> 0.16".into(),
        });
        measured.lock_at = Some(2500);
        store.save_scan(&[measured], true, 3000).unwrap();
        let mut plain = facts("one", vec![item("y", 7), item("z", 8)]);
        plain.lock_at = Some(3500);
        store.save_scan(&[plain], true, 4000).unwrap();
        let p = store.project("one").unwrap().unwrap();
        assert_eq!(
            (p.deps, p.deps_detail.as_str(), p.deps_at, p.held_back),
            (Some(2), "uv: ruff 0.14 -> 0.16", Some(3000), Some(5)),
            "a scan that does not measure deps keeps the last measurement"
        );
        assert_eq!(p.lock_at, Some(3500), "every scan reads the lock's age");
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

    /// A project left unread keeps everything the last scan recorded, and is
    /// not absent: only its scan error changes.
    #[test]
    fn a_held_project_keeps_its_last_scan() {
        let dir = scratch("held");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        store
            .save_scan(&[facts("one", vec![item("x", 5)])], true, 1000)
            .unwrap();
        let held = Facts::held("one", Path::new("/r/one"), "settings changed".into());
        assert!(store.save_scan(&[held], true, 2000).unwrap().is_empty());
        let tasks: Vec<_> = store.tasks().unwrap().into_iter().map(|t| t.key).collect();
        assert_eq!(tasks, ["x"]);
        let p = store.project("one").unwrap().unwrap();
        assert_eq!((p.dirty, p.absent_since), (3, None));
        assert!(matches!(p.ci, Ci::Failing(_)), "{:?}", p.ci);
        assert_eq!(p.ci_runs[0].1, "https://github.com/shakfu/x/actions/runs/7");
        assert_eq!(
            p.faults,
            [
                (Fault::GitSettingsChanged, "settings changed".to_string()),
                (
                    Fault::TodoLint,
                    "line 1: the file must start with `# TODO`".to_string()
                ),
            ],
            "the lint error the last scan read stands"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file that could not be read is not an empty file: the tasks and their
    /// ages stand until a scan can read it again.
    #[test]
    fn an_unreadable_todo_keeps_the_tasks() {
        let dir = scratch("unread");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        store
            .save_scan(&[facts("one", vec![item("x", 5)])], true, 1000)
            .unwrap();
        let unread = Facts {
            todo: None,
            todo_unread: true,
            faults: vec![(Fault::TodoUnreadable, "TODO.md: permission denied".into())],
            ..facts("one", vec![])
        };
        store.save_scan(&[unread], true, 2000).unwrap();
        let kept: Vec<_> = store
            .tasks()
            .unwrap()
            .into_iter()
            .map(|t| (t.key, t.first_seen))
            .collect();
        assert_eq!(kept, [("x".to_string(), 1000)]);
        let p = store.project("one").unwrap().unwrap();
        assert_eq!((p.has_todo, p.lint_errors), (true, 1));
        assert_eq!(
            p.faults,
            [
                (
                    Fault::TodoUnreadable,
                    "TODO.md: permission denied".to_string()
                ),
                (
                    Fault::TodoLint,
                    "line 1: the file must start with `# TODO`".to_string()
                ),
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A scan that did not ask CI says nothing about it: an offline scan
    /// keeps `CiUnknown`, and the next online scan that reads CI mends it.
    #[test]
    fn ci_unknown_stands_until_ci_is_read() {
        let dir = scratch("ci-unknown");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        let lost = Facts {
            faults: vec![
                (Fault::CiUnknown, "gh run list: timed out after 60s".into()),
                (Fault::TodoLint, "line 1: x".into()),
            ],
            ci: Ci::Unknown("gh run list: timed out after 60s".into()),
            ..facts("one", vec![])
        };
        let kinds = |store: &Store| -> Vec<Fault> {
            let p = store.project("one").unwrap().unwrap();
            p.faults.into_iter().map(|(f, _)| f).collect()
        };
        store.save_scan(&[lost], true, 1000).unwrap();
        assert_eq!(kinds(&store), [Fault::CiUnknown, Fault::TodoLint]);
        let offline = Facts {
            ci: Ci::Unknown("offline".into()),
            ci_read: false,
            ..facts("one", vec![])
        };
        store.save_scan(&[offline], true, 2000).unwrap();
        assert_eq!(kinds(&store), [Fault::CiUnknown, Fault::TodoLint]);
        store
            .save_scan(&[facts("one", vec![])], true, 3000)
            .unwrap();
        assert_eq!(kinds(&store), [Fault::TodoLint]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `sync` writes `gh:N` into an item, which changes its key from its text
    /// to the issue number. Its age is the item's, not the key's.
    #[test]
    fn a_task_keeps_its_first_seen_when_its_key_changes() {
        let dir = scratch("rekey");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        let unlinked = ScannedItem {
            gh: None,
            ..item("fix it", 5)
        };
        store
            .save_scan(&[facts("one", vec![unlinked])], true, 1000)
            .unwrap();
        let linked = ScannedItem {
            key: "gh:9".into(),
            text: "Fix  it".into(),
            gh: Some(9),
            ..item("fix it", 5)
        };
        store
            .save_scan(&[facts("one", vec![linked])], true, 2000)
            .unwrap();
        let tasks = store.tasks().unwrap();
        assert_eq!((tasks[0].key.as_str(), tasks[0].first_seen), ("gh:9", 1000));
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

    /// Each scan records what changed against the state before it: items
    /// opened and closed, CI against the last state a scan knew, a project
    /// broken, mended, new or gone.
    #[test]
    fn a_scan_records_what_changed() {
        use crate::history::Change;
        let dir = scratch("history");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        let mut one = facts("one", vec![item("x", 5), item("y", 6)]);
        store.save_scan(&[one.clone()], true, 1000).unwrap();
        let first = store.scans(10).unwrap();
        assert_eq!(first.len(), 1);
        assert!(
            store.scan_changes(first[0].id).unwrap().is_empty(),
            "a first scan has no news"
        );

        // Offline: CI unknown is not news, and the failing state is kept.
        one.todo.as_mut().unwrap().items = vec![item("y", 6), item("z", 7)];
        one.todo.as_mut().unwrap().lint_errors = 0;
        one.faults.clear();
        one.ci = Ci::Unknown("offline".into());
        store
            .save_scan(&[one.clone(), facts("two", vec![])], true, 2000)
            .unwrap();
        let scan = store.scans(1).unwrap().remove(0);
        let changes: Vec<(String, Change)> = store
            .scan_changes(scan.id)
            .unwrap()
            .into_iter()
            .map(|c| (c.project, c.change))
            .collect();
        let high = |t: &str| (t.to_string(), "high".to_string());
        assert_eq!(
            changes,
            [
                (
                    "one".into(),
                    Change::Opened {
                        priority: high("z").1,
                        text: high("z").0
                    }
                ),
                (
                    "one".into(),
                    Change::Closed {
                        priority: high("x").1,
                        text: high("x").0
                    }
                ),
                ("one".into(), Change::Mended(Fault::TodoLint)),
                ("two".into(), Change::New),
            ]
        );
        assert_eq!(
            (
                scan.projects,
                scan.open,
                scan.opened,
                scan.closed,
                scan.other,
                scan.faults
            ),
            (2, 2, 1, 1, 2, [0, 0, 0, 0, 0, 1, 0])
        );

        // Online again: passing against the failing state before the offline scan.
        one.ci = Ci::Passing;
        store.save_scan(&[one.clone()], true, 3000).unwrap();
        let scan = store.scans(1).unwrap().remove(0);
        let changes: Vec<_> = store
            .scan_changes(scan.id)
            .unwrap()
            .into_iter()
            .map(|c| (c.project, c.change))
            .collect();
        assert_eq!(
            changes,
            [
                ("one".to_string(), Change::CiPassing),
                ("two".to_string(), Change::Absent),
            ]
        );
        assert_eq!(store.failing_since().unwrap().get("one"), None);

        one.ci = Ci::Failing(vec!["test".into()]);
        store.save_scan(&[one.clone()], false, 4000).unwrap();
        store.save_scan(&[one], false, 5000).unwrap();
        assert_eq!(
            store.failing_since().unwrap().get("one"),
            Some(&crate::history::Since {
                at: 4000,
                known: true
            })
        );
        assert_eq!(store.scans(10).unwrap().len(), 5);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn old_scans_are_deleted_with_their_rows() {
        let dir = scratch("history-kept");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        for at in 0..crate::history::KEPT + 3 {
            store.save_scan(&[facts("one", vec![])], true, at).unwrap();
        }
        let scans = store.scans(1000).unwrap();
        assert_eq!(scans.len() as i64, crate::history::KEPT);
        let rows: i64 = store
            .conn
            .query_row("SELECT count(*) FROM scan_projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, crate::history::KEPT);
        let _ = std::fs::remove_dir_all(dir);
    }
}
