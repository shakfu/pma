//! What a front end may read and do: `Query` returns a `View` of plain data,
//! and `Action` returns an `Outcome`. The CLI, the TUI and the web each render
//! these, so a front end that matches every variant cannot fall behind
//! another. Design: `docs/dev/frontends.md`.
//!
//! An action that runs agents or waits on the network is queued as a job
//! running the same CLI command, so no front end blocks on it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{Config, Workspace};
use crate::keys::Act;
use crate::store::{
    Attempt, Campaign, Job, Note, Preset, RouteRevision, Run, RunState, Session, Store,
    WorkflowInstance, WorkflowRevision,
};
use crate::{dates, dispatch, jobs, next, publish, rank, report_runs, screen, store, todo};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Projects named outright and those carrying any of `tags`; empty is all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    pub projects: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    Next {
        of: Selection,
        all: bool,
    },
    Matrix {
        of: Selection,
        quadrant: Option<rank::Quadrant>,
        all: bool,
    },
    /// `all` ranks untiered projects as tier 5.
    Status {
        of: Selection,
        explain: bool,
        all: bool,
    },
    Stale {
        of: Selection,
        count: Option<usize>,
    },
    Report {
        by: report_runs::By,
    },
    /// Runs that are not final: what waits on a reviewer.
    Runs,
    /// What waits on you, what is under way, and what agents can take.
    Inbox {
        of: Selection,
    },
    /// Jobs, running agents, workflow instances and finished runs, those
    /// in `finished` alone when it is set.
    Activity {
        finished: Option<RunState>,
    },
    /// Workflow revisions and campaigns.
    Library,
    /// One run, with its diff. Showing it records the tree shown, which an
    /// approval then requires.
    Run {
        id: i64,
    },
    /// Jobs and running agents, as `pma watch` reads them.
    Agents,
    Jobs,
    JobLog {
        id: i64,
    },
    Notes,
    Workflows,
    Campaigns,
    Campaign {
        name: String,
    },
    /// One project: its state, its open tasks and its runs.
    Project {
        name: String,
    },
    /// One task in full: a TODO.md item by line, or a signal by key.
    Task {
        project: String,
        line: Option<i64>,
        key: Option<String>,
    },
    /// Settings, roots, agents, presets and route revisions.
    Settings,
    /// One recorded scan and what it changed.
    Scan {
        id: i64,
    },
    /// One route revision's policy document.
    RouteDoc {
        revision: i64,
    },
}

impl Query {
    /// The task a dispatch target names: `name:12` is the item on line 12,
    /// `name:ci` the signal.
    pub fn task(project: &str, target: &str) -> Query {
        let rest = target
            .strip_prefix(project)
            .and_then(|r| r.strip_prefix(':'))
            .unwrap_or(target);
        match rest.parse::<i64>() {
            Ok(line) => Query::Task {
                project: project.to_string(),
                line: Some(line),
                key: None,
            },
            Err(_) => Query::Task {
                project: project.to_string(),
                line: None,
                key: Some(rest.to_string()),
            },
        }
    }
}

pub enum View {
    Next {
        header: String,
        lists: next::Next,
        limit: Option<usize>,
        /// Each project's GitHub page, for issue links.
        repos: BTreeMap<String, String>,
    },
    Matrix {
        header: String,
        placed: Vec<rank::Placed>,
        limit: Option<usize>,
        quadrant: Option<rank::Quadrant>,
        today: i64,
        repos: BTreeMap<String, String>,
    },
    Status {
        header: String,
        portfolio: Box<Portfolio>,
        explain: bool,
        /// The latest scans, newest first.
        scans: Vec<crate::history::Scan>,
        /// What the latest scan changed, in the selected projects.
        changes: Vec<crate::history::ProjectChange>,
        /// When each failing project's CI began failing.
        since: std::collections::HashMap<String, crate::history::Since>,
    },
    Stale {
        header: String,
        placed: Vec<rank::Placed>,
        limit: usize,
    },
    Report {
        runs: Vec<Run>,
        attempts: Vec<Attempt>,
        by: report_runs::By,
    },
    Runs(Vec<Run>),
    Inbox {
        header: String,
        lists: next::Next,
        state: PathBuf,
        jobs: Vec<Job>,
        runs: Vec<Run>,
        now: i64,
        repos: BTreeMap<String, String>,
    },
    Activity {
        state: PathBuf,
        jobs: Vec<Job>,
        instances: Vec<WorkflowInstance>,
        runs: Vec<Run>,
        finished: Option<RunState>,
        now: i64,
    },
    Library {
        revisions: Vec<WorkflowRevision>,
        campaigns: Vec<Campaign>,
    },
    Run {
        run: Box<Run>,
        attempts: Vec<Attempt>,
        /// `None` when the project keeps no `CHANGELOG.md`; empty when the
        /// agent drafted no entry.
        changelog: Option<String>,
        diff: String,
        /// Set when the tree moved while it was read, so no approval may
        /// rest on what was shown.
        moved: bool,
        /// The project's GitHub page, for the issue link.
        repo: Option<String>,
    },
    Agents {
        state: PathBuf,
        jobs: Vec<Job>,
        runs: Vec<Run>,
        now: i64,
    },
    Jobs(Vec<Job>),
    /// `None` until the job writes output.
    JobLog {
        id: i64,
        text: Option<String>,
        /// Stopped at a plan, which an approval runs.
        awaiting: bool,
    },
    Notes(Vec<Note>),
    Workflows {
        revisions: Vec<WorkflowRevision>,
        instances: Vec<WorkflowInstance>,
    },
    Campaigns(Vec<Campaign>),
    Campaign {
        campaign: Campaign,
        runs: Vec<Run>,
    },
    Project {
        name: String,
        path: PathBuf,
        /// When a scan last read it; what the view shows is that old.
        scanned_at: Option<i64>,
        tags: Vec<String>,
        /// The verify command set for it; `None` detects one at dispatch.
        verify: Option<String>,
        /// Its states and status cells, as `pma status` shows them; `None`
        /// for a project no scan has found lately.
        status: Option<crate::report::StatusLine>,
        /// Its open tasks, in matrix order.
        placed: Vec<rank::Placed>,
        today: i64,
        /// Every run it has had, newest first.
        runs: Vec<Run>,
        /// Its jobs, newest first, among the newest 50.
        jobs: Vec<Job>,
        /// The state directory, where agent logs are read from.
        state: PathBuf,
        /// What `pma lint` and `pma prune` would say of its TODO.md; `None`
        /// when it has none.
        todo: Option<Vec<TodoNote>>,
        /// Its GitHub page, for issue links.
        repo: Option<String>,
    },
    Settings {
        /// Each setting: key, value, and `set`, `default` or `unknown`.
        settings: Vec<(String, String, &'static str)>,
        roots: Vec<PathBuf>,
        agents: Vec<crate::worker::Worker>,
        default_agent: String,
        presets: Vec<Preset>,
        default_preset: Option<String>,
        routes: Vec<RouteRevision>,
    },
    Scan {
        scan: crate::history::Scan,
        changes: Vec<crate::history::ProjectChange>,
    },
    RouteDoc {
        revision: RouteRevision,
    },
    Task {
        placed: rank::Placed,
        today: i64,
        /// The TODO.md item, with its indented description; `None` for a
        /// signal, or when the file no longer holds the line.
        item: Option<todo::Item>,
        /// What a signal says: the failing workflows, the outdated packages.
        signal: Vec<String>,
        /// The GitHub repository, for an issue link.
        repo: Option<String>,
        /// Runs on it that are not final.
        held_by: Vec<Run>,
    },
}

/// One line of a project's TODO.md that lint or prune would name.
#[derive(Debug, Clone, PartialEq)]
pub struct TodoNote {
    pub line: i64,
    /// `error`, `warning`, `done`, `done section` or `kept`.
    pub kind: &'static str,
    pub text: String,
}

/// What lint and prune say of a TODO.md: its diagnostics, then what prune
/// would remove or keep. A file with errors gets its diagnostics alone.
pub fn todo_notes(text: &str) -> Vec<TodoNote> {
    let mut parsed = todo::parse(text);
    parsed.diagnostics.sort_by_key(|d| d.line);
    let mut out: Vec<TodoNote> = parsed
        .diagnostics
        .iter()
        .map(|d| TodoNote {
            line: d.line as i64,
            kind: match d.severity {
                todo::Severity::Error => "error",
                todo::Severity::Warning => "warning",
            },
            text: d.message.clone(),
        })
        .collect();
    if parsed.has_errors() {
        return out;
    }
    let pruned = todo::prune(text);
    for done in &pruned.done_sections {
        out.push(TodoNote {
            line: done.first as i64,
            kind: "done section",
            text: format!("`## Done`, lines {}-{}", done.first, done.last),
        });
    }
    for item in &pruned.items {
        out.push(TodoNote {
            line: item.line as i64,
            kind: "done",
            text: item.text.clone(),
        });
    }
    for (item, open) in &pruned.held {
        out.push(TodoNote {
            line: item.line as i64,
            kind: "kept",
            text: format!("{}: line {open} under it is open", item.text),
        });
    }
    out
}

/// The worker a queued action names, as `-a`, `-m` and `-p` do.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkerChoice {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub preset: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Approves one run, read or not; the reviewer has read it. `seen` is
    /// the state a front end showed it in: the run must still be in it.
    ApproveRun {
        id: i64,
        seen: Option<String>,
        /// A bundle's items, numbered from 1, to leave open.
        drop: Vec<usize>,
    },
    /// Approves several runs at once, each of which must be clean.
    ApproveRuns {
        ids: Vec<i64>,
    },
    RejectRun {
        id: i64,
        keep_git_changes: bool,
        infra: bool,
        seen: Option<String>,
    },
    AddReviewTime {
        id: i64,
        minutes: u32,
    },
    /// Reads each open pull request's state from GitHub.
    SettlePrs,
    /// Merges a run's open pull request.
    MergePr {
        id: i64,
    },
    /// Has an agent resolve the conflicts of a run's open pull request.
    FixConflicts {
        id: i64,
    },
    StopJob {
        id: i64,
    },
    ApproveJob {
        id: i64,
    },
    StopWorkflow {
        instance: i64,
    },
    AddNote {
        text: String,
    },
    EditNote {
        id: i64,
        text: String,
    },
    RemoveNote {
        id: i64,
    },

    // Queued as jobs.
    Scan {
        of: Selection,
        offline: bool,
        deps: bool,
    },
    Dispatch {
        targets: Vec<String>,
        tags: Vec<String>,
        auto: bool,
        count: Option<usize>,
        retry: bool,
        worker: WorkerChoice,
        /// A prompt of the developer's own over the one project named, in
        /// place of a task.
        prompt: Option<String>,
    },
    /// Starts or resumes an instance; with `approve`, runs that plan.
    WorkflowRun {
        name: String,
        projects: Vec<String>,
        tag: Option<String>,
        instance: Option<i64>,
        set: Vec<String>,
        approve: Option<String>,
        unattended: bool,
        local: bool,
        cap: Vec<String>,
        worker: WorkerChoice,
    },
    ReworkRun {
        id: i64,
        feedback: String,
    },
    /// Rejects a failed run and dispatches its task again with the same
    /// agent and model.
    RetryRun {
        id: i64,
    },
    /// `pma run <name> <project>`: a named job over one project.
    Run {
        name: String,
        project: String,
        review: bool,
        worktree: bool,
        push: bool,
        worker: WorkerChoice,
    },
    Publish {
        ids: Vec<i64>,
        all_approved: bool,
        /// A ready bundle's items to leave open, once its approver has said;
        /// `None` when no one was asked.
        drop: Option<Vec<usize>>,
        target: publish::Target,
        /// Replaces the task text as the commit's subject.
        subject: Option<String>,
    },
    RunCampaign {
        name: String,
        count: Option<usize>,
    },
    Sync {
        of: Selection,
        apply: bool,
    },
    Verify {
        of: Selection,
    },

    // Configuring, in place. Each is a `pma` command the CLI also has.
    /// `pma config <key> <value>`, including `projects.<name>.*` keys.
    SetConfig {
        key: String,
        value: String,
    },
    ResetConfig {
        key: String,
    },
    AddRoot {
        path: String,
    },
    RemoveRoot {
        path: String,
    },
    /// `pma agent set <name> <field> <value>`.
    SetAgentField {
        name: String,
        field: String,
        value: String,
    },
    RemoveAgent {
        name: String,
    },
    SetPreset {
        name: String,
        agent: String,
        model: Option<String>,
        args: Vec<String>,
    },
    RemovePreset {
        name: String,
    },
    ActivateRoute {
        revision: i64,
    },
    /// Stores the policy document at `path` as a draft revision.
    ProposeRoute {
        path: String,
    },
    AddCampaign {
        name: String,
        text: String,
        projects: Vec<String>,
        class: Option<String>,
    },
    RemoveCampaign {
        name: String,
    },
    SetTier {
        project: String,
        tier: Option<u8>,
    },
    Tag {
        project: String,
        tag: String,
        add: bool,
    },
    /// Removes finished items and `Done` sections from the project's
    /// TODO.md, as `pma prune --apply` does.
    Prune {
        project: String,
    },
    /// Queued: `pma deps refresh <path>` over the project's clone.
    RefreshDeps {
        project: String,
    },
    /// Rewrites the mechanical errors in the project's TODO.md, as
    /// `pma lint --fix --apply` does.
    FixTodo {
        project: String,
    },
}

/// What an action did. `said` goes to standard output, `warnings` to
/// standard error.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub said: Vec<String>,
    pub warnings: Vec<String>,
}

impl Outcome {
    fn said(line: impl Into<String>) -> Outcome {
        Outcome {
            said: vec![line.into()],
            warnings: Vec::new(),
        }
    }
}

/// Scans the status view lists.
pub const SCANS_SHOWN: usize = 10;

/// The states the activity tab filters finished runs to, in the order a
/// key cycles them; `None` is every one. A `done` run changed no files.
pub const FINISHED: [Option<RunState>; 6] = [
    None,
    Some(RunState::Rejected),
    Some(RunState::Pushed),
    Some(RunState::Merged),
    Some(RunState::Closed),
    Some(RunState::Applied),
];

/// Finished runs the activity tab lists, newest first.
pub const FINISHED_SHOWN: usize = 100;

/// What the last scan recorded, for tiered projects.
pub struct Portfolio {
    pub cfg: Config,
    pub today: i64,
    pub projects: Vec<(rank::Project, store::ProjectRow)>,
    pub tasks: Vec<rank::Task>,
    /// Projects with no tier of their own.
    pub untiered: usize,
    /// The tier the untiered projects are ranked at, from `default_tier` or
    /// `--all`; `None` leaves them out of `projects`.
    pub ranked_as: Option<u8>,
    pub scanned_ago: i64,
}

impl Portfolio {
    pub fn header(&self) -> String {
        let ago = crate::report::ago(self.scanned_ago);
        let n = self.projects.len();
        match (self.untiered, self.ranked_as) {
            (0, _) => format!("last scan {ago}; {n} projects"),
            (u, Some(t)) => {
                format!("last scan {ago}; {n} projects, {u} untiered ranked as tier {t}")
            }
            (u, None) => format!(
                "last scan {ago}; {n} tiered projects, {u} untiered \
                 (pma project tier <1-5> <project>..., or pma config default_tier <1-5>)"
            ),
        }
    }
}

pub fn config(store: &Store) -> Result<Config> {
    let rows = store.config_rows()?;
    Ok(Config::with_overrides(
        rows.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    )?)
}

pub fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

/// The projects named outright together with every project carrying one of
/// `tags`. Empty means all of them, so a `--tag` that matches nothing is an
/// error rather than a silent selection of the whole portfolio.
pub fn select(store: &Store, names: &[String], tags: &[String]) -> Result<Vec<String>> {
    if tags.is_empty() {
        return Ok(names.to_vec());
    }
    let mut out = names.to_vec();
    for project in store.projects_tagged(tags)? {
        if !out.contains(&project) {
            out.push(project);
        }
    }
    if out.is_empty() {
        return Err(format!("no project carries `{}`", tags.join("` or `")).into());
    }
    Ok(out)
}

pub fn portfolio(store: &Store, of: &Selection, all: bool) -> Result<Portfolio> {
    let cfg = config(store)?;
    let now = dates::now();
    let today = dates::day(now);
    let last = store
        .last_scan()?
        .ok_or("nothing scanned yet; run `pma scan`")?;

    let names = select(store, &of.projects, &of.tags)?;
    let rows = store.projects()?;
    for n in &names {
        if !rows.iter().any(|r| &r.name == n) {
            return Err(format!("unknown project `{n}`").into());
        }
    }
    // A project no scan can find has nothing to dispatch against, so it does
    // not compete for attention. `pma project` still lists it.
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|r| names.is_empty() || names.contains(&r.name))
        .filter(|r| r.absent_since.is_none())
        .collect();
    let ranked_as = cfg.tier_or_default(None).or(all.then_some(5));
    let untiered = rows.iter().filter(|r| r.tier.is_none()).count();
    let task_rows = store.tasks()?;

    let mut projects = Vec::new();
    let mut tasks = Vec::new();
    for row in rows {
        let Some(tier) = row.tier.or(ranked_as) else {
            continue;
        };
        let own: Vec<&store::TaskRow> =
            task_rows.iter().filter(|t| t.project == row.name).collect();
        let project = rank::Project {
            name: row.name.clone(),
            tier,
            open: own.iter().map(|t| t.priority).collect(),
            idle_days: row.last_activity.map(|t| (today - dates::day(t)).max(0)),
            ci: row.ci.clone(),
            dirty: row.dirty,
            leftover: row.leftover,
            ahead: row.ahead,
            deps: row.deps.map(|moves| {
                let days = |t: i64| (today - dates::day(t)).max(0);
                rank::Deps {
                    moves,
                    measured: row.deps_at.map_or(0, days),
                    lock: row.lock_at.map(days),
                    held_back: row.held_back,
                }
            }),
        };
        tasks.extend(own.iter().map(|t| rank::Task {
            project: row.name.clone(),
            tier,
            priority: t.priority,
            text: t.text.clone(),
            line: Some(t.line),
            key: Some(t.key.clone()),
            group: t.group.clone(),
            due: t.due.as_deref().and_then(dates::parse),
            tagged_urgent: t.tags.iter().any(|g| g == "urgent"),
            signal_urgent: false,
            eligible: t.tags.iter().any(|g| g == "agent") && !t.tags.iter().any(|g| g == "manual"),
            manual: t.tags.iter().any(|g| g == "manual"),
            gh: t.gh,
            age_days:
                (today - dates::day(t.added_at.unwrap_or(t.first_seen).min(t.first_seen))).max(0),
        }));
        let mut signals = rank::signal_tasks(&cfg, &project);
        // Ranked at a default tier, a project's signals are shown, but an
        // agent takes them only when named: no one chose it for `--auto`.
        if row.tier.is_none() {
            signals.iter_mut().for_each(|s| s.eligible = false);
        }
        tasks.extend(signals);
        projects.push((project, row));
    }
    Ok(Portfolio {
        cfg,
        today,
        projects,
        tasks,
        untiered,
        ranked_as,
        scanned_ago: now - last,
    })
}

/// Whether a run that is not final holds this task, by key or by its text.
pub fn has_run(runs: &[Run], project: &str, key: &str, text: &str) -> bool {
    runs.iter().any(|r| {
        !r.state.is_final()
            && r.project == project
            && (r.task_key == key
                || todo::normal_text(&r.text) == todo::normal_text(text)
                || r.items
                    .iter()
                    .any(|i| i.key == key || todo::normal_text(&i.text) == todo::normal_text(text)))
    })
}

pub fn next_lists(store: &Store, p: &Portfolio) -> Result<next::Next> {
    let active = store.runs()?;
    let absent: Vec<String> = store
        .projects()?
        .into_iter()
        .filter(|r| r.absent_since.is_some())
        .map(|r| r.name)
        .collect();
    let placed = rank::place(&p.cfg, p.tasks.clone(), p.today);
    let mut spent = std::collections::HashMap::new();
    for x in placed.iter().filter(|x| x.task.eligible) {
        let revision = dispatch::revision(&x.task.text);
        let n = store.consumed_attempts(&x.task.project, &revision)?;
        spent.insert((x.task.project.clone(), revision), n);
    }
    // What `--auto` checks before it takes a task, in the order it checks.
    let taken = |x: &rank::Placed| {
        let t = &x.task;
        if absent.contains(&t.project) {
            return next::Taken::Absent;
        }
        if let Some(key) = &t.key
            && has_run(&active, &t.project, key, &t.text)
        {
            return next::Taken::Held;
        }
        let used = spent
            .get(&(t.project.clone(), dispatch::revision(&t.text)))
            .copied()
            .unwrap_or(0);
        match used >= dispatch::ATTEMPT_LIMIT {
            true => next::Taken::Exhausted(used),
            false => next::Taken::Free,
        }
    };
    Ok(next::build(&placed, &active, p.today, taken))
}

/// A run's drafted changelog entries: `None` when its project keeps no
/// `CHANGELOG.md` and the agent drafted none, empty when it should have.
pub fn changelog(home: &Path, run: &Run) -> Option<String> {
    match std::fs::read_to_string(dispatch::changelog_draft(home, run.id)) {
        Ok(text) => Some(text),
        Err(_) => dispatch::wants_changelog(run).then(String::new),
    }
}

/// Whether a job or an agent run is under way, so a front end refreshes
/// often rather than now and then.
pub fn busy(store: &Store) -> Result<bool> {
    Ok(store
        .jobs(50)?
        .iter()
        .any(|j| matches!(j.state.as_str(), "queued" | "running"))
        || store.runs()?.iter().any(|r| r.state == RunState::Running))
}

/// A run or job that reached a state worth telling the user about.
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    /// `run` or `job`.
    pub kind: &'static str,
    pub id: i64,
    /// Empty for a job.
    pub project: String,
    pub text: String,
    pub state: String,
}

impl Transition {
    /// One line to tell: `#4 spore: ready for review: fix CI`.
    pub fn line(&self) -> String {
        match self.kind {
            "run" => format!(
                "#{} {}: {}: {}",
                self.id,
                self.project,
                // A staged or committed run waits on one more step.
                match self.state.as_str() {
                    "ready" => "ready for review",
                    "staged" => "staged in its clone, commit next",
                    "committed" => "committed in its clone, push next",
                    s => s,
                },
                self.text
            ),
            _ => format!("job {} {}: {}", self.id, self.state, self.text),
        }
    }

    /// The row it is about, for a front end to open.
    pub fn pick(&self) -> screen::Pick {
        match self.kind {
            "run" => screen::Pick::Run {
                id: self.id,
                clean: false,
                fixing: false,
                state: self.state.clone(),
                project: self.project.clone(),
            },
            _ => screen::Pick::Job {
                id: self.id,
                awaiting: self.state == "awaiting-approval",
            },
        }
    }
}

/// What a front end watches between loads: the store's data version, and
/// the state of every run and job, so a change can be named rather than
/// found by reloading everything on a timer.
#[derive(Debug)]
pub struct Watch {
    version: i64,
    runs: BTreeMap<i64, String>,
    jobs: BTreeMap<i64, String>,
    /// Whether a job or a run is under way, as `busy` says.
    pub busy: bool,
}

impl Watch {
    pub fn new(store: &Store) -> Result<Watch> {
        let mut w = Watch {
            version: store.data_version()?,
            runs: BTreeMap::new(),
            jobs: BTreeMap::new(),
            busy: false,
        };
        w.take(store)?;
        Ok(w)
    }

    /// `None` when nothing was committed since the last look; otherwise the
    /// transitions worth a notice, which may be none.
    pub fn poll(&mut self, store: &Store) -> Result<Option<Vec<Transition>>> {
        let version = store.data_version()?;
        if version == self.version {
            return Ok(None);
        }
        self.version = version;
        Ok(Some(self.take(store)?))
    }

    fn take(&mut self, store: &Store) -> Result<Vec<Transition>> {
        let mut out = Vec::new();
        let runs = store.runs()?;
        let jobs = store.jobs(50)?;
        self.busy = jobs
            .iter()
            .any(|j| matches!(j.state.as_str(), "queued" | "running"))
            || runs.iter().any(|r| r.state == RunState::Running);
        let mut seen = BTreeMap::new();
        for r in &runs {
            let state = r.state.name().to_string();
            let moved = self.runs.get(&r.id) != Some(&state);
            // Reaching review, or leaving it, is the news; starting is not.
            let told = matches!(
                r.state,
                RunState::Ready
                    | RunState::Failed
                    | RunState::Staged
                    | RunState::Committed
                    | RunState::Pushed
                    | RunState::Merged
                    | RunState::Applied
                    | RunState::PrOpen
            );
            if moved && told && !self.runs.is_empty() {
                out.push(Transition {
                    kind: "run",
                    id: r.id,
                    project: r.project.clone(),
                    text: r.text.clone(),
                    state: state.clone(),
                });
            }
            seen.insert(r.id, state);
        }
        self.runs = seen;
        let mut seen = BTreeMap::new();
        for j in &jobs {
            let moved = self.jobs.get(&j.id) != Some(&j.state);
            let told = matches!(
                j.state.as_str(),
                "done" | "failed" | "awaiting-approval" | "stopped"
            );
            if moved && told && !self.jobs.is_empty() {
                out.push(Transition {
                    kind: "job",
                    id: j.id,
                    project: String::new(),
                    text: format!("pma {}", j.args.join(" ")),
                    state: j.state.clone(),
                });
            }
            seen.insert(j.id, j.state.clone());
        }
        self.jobs = seen;
        Ok(out)
    }
}

/// A list a front end offers before an act, and the flags it may add. The
/// choice and the flags on become the one input line `screen::action` reads,
/// so a typed line and a chosen one build the same action.
#[derive(Debug, Clone, PartialEq)]
pub struct Choices {
    pub options: Vec<crate::catalog::Choice>,
    pub flags: Vec<Flag>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Flag {
    /// The key that toggles it in a list.
    pub key: &'static str,
    /// What it adds to the input line.
    pub arg: &'static str,
    pub label: &'static str,
}

impl Choices {
    /// The input line for `name` with the flags whose `on` entry is set.
    pub fn line(&self, name: &str, on: &[bool]) -> String {
        let mut line = name.to_string();
        for (f, on) in self.flags.iter().zip(on) {
            if *on {
                line.push(' ');
                line.push_str(f.arg);
            }
        }
        line
    }
}

/// Whether a prompt for `act` starts from what the store holds, so a front
/// end asks `initial_input` rather than `screen::initial_input`.
pub fn drafts(act: Act) -> bool {
    act == Act::Commit
}

/// Text a prompt for `act` on `pick` starts with: a commit's subject starts
/// as its run's task text.
pub fn initial_input(store: &Store, act: Act, pick: &screen::Pick) -> Result<String> {
    Ok(match (act, pick) {
        (Act::Commit, screen::Pick::Run { id, .. }) => store.run(*id)?.text,
        _ => screen::initial_input(act, pick),
    })
}

/// What a front end lists for an act that asks `Ask::Check`: each row, and
/// whether it starts checked. A bundle's items, for its approval: the ones
/// an earlier approval dropped start unchecked.
pub fn checklist(store: &Store, act: Act, pick: &screen::Pick) -> Result<Vec<(String, bool)>> {
    match (act, pick) {
        (Act::Push, screen::Pick::Run { state, .. }) if state != "ready" => Ok(Vec::new()),
        (Act::ApproveRun | Act::Push, screen::Pick::Run { id, .. }) => Ok(store
            .run(*id)?
            .items
            .into_iter()
            .map(|i| (i.text, !i.dropped))
            .collect()),
        _ => Ok(Vec::new()),
    }
}

/// What a front end lists for an act that asks `Ask::Choose`.
pub fn choices(store: &Store, act: Act) -> Result<Choices> {
    match act {
        Act::Run => Ok(Choices {
            options: crate::catalog::choices(store)?.0,
            flags: vec![
                Flag {
                    key: "p",
                    arg: "--push",
                    label: "push when clean",
                },
                Flag {
                    key: "w",
                    arg: "--worktree",
                    label: "edit in a worktree",
                },
            ],
        }),
        Act::Prune => {
            let choice = |name: &str, about: &str| crate::catalog::Choice {
                name: name.into(),
                reads: "project".into(),
                about: about.into(),
            };
            Ok(Choices {
                options: vec![
                    choice("prune", "Remove finished items and `## Done` sections"),
                    choice(
                        "fix",
                        "Rewrite mechanical errors: the title, section headings, plain bullets, odd checkboxes",
                    ),
                ],
                flags: Vec::new(),
            })
        }
        Act::ScanProject => {
            let choice = |name: &str, about: &str| crate::catalog::Choice {
                name: name.into(),
                reads: "project".into(),
                about: about.into(),
            };
            Ok(Choices {
                options: vec![
                    choice("scan", "Read its TODO.md, git state and CI again"),
                    choice("deps", "Scan, and measure its dependency locks too"),
                    choice("offline", "Scan without GitHub: TODO.md and git only"),
                    choice(
                        "refresh-deps",
                        "Move each lock within its constraints, in the clone (`pma deps refresh`)",
                    ),
                ],
                flags: Vec::new(),
            })
        }
        other => Err(format!("{other:?} offers no list").into()),
    }
}

/// Marks runs whose session died `failed`, when no session holds the lock.
pub fn settle_interrupted(store: &Store) -> Result<()> {
    if let Some(s) = Session::try_acquire(&store::home()?)? {
        store.fail_interrupted_runs(&s)?;
    }
    Ok(())
}

/// Each project's GitHub page, by name, for issue links.
fn repos(store: &Store) -> Result<BTreeMap<String, String>> {
    Ok(store
        .projects()?
        .into_iter()
        .filter_map(|r| {
            r.slug
                .as_deref()
                .and_then(crate::scan::github_url)
                .map(|url| (r.name, url))
        })
        .collect())
}

pub fn query(store: &Store, q: Query) -> Result<View> {
    Ok(match q {
        Query::Next { of, all } => {
            let p = portfolio(store, &of, false)?;
            View::Next {
                header: p.header(),
                lists: next_lists(store, &p)?,
                limit: (!all).then_some(p.cfg.quadrant_limit as usize),
                repos: repos(store)?,
            }
        }
        Query::Matrix { of, quadrant, all } => {
            let p = portfolio(store, &of, false)?;
            View::Matrix {
                header: p.header(),
                limit: (!all).then_some(p.cfg.quadrant_limit as usize),
                today: p.today,
                placed: rank::place(&p.cfg, p.tasks, p.today),
                quadrant,
                repos: repos(store)?,
            }
        }
        Query::Status { of, explain, all } => {
            let p = portfolio(store, &of, all)?;
            let scans = store.scans(SCANS_SHOWN)?;
            let names = select(store, &of.projects, &of.tags)?;
            let changes = match scans.first() {
                Some(latest) => store
                    .scan_changes(latest.id)?
                    .into_iter()
                    .filter(|c| names.is_empty() || names.contains(&c.project))
                    .collect(),
                None => Vec::new(),
            };
            View::Status {
                header: p.header(),
                portfolio: Box::new(p),
                explain,
                scans,
                changes,
                since: store.failing_since()?,
            }
        }
        Query::Stale { of, count } => {
            let p = portfolio(store, &of, false)?;
            View::Stale {
                header: p.header(),
                limit: count.unwrap_or(p.cfg.quadrant_limit as usize),
                placed: rank::place(&p.cfg, p.tasks, p.today),
            }
        }
        Query::Report { by } => View::Report {
            runs: store.runs()?,
            attempts: store.attempts(None)?,
            by,
        },
        Query::Inbox { of } => {
            settle_interrupted(store)?;
            // The first view a new store shows: it says to scan rather
            // than failing, and still lists runs and jobs.
            let (header, lists) = match store.last_scan()? {
                None => (
                    "nothing scanned yet; `c` scans every root".to_string(),
                    next::Next::default(),
                ),
                Some(_) => {
                    let p = portfolio(store, &of, false)?;
                    (p.header(), next_lists(store, &p)?)
                }
            };
            View::Inbox {
                header,
                lists,
                repos: repos(store)?,
                state: store::state_home()?,
                jobs: store.jobs(50)?,
                runs: store.runs()?,
                now: dates::now(),
            }
        }
        Query::Activity { finished } => View::Activity {
            state: store::state_home()?,
            jobs: store.jobs(50)?,
            instances: store.workflow_instances()?,
            runs: store.runs()?,
            finished,
            now: dates::now(),
        },
        Query::Library => View::Library {
            revisions: store.workflow_revisions()?,
            campaigns: store.campaigns()?,
        },
        Query::Runs => {
            settle_interrupted(store)?;
            View::Runs(
                store
                    .runs()?
                    .into_iter()
                    .filter(|r| !r.state.is_final())
                    .collect(),
            )
        }
        Query::Run { id } => {
            let run = store.run(id)?;
            let home = store::state_home()?;
            // The tree on both sides of the diff, so an approval can require
            // what was shown. A tree that moved meanwhile is not recorded.
            let approvable = matches!(run.state, RunState::Ready | RunState::Approved);
            let before = approvable.then(|| dispatch::tree(&run).ok()).flatten();
            // A workflow node that is not an `edit` has no branch, and its tree
            // is gone once its output is read.
            let diff = if run.branch.is_empty() {
                "(none: this node changes no files)".to_string()
            } else {
                dispatch::diff(&run).unwrap_or_else(|e| format!("(no diff: {e})"))
            };
            let after = approvable.then(|| dispatch::tree(&run).ok()).flatten();
            let moved = match (before, after) {
                (Some(a), Some(b)) if a == b => {
                    store.set_reviewed_tree(run.id, &a)?;
                    false
                }
                (Some(_), Some(_)) => true,
                _ => false,
            };
            View::Run {
                attempts: store.attempts(Some(run.id))?,
                changelog: changelog(&home, &run),
                repo: store
                    .project(&run.project)?
                    .and_then(|r| r.slug)
                    .as_deref()
                    .and_then(crate::scan::github_url),
                run: Box::new(run),
                diff,
                moved,
            }
        }
        Query::Agents => View::Agents {
            state: store::state_home()?,
            jobs: store.jobs(50)?,
            runs: store.runs()?,
            now: dates::now(),
        },
        Query::Jobs => View::Jobs(store.jobs(20)?),
        Query::JobLog { id } => {
            let job = store.job(id)?;
            View::JobLog {
                id,
                text: crate::agent::read_log(&jobs::log_path(id)?),
                awaiting: job.state == "awaiting-approval",
            }
        }
        Query::Notes => View::Notes(store.notes()?),
        Query::Workflows => View::Workflows {
            revisions: store.workflow_revisions()?,
            instances: store.workflow_instances()?,
        },
        Query::Campaigns => View::Campaigns(store.campaigns()?),
        Query::Campaign { name } => View::Campaign {
            campaign: store.campaign(&name)?,
            runs: store.runs()?,
        },
        Query::Task { project, line, key } => task_view(store, &project, line, key.as_deref())?,
        Query::Settings => {
            let rows = store.config_rows()?;
            let cfg = Config::with_overrides(rows.iter().map(|(k, v)| (k.as_str(), v.as_str())))?;
            let mut settings: Vec<(String, String, &'static str)> = Config::keys()
                .into_iter()
                .map(|key| {
                    let set = rows.iter().any(|(k, _)| *k == key);
                    let value = cfg.get(&key).unwrap_or_default();
                    (key, value, if set { "set" } else { "default" })
                })
                .collect();
            settings.extend(
                rows.iter()
                    .filter(|(k, _)| k.starts_with("projects."))
                    .map(|(k, v)| (k.clone(), v.clone(), "set")),
            );
            settings.extend(
                rows.iter()
                    .filter(|(k, _)| {
                        Config::default().get(k).is_none() && !k.starts_with("projects.")
                    })
                    .map(|(k, v)| (k.clone(), v.clone(), "unknown")),
            );
            View::Settings {
                settings,
                roots: store.roots()?,
                agents: store.agents()?,
                default_agent: cfg.agent.clone(),
                presets: store.presets()?,
                default_preset: cfg.preset.clone(),
                routes: store.route_revisions()?,
            }
        }
        Query::Scan { id } => {
            let scan = store
                .scans(crate::history::KEPT as usize)?
                .into_iter()
                .find(|s| s.id == id)
                .ok_or_else(|| format!("no scan {id}"))?;
            View::Scan {
                scan,
                changes: store.scan_changes(id)?,
            }
        }
        Query::RouteDoc { revision } => View::RouteDoc {
            revision: store
                .route_revisions()?
                .into_iter()
                .find(|r| r.revision == revision)
                .ok_or_else(|| format!("no route revision {revision}"))?,
        },
        Query::Project { name } => {
            let row = store
                .project(&name)?
                .ok_or_else(|| format!("unknown project `{name}`"))?;
            let p = portfolio(
                store,
                &Selection {
                    projects: vec![name.clone()],
                    tags: Vec::new(),
                },
                true,
            )?;
            let status = p.projects.first().and_then(|(project, row)| {
                let r = crate::report::StatusRow {
                    project,
                    tiered: row.tier.is_some(),
                    has_todo: row.has_todo,
                    lint_errors: row.lint_errors,
                    faults: &row.faults,
                };
                crate::report::status_rows(&p.cfg, &[r]).into_iter().next()
            });
            let mut runs: Vec<Run> = store
                .runs()?
                .into_iter()
                .filter(|r| r.project == name)
                .collect();
            runs.sort_by_key(|r| std::cmp::Reverse(r.id));
            let jobs = store
                .jobs(50)?
                .into_iter()
                .filter(|j| j.project.as_deref() == Some(name.as_str()))
                .collect();
            let todo = row
                .has_todo
                .then(|| std::fs::read_to_string(row.path.join("TODO.md")).ok())
                .flatten()
                .map(|text| todo_notes(&text));
            View::Project {
                jobs,
                todo,
                repo: row.slug.as_deref().and_then(crate::scan::github_url),
                state: store::state_home()?,
                scanned_at: row.scanned_at,
                path: row.path,
                tags: store
                    .project_tags()?
                    .into_iter()
                    .filter(|(p, _)| *p == name)
                    .map(|(_, t)| t)
                    .collect(),
                verify: p.cfg.project(&name).verify,
                status,
                placed: rank::place(&p.cfg, p.tasks, p.today),
                today: p.today,
                runs,
                name,
            }
        }
    })
}

/// The task in `project` on `line`, or with `key`, with everything the
/// store and its TODO.md know about it.
fn task_view(store: &Store, project: &str, line: Option<i64>, key: Option<&str>) -> Result<View> {
    let row = store
        .project(project)?
        .ok_or_else(|| format!("unknown project `{project}`"))?;
    let p = portfolio(
        store,
        &Selection {
            projects: vec![project.to_string()],
            tags: Vec::new(),
        },
        true,
    )?;
    let today = p.today;
    let placed = rank::place(&p.cfg, p.tasks, today)
        .into_iter()
        .find(|t| match (line, key) {
            (Some(l), _) => t.task.line == Some(l),
            (None, Some(k)) => t.task.line.is_none() && t.task.key.as_deref() == Some(k),
            (None, None) => false,
        })
        .ok_or_else(|| match (line, key) {
            (Some(l), _) => format!("{project} has no open task on line {l}; rescan it"),
            _ => format!("{project} has no `{}` signal now", key.unwrap_or("")),
        })?;
    let item = line.and_then(|l| {
        let text = std::fs::read_to_string(row.path.join("TODO.md")).ok()?;
        todo::parse(&text)
            .items
            .into_iter()
            .find(|i| i.line as i64 == l)
    });
    let signal = match key {
        Some("ci") => {
            let mut lines = match &row.ci {
                crate::scan::Ci::Failing(workflows) => workflows
                    .iter()
                    .map(|w| {
                        let url = row
                            .ci_runs
                            .iter()
                            .find(|(name, _)| name == w)
                            .map(|(_, u)| format!("  {u}"))
                            .unwrap_or_default();
                        format!("{w}{url}")
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let since = store.failing_since()?;
            let latest = store.last_scan()?;
            lines.push(format!(
                "failing {}",
                crate::history::since_text(since.get(project), latest, dates::now())
            ));
            lines
        }
        Some("deps") => row.deps_detail.lines().map(str::to_string).collect(),
        _ => Vec::new(),
    };
    let task_key = placed.task.key.clone();
    Ok(View::Task {
        held_by: store
            .runs()?
            .into_iter()
            .filter(|r| {
                r.project == project
                    && !r.state.is_final()
                    && Some(&r.task_key) == task_key.as_ref()
            })
            .collect(),
        placed,
        today,
        item,
        signal,
        repo: row.slug.as_deref().and_then(crate::scan::github_url),
    })
}

/// Refuses a run that left the state a front end showed it in.
fn unmoved(run: &Run, seen: Option<&str>) -> Result<()> {
    match seen {
        Some(s) if s != run.state.name() => Err(format!(
            "run #{} is now {}, not {s}; read it again",
            run.id,
            run.state.name()
        )
        .into()),
        _ => Ok(()),
    }
}

/// Approves the ready runs among `ids`, for `target` to publish, or none of
/// them. A bundle's approver must have said which items are done: `drop`.
pub fn approve_ready(
    store: &Store,
    ids: &[i64],
    target: publish::Target,
    drop: Option<&[usize]>,
) -> Result<()> {
    let by = whoami();
    let runs = ids
        .iter()
        .map(|id| store.run(*id))
        .collect::<Result<Vec<_>>>()?;
    for run in runs.iter().filter(|r| r.state == RunState::Ready) {
        match drop {
            Some(drop) => dispatch::check_drops(run, drop)?,
            None => {
                if let Some(why) = dispatch::needs_approval(run) {
                    return Err(why.into());
                }
            }
        }
    }
    if let Some(why) = runs.iter().find_map(|r| publish::keeps_pr(r, target)) {
        return Err(why.into());
    }
    for mut run in runs {
        if run.state == RunState::Ready {
            if let Some(drop) = drop.filter(|_| !run.items.is_empty()) {
                store.set_dropped(run.id, drop)?;
            }
            dispatch::approve(store, &mut run, &by)?;
        }
    }
    Ok(())
}

pub fn act(store: &Store, a: Action) -> Result<Outcome> {
    // A reviewer who commits, pushes or opens a pull request for a ready
    // run from a view has read it: it is approved first, as one act. Staging
    // is the review itself and takes a ready run as it is.
    if let Action::Publish {
        ids,
        all_approved: false,
        target,
        drop,
        ..
    } = &a
        && *target != publish::Target::Stage
    {
        approve_ready(store, ids, *target, drop.as_deref())?;
    }
    let note_text = |t: String| -> Result<String> {
        let t = t.trim().to_string();
        if t.is_empty() {
            return Err("a note needs text".into());
        }
        Ok(t)
    };
    Ok(match a {
        Action::ApproveRun { id, seen, drop } => {
            let mut run = store.run(id)?;
            unmoved(&run, seen.as_deref())?;
            dispatch::check_drops(&run, &drop)?;
            // Before the approval: an approved run must never hold an
            // earlier choice. A failed approval leaves the run ready, and the
            // next one writes the choice again.
            if !run.items.is_empty() {
                store.set_dropped(id, &drop)?;
            }
            dispatch::approve(store, &mut run, &whoami())?;
            Outcome::default()
        }
        Action::ApproveRuns { ids } => {
            let mut runs = Vec::new();
            for id in &ids {
                let run = store.run(*id)?;
                let reasons = crate::accept::review_reasons(&run);
                if !reasons.is_empty() {
                    return Err(format!(
                        "#{id} is not clean, so it is not a batch approval: {}. \
                         Approve it on its own after reading it.",
                        reasons.join("; ")
                    )
                    .into());
                }
                runs.push(run);
            }
            let by = whoami();
            let mut out = Outcome::default();
            for mut run in runs {
                dispatch::approve(store, &mut run, &by)?;
                out.said
                    .push(format!("#{} {}: approved", run.id, run.project));
            }
            out
        }
        Action::RejectRun {
            id,
            keep_git_changes,
            infra,
            seen,
        } => {
            // Rejecting changes the worktree, so it holds the session.
            let session = Session::acquire(&store::home()?)?;
            store.fail_interrupted_runs(&session)?;
            let mut run = store.run(id)?;
            unmoved(&run, seen.as_deref())?;
            let files = dispatch::reject(store, &mut run, keep_git_changes, infra)?;
            let plural = if files == 1 { "" } else { "s" };
            Outcome::said(match run.workspace {
                _ if run.state == RunState::PrOpen => format!(
                    "Fix of run #{id} rejected; its pull request stays open: {}",
                    run.outcome.as_deref().unwrap_or_default()
                ),
                _ if files == 0 => format!("Run #{id} rejected; it changed no files."),
                Workspace::Clone => format!(
                    "Run #{id} rejected; reverted {files} file{plural} in {}.",
                    run.repo.display()
                ),
                Workspace::Worktree => {
                    format!("Run #{id} rejected; removed its worktree of {files} file{plural}.")
                }
            })
        }
        Action::AddReviewTime { id, minutes } => {
            store.run(id)?;
            store.add_review_seconds(id, i64::from(minutes) * 60)?;
            Outcome::default()
        }
        Action::SettlePrs => {
            let mut out = Outcome::default();
            publish::settle(store, |run, outcome| match outcome {
                Ok(o) => out.said.push(format!("#{} {}: {o}", run.id, run.project)),
                Err(e) => out.warnings.push(format!(
                    "#{} {}: pull request state unknown: {e}",
                    run.id, run.project
                )),
            })?;
            out
        }
        Action::MergePr { id } => {
            let mut run = store.run(id)?;
            Outcome::said(publish::merge(store, &mut run)?)
        }
        Action::StopJob { id } => Outcome::said(jobs::stop(store, id)?),
        Action::ApproveJob { id } => Outcome::said(jobs::approve(store, id)?),
        Action::StopWorkflow { instance } => {
            let _session = Session::acquire(&store::home()?)?;
            let found = store
                .workflow_instance(instance)?
                .ok_or_else(|| format!("no workflow instance {instance}"))?;
            if found.finished_at.is_some() {
                return Err(format!(
                    "instance {instance} is already {}",
                    found.outcome.as_deref().unwrap_or("finished")
                )
                .into());
            }
            store.finish_instance(instance, "stopped")?;
            let _ = std::fs::remove_dir_all(
                store::state_home()?
                    .join("workflow-trees")
                    .join(instance.to_string()),
            );
            Outcome::said(format!(
                "instance {instance} stopped; runs it made stand in `pma review`"
            ))
        }
        Action::AddNote { text } => {
            let id = store.add_note(&note_text(text)?, dates::now())?;
            Outcome::said(format!("#{id}"))
        }
        Action::EditNote { id, text } => {
            if !store.edit_note(id, &note_text(text)?, dates::now())? {
                return Err(format!("no note #{id}").into());
            }
            Outcome::default()
        }
        Action::RemoveNote { id } => {
            if !store.remove_note(id)? {
                return Err(format!("no note #{id}").into());
            }
            Outcome::default()
        }
        Action::SetConfig { key, value } => {
            Config::default()
                .set(&key, &value)
                .map_err(|e| format!("{key}: {e}"))?;
            store.set_config(&key, value.trim())?;
            Outcome::said(format!("{key} = {}", value.trim()))
        }
        Action::ResetConfig { key } => match store.reset_config(&key)? {
            true => Outcome::said(format!("{key} is at its default")),
            false => Outcome::said(format!("{key} was not set")),
        },
        Action::AddRoot { path } => {
            let dir = PathBuf::from(&path);
            let dir = dir
                .canonicalize()
                .map_err(|e| format!("{}: {e}", dir.display()))?;
            if !dir.is_dir() {
                return Err(format!("{} is not a directory", dir.display()).into());
            }
            match store.add_root(&dir)? {
                true => Outcome::said(format!("{} is a root; `c` scans it", dir.display())),
                false => Outcome::said(format!("{} is already a root", dir.display())),
            }
        }
        Action::RemoveRoot { path } => {
            let dir = PathBuf::from(&path);
            let canonical = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if !store.remove_root(&canonical)? && !store.remove_root(&dir)? {
                return Err(format!("{} is not a root", dir.display()).into());
            }
            Outcome::said(format!(
                "{} removed; the next full scan marks its projects absent",
                dir.display()
            ))
        }
        Action::SetAgentField { name, field, value } => {
            let mut w = store
                .agent(&name)
                .unwrap_or_else(|_| crate::worker::blank(&name));
            crate::worker::set_field(&mut w, &field, value)?;
            store.set_agent(&w)?;
            Outcome::said(format!("{name}: {field} set"))
        }
        Action::RemoveAgent { name } => {
            if !store.remove_agent(&name)? {
                return Err(format!("unknown agent `{name}`").into());
            }
            Outcome::default()
        }
        Action::SetPreset {
            name,
            agent,
            model,
            args,
        } => {
            if !store.agents()?.iter().any(|w| w.name == agent) {
                return Err(format!("unknown agent `{agent}`; the agents list names them").into());
            }
            store.set_preset(&Preset {
                name: name.clone(),
                agent: agent.clone(),
                model: model.clone().filter(|m| !m.is_empty()),
                args,
            })?;
            Outcome::said(format!(
                "preset `{name}`: {agent}{}",
                model.map(|m| format!(" at {m}")).unwrap_or_default()
            ))
        }
        Action::RemovePreset { name } => {
            if !store.remove_preset(&name)? {
                return Err(format!("unknown preset `{name}`").into());
            }
            Outcome::default()
        }
        Action::ActivateRoute { revision } => {
            store.activate_route(revision, &whoami(), false)?;
            Outcome::said(format!("route revision {revision} is in effect"))
        }
        Action::ProposeRoute { path } => {
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
            crate::route::Policy::parse(&text)?;
            let n = store.add_route_revision(&text, &whoami())?;
            Outcome::said(format!("route revision {n} stored as a draft"))
        }
        Action::AddCampaign {
            name,
            text,
            projects,
            class,
        } => {
            let known = store.projects()?;
            for p in &projects {
                if !known.iter().any(|r| &r.name == p) {
                    return Err(format!("unknown project `{p}`; run `pma scan`").into());
                }
            }
            let class = match class.as_deref() {
                None => crate::class::Class::Specified,
                Some(c) => crate::class::Class::parse(c)
                    .ok_or_else(|| format!("unknown class `{c}`; expected A, A-, B, C or D"))?,
            };
            if !class.dispatchable() {
                return Err(format!("class {} is never dispatched", class.name()).into());
            }
            let mut members: Vec<(String, Option<i64>)> =
                projects.iter().map(|p| (p.clone(), None)).collect();
            members.sort();
            members.dedup();
            store.add_campaign(&Campaign {
                name: name.clone(),
                text,
                description: String::new(),
                class,
                created_at: dates::now(),
                members,
            })?;
            Outcome::said(format!("campaign `{name}` defined; `r` runs it"))
        }
        Action::RemoveCampaign { name } => {
            if !store.remove_campaign(&name)? {
                return Err(format!("unknown campaign `{name}`").into());
            }
            Outcome::default()
        }
        Action::SetTier { project, tier } => {
            let row = store
                .project(&project)?
                .ok_or_else(|| format!("unknown project `{project}`"))?;
            store.set_tier(&project, &row.path, tier)?;
            Outcome::said(match tier {
                Some(t) => format!("{project}: tier {t}"),
                None => format!("{project}: untiered"),
            })
        }
        Action::Tag { project, tag, add } => {
            let tag = crate::projects::normal_tag(&tag)?;
            store
                .project(&project)?
                .ok_or_else(|| format!("unknown project `{project}`"))?;
            Outcome::said(match add {
                true if store.add_project_tag(&project, &tag)? => format!("{project}: #{tag}"),
                true => format!("{project} already carries `{tag}`"),
                false if store.remove_project_tag(&project, &tag)? => {
                    format!("{project}: `{tag}` removed")
                }
                false => format!("{project} does not carry `{tag}`"),
            })
        }
        Action::Prune { project } => {
            let row = store
                .project(&project)?
                .ok_or_else(|| format!("unknown project `{project}`"))?;
            let file = row.path.join("TODO.md");
            let text =
                std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            if todo::parse(&text).has_errors() {
                return Err(format!("{}: lint errors; fix them first", file.display()).into());
            }
            let pruned = todo::prune(&text);
            let removals = pruned.done_sections.len() + pruned.items.len();
            if removals == 0 {
                return Ok(Outcome::said("nothing to prune"));
            }
            todo::save(&file, &pruned.text).map_err(|e| format!("{}: {e}", file.display()))?;
            Outcome::said(format!(
                "{removals} removals made in {}; the edit is uncommitted",
                file.display()
            ))
        }
        Action::FixTodo { project } => {
            let row = store
                .project(&project)?
                .ok_or_else(|| format!("unknown project `{project}`"))?;
            let file = row.path.join("TODO.md");
            let text =
                std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let fixed = todo::fix(&text);
            if fixed.fixes.is_empty() {
                return Ok(Outcome::said("nothing to fix"));
            }
            todo::save(&file, &fixed.text).map_err(|e| format!("{}: {e}", file.display()))?;
            Outcome::said(format!(
                "{} lines rewritten in {}; the edit is uncommitted",
                fixed.fixes.len(),
                file.display()
            ))
        }
        Action::RefreshDeps { project } => {
            let row = store
                .project(&project)?
                .ok_or_else(|| format!("unknown project `{project}`"))?;
            let args = vec![
                "deps".to_string(),
                "refresh".into(),
                row.path.to_string_lossy().into_owned(),
            ];
            let id = jobs::queue(store, &args, &std::env::current_dir()?, Some(&project))?;
            Outcome::said(format!("job {id} queued: pma {}", args.join(" ")))
        }
        Action::Scan {
            offline: true,
            deps: true,
            ..
        } => return Err("a scan is offline or reads dependencies, not both".into()),
        queued => {
            let args = command_line(&queued).expect("every other action is queued");
            let project = project_of(&queued);
            let id = jobs::queue(store, &args, &std::env::current_dir()?, project.as_deref())?;
            Outcome::said(format!("job {id} queued: pma {}", args.join(" ")))
        }
    })
}

/// The project every name or `name:task` target names, when they name one.
pub fn one_project(names: &[String]) -> Option<String> {
    let mut projects = names.iter().map(|n| n.split(':').next().unwrap_or(n));
    let first = projects.next()?;
    projects.all(|p| p == first).then(|| first.to_string())
}

/// The one project a queued action is about, when every name it holds is
/// that project, so the job shows in that project's view.
fn project_of(a: &Action) -> Option<String> {
    match a {
        Action::Run { project, .. } => Some(project.clone()),
        Action::Dispatch { targets, .. } => one_project(targets),
        Action::WorkflowRun { projects, .. } => one_project(projects),
        Action::Scan { of, .. } | Action::Sync { of, .. } | Action::Verify { of } => {
            one_project(&of.projects)
        }
        _ => None,
    }
}

/// The CLI command a queued action runs; `None` for one that runs in place.
pub fn command_line(a: &Action) -> Option<Vec<String>> {
    let mut v: Vec<String> = Vec::new();
    let mut push = |s: &str| v.push(s.to_string());
    let flag = |v: &mut Vec<String>, on: bool, name: &str| {
        if on {
            v.push(name.to_string());
        }
    };
    // `--name=value`, so a value that starts with `-` is not read as a flag.
    let opt = |v: &mut Vec<String>, name: &str, value: &Option<String>| {
        if let Some(x) = value {
            v.push(format!("{name}={x}"));
        }
    };
    let worker = |v: &mut Vec<String>, w: &WorkerChoice| {
        opt(v, "--agent", &w.agent);
        opt(v, "--model", &w.model);
        opt(v, "--preset", &w.preset);
    };
    let tags = |v: &mut Vec<String>, tags: &[String]| {
        for t in tags {
            v.push(format!("--tag={t}"));
        }
    };
    match a {
        Action::Scan { of, offline, deps } => {
            push("scan");
            v.extend(of.projects.iter().cloned());
            tags(&mut v, &of.tags);
            flag(&mut v, *offline, "--offline");
            flag(&mut v, *deps, "--deps");
        }
        Action::Dispatch {
            targets,
            tags: t,
            auto,
            count,
            retry,
            worker: w,
            prompt,
        } => {
            push("dispatch");
            v.extend(targets.iter().cloned());
            tags(&mut v, t);
            flag(&mut v, *auto, "--auto");
            opt(&mut v, "--count", &count.map(|n| n.to_string()));
            flag(&mut v, *retry, "--retry");
            opt(&mut v, "--prompt", prompt);
            worker(&mut v, w);
        }
        Action::WorkflowRun {
            name,
            projects,
            tag,
            instance,
            set,
            approve,
            unattended,
            local,
            cap,
            worker: w,
        } => {
            v.extend(["workflow".to_string(), "run".into(), name.clone()]);
            v.extend(projects.iter().cloned());
            opt(&mut v, "--tag", tag);
            opt(&mut v, "--instance", &instance.map(|n| n.to_string()));
            for s in set {
                v.push(format!("--set={s}"));
            }
            opt(&mut v, "--approve", approve);
            flag(&mut v, *unattended, "--unattended");
            flag(&mut v, *local, "--local");
            for c in cap {
                v.push(format!("--cap={c}"));
            }
            worker(&mut v, w);
        }
        Action::Run {
            name,
            project,
            review,
            worktree,
            push,
            worker: w,
        } => {
            v.extend(["run".to_string(), name.clone(), project.clone()]);
            flag(&mut v, *review, "--review");
            flag(&mut v, *worktree, "--worktree");
            flag(&mut v, *push, "--push");
            if let Some(agent) = &w.agent {
                let model = w
                    .model
                    .as_deref()
                    .map_or(String::new(), |m| format!(":{m}"));
                v.push(format!("--worker={agent}{model}"));
            }
        }
        Action::ReworkRun { id, feedback } => {
            v.extend([
                "review".to_string(),
                id.to_string(),
                format!("--rework={feedback}"),
            ]);
        }
        Action::FixConflicts { id } => {
            v.extend(["merge".to_string(), id.to_string(), "--fix".into()]);
        }
        Action::RetryRun { id } => {
            v.extend(["review".to_string(), id.to_string(), "--retry".into()]);
        }
        Action::Publish {
            ids,
            all_approved,
            target,
            subject,
            drop: _,
        } => {
            push(target.name());
            v.extend(ids.iter().map(i64::to_string));
            flag(&mut v, *all_approved, "--all-approved");
            opt(&mut v, "--subject", subject);
        }
        Action::RunCampaign { name, count } => {
            v.extend(["campaign".to_string(), "run".into(), name.clone()]);
            opt(&mut v, "--count", &count.map(|n| n.to_string()));
        }
        Action::Sync { of, apply } => {
            push("sync");
            v.extend(of.projects.iter().cloned());
            tags(&mut v, &of.tags);
            flag(&mut v, *apply, "--apply");
        }
        Action::Verify { of } => {
            push("verify");
            v.extend(of.projects.iter().cloned());
            tags(&mut v, &of.tags);
        }
        Action::ApproveRun { .. }
        | Action::ApproveRuns { .. }
        | Action::RejectRun { .. }
        | Action::AddReviewTime { .. }
        | Action::SettlePrs
        | Action::MergePr { .. }
        | Action::StopJob { .. }
        | Action::ApproveJob { .. }
        | Action::StopWorkflow { .. }
        | Action::AddNote { .. }
        | Action::EditNote { .. }
        | Action::RemoveNote { .. }
        | Action::SetConfig { .. }
        | Action::ResetConfig { .. }
        | Action::AddRoot { .. }
        | Action::RemoveRoot { .. }
        | Action::SetAgentField { .. }
        | Action::RemoveAgent { .. }
        | Action::SetPreset { .. }
        | Action::RemovePreset { .. }
        | Action::ActivateRoute { .. }
        | Action::ProposeRoute { .. }
        | Action::AddCampaign { .. }
        | Action::RemoveCampaign { .. }
        | Action::SetTier { .. }
        | Action::Tag { .. }
        | Action::Prune { .. }
        | Action::FixTodo { .. }
        // Queued by `act` itself, with the project's path resolved.
        | Action::RefreshDeps { .. } => return None,
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A job shows in a project's view when everything it names is in that
    /// project: several of its tasks too.
    #[test]
    fn a_job_is_about_one_project_when_every_name_is() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            one_project(&names(&["a:5", "a:9", "a"])).as_deref(),
            Some("a")
        );
        assert_eq!(one_project(&names(&["a:5", "b:2"])), None);
        assert_eq!(one_project(&[]), None);
    }

    /// A queued action's command line is one the CLI parses, with each flag
    /// spelled as the CLI spells it.
    #[test]
    fn queued_actions_name_their_cli_command() {
        let line = |a: Action| command_line(&a).expect("queued");
        let w = WorkerChoice {
            agent: None,
            model: Some("haiku".into()),
            preset: None,
        };
        assert_eq!(
            line(Action::Run {
                name: "fix-ci".into(),
                project: "spore".into(),
                review: false,
                worktree: true,
                push: true,
                worker: WorkerChoice {
                    agent: Some("claude".into()),
                    model: Some("opus".into()),
                    preset: None,
                },
            }),
            [
                "run",
                "fix-ci",
                "spore",
                "--worktree",
                "--push",
                "--worker=claude:opus"
            ]
        );
        assert_eq!(
            line(Action::WorkflowRun {
                name: "review-fix".into(),
                projects: vec!["argdec".into()],
                tag: None,
                instance: Some(3),
                set: vec!["breadth=5".into()],
                approve: Some("abc".into()),
                unattended: false,
                local: true,
                cap: vec![],
                worker: w.clone(),
            }),
            [
                "workflow",
                "run",
                "review-fix",
                "argdec",
                "--instance=3",
                "--set=breadth=5",
                "--approve=abc",
                "--local",
                "--model=haiku"
            ]
        );
        assert_eq!(
            line(Action::Dispatch {
                targets: vec![],
                tags: vec!["py".into()],
                auto: true,
                count: Some(2),
                retry: false,
                worker: w.clone(),
                prompt: None,
            }),
            [
                "dispatch",
                "--tag=py",
                "--auto",
                "--count=2",
                "--model=haiku"
            ]
        );
        assert_eq!(
            line(Action::Publish {
                ids: vec![4],
                all_approved: false,
                target: publish::Target::Pr,
                subject: None,
                drop: None,
            }),
            ["pr", "4"]
        );
    }

    /// A watch sees a commit by another connection, and names the runs that
    /// reached review or left it; what it saw first is not news.
    #[test]
    fn a_watch_names_runs_that_reach_review() {
        let dir = std::env::temp_dir().join(format!("pma-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.db");
        let writer = Store::open(&path).unwrap();
        let mut run = Run {
            project: "p".into(),
            task_key: "k".into(),
            text: "Fix it".into(),
            agent: "claude".into(),
            repo: PathBuf::from("/r"),
            worktree: PathBuf::from("/w"),
            state: RunState::Running,
            ..Run::default()
        };
        writer.insert_run(&mut run).unwrap();

        let reader = Store::open(&path).unwrap();
        let mut watch = Watch::new(&reader).unwrap();
        assert!(watch.busy);
        assert_eq!(
            watch.poll(&reader).unwrap(),
            None,
            "nothing committed since"
        );

        run.state = RunState::Ready;
        writer.update_run(&run).unwrap();
        let moved = watch.poll(&reader).unwrap().expect("a commit was seen");
        assert_eq!(moved.len(), 1);
        assert_eq!(
            (moved[0].kind, moved[0].id, moved[0].state.as_str()),
            ("run", 1, "ready")
        );
        assert_eq!(moved[0].project, "p");
        assert!(!watch.busy);

        // A new run starting is a change, but not a notice.
        let mut second = Run {
            state: RunState::Running,
            ..run.clone()
        };
        writer.insert_run(&mut second).unwrap();
        assert_eq!(watch.poll(&reader).unwrap(), Some(Vec::new()));
        assert!(watch.busy);

        // A staged run says what comes next.
        run.state = RunState::Staged;
        writer.update_run(&run).unwrap();
        let moved = watch.poll(&reader).unwrap().expect("a commit was seen");
        assert_eq!(moved.len(), 1);
        assert!(
            moved[0].line().contains("staged in its clone, commit next"),
            "{}",
            moved[0].line()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
