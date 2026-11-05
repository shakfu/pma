use super::*;

const VERSION: i64 = 50;

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

/// Version 47. What herdr last said of the agent in a run's pane.
const HERDR_AGENT_STATE: &str = "ALTER TABLE runs ADD COLUMN agent_state TEXT;";

/// Version 50. The minos room a run's last attempt spoke in, and how a worker
/// runs when `minosb` holds its pipes.
const MINOS_ROOMS: &str = "
ALTER TABLE runs ADD COLUMN room TEXT;
ALTER TABLE agents ADD COLUMN relay TEXT;
ALTER TABLE agents ADD COLUMN relay_args TEXT NOT NULL DEFAULT '[]';
";

/// Version 49. How a worker starts a session a person drives, as a JSON array.
const INTERACTIVE_ARGS: &str =
    "ALTER TABLE agents ADD COLUMN interactive_args TEXT NOT NULL DEFAULT '[]';";

/// What each project's files looked like when last read; `scan::stamp`.
const PROJECT_STAMPS: &str = "
CREATE TABLE IF NOT EXISTS project_stamps (
    project TEXT PRIMARY KEY,
    stamp TEXT NOT NULL
);
";

/// Version 46. The herdr pane a run's last attempt ran in, and whether a
/// person drives its agent there.
const HERDR_PANES: &str = "
ALTER TABLE runs ADD COLUMN pane TEXT;
ALTER TABLE runs ADD COLUMN interactive INTEGER NOT NULL DEFAULT 0;
";

/// Version 45. What the agent was refused in its last attempt, as a JSON array;
/// when a reviewer first read the run, until they decide on it; and the time
/// measured that way, apart from `review_seconds`, which a person states.
const DENIED_AND_REVIEW_CLOCK: &str = "
ALTER TABLE runs ADD COLUMN denied TEXT;
ALTER TABLE runs ADD COLUMN review_started INTEGER;
ALTER TABLE runs ADD COLUMN review_measured INTEGER;
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

/// Brings the schema at `path` up to `VERSION`; returns the version it had.
pub(super) fn migrate(conn: &Connection, path: &Path) -> Result<i64> {
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
                DENIED_AND_REVIEW_CLOCK,
                HERDR_PANES,
                HERDR_AGENT_STATE,
                PROJECT_STAMPS,
                INTERACTIVE_ARGS,
                MINOS_ROOMS,
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
    Ok(version)
}

impl Store {
    /// The migrations that change `settings.toml`, run once it is loaded.
    pub(super) fn migrate_settings(&self, version: i64) -> Result<()> {
        // After the file is read, so the seed is written to it and survives
        // the next edit. A worker already of that name is left alone.
        // Earlier migrations changed agent, preset and config rows in the
        // tables alone, which the edits below would undo. They go to the file
        // first.
        if version < 49 {
            let (_, file) = crate::settings::read(&self.settings)?;
            let config: Vec<_> = self
                .config_rows()?
                .into_iter()
                .filter(|row| !file.config.contains(row))
                .collect();
            let agents: Vec<_> = self
                .agents()?
                .into_iter()
                .filter(|w| !file.agents.contains(w))
                .collect();
            let presets: Vec<_> = self
                .presets()?
                .into_iter()
                .filter(|p| !file.presets.contains(p))
                .collect();
            if !(agents.is_empty() && presets.is_empty() && config.is_empty()) {
                self.change_settings(|doc| {
                    for (k, v) in &config {
                        crate::settings::set_config(doc, k, v);
                    }
                    agents
                        .iter()
                        .for_each(|w| crate::settings::set_agent(doc, w));
                    presets
                        .iter()
                        .for_each(|p| crate::settings::set_preset(doc, p));
                })?;
            }
        }
        let named = |w: &Worker| w.name == crate::worker::REFRESH;
        if version < 39 && !self.agents()?.iter().any(named) {
            self.change_settings(|doc| {
                crate::settings::set_agent(doc, &crate::worker::Worker::refresh())
            })?;
        }
        // `--interactive` derived claude's form from its headless one; the
        // file now states it. Only a `claude` worker without one gets it.
        if version < 49 {
            let template = crate::worker::Worker::claude();
            for mut w in self.agents()? {
                if w.command == template.command && w.interactive_args.is_empty() {
                    w.interactive_args = template.interactive_args.clone();
                    self.change_settings(|doc| crate::settings::set_agent(doc, &w))?;
                    eprintln!(
                        "pma: wrote agents.{}.interactive_args to {}",
                        w.name,
                        self.settings.display()
                    );
                }
            }
        }
        // As for `interactive_args`: a `claude` worker without a relay form
        // gets the template's, in the file.
        if version < 50 {
            let template = crate::worker::Worker::claude();
            for mut w in self.agents()? {
                if w.command == template.command && w.relay.is_none() && w.relay_args.is_empty() {
                    w.relay = template.relay.clone();
                    w.relay_args = template.relay_args.clone();
                    self.change_settings(|doc| crate::settings::set_agent(doc, &w))?;
                    eprintln!(
                        "pma: wrote agents.{}.relay to {}",
                        w.name,
                        self.settings.display()
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::*;

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
                store
                    .conn
                    .execute_batch(
                        "ALTER TABLE runs DROP COLUMN room;
                         ALTER TABLE agents DROP COLUMN relay;
                         ALTER TABLE agents DROP COLUMN relay_args;
                         ALTER TABLE runs DROP COLUMN denied;
                         ALTER TABLE runs DROP COLUMN review_started;
                         ALTER TABLE runs DROP COLUMN review_measured;
                         ALTER TABLE runs DROP COLUMN agent_state;
                         ALTER TABLE runs DROP COLUMN pane;
                         ALTER TABLE runs DROP COLUMN interactive;
                         ALTER TABLE agents DROP COLUMN interactive_args;",
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
        ALTER TABLE runs DROP COLUMN room;
        ALTER TABLE agents DROP COLUMN relay;
        ALTER TABLE agents DROP COLUMN relay_args;
        ALTER TABLE agents DROP COLUMN interactive_args;
        ALTER TABLE runs DROP COLUMN agent_state;
        ALTER TABLE runs DROP COLUMN denied;
        ALTER TABLE runs DROP COLUMN review_started;
        ALTER TABLE runs DROP COLUMN review_measured;
        ALTER TABLE runs DROP COLUMN pane;
        ALTER TABLE runs DROP COLUMN interactive;
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

    /// Version 49 writes claude's interactive form into `settings.toml`, where
    /// a `claude` worker has none. One that states its own keeps it, and a
    /// worker of another command gets nothing.
    #[test]
    fn claude_gets_its_interactive_args_in_the_file() {
        let dir = scratch("interactive-args");
        let path = dir.join("p.db");
        let file = dir.join(crate::settings::FILE);
        {
            let store = Store::open(&path).unwrap();
            let mut mine = Worker::claude();
            mine.name = "mine".into();
            mine.interactive_args = vec!["{prompt}".into()];
            store
                .change_settings(|doc| crate::settings::set_agent(doc, &mine))
                .unwrap();
            let text = std::fs::read_to_string(&file).unwrap();
            let start = text.find("[agents.claude]").unwrap();
            let line = start + text[start..].find("interactive_args").unwrap();
            let end = line + text[line..].find('\n').unwrap() + 1;
            std::fs::write(&file, format!("{}{}", &text[..line], &text[end..])).unwrap();
            store
                .conn
                .execute_batch(
                    "ALTER TABLE agents DROP COLUMN interactive_args;
                     ALTER TABLE runs DROP COLUMN room;
                     ALTER TABLE agents DROP COLUMN relay;
                     ALTER TABLE agents DROP COLUMN relay_args;",
                )
                .unwrap();
            store.conn.pragma_update(None, "user_version", 48).unwrap();
        }
        let store = Store::open(&path).unwrap();
        let args = |name: &str| store.agent(name).unwrap().interactive_args;
        assert_eq!(args("claude"), Worker::claude().interactive_args);
        assert_eq!(args("mine"), ["{prompt}"]);
        assert!(args("opencode").is_empty());
        let text = std::fs::read_to_string(&file).unwrap();
        let (_, settings) = crate::settings::read(&file).unwrap();
        let claude = settings.agents.iter().find(|w| w.name == "claude").unwrap();
        assert_eq!(
            claude.interactive_args,
            Worker::claude().interactive_args,
            "{text}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Version 50 gives each `claude` worker its relay form in the file, and a
    /// run its room. A worker of another command gets no relay.
    #[test]
    fn claude_gets_its_relay_and_a_run_its_room() {
        let dir = scratch("relay");
        let path = dir.join("p.db");
        let file = dir.join(crate::settings::FILE);
        {
            let store = Store::open(&path).unwrap();
            let text = std::fs::read_to_string(&file).unwrap();
            let kept: Vec<&str> = text.lines().filter(|l| !l.starts_with("relay")).collect();
            std::fs::write(&file, kept.join("\n") + "\n").unwrap();
            store
                .conn
                .execute_batch(
                    "ALTER TABLE runs DROP COLUMN room;
                     ALTER TABLE agents DROP COLUMN relay;
                     ALTER TABLE agents DROP COLUMN relay_args;",
                )
                .unwrap();
            store.conn.pragma_update(None, "user_version", 49).unwrap();
        }
        let store = Store::open(&path).unwrap();
        let claude = store.agent("claude").unwrap();
        assert_eq!(claude.relay.as_deref(), Some("claude"));
        assert_eq!(claude.relay_args, Worker::claude().relay_args);
        assert_eq!(store.agent("opencode").unwrap().relay, None);
        let (_, settings) = crate::settings::read(&file).unwrap();
        let in_file = settings.agents.iter().find(|w| w.name == "claude").unwrap();
        assert_eq!(in_file.relay_args, Worker::claude().relay_args);

        let mut run = Run::blank();
        store.insert_run(&mut run).unwrap();
        assert_eq!(store.run(run.id).unwrap().room, None);
        store.set_room(run.id, Some("r9")).unwrap();
        assert_eq!(store.run(run.id).unwrap().room.as_deref(), Some("r9"));
        store.set_room(run.id, None).unwrap();
        assert_eq!(store.run(run.id).unwrap().room, None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
