//! A `View` as an interactive front end shows it: tables of rows to select,
//! or a detail of fields and sections; and how a key on a selected row
//! becomes an `Action`. The TUI and the web both draw a `Screen` and send what
//! `action` builds, so neither decides for itself what a row is, what a key
//! does to it, or what a state means. Each cell carries a `Tone`, so both
//! colour a failed run or a critical task alike.

use std::collections::BTreeMap;

use crate::api::{Action, Selection, View, WorkerChoice};
use crate::keys::{Act, Tab};
use crate::store::{Job, Run, RunState};
use crate::{accept, dates, history, next, publish, rank, report, report_runs, scan, watch};

/// What a row is about.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    Nothing,
    /// A task, by its dispatch target; `retry` once its attempts are used.
    Task {
        project: String,
        target: String,
        retry: bool,
    },
    /// Tasks marked together, by dispatch target. Dispatching them makes
    /// one run per project (`docs/dev/bundled-dispatch.md`).
    Tasks {
        targets: Vec<String>,
        retry: bool,
    },
    /// Projects marked together, for one `pma run` over them all.
    Projects {
        names: Vec<String>,
    },
    /// A task no agent may take.
    Manual {
        project: String,
    },
    Run {
        id: i64,
        /// Ready, with no reason to read it: a batch approval may take it.
        clean: bool,
        /// A conflict fix, which publishes only to its open pull request.
        fixing: bool,
        /// The state the row showed. Approving or rejecting is refused once
        /// the run has left it, so no one acts on a run they did not see.
        state: String,
        project: String,
    },
    Job {
        id: i64,
        awaiting: bool,
    },
    Instance {
        id: i64,
        workflow: String,
        open: bool,
    },
    Note {
        id: i64,
        text: String,
    },
    Campaign {
        name: String,
    },
    /// A setting, with the value it shows.
    Setting {
        key: String,
        value: String,
    },
    Root {
        path: String,
    },
    Agent {
        name: String,
    },
    Preset {
        name: String,
        agent: String,
        model: Option<String>,
        args: Vec<String>,
    },
    Route {
        revision: i64,
        active: bool,
    },
    Scan {
        id: i64,
    },
}

/// What a value means, for a front end to colour: its palette, one meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    /// Context: ages, ids, counts of nothing.
    Muted,
    /// Under way.
    Accent,
    /// Done and accepted.
    Good,
    /// Waits on you.
    Warn,
    /// Failed, or critical.
    Bad,
    /// A total.
    Strong,
}

impl Tone {
    pub fn name(self) -> &'static str {
        match self {
            Tone::Plain => "plain",
            Tone::Muted => "muted",
            Tone::Accent => "accent",
            Tone::Good => "good",
            Tone::Warn => "warn",
            Tone::Bad => "bad",
            Tone::Strong => "strong",
        }
    }
}

/// How a column lays out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Short text, as wide as its longest value.
    Text,
    /// The column that takes the remaining width; truncated, never wrapped.
    Wide,
    /// Right-aligned.
    Number,
    /// A state or a priority: a short word shown as a badge.
    Badge,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub title: &'static str,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub text: String,
    pub tone: Tone,
    /// A page on GitHub the cell names, for a front end that can open one.
    pub link: Option<String>,
}

/// Whether `act` can apply to a row about `pick`. An act that needs no row
/// fits any. A front end offers only the acts that fit the selected row.
pub fn fits(act: Act, pick: &Pick) -> bool {
    match act {
        Act::Dispatch | Act::DispatchPush => {
            matches!(pick, Pick::Task { .. } | Pick::Tasks { .. })
        }
        Act::Verify => matches!(pick, Pick::Task { .. } | Pick::Manual { .. }),
        Act::AddReviewTime => matches!(pick, Pick::Run { .. }),
        Act::ApproveRun
        | Act::RejectRun
        | Act::ReworkRun
        | Act::RetryRun
        | Act::Pr
        | Act::Merge
        | Act::FixConflicts => matches!(pick, Pick::Run { state, .. } if run_allows(act, state)),
        Act::Stage | Act::Commit | Act::Push => {
            matches!(pick, Pick::Run { state, fixing: false, .. } if run_allows(act, state))
        }
        Act::ApproveJob => matches!(pick, Pick::Job { awaiting: true, .. }),
        Act::StopJob => matches!(pick, Pick::Job { .. }),
        Act::StopWorkflow => matches!(pick, Pick::Instance { open: true, .. }),
        Act::ResumeWorkflow => matches!(pick, Pick::Instance { open: true, .. }),
        Act::RunCampaign | Act::RemoveCampaign => matches!(pick, Pick::Campaign { .. }),
        Act::EditNote | Act::RemoveNote => matches!(pick, Pick::Note { .. }),
        Act::EditSetting | Act::ResetSetting => matches!(pick, Pick::Setting { .. }),
        Act::RemoveRoot => matches!(pick, Pick::Root { .. }),
        Act::EditAgent | Act::RemoveAgent | Act::UseAgent => matches!(pick, Pick::Agent { .. }),
        Act::EditPreset | Act::RemovePreset | Act::UsePreset => {
            matches!(pick, Pick::Preset { .. })
        }
        Act::ActivateRoute => matches!(pick, Pick::Route { active: false, .. }),
        _ => !act.on_row(),
    }
}

/// Whether a run in `state` takes `act`, as `dispatch` and `publish` check
/// it. A ready run takes commit, pull request and push: `api::act`
/// approves it first.
fn run_allows(act: Act, state: &str) -> bool {
    let takes: &[&str] = match act {
        Act::ApproveRun => &["ready", "approved"],
        Act::RejectRun => &["ready", "failed", "approved", "staged", "committed"],
        Act::ReworkRun => &["ready", "failed", "approved"],
        Act::RetryRun => &["failed"],
        Act::Stage | Act::Pr => &["ready", "approved"],
        Act::Commit => &["ready", "approved", "staged"],
        Act::Push => &["ready", "approved", "staged", "committed"],
        Act::Merge | Act::FixConflicts => &["pr-open"],
        _ => return true,
    };
    takes.contains(&state)
}

/// The act that moves a row on from the state it is in, which a front end
/// keeps in view: a staged run commits next, a committed one pushes.
pub fn next_act(pick: &Pick) -> Option<Act> {
    match pick {
        Pick::Run { state, fixing, .. } => match state.as_str() {
            "approved" if *fixing => Some(Act::Pr),
            "ready" => Some(Act::ApproveRun),
            "failed" => Some(Act::RetryRun),
            "staged" => Some(Act::Commit),
            "committed" | "approved" => Some(Act::Push),
            _ => None,
        },
        _ => None,
    }
}

/// The 1-based numbers an `Ask::Check` left unchecked, from its input.
fn unchecked(input: &str) -> Result<Vec<usize>, String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| {
            n.parse()
                .map_err(|_| format!("`{n}` is not an item number"))
        })
        .collect()
}

/// The marked task rows as one pick; `Nothing` when none is a task. Retried
/// when any of them would be on its own.
pub fn marked(marks: &[Pick]) -> Pick {
    let mut targets = Vec::new();
    let mut names = Vec::new();
    let mut again = false;
    for m in marks {
        match m {
            Pick::Task { target, retry, .. } => {
                targets.push(target.clone());
                again |= retry;
            }
            Pick::Manual { project } => names.push(project.clone()),
            _ => {}
        }
    }
    match (targets.is_empty(), names.is_empty()) {
        (false, _) => Pick::Tasks {
            targets,
            retry: again,
        },
        (true, false) => Pick::Projects { names },
        (true, true) => Pick::Nothing,
    }
}

impl Pick {
    /// The project a row is about, when it names one.
    pub fn project(&self) -> Option<&str> {
        match self {
            Pick::Task { project, .. } | Pick::Manual { project } | Pick::Run { project, .. } => {
                Some(project)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub cells: Vec<Cell>,
    pub pick: Pick,
}

impl Item {
    /// The text of each cell, for a test or a plain listing.
    pub fn texts(&self) -> Vec<&str> {
        self.cells.iter().map(|c| c.text.as_str()).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pane {
    pub title: String,
    pub columns: Vec<Column>,
    pub items: Vec<Item>,
    /// What an empty pane says instead of nothing.
    pub empty: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// Text to read, wrapped.
    Prose,
    /// A unified diff: lines starting `+`, `-` and `@@` mean something.
    Diff,
    /// Output kept as written, in a fixed-width font.
    Log,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Detail {
    pub title: String,
    /// What it is about, so the acts that fit it apply from inside it.
    pub pick: Pick,
    /// A badge beside the title, such as the run's state.
    pub badge: Option<Cell>,
    /// A warning above everything else.
    pub notice: Option<String>,
    pub fields: Vec<(String, String)>,
    pub sections: Vec<(String, Section, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Panes(Vec<Pane>),
    Detail(Box<Detail>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    /// The tab it belongs to; `None` for a detail opened from one.
    pub tab: Option<Tab>,
    /// A line above the body, such as when the last scan ran.
    pub header: Option<String>,
    /// Facts above the body, label and value: a project's tier, state, CI.
    pub summary: Vec<(String, Cell)>,
    pub body: Body,
}

fn cell(text: impl Into<String>, tone: Tone) -> Cell {
    Cell {
        text: text.into(),
        tone,
        link: None,
    }
}

/// `c` linking to `url`, when the URL is a GitHub page. Run URLs come from
/// `gh`'s output; a page must not receive any other scheme.
fn linked(c: Cell, url: Option<&str>) -> Cell {
    Cell {
        link: url
            .filter(|u| u.starts_with("https://github.com/"))
            .map(String::from),
        ..c
    }
}

fn plain(text: impl Into<String>) -> Cell {
    cell(text, Tone::Plain)
}

fn muted(text: impl Into<String>) -> Cell {
    cell(text, Tone::Muted)
}

const fn col(title: &'static str, kind: Kind) -> Column {
    Column { title, kind }
}

/// A run's state, by what it asks of you.
pub fn run_tone(state: &str) -> Tone {
    match state {
        "queued" | "running" | "pr-open" => Tone::Accent,
        "ready" => Tone::Warn,
        "approved" | "pushed" | "merged" | "applied" | "done" => Tone::Good,
        "failed" | "rejected" => Tone::Bad,
        _ => Tone::Muted,
    }
}

pub fn job_tone(state: &str) -> Tone {
    match state {
        "queued" | "running" => Tone::Accent,
        "awaiting-approval" => Tone::Warn,
        "done" | "approved" => Tone::Good,
        "failed" => Tone::Bad,
        _ => Tone::Muted,
    }
}

pub fn priority_tone(priority: &str) -> Tone {
    match priority {
        "critical" => Tone::Bad,
        "high" => Tone::Warn,
        "low" => Tone::Muted,
        _ => Tone::Plain,
    }
}

/// A project's worst state, as `pma status` names it.
fn state_tone(state: &str) -> Tone {
    match state.split(" +").next().unwrap_or(state) {
        "failing CI"
        | "git settings changed"
        | "git unreadable"
        | "TODO unreadable"
        | "TODO lint"
        | "critical" => Tone::Bad,
        "CI unknown" | "unpublished" | "unpublished unknown" | "scan partial" | "stale deps" => {
            Tone::Warn
        }
        "idle" => Tone::Muted,
        "clear" => Tone::Good,
        _ => Tone::Plain,
    }
}

/// How soon a task is due, by how soon.
fn when_tone(when: &str) -> Tone {
    match when {
        w if w.starts_with("overdue") || w == "due today" => Tone::Bad,
        w if w.starts_with("due") || w == "#urgent" || w == "signal" => Tone::Warn,
        _ => Tone::Muted,
    }
}

fn task_pick(p: &rank::Placed) -> Pick {
    let t = &p.task;
    match (&t.key, t.line) {
        (Some(_), Some(line)) => Pick::Task {
            project: t.project.clone(),
            target: format!("{}:{line}", t.project),
            retry: false,
        },
        (Some(key), None) => Pick::Task {
            project: t.project.clone(),
            target: format!("{}:{key}", t.project),
            retry: false,
        },
        (None, _) => Pick::Manual {
            project: t.project.clone(),
        },
    }
}

fn next_pick(s: &next::Subject) -> Pick {
    match s {
        next::Subject::Task {
            project,
            target,
            retry,
        } => Pick::Task {
            project: project.clone(),
            target: target.clone(),
            retry: *retry,
        },
        next::Subject::Manual { project } => Pick::Manual {
            project: project.clone(),
        },
        next::Subject::Run {
            id,
            fixing,
            state,
            project,
        } => Pick::Run {
            id: *id,
            clean: false,
            fixing: *fixing,
            state: state.clone(),
            project: project.clone(),
        },
    }
}

/// A task's cells as `pma next` builds them: place, tier, priority, when,
/// an optional reason, and its text.
fn task_cells(cells: &[String]) -> Vec<Cell> {
    let get = |i: usize| cells.get(i).cloned().unwrap_or_default();
    let (why, text) = match cells.len() {
        n if n >= 6 => (get(4), get(5)),
        _ => (String::new(), get(4)),
    };
    vec![
        plain(get(0)),
        muted(get(1)),
        cell(get(2), priority_tone(&get(2))),
        cell(get(3), when_tone(&get(3))),
        cell(why, Tone::Warn),
        plain(text),
    ]
}

const TASK_COLUMNS: [Column; 7] = [
    col("task", Kind::Text),
    col("tier", Kind::Text),
    col("priority", Kind::Badge),
    col("when", Kind::Text),
    col("why", Kind::Text),
    col("title", Kind::Wide),
    col("issue", Kind::Text),
];

/// A task's issue as `#N`, linked to it when the project's GitHub page is
/// known; empty when the task has none.
fn issue_cell(gh: Option<i64>, repo: Option<&String>) -> Cell {
    match gh {
        None => plain(""),
        Some(n) => linked(
            muted(format!("#{n}")),
            repo.map(|r| format!("{r}/issues/{n}")).as_deref(),
        ),
    }
}

fn run_cells(r: &Run, now: i64) -> Vec<Cell> {
    vec![
        muted(format!("#{}", r.id)),
        cell(r.state.name(), run_tone(r.state.name())),
        plain(r.project.clone()),
        plain(r.text.clone()),
        cell(
            r.cost_usd.map_or("-".into(), |c| format!("${c:.2}")),
            Tone::Muted,
        ),
        muted(r.dispatched_at.map_or("-".into(), |t| report::ago(now - t))),
    ]
}

const RUN_COLUMNS: [Column; 6] = [
    col("run", Kind::Text),
    col("state", Kind::Badge),
    col("project", Kind::Text),
    col("task", Kind::Wide),
    col("cost", Kind::Number),
    col("started", Kind::Text),
];

fn run_detail(
    run: &Run,
    attempts: &[crate::store::Attempt],
    changelog: Option<String>,
    diff: String,
    moved: bool,
    repo: Option<&str>,
) -> Detail {
    let fields = report::run_fields(run)
        .into_iter()
        .filter(|r| r[0] != "task")
        .map(|r| match (r[0].as_str(), run.gh, repo) {
            // The issue as its page, where the number alone would be.
            ("issue", Some(n), Some(url)) => ("issue".to_string(), format!("{url}/issues/{n}")),
            _ => (r[0].clone(), r[1].clone()),
        })
        .collect();
    let mut sections = Vec::new();
    if !run.items.is_empty() {
        sections.push(("Items".into(), Section::Prose, report::item_lines(run, "")));
    }
    if let Some(s) = run.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        sections.push(("Summary".into(), Section::Prose, s.to_string()));
    }
    match changelog.as_deref().map(str::trim) {
        Some("") => sections.push((
            "Changelog".into(),
            Section::Prose,
            "No entry drafted.".into(),
        )),
        Some(entries) => sections.push(("Changelog".into(), Section::Prose, entries.into())),
        None => {}
    }
    let reasons = accept::review_reasons(run);
    sections.push((
        "Why read it".into(),
        Section::Prose,
        match reasons.is_empty() {
            true => "Every recorded gate is clean.".into(),
            false => reasons
                .iter()
                .map(|r| format!("- {r}"))
                .collect::<Vec<_>>()
                .join("\n"),
        },
    ));
    if attempts.len() > 1 {
        sections.push((
            "Attempts".into(),
            Section::Log,
            report::table(&report::attempt_rows(attempts), ""),
        ));
    }
    sections.push(("Diff".into(), Section::Diff, diff));
    Detail {
        title: format!("#{} {}", run.id, run.text),
        pick: run_pick(run),
        badge: Some(cell(run.state.name(), run_tone(run.state.name()))),
        notice: moved
            .then(|| "The run changed while it was shown. Read it again before approving.".into()),
        fields,
        sections,
    }
}

fn run_pick(r: &Run) -> Pick {
    Pick::Run {
        id: r.id,
        clean: r.state == RunState::Ready && accept::review_reasons(r).is_empty(),
        fixing: r.fixing_pr().is_some(),
        state: r.state.name().into(),
        project: r.project.clone(),
    }
}

/// Runs and jobs in one list: under way first, then newest first. A running
/// agent's row says what its log last showed. `with_project` adds the
/// project column, for a list across projects.
fn activity_pane(
    state: &std::path::Path,
    runs: &[Run],
    jobs: &[Job],
    now: i64,
    with_project: bool,
) -> Pane {
    let agents = watch::agents(state, runs, now);
    let doing = |r: &Run| -> String {
        let Some(a) = agents.iter().find(|a| a.run.id == r.id) else {
            return String::new();
        };
        let what = a
            .progress
            .last_tool
            .clone()
            .or_else(|| a.progress.last_text.clone())
            .unwrap_or_else(|| "starting".into());
        match a.progress.tool_calls {
            0 => what,
            n => format!("{what}  ({n} tools, quiet {})", a.quiet),
        }
    };
    let row = |item: Cell,
               state: Cell,
               project: &str,
               what: String,
               doing: String,
               cost: Cell,
               age: Cell| {
        let mut cells = vec![item, state];
        if with_project {
            cells.push(plain(project));
        }
        cells.extend([plain(what), cell(doing, Tone::Accent), cost, age]);
        cells
    };
    let mut rows: Vec<(bool, i64, Item)> = runs
        .iter()
        .map(|r| {
            (
                r.state == RunState::Running,
                r.dispatched_at.unwrap_or(0),
                Item {
                    cells: row(
                        muted(format!("#{}", r.id)),
                        cell(r.state.name(), run_tone(r.state.name())),
                        &r.project,
                        r.text.clone(),
                        doing(r),
                        muted(r.cost_usd.map_or("-".into(), |c| format!("${c:.2}"))),
                        muted(r.dispatched_at.map_or("-".into(), |t| report::ago(now - t))),
                    ),
                    pick: run_pick(r),
                },
            )
        })
        .chain(jobs.iter().map(|j| {
            (
                matches!(j.state.as_str(), "queued" | "running" | "awaiting-approval"),
                j.created_at,
                Item {
                    cells: row(
                        muted(format!("job {}", j.id)),
                        cell(j.state.clone(), job_tone(&j.state)),
                        j.project.as_deref().unwrap_or(""),
                        format!("pma {}", j.args.join(" ")),
                        String::new(),
                        muted(""),
                        muted(report::ago(now - j.created_at)),
                    ),
                    pick: Pick::Job {
                        id: j.id,
                        awaiting: j.state == "awaiting-approval",
                    },
                },
            )
        }))
        .collect();
    rows.sort_by_key(|(active, at, _)| (!*active, std::cmp::Reverse(*at)));
    let mut columns = vec![col("item", Kind::Text), col("state", Kind::Badge)];
    if with_project {
        columns.push(col("project", Kind::Text));
    }
    columns.extend([
        col("what", Kind::Wide),
        col("doing", Kind::Wide),
        col("cost", Kind::Number),
        col("age", Kind::Text),
    ]);
    Pane {
        title: "Activity".into(),
        columns,
        items: rows.into_iter().map(|(_, _, i)| i).collect(),
        empty: "No agent has worked on it yet.",
    }
}

/// `pma next`'s rows as items, each about what the row is about.
/// `pma next`'s task rows as items, each with its issue linked.
fn next_items(rows: Vec<next::Row>, repos: &BTreeMap<String, String>) -> Vec<Item> {
    rows.into_iter()
        .map(|r| {
            let mut cells = task_cells(&r.cells);
            let project = match &r.subject {
                next::Subject::Task { project, .. } | next::Subject::Manual { project } => {
                    Some(project)
                }
                next::Subject::Run { .. } => None,
            };
            cells.push(issue_cell(r.issue, project.and_then(|p| repos.get(p))));
            Item {
                pick: next_pick(&r.subject),
                cells,
            }
        })
        .collect()
}

/// `pma next`'s waiting runs as items.
fn run_items(rows: Vec<next::Row>) -> Vec<Item> {
    rows.into_iter()
        .map(|r| Item {
            pick: next_pick(&r.subject),
            cells: run_row(&r.cells),
        })
        .collect()
}

/// A waiting run's cells as `pma next` builds them: run, state, what is
/// next, project and task.
fn run_row(c: &[String]) -> Vec<Cell> {
    let get = |i: usize| c.get(i).cloned().unwrap_or_default();
    vec![
        muted(get(0)),
        cell(get(1), run_tone(&get(1))),
        cell(get(2), Tone::Warn),
        plain(get(3)),
        plain(get(4)),
    ]
}

const WAITING_COLUMNS: [Column; 5] = [
    col("item", Kind::Text),
    col("state", Kind::Badge),
    col("next", Kind::Text),
    col("project", Kind::Text),
    col("what", Kind::Wide),
];

/// Each running agent, with what its log says.
fn agents_pane(state: &std::path::Path, runs: &[Run], now: i64) -> Pane {
    let agents = watch::agents(state, runs, now)
        .into_iter()
        .map(|a| Item {
            cells: vec![
                muted(format!("#{}", a.run.id)),
                plain(a.run.project.clone()),
                plain(a.run.node.clone().unwrap_or_else(|| "dispatch".into())),
                muted(a.run.agent.clone()),
                cell(a.running, Tone::Accent),
                muted(a.quiet),
                plain(a.progress.tool_calls.to_string()),
                plain(
                    a.progress
                        .last_tool
                        .or(a.progress.last_text)
                        .unwrap_or_default(),
                ),
            ],
            pick: run_pick(a.run),
        })
        .collect();
    Pane {
        title: "Running agents".into(),
        columns: vec![
            col("run", Kind::Text),
            col("project", Kind::Text),
            col("node", Kind::Text),
            col("agent", Kind::Text),
            col("running", Kind::Text),
            col("quiet", Kind::Text),
            col("tools", Kind::Number),
            col("doing", Kind::Wide),
        ],
        items: agents,
        empty: "No agent is running.",
    }
}

fn job_item(j: &Job, cells: Vec<Cell>) -> Item {
    Item {
        cells,
        pick: Pick::Job {
            id: j.id,
            awaiting: j.state == "awaiting-approval",
        },
    }
}

fn open_jobs_pane(jobs: &[Job]) -> Pane {
    Pane {
        title: "Open jobs".into(),
        columns: vec![
            col("job", Kind::Text),
            col("state", Kind::Badge),
            col("command", Kind::Wide),
        ],
        items: watch::open_jobs(jobs)
            .into_iter()
            .map(|j| {
                job_item(
                    j,
                    vec![
                        muted(format!("#{}", j.id)),
                        cell(j.state.clone(), job_tone(&j.state)),
                        plain(format!("pma {}", j.args.join(" "))),
                    ],
                )
            })
            .collect(),
        empty: "No job is queued, running or waiting.",
    }
}

fn jobs_pane(jobs: &[Job], now: i64) -> Pane {
    Pane {
        title: "Jobs".into(),
        columns: vec![
            col("job", Kind::Text),
            col("state", Kind::Badge),
            col("queued", Kind::Text),
            col("command", Kind::Wide),
        ],
        items: jobs
            .iter()
            .map(|j| {
                job_item(
                    j,
                    vec![
                        muted(format!("#{}", j.id)),
                        cell(j.state.clone(), job_tone(&j.state)),
                        muted(report::ago(now - j.created_at)),
                        plain(format!("pma {}", j.args.join(" "))),
                    ],
                )
            })
            .collect(),
        empty: "No jobs. `--detach` on dispatch or workflow run queues one.",
    }
}

/// Runs that reached a final state, newest decision first, in `only` alone
/// when it is set.
fn finished_pane(runs: &[Run], only: Option<RunState>, now: i64) -> Pane {
    let when = |r: &Run| r.published_at.or(r.decided_at).unwrap_or(0);
    let mut shown: Vec<&Run> = runs
        .iter()
        .filter(|r| r.state.is_final() && r.state != RunState::Done)
        .filter(|r| only.is_none_or(|s| r.state == s))
        .collect();
    shown.sort_by_key(|r| std::cmp::Reverse((when(r), r.id)));
    let total = shown.len();
    shown.truncate(crate::api::FINISHED_SHOWN);
    let items = shown
        .into_iter()
        .map(|r| Item {
            cells: vec![
                muted(format!("#{}", r.id)),
                cell(r.state.name().to_string(), run_tone(r.state.name())),
                plain(r.project.clone()),
                plain(r.text.clone()),
                muted(match when(r) {
                    0 => String::new(),
                    t => report::ago(now - t),
                }),
            ],
            pick: run_pick(r),
        })
        .collect();
    let of = only.map_or("every state".to_string(), |s| s.name().to_string());
    Pane {
        title: match total > crate::api::FINISHED_SHOWN {
            true => format!(
                "Finished runs: {of}, newest {} of {total}",
                crate::api::FINISHED_SHOWN
            ),
            false => format!("Finished runs: {of}"),
        },
        columns: vec![
            col("run", Kind::Text),
            col("state", Kind::Badge),
            col("project", Kind::Text),
            col("task", Kind::Wide),
            col("decided", Kind::Text),
        ],
        items,
        empty: "No finished run in this state. Press b for another.",
    }
}

fn instances_pane(instances: &[crate::store::WorkflowInstance], now: i64) -> Pane {
    let items = instances
        .iter()
        .map(|i| {
            let outcome = i.outcome.clone().unwrap_or_else(|| "open".into());
            Item {
                cells: vec![
                    muted(i.id.to_string()),
                    plain(i.workflow.clone()),
                    plain(i.target.clone()),
                    cell(
                        outcome.clone(),
                        match outcome.as_str() {
                            "open" => Tone::Accent,
                            "finished" => Tone::Good,
                            "failed" => Tone::Bad,
                            _ => Tone::Muted,
                        },
                    ),
                    muted(report::ago(now - i.started_at)),
                ],
                pick: Pick::Instance {
                    id: i.id,
                    workflow: i.workflow.clone(),
                    open: i.finished_at.is_none(),
                },
            }
        })
        .collect();
    Pane {
        title: "Workflow instances".into(),
        columns: vec![
            col("instance", Kind::Text),
            col("workflow", Kind::Text),
            col("target", Kind::Wide),
            col("status", Kind::Badge),
            col("started", Kind::Text),
        ],
        items,
        empty: "No instance. Press f to start one.",
    }
}

fn revisions_pane(revisions: &[crate::store::WorkflowRevision]) -> Pane {
    let items = revisions
        .iter()
        .map(|r| Item {
            cells: vec![
                muted(r.revision.to_string()),
                match r.activated_at {
                    None => muted("draft"),
                    Some(_) => cell("active", Tone::Good),
                },
                plain(
                    r.document()
                        .map(|d| {
                            d.workflows
                                .iter()
                                .map(|w| w.name.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default(),
                ),
                muted(match r.worst_case_cost {
                    Some(c) => format!("${c:.2} per unit"),
                    None => String::new(),
                }),
            ],
            pick: Pick::Nothing,
        })
        .collect();
    Pane {
        title: "Workflow revisions".into(),
        columns: vec![
            col("revision", Kind::Text),
            col("status", Kind::Badge),
            col("workflows", Kind::Wide),
            col("worst case", Kind::Number),
        ],
        items,
        empty: "No revision. `pma workflow propose <file>` stores one.",
    }
}

/// What lint and prune say of a project's TODO.md, a line each.
fn todo_pane(todo: Option<Vec<crate::api::TodoNote>>) -> Pane {
    let (items, empty) = match todo {
        None => (Vec::new(), "No TODO.md."),
        Some(notes) => (
            notes
                .into_iter()
                .map(|n| Item {
                    cells: vec![
                        muted(n.line.to_string()),
                        cell(
                            n.kind,
                            match n.kind {
                                "error" => Tone::Bad,
                                "warning" => Tone::Warn,
                                "kept" => Tone::Muted,
                                _ => Tone::Accent,
                            },
                        ),
                        plain(n.text),
                    ],
                    pick: Pick::Nothing,
                })
                .collect(),
            "TODO.md is clean: no lint problem, nothing to prune.",
        ),
    };
    Pane {
        title: "TODO.md".into(),
        columns: vec![
            col("line", Kind::Text),
            col("kind", Kind::Badge),
            col("what", Kind::Wide),
        ],
        items,
        empty,
    }
}

fn campaigns_pane(all: &[crate::store::Campaign]) -> Pane {
    let items = all
        .iter()
        .map(|c| {
            let taken = c.members.iter().filter(|(_, r)| r.is_some()).count();
            Item {
                cells: vec![
                    plain(c.name.clone()),
                    muted(format!("class {}", c.class.name())),
                    cell(
                        format!("{taken}/{}", c.members.len()),
                        match taken == c.members.len() {
                            true => Tone::Good,
                            false => Tone::Accent,
                        },
                    ),
                    plain(c.text.clone()),
                ],
                pick: Pick::Campaign {
                    name: c.name.clone(),
                },
            }
        })
        .collect();
    Pane {
        title: "Campaigns".into(),
        columns: vec![
            col("campaign", Kind::Text),
            col("class", Kind::Text),
            col("dispatched", Kind::Number),
            col("task", Kind::Wide),
        ],
        items,
        empty: "No campaigns. `pma campaign add` defines one.",
    }
}

/// A status row's cells, toned: `pma status`'s project, tier, state, open,
/// idle, ci, deps, local and todo.
/// A change's tone: bad news, good news, or work arriving.
pub fn change_tone(c: &history::Change) -> Tone {
    match c {
        c if c.bad() => Tone::Bad,
        c if c.good() => Tone::Good,
        _ => Tone::Accent,
    }
}

fn changes_pane(
    changes: Vec<history::ProjectChange>,
    projects: &[(rank::Project, crate::store::ProjectRow)],
) -> Pane {
    let repo = |name: &str| {
        projects
            .iter()
            .find(|(_, r)| r.name == name)
            .and_then(|(_, r)| r.slug.as_deref())
            .and_then(scan::github_url)
    };
    Pane {
        title: "Changed at the last scan".into(),
        columns: vec![
            col("project", Kind::Text),
            col("change", Kind::Badge),
            col("detail", Kind::Wide),
        ],
        items: changes
            .into_iter()
            .map(|c| {
                let url = repo(&c.project);
                let detail = match &c.change {
                    history::Change::CiFailing(_) => url.as_ref().map(|u| format!("{u}/actions")),
                    _ => None,
                };
                Item {
                    cells: vec![
                        linked(plain(c.project.clone()), url.as_deref()),
                        cell(c.change.name(), change_tone(&c.change)),
                        linked(plain(c.change.detail()), detail.as_deref()),
                    ],
                    pick: Pick::Manual { project: c.project },
                }
            })
            .collect(),
        empty: "The last scan changed nothing since the one before.",
    }
}

fn scans_pane(scans: Vec<history::Scan>, now: i64) -> Pane {
    let pick = |s: &history::Scan| Pick::Scan { id: s.id };
    let count = |n: i64, tone: Tone| match n {
        0 => muted("0"),
        n => cell(n.to_string(), tone),
    };
    // A fault no scan found leaves its cells empty, so its column is hidden.
    let fault = |n: i64, f: scan::Fault| match (n, f) {
        (0, _) => plain(""),
        (n, scan::Fault::GitSettingsChanged) => cell(n.to_string(), Tone::Bad),
        (n, _) => cell(n.to_string(), Tone::Warn),
    };
    let mut columns = vec![
        col("scan", Kind::Text),
        col("when", Kind::Text),
        col("kind", Kind::Text),
        col("projects", Kind::Number),
        col("failing", Kind::Number),
    ];
    columns.extend(scan::Fault::ALL.map(|f| col(f.name(), Kind::Number)));
    columns.extend([
        col("open", Kind::Number),
        col("opened", Kind::Number),
        col("closed", Kind::Number),
        col("other", Kind::Number),
    ]);
    Pane {
        title: "Recent scans".into(),
        columns,
        items: scans
            .into_iter()
            .map(|s| Item {
                cells: [
                    muted(format!("#{}", s.id)),
                    plain(report::ago(now - s.at)),
                    muted(if s.full { "full" } else { "partial" }),
                    plain(s.projects.to_string()),
                    count(s.failing, Tone::Bad),
                ]
                .into_iter()
                .chain(
                    scan::Fault::ALL
                        .iter()
                        .zip(s.faults)
                        .map(|(f, n)| fault(n, *f)),
                )
                .chain([
                    plain(s.open.to_string()),
                    count(s.opened, Tone::Accent),
                    count(s.closed, Tone::Good),
                    count(s.other, Tone::Plain),
                ])
                .collect(),
                pick: pick(&s),
            })
            .collect(),
        empty: "No scan recorded yet.",
    }
}

fn status_cells(c: &[String]) -> Vec<Cell> {
    let get = |i: usize| c.get(i).cloned().unwrap_or_default();
    let ci = get(5);
    let local = get(7);
    let todo = get(8);
    vec![
        plain(get(0)),
        muted(get(1)),
        cell(get(2), state_tone(&get(2))),
        plain(get(3)),
        muted(get(4)),
        cell(
            ci.clone(),
            match ci.as_str() {
                "failing" => Tone::Bad,
                "passing" => Tone::Good,
                _ => Tone::Muted,
            },
        ),
        cell(
            get(6),
            match get(6).as_str() {
                "-" | "0" => Tone::Muted,
                _ => Tone::Warn,
            },
        ),
        cell(
            local.clone(),
            match local.as_str() {
                "clean" => Tone::Muted,
                _ => Tone::Warn,
            },
        ),
        cell(
            todo.clone(),
            match todo.as_str() {
                "ok" => Tone::Muted,
                "missing" => Tone::Warn,
                _ => Tone::Bad,
            },
        ),
    ]
}

/// The screen a view makes. Every view has one, so a query a front end can
/// send is a screen it can draw.
pub fn screen(view: View) -> Screen {
    let panes = |tab: Tab, header: Option<String>, panes: Vec<Pane>| Screen {
        tab: Some(tab),
        header,
        summary: Vec::new(),
        body: Body::Panes(panes),
    };
    let detail = |tab: Option<Tab>, header: Option<String>, d: Detail| Screen {
        tab,
        header,
        summary: Vec::new(),
        body: Body::Detail(Box::new(d)),
    };
    let now = dates::now();
    match view {
        View::Next {
            header,
            lists,
            repos,
            ..
        } => panes(
            Tab::Inbox,
            Some(header),
            vec![
                Pane {
                    title: "Ready for agents".into(),
                    columns: TASK_COLUMNS.to_vec(),
                    items: next_items(lists.agents, &repos),
                    empty: "Nothing an agent may take. Tag a TODO.md item #agent.",
                },
                Pane {
                    title: "Runs waiting on you".into(),
                    columns: WAITING_COLUMNS.to_vec(),
                    items: run_items(lists.runs),
                    empty: "No run waits on you.",
                },
                Pane {
                    title: "Tasks for you".into(),
                    columns: TASK_COLUMNS.to_vec(),
                    items: next_items(lists.tasks, &repos),
                    empty: "No critical or urgent task needs you.",
                },
            ],
        ),
        View::Inbox {
            header,
            lists,
            state,
            jobs,
            runs,
            now: at,
            repos,
        } => {
            // Runs waiting on a decision, then plans waiting for approval.
            let mut waiting = run_items(lists.runs);
            waiting.extend(
                jobs.iter()
                    .filter(|j| j.state == "awaiting-approval")
                    .map(|j| Item {
                        cells: vec![
                            muted(format!("job {}", j.id)),
                            cell(j.state.clone(), job_tone(&j.state)),
                            cell("approve plan", Tone::Warn),
                            plain(j.project.clone().unwrap_or_default()),
                            plain(format!("pma {}", j.args.join(" "))),
                        ],
                        pick: Pick::Job {
                            id: j.id,
                            awaiting: true,
                        },
                    }),
            );
            let running: Vec<Run> = runs
                .into_iter()
                .filter(|r| r.state == RunState::Running)
                .collect();
            let open: Vec<Job> = jobs
                .into_iter()
                .filter(|j| matches!(j.state.as_str(), "queued" | "running"))
                .collect();
            let mut working = activity_pane(&state, &running, &open, at, true);
            working.title = "Working now".into();
            working.empty = "No agent or job is under way.";
            // Tasks agents may take, then the critical and urgent ones only
            // you can.
            let for_cell = |who: &str, items: Vec<Item>| -> Vec<Item> {
                items
                    .into_iter()
                    .map(|mut i| {
                        i.cells.push(match who {
                            "agent" => cell("agent", Tone::Accent),
                            _ => muted("you"),
                        });
                        i
                    })
                    .collect()
            };
            let mut tasks = for_cell("agent", next_items(lists.agents, &repos));
            tasks.extend(for_cell("you", next_items(lists.tasks, &repos)));
            let mut columns = TASK_COLUMNS.to_vec();
            columns.push(col("for", Kind::Badge));
            panes(
                Tab::Inbox,
                Some(header),
                vec![
                    Pane {
                        title: "Waiting on you".into(),
                        columns: WAITING_COLUMNS.to_vec(),
                        items: waiting,
                        empty: "Nothing waits on you.",
                    },
                    working,
                    Pane {
                        title: "Tasks".into(),
                        columns,
                        items: tasks,
                        empty: "No task is ready for an agent, and none needs you now.",
                    },
                ],
            )
        }
        View::Matrix {
            header,
            placed,
            today,
            repos,
            ..
        } => {
            let mut quadrants: Vec<Pane> = rank::Quadrant::ALL
                .iter()
                .map(|q| Pane {
                    title: format!("{q:?} {}", q.title()),
                    columns: vec![
                        col("task", Kind::Text),
                        col("priority", Kind::Badge),
                        col("when", Kind::Text),
                        col("title", Kind::Wide),
                        col("issue", Kind::Text),
                    ],
                    items: Vec::new(),
                    empty: "Nothing here.",
                })
                .collect();
            for p in &placed {
                let place = match p.task.line {
                    Some(line) => format!("{}:{line}", p.task.project),
                    None => p.task.project.clone(),
                };
                let when = report::when(p, today);
                let priority = p.task.priority.name();
                quadrants[p.quadrant as usize].items.push(Item {
                    cells: vec![
                        plain(place),
                        cell(priority, priority_tone(priority)),
                        cell(when.clone(), when_tone(&when)),
                        plain(p.task.text.clone()),
                        issue_cell(p.task.gh, repos.get(&p.task.project)),
                    ],
                    pick: task_pick(p),
                });
            }
            panes(Tab::Matrix, Some(header), quadrants)
        }
        View::Status {
            header,
            portfolio: p,
            scans,
            changes,
            since,
            ..
        } => {
            let rows: Vec<report::StatusRow> = p
                .projects
                .iter()
                .map(|(project, row)| report::StatusRow {
                    project,
                    tiered: row.tier.is_some(),
                    has_todo: row.has_todo,
                    lint_errors: row.lint_errors,
                    faults: &row.faults,
                })
                .collect();
            let row_of = |name: &str| p.projects.iter().map(|(_, r)| r).find(|r| r.name == name);
            let repo = |name: &str| {
                row_of(name)
                    .and_then(|r| r.slug.as_deref())
                    .and_then(scan::github_url)
            };
            let items: Vec<Item> = report::status_rows(&p.cfg, &rows)
                .into_iter()
                .map(|(_, c)| {
                    let name = c.first().cloned().unwrap_or_default();
                    let url = repo(&name);
                    let mut cells = status_cells(&c);
                    cells[0] = linked(cells[0].clone(), url.as_deref());
                    cells[5] = linked(
                        cells[5].clone(),
                        url.map(|u| format!("{u}/actions")).as_deref(),
                    );
                    Item {
                        pick: Pick::Manual { project: name },
                        cells,
                    }
                })
                .collect();
            let latest = scans.first().map(|s| s.at);
            let mut failing = Vec::new();
            for item in &items {
                let Pick::Manual { project } = &item.pick else {
                    continue;
                };
                let Some(row) = row_of(project) else {
                    continue;
                };
                let scan::Ci::Failing(workflows) = &row.ci else {
                    continue;
                };
                let actions = repo(project).map(|u| format!("{u}/actions"));
                for w in workflows {
                    let run = row
                        .ci_runs
                        .iter()
                        .find(|(name, _)| name == w)
                        .map(|(_, u)| u.clone())
                        .or_else(|| actions.clone());
                    failing.push(Item {
                        cells: vec![
                            linked(plain(project.clone()), repo(project).as_deref()),
                            linked(cell(w.clone(), Tone::Bad), run.as_deref()),
                            plain(history::since_text(since.get(project), latest, now)),
                            linked(muted(run.clone().unwrap_or_default()), run.as_deref()),
                        ],
                        pick: Pick::Manual {
                            project: project.clone(),
                        },
                    });
                }
            }
            panes(
                Tab::Projects,
                Some(header),
                vec![
                    Pane {
                        title: "Projects, worst first".into(),
                        columns: vec![
                            col("project", Kind::Text),
                            col("tier", Kind::Text),
                            col("state", Kind::Badge),
                            col("open", Kind::Number),
                            col("idle", Kind::Number),
                            col("ci", Kind::Badge),
                            col("deps", Kind::Number),
                            col("local", Kind::Text),
                            col("todo", Kind::Text),
                        ],
                        items,
                        empty: "No tiered project. `pma project tier <1-5> <project>` ranks one.",
                    },
                    Pane {
                        title: "Failing CI".into(),
                        columns: vec![
                            col("project", Kind::Text),
                            col("workflow", Kind::Badge),
                            col("failing", Kind::Text),
                            col("run", Kind::Wide),
                        ],
                        items: failing,
                        empty: "No workflow fails on a default branch.",
                    },
                    changes_pane(changes, &p.projects),
                    scans_pane(scans, now),
                ],
            )
        }
        View::Stale {
            header,
            placed,
            limit,
        } => detail(
            None,
            Some(header),
            Detail {
                title: "Stale tasks".into(),
                pick: Pick::Nothing,
                badge: None,
                notice: None,
                fields: Vec::new(),
                sections: vec![(
                    "Oldest first".into(),
                    Section::Log,
                    report::stale(&placed, limit),
                )],
            },
        ),
        View::Report { runs, attempts, by } => {
            let by_name = report_runs::By::ALL
                .iter()
                .find(|(_, b)| *b == by)
                .map_or("?", |(n, _)| n);
            let (summary, items) = match report_runs::table_of(&runs, &attempts, by) {
                None => ("No finished runs yet.".to_string(), Vec::new()),
                Some((summary, rows)) => {
                    let last = rows.len().saturating_sub(1);
                    let items = rows
                        .into_iter()
                        .enumerate()
                        .skip(1)
                        .map(|(i, r)| Item {
                            cells: r
                                .into_iter()
                                .map(|t| match i == last {
                                    true => cell(t, Tone::Strong),
                                    false => plain(t),
                                })
                                .collect(),
                            pick: Pick::Nothing,
                        })
                        .collect();
                    (summary, items)
                }
            };
            let mut columns = vec![col(by_name, Kind::Wide)];
            columns.extend(report_runs::HEADINGS.iter().map(|h| col(h, Kind::Number)));
            panes(
                Tab::Report,
                Some(summary),
                vec![Pane {
                    title: format!("Runs by {by_name}"),
                    columns,
                    items,
                    empty: "Dispatch some tasks first.",
                }],
            )
        }
        View::Runs(runs) => {
            let items = runs
                .iter()
                .map(|r| Item {
                    cells: run_cells(r, now),
                    pick: run_pick(r),
                })
                .collect::<Vec<_>>();
            panes(
                Tab::Inbox,
                None,
                vec![Pane {
                    title: "Runs not yet final".into(),
                    columns: RUN_COLUMNS.to_vec(),
                    items,
                    empty: "No run waits on a decision.",
                }],
            )
        }
        View::Run {
            run,
            attempts,
            changelog,
            diff,
            moved,
            repo,
        } => detail(
            None,
            Some(run.project.clone()),
            run_detail(&run, &attempts, changelog, diff, moved, repo.as_deref()),
        ),
        View::Agents {
            state,
            jobs,
            runs,
            now,
        } => panes(
            Tab::Activity,
            None,
            vec![agents_pane(&state, &runs, now), open_jobs_pane(&jobs)],
        ),
        View::Activity {
            state,
            jobs,
            instances,
            runs,
            finished,
            now: at,
        } => {
            let running: Vec<Run> = runs
                .iter()
                .filter(|r| r.state == RunState::Running)
                .cloned()
                .collect();
            panes(
                Tab::Activity,
                None,
                vec![
                    activity_pane(&state, &running, &jobs, at, true),
                    instances_pane(&instances, at),
                    finished_pane(&runs, finished, at),
                ],
            )
        }
        View::Jobs(jobs) => panes(Tab::Activity, None, vec![jobs_pane(&jobs, now)]),
        View::JobLog {
            id,
            text: log,
            awaiting,
        } => detail(
            None,
            None,
            Detail {
                title: format!("Job #{id}"),
                pick: Pick::Job { id, awaiting },
                badge: None,
                notice: None,
                fields: Vec::new(),
                sections: vec![(
                    "Output".into(),
                    Section::Log,
                    log.unwrap_or_else(|| "No output yet.".into()),
                )],
            },
        ),
        View::Notes(notes) => {
            let items = notes
                .into_iter()
                .map(|n| Item {
                    cells: vec![
                        muted(format!("#{}", n.id)),
                        muted(dates::format(dates::day(n.updated_at))),
                        plain(n.text.clone()),
                    ],
                    pick: Pick::Note {
                        id: n.id,
                        text: n.text,
                    },
                })
                .collect::<Vec<_>>();
            panes(
                Tab::Notes,
                None,
                vec![Pane {
                    title: "Notes".into(),
                    columns: vec![
                        col("note", Kind::Text),
                        col("updated", Kind::Text),
                        col("text", Kind::Wide),
                    ],
                    items,
                    empty: "No notes. Press n to add one.",
                }],
            )
        }
        View::Workflows {
            revisions,
            instances,
        } => panes(
            Tab::Library,
            None,
            vec![instances_pane(&instances, now), revisions_pane(&revisions)],
        ),
        View::Library {
            revisions,
            campaigns,
        } => panes(
            Tab::Library,
            None,
            vec![revisions_pane(&revisions), campaigns_pane(&campaigns)],
        ),
        View::Campaigns(all) => panes(Tab::Library, None, vec![campaigns_pane(&all)]),
        View::Project {
            name,
            path,
            scanned_at,
            tags,
            verify,
            status,
            placed,
            today,
            runs,
            jobs,
            state,
            todo,
            repo,
        } => {
            let mut summary: Vec<(String, Cell)> = Vec::new();
            match &status {
                None => summary.push((
                    "state".into(),
                    cell("not found by the last scan", Tone::Warn),
                )),
                Some((states, c)) => {
                    let toned = status_cells(c);
                    for (i, label) in report::STATUS_HEADINGS.iter().enumerate().skip(1) {
                        let mut v = toned[i].clone();
                        // Every state by name; each one's reason follows.
                        if i == 2 {
                            v.text = states
                                .iter()
                                .map(|(st, _)| st.name())
                                .collect::<Vec<_>>()
                                .join(", ");
                        }
                        if !v.text.is_empty() && v.text != "-" {
                            summary.push((label.to_string(), v));
                        }
                    }
                    for (st, why) in states.iter().filter(|(_, why)| !why.is_empty()) {
                        summary.push((
                            st.name().to_string(),
                            cell(why.clone(), state_tone(st.name())),
                        ));
                    }
                }
            }
            summary.push((
                "scanned".into(),
                match scanned_at {
                    None => cell("never", Tone::Warn),
                    Some(t) => cell(
                        report::ago(now - t),
                        // A day's drift is enough for CI or a branch to move.
                        match now - t > 86_400 {
                            true => Tone::Warn,
                            false => Tone::Muted,
                        },
                    ),
                },
            ));
            summary.push((
                "verify".into(),
                match verify {
                    Some(v) => plain(v),
                    None => muted("detected at dispatch"),
                },
            ));
            if !tags.is_empty() {
                summary.push(("tags".into(), plain(tags.join(", "))));
            }
            let tasks = placed
                .iter()
                .map(|p| {
                    let when = report::when(p, today);
                    let priority = p.task.priority.name();
                    Item {
                        cells: vec![
                            plain(match p.task.line {
                                Some(line) => format!("{}:{line}", p.task.project),
                                None => p.task.project.clone(),
                            }),
                            cell(priority, priority_tone(priority)),
                            cell(when.clone(), when_tone(&when)),
                            // `agent`: `--auto` takes it. `named`: an agent
                            // takes it when dispatched by name. `you`: no
                            // agent may.
                            match (p.task.eligible, p.task.manual, p.task.key.is_some()) {
                                (true, _, _) => cell("agent", Tone::Accent),
                                (false, false, true) => muted("named"),
                                _ => muted("you"),
                            },
                            plain(p.task.text.clone()),
                            issue_cell(p.task.gh, repo.as_ref()),
                        ],
                        pick: task_pick(p),
                    }
                })
                .collect();
            Screen {
                tab: Some(Tab::Project),
                header: Some(format!("{name}  {}", path.display())),
                summary,
                body: Body::Panes(vec![
                    Pane {
                        title: "Open tasks".into(),
                        columns: vec![
                            col("task", Kind::Text),
                            col("priority", Kind::Badge),
                            col("when", Kind::Text),
                            col("for", Kind::Badge),
                            col("title", Kind::Wide),
                            col("issue", Kind::Text),
                        ],
                        items: tasks,
                        empty: "No open task in its TODO.md.",
                    },
                    activity_pane(&state, &runs, &jobs, now, false),
                    todo_pane(todo),
                ]),
            }
        }
        View::Settings {
            settings,
            roots,
            agents,
            default_agent,
            presets,
            default_preset,
            routes,
        } => panes(
            Tab::Settings,
            None,
            vec![
                Pane {
                    title: "Settings".into(),
                    columns: vec![
                        col("key", Kind::Text),
                        col("value", Kind::Wide),
                        col("source", Kind::Badge),
                    ],
                    items: settings
                        .into_iter()
                        .map(|(key, value, source)| Item {
                            cells: vec![
                                plain(key.clone()),
                                plain(value.clone()),
                                cell(
                                    source,
                                    match source {
                                        "set" => Tone::Accent,
                                        "unknown" => Tone::Warn,
                                        _ => Tone::Muted,
                                    },
                                ),
                            ],
                            pick: Pick::Setting { key, value },
                        })
                        .collect(),
                    empty: "No settings.",
                },
                Pane {
                    title: "Roots".into(),
                    columns: vec![col("directory", Kind::Wide)],
                    items: roots
                        .into_iter()
                        .map(|r| {
                            let path = r.to_string_lossy().into_owned();
                            Item {
                                cells: vec![plain(path.clone())],
                                pick: Pick::Root { path },
                            }
                        })
                        .collect(),
                    empty: "No root. `r` adds a directory whose git repositories are projects.",
                },
                Pane {
                    title: "Agents".into(),
                    columns: vec![
                        col("agent", Kind::Text),
                        col("default", Kind::Badge),
                        col("command", Kind::Text),
                        col("parse", Kind::Text),
                        col("flags", Kind::Wide),
                    ],
                    items: agents
                        .iter()
                        .map(|w| {
                            let flags: Vec<&str> = [
                                (w.reports_cost, "cost"),
                                (w.enforces_budget, "budget"),
                                (w.sandbox, "sandbox"),
                                (w.allow.is_some(), "allowlist"),
                                (w.resumes, "resumes"),
                            ]
                            .into_iter()
                            .filter_map(|(on, name)| on.then_some(name))
                            .collect();
                            Item {
                                cells: vec![
                                    plain(w.name.clone()),
                                    match w.name == default_agent {
                                        true => cell("default", Tone::Accent),
                                        false => plain(""),
                                    },
                                    muted(w.command.clone()),
                                    muted(w.parse.name()),
                                    muted(flags.join(", ")),
                                ],
                                pick: Pick::Agent {
                                    name: w.name.clone(),
                                },
                            }
                        })
                        .collect(),
                    empty: "No agent. `pma agent set <name> command <program>` adds one.",
                },
                Pane {
                    title: "Presets".into(),
                    columns: vec![
                        col("preset", Kind::Text),
                        col("default", Kind::Badge),
                        col("agent", Kind::Text),
                        col("model", Kind::Text),
                        col("args", Kind::Wide),
                    ],
                    items: presets
                        .iter()
                        .map(|p| Item {
                            cells: vec![
                                plain(p.name.clone()),
                                match default_preset.as_deref() == Some(p.name.as_str()) {
                                    true => cell("default", Tone::Accent),
                                    false => plain(""),
                                },
                                match agents.iter().any(|w| w.name == p.agent) {
                                    true => plain(p.agent.clone()),
                                    false => {
                                        cell(format!("{} (no such agent)", p.agent), Tone::Bad)
                                    }
                                },
                                muted(p.model.clone().unwrap_or_else(|| "its own".into())),
                                muted(p.args.join(" ")),
                            ],
                            pick: Pick::Preset {
                                name: p.name.clone(),
                                agent: p.agent.clone(),
                                model: p.model.clone(),
                                args: p.args.clone(),
                            },
                        })
                        .collect(),
                    empty: "No preset. `p` names a worker, a model and its arguments.",
                },
                Pane {
                    title: "Route revisions".into(),
                    columns: vec![
                        col("revision", Kind::Text),
                        col("status", Kind::Badge),
                        col("routes", Kind::Number),
                        col("proposed by", Kind::Text),
                        col("age", Kind::Text),
                    ],
                    items: routes
                        .iter()
                        .map(|r| {
                            let (state, tone) = match (r.activated_at, r.shadow) {
                                (None, _) => ("draft".to_string(), Tone::Muted),
                                (Some(_), true) => ("shadow".to_string(), Tone::Warn),
                                (Some(_), false) => ("active".to_string(), Tone::Good),
                            };
                            Item {
                                cells: vec![
                                    muted(r.revision.to_string()),
                                    cell(state, tone),
                                    plain(
                                        r.policy().map(|p| p.routes.len()).unwrap_or(0).to_string(),
                                    ),
                                    muted(r.proposed_by.clone()),
                                    muted(report::ago(now - r.created_at)),
                                ],
                                pick: Pick::Route {
                                    revision: r.revision,
                                    active: r.activated_at.is_some() && !r.shadow,
                                },
                            }
                        })
                        .collect(),
                    empty: "No routing policy: dispatch uses the default agent and each task's class. `f` proposes one from a file.",
                },
            ],
        ),
        View::Scan { scan: sc, changes } => {
            let mut fields = vec![
                ("when".into(), report::ago(now - sc.at)),
                (
                    "kind".into(),
                    if sc.full { "full" } else { "partial" }.into(),
                ),
                ("projects".into(), sc.projects.to_string()),
                ("CI failing".into(), sc.failing.to_string()),
            ];
            for (f, n) in scan::Fault::ALL.iter().zip(sc.faults) {
                if n > 0 {
                    fields.push((f.name().to_string(), n.to_string()));
                }
            }
            fields.push(("open items".into(), sc.open.to_string()));
            fields.push((
                "changed".into(),
                format!(
                    "{} opened, {} closed, {} other",
                    sc.opened, sc.closed, sc.other
                ),
            ));
            detail(
                None,
                None,
                Detail {
                    title: format!("Scan #{}", sc.id),
                    pick: Pick::Scan { id: sc.id },
                    badge: None,
                    notice: None,
                    fields,
                    sections: vec![(
                        "Changes since the scan before".into(),
                        Section::Log,
                        match changes.is_empty() {
                            true => "No change.".into(),
                            false => report::changes(&changes),
                        },
                    )],
                },
            )
        }
        View::RouteDoc { revision: r } => {
            let (state, tone) = match (r.activated_at, r.shadow) {
                (None, _) => ("draft", Tone::Muted),
                (Some(_), true) => ("shadow", Tone::Warn),
                (Some(_), false) => ("active", Tone::Good),
            };
            let mut fields = vec![
                ("proposed by".into(), r.proposed_by.clone()),
                ("proposed".into(), report::ago(now - r.created_at)),
            ];
            if let Some(by) = &r.activated_by {
                fields.push(("activated by".into(), by.clone()));
            }
            detail(
                None,
                None,
                Detail {
                    title: format!("Route revision {}", r.revision),
                    pick: Pick::Route {
                        revision: r.revision,
                        active: r.activated_at.is_some() && !r.shadow,
                    },
                    badge: Some(cell(state, tone)),
                    notice: None,
                    fields,
                    sections: vec![("Policy".into(), Section::Log, r.document.clone())],
                },
            )
        }
        View::Task {
            placed,
            today,
            item,
            signal,
            repo,
            held_by,
        } => {
            let t = &placed.task;
            let priority = t.priority.name();
            let mut fields: Vec<(String, String)> = vec![("project".into(), t.project.clone())];
            match t.line {
                Some(line) => fields.push(("where".into(), format!("TODO.md:{line}"))),
                None => fields.push((
                    "where".into(),
                    format!(
                        "the `{}` signal of the last scan",
                        t.key.as_deref().unwrap_or("-")
                    ),
                )),
            }
            if let Some(g) = &t.group {
                fields.push(("group".into(), g.clone()));
            }
            if let Some(i) = &item {
                if !i.tags.is_empty() {
                    fields.push((
                        "tags".into(),
                        i.tags
                            .iter()
                            .map(|g| format!("#{g}"))
                            .collect::<Vec<_>>()
                            .join(" "),
                    ));
                }
                if let Some(due) = &i.due {
                    fields.push(("due".into(), due.clone()));
                }
                if let Some(gh) = i.gh {
                    fields.push((
                        "issue".into(),
                        match &repo {
                            Some(r) => format!("{r}/issues/{gh}"),
                            None => format!("gh:{gh}"),
                        },
                    ));
                }
                if let Some(id) = &i.id {
                    fields.push(("id".into(), id.clone()));
                }
            }
            fields.push(("when".into(), report::when(&placed, today)));
            fields.push((
                "placed".into(),
                format!(
                    "{:?} {}, importance {:.1}, tier {}",
                    placed.quadrant,
                    placed.quadrant.title(),
                    placed.importance,
                    t.tier
                ),
            ));
            fields.push((
                "for".into(),
                match (t.eligible, t.manual, t.key.is_some()) {
                    (true, _, _) => "an agent: `--auto` takes it".into(),
                    (false, false, true) => "an agent, when dispatched by name".into(),
                    _ => "you: no agent may take it".into(),
                },
            ));
            fields.push(("open".into(), format!("{}d", t.age_days)));
            for r in &held_by {
                fields.push(("held by".into(), format!("#{} {}", r.id, r.state.name())));
            }
            // The title is one line in a TUI dialog; the text in full is the
            // first thing to read.
            let mut sections = vec![("Task".into(), Section::Prose, t.text.clone())];
            if let Some(i) = &item
                && !i.description.is_empty()
            {
                sections.push((
                    "Description".into(),
                    Section::Prose,
                    i.description
                        .iter()
                        .map(|l| l.trim_start().to_string())
                        .collect::<Vec<_>>()
                        .join("\n"),
                ));
            }
            if !signal.is_empty() {
                sections.push(("Signal".into(), Section::Log, signal.join("\n")));
            }
            detail(
                None,
                Some(t.project.clone()),
                Detail {
                    title: report::truncate(&t.text, 72),
                    pick: task_pick(&placed),
                    badge: Some(cell(priority, priority_tone(priority))),
                    notice: None,
                    fields,
                    sections,
                },
            )
        }
        View::Campaign { campaign, runs } => {
            let fields = campaign
                .members
                .iter()
                .map(|(project, run)| {
                    let state = match run.and_then(|id| runs.iter().find(|r| r.id == id)) {
                        None => "not dispatched".to_string(),
                        Some(r) => format!("#{} {}", r.id, r.state.name()),
                    };
                    (project.clone(), state)
                })
                .collect();
            detail(
                None,
                None,
                Detail {
                    title: campaign.name.clone(),
                    pick: Pick::Campaign {
                        name: campaign.name.clone(),
                    },
                    badge: None,
                    notice: None,
                    fields,
                    sections: vec![("Task".into(), Section::Prose, campaign.text.clone())],
                },
            )
        }
    }
}

/// Text a prompt starts with: a note being edited starts as its text.
pub fn initial_input(act: Act, pick: &Pick) -> String {
    match (act, pick) {
        (Act::EditNote, Pick::Note { text, .. }) => text.clone(),
        (Act::EditSetting, Pick::Setting { value, .. }) => value.clone(),
        (
            Act::EditPreset,
            Pick::Preset {
                agent, model, args, ..
            },
        ) => {
            let mut line = agent.clone();
            if let Some(m) = model {
                line.push(':');
                line.push_str(m);
            }
            for a in args {
                line.push(' ');
                line.push_str(a);
            }
            line
        }
        _ => String::new(),
    }
}

/// The question a confirmation asks.
pub fn question(act: Act, pick: &Pick) -> String {
    let what = match pick {
        Pick::Task { target, .. } => target.clone(),
        Pick::Tasks { targets, .. } => {
            format!("{} tasks ({})", targets.len(), targets.join(", "))
        }
        Pick::Projects { names } => names.join(", "),
        Pick::Manual { project } => project.clone(),
        Pick::Run { id, .. } | Pick::Job { id, .. } | Pick::Note { id, .. } => format!("#{id}"),
        Pick::Instance { id, .. } => format!("instance {id}"),
        Pick::Campaign { name } => name.clone(),
        Pick::Setting { key, .. } => key.clone(),
        Pick::Root { path } => path.clone(),
        Pick::Agent { name } | Pick::Preset { name, .. } => name.clone(),
        Pick::Route { revision, .. } => format!("route revision {revision}"),
        Pick::Scan { id } => format!("scan {id}"),
        Pick::Nothing => String::new(),
    };
    // Publishing a ready run approves it first (`api::act`); the question
    // says so.
    let ready = matches!(pick, Pick::Run { state, .. } if state == "ready");
    let verb = match act {
        Act::Dispatch => "dispatch",
        Act::DispatchPush => "dispatch, and push when clean,",
        Act::RejectRun => "reject",
        Act::RetryRun => "retry the task of",
        Act::Stage => "stage in its clone",
        Act::Commit if ready => "approve and commit in its clone",
        Act::Commit => "commit in its clone",
        Act::Pr if ready => "approve and open a pull request for",
        Act::Pr => "open a pull request for",
        Act::Push if ready => "approve and push",
        Act::Push => "push",
        Act::Merge => "merge the pull request of",
        Act::FixConflicts => "have an agent fix the conflicts of",
        Act::StopJob | Act::StopWorkflow => "stop",
        Act::RemoveNote => "remove note",
        Act::RunCampaign => "run campaign",
        Act::RemoveCampaign => "remove campaign",
        Act::ApproveRuns => "approve every clean run",
        Act::PushApproved => "push every approved run",
        Act::Sync => "sync critical items to GitHub issues",
        Act::ResetSetting => "reset to its default:",
        Act::RemoveRoot => "remove root",
        Act::RemoveAgent => "remove agent",
        Act::RemovePreset => "remove preset",
        Act::ActivateRoute => "put in effect:",
        Act::Prune => "remove finished items from TODO.md in the project in view",
        _ => "do this to",
    };
    format!("{verb} {what}?").replace(" ?", "?")
}

/// The action `act` takes on `pick`. `pane` is the list the pick is in, for
/// an act over many rows; `input` is what a prompt read.
pub fn action(act: Act, pick: &Pick, pane: &[Item], input: &str) -> Result<Action, String> {
    action_in(act, pick, pane, input, "")
}

/// `action`, on a view of `project`: an act on the project itself, such as
/// rescanning it, needs no row.
pub fn action_in(
    act: Act,
    pick: &Pick,
    pane: &[Item],
    input: &str,
    project: &str,
) -> Result<Action, String> {
    let input = input.trim();
    let worker = WorkerChoice::default();
    let need = |what: &str| Err(format!("select {what} first"));
    Ok(match (act, pick) {
        (Act::Scan, _) => Action::Scan {
            of: Selection::default(),
            offline: false,
            deps: false,
        },
        (Act::ScanProject, _) => {
            if project.is_empty() {
                return Err("open a project first".into());
            }
            let of = Selection {
                projects: vec![project.to_string()],
                tags: Vec::new(),
            };
            match input {
                "" | "scan" => Action::Scan {
                    of,
                    offline: false,
                    deps: false,
                },
                "deps" => Action::Scan {
                    of,
                    offline: false,
                    deps: true,
                },
                "offline" => Action::Scan {
                    of,
                    offline: true,
                    deps: false,
                },
                "refresh-deps" => Action::RefreshDeps {
                    project: project.to_string(),
                },
                other => return Err(format!("unknown choice `{other}`")),
            }
        }
        (Act::Sync, _) => Action::Sync {
            of: Selection::default(),
            apply: true,
        },
        (Act::AddNote, _) => Action::AddNote { text: input.into() },
        (Act::SettlePrs, _) => Action::SettlePrs,
        (Act::Run, pick) => {
            let projects = match (pick, pick.project()) {
                (Pick::Projects { names }, _) => names.clone(),
                (_, Some(p)) => vec![p.to_string()],
                (_, None) if !project.is_empty() => vec![project.to_string()],
                (_, None) => return Err("open a project, or select a row that names one".into()),
            };
            let mut words = input.split_whitespace();
            let name = words
                .next()
                .ok_or("type what to run, such as `review` or `fix-ci`; `pma run` lists them")?
                .to_string();
            let (mut review, mut worktree, mut push, mut agent, mut model) =
                (false, false, false, None, None);
            for w in words {
                match w {
                    "--review" => review = true,
                    "--worktree" => worktree = true,
                    "--push" => push = true,
                    _ if w.starts_with("--worker=") || w.starts_with("-w=") => {
                        let v = w.split_once('=').map_or("", |(_, v)| v);
                        let (a, m) = v.split_once(':').map_or((v, None), |(a, m)| (a, Some(m)));
                        agent = Some(a.to_string());
                        model = m.map(str::to_string);
                    }
                    _ => return Err(format!("unknown flag `{w}`")),
                }
            }
            Action::Run {
                name,
                projects,
                review,
                worktree,
                push,
                worker: WorkerChoice {
                    agent,
                    model,
                    preset: None,
                },
            }
        }
        (Act::StartWorkflow, _) => {
            let mut words = input.split_whitespace();
            let name = words
                .next()
                .ok_or("type a workflow name, then its target")?
                .to_string();
            let (mut projects, mut set) = (Vec::new(), Vec::new());
            let (mut local, mut unattended, mut model) = (false, false, None);
            for w in words {
                match w {
                    "--local" => local = true,
                    "--unattended" => unattended = true,
                    _ if w.starts_with("--model=") => model = Some(w[8..].to_string()),
                    _ if w.starts_with("--set=") => set.push(w[6..].to_string()),
                    _ if w.starts_with('-') => return Err(format!("unknown flag `{w}`")),
                    _ => projects.push(w.to_string()),
                }
            }
            Action::WorkflowRun {
                name,
                projects,
                tag: None,
                instance: None,
                set,
                approve: None,
                unattended,
                local,
                cap: Vec::new(),
                worker: WorkerChoice {
                    model,
                    ..WorkerChoice::default()
                },
            }
        }
        (Act::Configure, pick) => {
            let project = match pick.project() {
                Some(p) => p.to_string(),
                None if !project.is_empty() => project.to_string(),
                None => return Err("open a project first".into()),
            };
            let (what, value) = input
                .split_once(char::is_whitespace)
                .map(|(w, v)| (w, v.trim()))
                .unwrap_or((input, ""));
            match what {
                "tier" => Action::SetTier {
                    tier: match value {
                        "none" => None,
                        t => Some(
                            t.parse::<u8>()
                                .ok()
                                .filter(|t| (1..=5).contains(t))
                                .ok_or("tier must be 1 to 5, or none")?,
                        ),
                    },
                    project,
                },
                "tag" => match value.split_at_checked(1) {
                    Some(("+", tag)) if !tag.is_empty() => Action::Tag {
                        project,
                        tag: tag.to_string(),
                        add: true,
                    },
                    Some(("-", tag)) if !tag.is_empty() => Action::Tag {
                        project,
                        tag: tag.to_string(),
                        add: false,
                    },
                    _ => return Err("`tag +name` adds a tag, `tag -name` removes one".into()),
                },
                "verify" | "sandbox" => Action::SetConfig {
                    key: format!("projects.{project}.{what}"),
                    value: value.to_string(),
                },
                _ => {
                    return Err(
                        "type `tier 1-5|none`, `tag +name|-name`, `verify <command>` or \
                         `sandbox on|off`"
                            .into(),
                    );
                }
            }
        }
        (Act::Prune, pick) => {
            let project = match pick.project() {
                Some(p) => p.to_string(),
                None if !project.is_empty() => project.to_string(),
                None => return Err("open a project first".into()),
            };
            match input {
                "" | "prune" => Action::Prune { project },
                "fix" => Action::FixTodo { project },
                other => return Err(format!("unknown choice `{other}`")),
            }
        }
        (Act::EditSetting, Pick::Setting { key, .. }) => {
            if input.is_empty() {
                return Err("type a value; `x` resets the setting".into());
            }
            Action::SetConfig {
                key: key.clone(),
                value: input.to_string(),
            }
        }
        (Act::ResetSetting, Pick::Setting { key, .. }) => Action::ResetConfig { key: key.clone() },
        (Act::EditSetting | Act::ResetSetting, _) => return need("a setting"),
        (Act::AddRoot, _) => {
            if input.is_empty() {
                return Err("type a directory".into());
            }
            Action::AddRoot {
                path: input.to_string(),
            }
        }
        (Act::RemoveRoot, Pick::Root { path }) => Action::RemoveRoot { path: path.clone() },
        (Act::RemoveRoot, _) => return need("a root"),
        (Act::EditAgent, Pick::Agent { name }) => {
            let (field, value) = input
                .split_once(char::is_whitespace)
                .ok_or("type a field and its value, such as `sandbox true`")?;
            Action::SetAgentField {
                name: name.clone(),
                field: field.to_string(),
                value: value.trim().to_string(),
            }
        }
        (Act::RemoveAgent, Pick::Agent { name }) => Action::RemoveAgent { name: name.clone() },
        (Act::UseAgent, Pick::Agent { name }) => Action::SetConfig {
            key: "agent".into(),
            value: name.clone(),
        },
        (Act::EditAgent | Act::RemoveAgent | Act::UseAgent, _) => return need("an agent"),
        (Act::EditPreset, Pick::Preset { name, .. }) => {
            let (agent, model, args) = preset_words(input)?;
            Action::SetPreset {
                name: name.clone(),
                agent,
                model,
                args,
            }
        }
        (Act::AddPreset, _) => {
            let (name, rest) = input
                .split_once(char::is_whitespace)
                .ok_or("type a name, then the agent: `fast claude:haiku`")?;
            let (agent, model, args) = preset_words(rest.trim())?;
            Action::SetPreset {
                name: name.to_string(),
                agent,
                model,
                args,
            }
        }
        (Act::RemovePreset, Pick::Preset { name, .. }) => {
            Action::RemovePreset { name: name.clone() }
        }
        (Act::UsePreset, Pick::Preset { name, .. }) => Action::SetConfig {
            key: "preset".into(),
            value: name.clone(),
        },
        (Act::EditPreset | Act::RemovePreset | Act::UsePreset, _) => return need("a preset"),
        (Act::ActivateRoute, Pick::Route { revision, .. }) => Action::ActivateRoute {
            revision: *revision,
        },
        (Act::ActivateRoute, _) => return need("a draft route revision"),
        (Act::ProposeRoute, _) => {
            if input.is_empty() {
                return Err("type the policy file's path".into());
            }
            Action::ProposeRoute {
                path: input.to_string(),
            }
        }
        (Act::AddCampaign, _) => {
            let mut words = input.split_whitespace();
            let name = words
                .next()
                .ok_or("type a name, the projects, then the task")?;
            let projects: Vec<String> = words
                .next()
                .ok_or("name the projects, separated by commas")?
                .split(',')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect();
            let (mut class, mut text) = (None, Vec::new());
            for w in words {
                match w.strip_prefix("--class=") {
                    Some(c) => class = Some(c.to_string()),
                    None => text.push(w),
                }
            }
            if projects.is_empty() || text.is_empty() {
                return Err("a campaign needs projects and a task text".into());
            }
            Action::AddCampaign {
                name: name.to_string(),
                text: text.join(" "),
                projects,
                class,
            }
        }
        (Act::RemoveCampaign, Pick::Campaign { name }) => {
            Action::RemoveCampaign { name: name.clone() }
        }
        (Act::RemoveCampaign, _) => return need("a campaign"),
        (Act::ApproveRuns, _) => {
            let ids: Vec<i64> = pane
                .iter()
                .filter_map(|i| match i.pick {
                    Pick::Run {
                        id, clean: true, ..
                    } => Some(id),
                    _ => None,
                })
                .collect();
            if ids.is_empty() {
                return Err("no clean run to approve".into());
            }
            Action::ApproveRuns { ids }
        }
        (Act::PushApproved, _) => {
            let ids: Vec<i64> = pane
                .iter()
                .filter_map(|i| match &i.pick {
                    // Not a ready run: no one has read it yet.
                    Pick::Run { id, state, .. } if state != "ready" && fits(Act::Push, &i.pick) => {
                        Some(*id)
                    }
                    _ => None,
                })
                .collect();
            if ids.is_empty() {
                return Err("no approved run to push".into());
            }
            Action::Publish {
                ids,
                all_approved: false,
                target: publish::Target::Push,
                subject: None,
                drop: None,
            }
        }
        (Act::Dispatch | Act::DispatchPush, Pick::Task { target, retry, .. }) => Action::Dispatch {
            targets: vec![target.clone()],
            tags: Vec::new(),
            auto: false,
            count: None,
            retry: *retry,
            worker,
            prompt: None,
            push: act == Act::DispatchPush,
        },
        (Act::Dispatch | Act::DispatchPush, Pick::Tasks { targets, retry }) => Action::Dispatch {
            targets: targets.clone(),
            tags: Vec::new(),
            auto: false,
            count: None,
            retry: *retry,
            worker,
            prompt: None,
            push: act == Act::DispatchPush,
        },
        (Act::Prompt, _) => {
            if project.is_empty() {
                return Err("open a project first".into());
            }
            if input.is_empty() {
                return Err("a prompt needs text".into());
            }
            Action::Dispatch {
                targets: vec![project.to_string()],
                tags: Vec::new(),
                auto: false,
                count: None,
                retry: false,
                worker,
                prompt: Some(input.to_string()),
                push: false,
            }
        }
        (Act::Dispatch | Act::DispatchPush, Pick::Manual { .. }) => {
            return Err("not a task for agents".into());
        }
        (Act::Verify, Pick::Task { project, .. } | Pick::Manual { project }) => Action::Verify {
            of: Selection {
                projects: vec![project.clone()],
                tags: Vec::new(),
            },
        },
        (Act::Dispatch | Act::DispatchPush | Act::Verify, _) => return need("a task"),
        (Act::ApproveRun, Pick::Run { id, state, .. }) => Action::ApproveRun {
            id: *id,
            seen: Some(state.clone()),
            drop: unchecked(input)?,
        },
        (Act::RejectRun, Pick::Run { id, state, .. }) => Action::RejectRun {
            id: *id,
            keep_git_changes: false,
            infra: false,
            seen: Some(state.clone()),
        },
        (Act::ReworkRun, Pick::Run { id, .. }) => {
            if input.is_empty() {
                return Err("rework needs feedback".into());
            }
            Action::ReworkRun {
                id: *id,
                feedback: input.into(),
            }
        }
        (Act::RetryRun, Pick::Run { id, state, .. }) if state == "failed" => {
            Action::RetryRun { id: *id }
        }
        (Act::RetryRun, Pick::Run { id, .. }) => {
            return Err(format!("run #{id} has not failed; reject or rework it"));
        }
        (Act::AddReviewTime, Pick::Run { id, .. }) => Action::AddReviewTime {
            id: *id,
            minutes: input
                .parse()
                .map_err(|_| "type the minutes as a whole number".to_string())?,
        },
        (Act::Stage | Act::Commit | Act::Pr | Act::Push, Pick::Run { id, state, .. }) => {
            Action::Publish {
                ids: vec![*id],
                all_approved: false,
                target: match act {
                    Act::Stage => publish::Target::Stage,
                    Act::Commit => publish::Target::Commit,
                    Act::Pr => publish::Target::Pr,
                    _ => publish::Target::Push,
                },
                // Only commit prompts; a blank subject keeps the task text.
                subject: Some(input)
                    .filter(|s| act == Act::Commit && !s.is_empty())
                    .map(str::to_string),
                // `u` on a ready run asked which items are done.
                drop: (act == Act::Push && state == "ready")
                    .then(|| unchecked(input))
                    .transpose()?,
            }
        }
        (Act::Merge, Pick::Run { id, .. }) => Action::MergePr { id: *id },
        (Act::FixConflicts, Pick::Run { id, .. }) => Action::FixConflicts { id: *id },
        (
            Act::ApproveRun
            | Act::RejectRun
            | Act::ReworkRun
            | Act::RetryRun
            | Act::AddReviewTime
            | Act::Stage
            | Act::Commit
            | Act::Pr
            | Act::Push
            | Act::Merge
            | Act::FixConflicts,
            _,
        ) => return need("a run"),
        (Act::ApproveJob, Pick::Job { id, awaiting: true }) => Action::ApproveJob { id: *id },
        (Act::ApproveJob, Pick::Job { id, .. }) => {
            return Err(format!("job #{id} is not waiting for approval"));
        }
        (Act::StopJob, Pick::Job { id, .. }) => Action::StopJob { id: *id },
        (Act::ApproveJob | Act::StopJob, _) => return need("a job"),
        (Act::StopWorkflow, Pick::Instance { id, .. }) => Action::StopWorkflow { instance: *id },
        (
            Act::ResumeWorkflow,
            Pick::Instance {
                id,
                workflow,
                open: true,
            },
        ) => Action::WorkflowRun {
            name: workflow.clone(),
            projects: Vec::new(),
            tag: None,
            instance: Some(*id),
            set: Vec::new(),
            approve: None,
            unattended: false,
            local: false,
            cap: Vec::new(),
            worker,
        },
        (Act::ResumeWorkflow, Pick::Instance { id, .. }) => {
            return Err(format!("instance {id} is finished"));
        }
        (Act::StopWorkflow | Act::ResumeWorkflow, _) => return need("an instance"),
        (Act::RunCampaign, Pick::Campaign { name }) => Action::RunCampaign {
            name: name.clone(),
            count: None,
        },
        (Act::RunCampaign, _) => return need("a campaign"),
        (Act::EditNote, Pick::Note { id, .. }) => Action::EditNote {
            id: *id,
            text: input.into(),
        },
        (Act::RemoveNote, Pick::Note { id, .. }) => Action::RemoveNote { id: *id },
        (Act::EditNote | Act::RemoveNote, _) => return need("a note"),
    })
}

/// `agent[:model] [args...]`, as `pma preset set` reads it after the name.
fn preset_words(input: &str) -> Result<(String, Option<String>, Vec<String>), String> {
    let mut words = input.split_whitespace();
    let first = words
        .next()
        .ok_or("name the agent, with its model after a colon: `claude:haiku`")?;
    let (agent, model) = match first.split_once(':') {
        Some((a, m)) => (a.to_string(), Some(m.to_string()).filter(|m| !m.is_empty())),
        None => (first.to_string(), None),
    };
    Ok((agent, model, words.map(String::from).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: i64, clean: bool) -> Item {
        Item {
            cells: vec![plain(format!("#{id}"))],
            pick: Pick::Run {
                fixing: false,
                id,
                clean,
                state: "ready".into(),
                project: "p".into(),
            },
        }
    }

    /// `D` is `d` with the push; `d` stays without it.
    #[test]
    fn dispatch_push_says_so() {
        let task = Pick::Task {
            project: "p".into(),
            target: "p:1".into(),
            retry: false,
        };
        let push = |act| match action(act, &task, &[], "") {
            Ok(Action::Dispatch { push, .. }) => push,
            other => panic!("{other:?}"),
        };
        assert!(push(Act::DispatchPush) && !push(Act::Dispatch));
        assert!(fits(Act::DispatchPush, &task));
    }

    /// `U` pushes what has been read: never a ready run, nor one fixing a
    /// pull request's conflicts.
    #[test]
    fn push_approved_takes_only_what_was_read() {
        let row = |id, state: &str, fixing| Item {
            cells: vec![],
            pick: Pick::Run {
                fixing,
                id,
                clean: true,
                state: state.into(),
                project: "p".into(),
            },
        };
        let pane = [
            row(1, "ready", false),
            row(2, "approved", false),
            row(3, "staged", false),
            row(4, "committed", false),
            row(5, "approved", true),
            row(6, "failed", false),
        ];
        let Ok(Action::Publish { ids, target, .. }) =
            action(Act::PushApproved, &Pick::Nothing, &pane, "")
        else {
            panic!("a publish");
        };
        assert_eq!(ids, [2, 3, 4]);
        assert_eq!(target, publish::Target::Push);
        assert!(action(Act::PushApproved, &Pick::Nothing, &pane[..1], "").is_err());
    }

    /// `u` carries what a ready run's approver left unchecked, and asks
    /// nothing of a run already approved.
    #[test]
    fn push_carries_the_items_left_open() {
        let drop = |state: &str, input| {
            let pick = Pick::Run {
                fixing: false,
                id: 1,
                clean: false,
                state: state.into(),
                project: "p".into(),
            };
            match action(Act::Push, &pick, &[], input) {
                Ok(Action::Publish { drop, .. }) => drop,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(drop("ready", "2,3"), Some(vec![2, 3]));
        assert_eq!(drop("ready", ""), Some(vec![]));
        assert_eq!(drop("approved", ""), None);
    }

    /// The act that moves a run on follows its state.
    #[test]
    fn a_run_names_the_act_that_moves_it_on() {
        let run = |state: &str| Pick::Run {
            id: 1,
            clean: false,
            fixing: false,
            state: state.into(),
            project: "p".into(),
        };
        assert_eq!(next_act(&run("ready")), Some(Act::ApproveRun));
        assert_eq!(next_act(&run("failed")), Some(Act::RetryRun));
        assert_eq!(next_act(&run("staged")), Some(Act::Commit));
        assert_eq!(next_act(&run("committed")), Some(Act::Push));
        assert_eq!(next_act(&run("pushed")), None);
        assert_eq!(next_act(&Pick::Nothing), None);
        for state in ["ready", "failed", "staged", "committed", "approved"] {
            let pick = run(state);
            assert!(fits(next_act(&pick).unwrap(), &pick), "{state}");
        }
    }

    /// A run offers only what its state takes; a finished one, review time.
    #[test]
    fn a_run_offers_what_its_state_takes() {
        let offered = |state: &str| -> Vec<Act> {
            let pick = Pick::Run {
                id: 1,
                clean: false,
                fixing: false,
                state: state.into(),
                project: "p".into(),
            };
            Act::ALL
                .into_iter()
                .filter(|a| a.on_row() && fits(*a, &pick))
                .collect()
        };
        assert_eq!(offered("pushed"), [Act::AddReviewTime]);
        assert_eq!(offered("rejected"), [Act::AddReviewTime]);
        assert_eq!(
            offered("staged"),
            [Act::RejectRun, Act::AddReviewTime, Act::Commit, Act::Push]
        );
        assert_eq!(
            offered("failed"),
            [
                Act::RejectRun,
                Act::AddReviewTime,
                Act::ReworkRun,
                Act::RetryRun
            ]
        );
        assert!(!offered("ready").contains(&Act::RetryRun));
        assert!(!offered("running").contains(&Act::RejectRun));
    }

    /// A conflict fix offers no publish but its pull request, and says so as
    /// the next step.
    #[test]
    fn a_conflict_fix_offers_only_its_pull_request() {
        let pick = |state: RunState, url: Option<&str>| {
            run_pick(&Run {
                state,
                outcome: url.map(str::to_string),
                ..Default::default()
            })
        };
        let url = Some("https://x/pull/1");
        let fix = pick(RunState::Approved, url);
        for act in [Act::Stage, Act::Commit, Act::Push] {
            assert!(!fits(act, &fix), "{act:?}");
            assert!(fits(act, &pick(RunState::Approved, None)), "{act:?}");
        }
        assert!(fits(Act::Pr, &fix) && fits(Act::RejectRun, &fix));
        assert_eq!(next_act(&fix), Some(Act::Pr));
        assert_eq!(next_act(&pick(RunState::Approved, None)), Some(Act::Push));
        assert!(!fits(Act::Push, &pick(RunState::Ready, url)));
        // Open, the run takes its merge keys, not a publish.
        let open = pick(RunState::PrOpen, url);
        assert!(fits(Act::Merge, &open) && !fits(Act::Pr, &open));
    }

    /// Activity lists finished runs newest first, those in one state when
    /// filtered; a run under way or a `done` node is not finished there.
    #[test]
    fn finished_runs_are_listed_newest_first() {
        let run = |id: i64, state: RunState, at: i64| Run {
            id,
            project: "p".into(),
            text: format!("task {id}"),
            state,
            decided_at: Some(at),
            ..Default::default()
        };
        let runs = vec![
            run(1, RunState::Rejected, 10),
            run(2, RunState::Pushed, 30),
            run(3, RunState::Rejected, 20),
            run(4, RunState::Running, 40),
            run(5, RunState::Done, 50),
        ];
        let ids = |only| -> Vec<String> {
            finished_pane(&runs, only, 100)
                .items
                .iter()
                .map(|i| i.cells[0].text.clone())
                .collect()
        };
        assert_eq!(ids(None), ["#2", "#3", "#1"]);
        assert_eq!(ids(Some(RunState::Rejected)), ["#3", "#1"]);
        assert!(ids(Some(RunState::Merged)).is_empty());
        assert_eq!(
            finished_pane(&runs, Some(RunState::Rejected), 100).title,
            "Finished runs: rejected"
        );
    }

    /// `P` dispatches what was typed over the project in view.
    #[test]
    fn a_prompt_is_a_dispatch_of_the_project_in_view() {
        assert!(action_in(Act::Prompt, &Pick::Nothing, &[], "fix it", "").is_err());
        assert!(action_in(Act::Prompt, &Pick::Nothing, &[], " ", "a").is_err());
        let Ok(Action::Dispatch {
            targets, prompt, ..
        }) = action_in(Act::Prompt, &Pick::Nothing, &[], "fix it", "a")
        else {
            panic!("a dispatch");
        };
        assert_eq!(
            (targets, prompt.as_deref()),
            (vec!["a".to_string()], Some("fix it"))
        );
    }

    /// An approval's input names the bundle items left unchecked.
    #[test]
    fn an_approval_drops_the_unchecked_items() {
        let run = Pick::Run {
            id: 3,
            clean: false,
            fixing: false,
            state: "ready".into(),
            project: "a".into(),
        };
        let drop = |input| match action(Act::ApproveRun, &run, &[], input) {
            Ok(Action::ApproveRun { drop, .. }) => Ok(drop),
            Ok(other) => panic!("{other:?}"),
            Err(e) => Err(e),
        };
        assert_eq!(drop(""), Ok(vec![]));
        assert_eq!(drop("2, 3"), Ok(vec![2, 3]));
        assert_eq!(drop("x"), Err("`x` is not an item number".into()));
    }

    /// Marked task rows are one pick, dispatched as one action; a row that
    /// is not a task is not part of it.
    #[test]
    fn marked_tasks_are_one_dispatch() {
        let task = |target: &str, retry| Pick::Task {
            project: "a".into(),
            target: target.into(),
            retry,
        };
        let run = Pick::Run {
            id: 1,
            clean: false,
            fixing: false,
            state: "ready".into(),
            project: "a".into(),
        };
        assert_eq!(marked(std::slice::from_ref(&run)), Pick::Nothing);
        let pick = marked(&[task("a:5", false), run, task("b:2", true)]);
        assert_eq!(
            pick,
            Pick::Tasks {
                targets: vec!["a:5".into(), "b:2".into()],
                retry: true,
            }
        );
        assert!(fits(Act::Dispatch, &pick) && !fits(Act::Verify, &pick));
        assert_eq!(
            question(Act::Dispatch, &pick),
            "dispatch 2 tasks (a:5, b:2)?"
        );
        let Ok(Action::Dispatch { targets, retry, .. }) = action(Act::Dispatch, &pick, &[], "")
        else {
            panic!("a dispatch");
        };
        assert_eq!((targets.len(), retry), (2, true));
    }

    #[test]
    fn keys_on_rows_build_actions() {
        let task = Pick::Task {
            project: "a".into(),
            target: "a:5".into(),
            retry: true,
        };
        assert_eq!(
            action(Act::Dispatch, &task, &[], ""),
            Ok(Action::Dispatch {
                targets: vec!["a:5".into()],
                tags: vec![],
                auto: false,
                count: None,
                retry: true,
                worker: WorkerChoice::default(),
                prompt: None,
                push: false,
            })
        );
        let pane = [run(1, true), run(2, false), run(3, true)];
        assert_eq!(
            action(Act::ApproveRuns, &Pick::Nothing, &pane, ""),
            Ok(Action::ApproveRuns { ids: vec![1, 3] })
        );
        assert!(
            action(Act::ApproveRun, &task, &[], "").is_err(),
            "not a run"
        );
        assert!(
            action(
                Act::ApproveJob,
                &Pick::Job {
                    id: 4,
                    awaiting: false
                },
                &[],
                ""
            )
            .unwrap_err()
            .contains("not waiting")
        );
        assert_eq!(
            action(
                Act::AddReviewTime,
                &Pick::Run {
                    id: 2,
                    clean: false,
                    fixing: false,
                    state: "ready".into(),
                    project: "p".into()
                },
                &[],
                " 5 "
            ),
            Ok(Action::AddReviewTime { id: 2, minutes: 5 })
        );
        assert!(
            action(
                Act::ReworkRun,
                &Pick::Run {
                    id: 2,
                    clean: false,
                    fixing: false,
                    state: "ready".into(),
                    project: "p".into()
                },
                &[],
                ""
            )
            .is_err()
        );
    }

    /// A row offers the acts that can apply to it: a task is dispatched,
    /// not approved; a run is approved, not dispatched.
    #[test]
    fn a_row_offers_only_the_acts_that_fit_it() {
        let task = Pick::Task {
            project: "a".into(),
            target: "a:5".into(),
            retry: false,
        };
        let run = Pick::Run {
            id: 1,
            clean: true,
            fixing: false,
            state: "ready".into(),
            project: "a".into(),
        };
        assert!(fits(Act::Dispatch, &task) && !fits(Act::ApproveRun, &task));
        assert!(fits(Act::ApproveRun, &run) && !fits(Act::Dispatch, &run));
        assert!(
            fits(Act::Scan, &Pick::Nothing),
            "a whole-view act fits anything"
        );
        assert!(!fits(
            Act::ApproveJob,
            &Pick::Job {
                id: 1,
                awaiting: false
            }
        ));
        for act in Act::ALL.into_iter().filter(|a| a.on_row()) {
            assert!(!fits(act, &Pick::Nothing), "{act:?} needs a row");
        }
        assert_eq!(run.project(), Some("a"));
    }

    /// Rescanning a project scans that project alone, and needs one open.
    #[test]
    fn a_project_rescan_scans_only_it() {
        assert_eq!(
            action_in(Act::ScanProject, &Pick::Nothing, &[], "", "margo"),
            Ok(Action::Scan {
                of: Selection {
                    projects: vec!["margo".into()],
                    tags: vec![],
                },
                offline: false,
                deps: false,
            })
        );
        assert!(action(Act::ScanProject, &Pick::Nothing, &[], "").is_err());
    }

    #[test]
    fn a_typed_workflow_start_reads_its_flags() {
        let Ok(Action::WorkflowRun {
            name,
            projects,
            local,
            unattended,
            worker,
            set,
            ..
        }) = action(
            Act::StartWorkflow,
            &Pick::Nothing,
            &[],
            "review-fix argdec --local --unattended --model=haiku --set=breadth=5",
        )
        else {
            panic!("a workflow run");
        };
        assert_eq!(
            (name.as_str(), projects),
            ("review-fix", vec!["argdec".to_string()])
        );
        assert!(local && unattended);
        assert_eq!(worker.model.as_deref(), Some("haiku"));
        assert_eq!(set, ["breadth=5"]);
        assert!(action(Act::StartWorkflow, &Pick::Nothing, &[], "w a --nope").is_err());
    }

    /// `run` takes its project from the row, or from the view when the row
    /// names none, and reads the flags `pma run` takes.
    #[test]
    fn a_typed_run_names_its_project_and_flags() {
        let row = Pick::Manual {
            project: "spore".into(),
        };
        let Ok(Action::Run {
            name,
            projects,
            review,
            worktree,
            push,
            worker,
        }) = action(Act::Run, &row, &[], "fix-ci --push --worker=claude:haiku")
        else {
            panic!("a run");
        };
        assert_eq!(
            (name.as_str(), projects),
            ("fix-ci", vec!["spore".to_string()])
        );
        assert!(push && !review && !worktree);
        assert_eq!(worker.agent.as_deref(), Some("claude"));
        assert_eq!(worker.model.as_deref(), Some("haiku"));
        let Ok(Action::Run { projects, .. }) =
            action_in(Act::Run, &Pick::Nothing, &[], "review", "argdec")
        else {
            panic!("a run on the project in view");
        };
        assert_eq!(projects, ["argdec"]);
        // Marked projects are one run over them all.
        let marks = [
            Pick::Manual {
                project: "a".into(),
            },
            Pick::Manual {
                project: "b".into(),
            },
        ];
        let Ok(Action::Run { projects, .. }) = action(Act::Run, &marked(&marks), &[], "review")
        else {
            panic!("a run over the marked");
        };
        assert_eq!(projects, ["a", "b"]);
        assert!(action(Act::Run, &Pick::Nothing, &[], "review").is_err());
        assert!(action(Act::Run, &row, &[], "").is_err());
        assert!(action(Act::Run, &row, &[], "review --nope").is_err());
    }

    /// The inbox puts each thing in one place: plans awaiting approval
    /// wait with the runs, a running job is under way, a done one is
    /// neither.
    #[test]
    fn the_inbox_puts_each_thing_in_one_place() {
        let job = |id, state: &str| Job {
            id,
            args: vec!["scan".into()],
            cwd: "/".into(),
            state: state.into(),
            created_at: 0,
            started_at: None,
            finished_at: None,
            pid: None,
            exit_code: None,
            awaiting: None,
            project: Some("p".into()),
        };
        let view = View::Inbox {
            header: "h".into(),
            lists: next::Next::default(),
            repos: BTreeMap::new(),
            state: std::env::temp_dir(),
            jobs: vec![
                job(1, "awaiting-approval"),
                job(2, "running"),
                job(3, "done"),
            ],
            runs: vec![Run {
                id: 7,
                project: "p".into(),
                text: "t".into(),
                state: RunState::Running,
                ..Default::default()
            }],
            now: 0,
        };
        let Body::Panes(p) = screen(view).body else {
            panic!("panes");
        };
        let titles: Vec<&str> = p.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["Waiting on you", "Working now", "Tasks"]);
        assert_eq!(
            p[0].items[0].texts()[..3],
            ["job 1", "awaiting-approval", "approve plan"]
        );
        let working: Vec<&str> = p[1].items.iter().map(|i| i.texts()[0]).collect();
        assert_eq!(working, ["#7", "job 2"]);
        assert_eq!(question(Act::Push, &p[1].items[0].pick), "push #7?");
        let ready = Pick::Run {
            id: 7,
            clean: false,
            fixing: false,
            state: "ready".into(),
            project: "p".into(),
        };
        assert_eq!(question(Act::Push, &ready), "approve and push #7?");
    }

    /// `e` on a project reads one setting a line: a tier, a tag, the
    /// verify command or the sandbox; a campaign and a preset read their
    /// words as the CLI does.
    #[test]
    fn typed_settings_build_their_actions() {
        let none = Pick::Nothing;
        assert_eq!(
            action_in(Act::Configure, &none, &[], "tier 2", "argdec"),
            Ok(Action::SetTier {
                project: "argdec".into(),
                tier: Some(2)
            })
        );
        assert_eq!(
            action_in(Act::Configure, &none, &[], "tag +oss", "argdec"),
            Ok(Action::Tag {
                project: "argdec".into(),
                tag: "oss".into(),
                add: true
            })
        );
        assert_eq!(
            action_in(Act::Configure, &none, &[], "verify make test", "argdec"),
            Ok(Action::SetConfig {
                key: "projects.argdec.verify".into(),
                value: "make test".into()
            })
        );
        assert!(action_in(Act::Configure, &none, &[], "tier 9", "argdec").is_err());
        assert!(action_in(Act::Configure, &none, &[], "colour red", "argdec").is_err());
        assert!(
            action(Act::Configure, &none, &[], "tier 1").is_err(),
            "no project"
        );
        assert_eq!(
            action(
                Act::AddCampaign,
                &none,
                &[],
                "c a,b --class=B fix the thing"
            ),
            Ok(Action::AddCampaign {
                name: "c".into(),
                text: "fix the thing".into(),
                projects: vec!["a".into(), "b".into()],
                class: Some("B".into()),
            })
        );
        assert_eq!(
            action(Act::AddPreset, &none, &[], "fast claude:haiku --x"),
            Ok(Action::SetPreset {
                name: "fast".into(),
                agent: "claude".into(),
                model: Some("haiku".into()),
                args: vec!["--x".into()],
            })
        );
        let preset = Pick::Preset {
            name: "fast".into(),
            agent: "claude".into(),
            model: Some("haiku".into()),
            args: vec!["--x".into()],
        };
        assert_eq!(initial_input(Act::EditPreset, &preset), "claude:haiku --x");
        assert_eq!(
            action(Act::UsePreset, &preset, &[], ""),
            Ok(Action::SetConfig {
                key: "preset".into(),
                value: "fast".into()
            })
        );
        assert_eq!(
            action_in(Act::ScanProject, &none, &[], "refresh-deps", "argdec"),
            Ok(Action::RefreshDeps {
                project: "argdec".into()
            })
        );
        let Ok(Action::Scan { deps, .. }) =
            action_in(Act::ScanProject, &none, &[], "deps", "argdec")
        else {
            panic!("a scan");
        };
        assert!(deps);
        assert_eq!(
            action_in(Act::Prune, &none, &[], "fix", "argdec"),
            Ok(Action::FixTodo {
                project: "argdec".into()
            })
        );
        assert_eq!(
            action_in(Act::Prune, &none, &[], "", "argdec"),
            Ok(Action::Prune {
                project: "argdec".into()
            })
        );
    }

    /// A task's issue is a link to the project's GitHub page when the scan
    /// recorded one, and a bare number otherwise.
    #[test]
    fn an_issue_links_to_its_page() {
        let placed = |project: &str| rank::Placed {
            task: rank::Task {
                project: project.into(),
                tier: 1,
                priority: crate::todo::Priority::High,
                text: "t".into(),
                line: Some(3),
                key: Some("k".into()),
                group: None,
                due: None,
                tagged_urgent: false,
                signal_urgent: false,
                age_days: 0,
                eligible: true,
                manual: false,
                gh: Some(7),
            },
            importance: 1.0,
            urgency: None,
            quadrant: rank::Quadrant::Q1,
        };
        let view = View::Matrix {
            header: "h".into(),
            placed: vec![placed("a"), placed("b")],
            limit: None,
            quadrant: None,
            today: 0,
            repos: BTreeMap::from([("a".to_string(), "https://github.com/o/a".to_string())]),
        };
        let Body::Panes(p) = screen(view).body else {
            panic!("panes");
        };
        let issue = |i: usize| p[0].items[i].cells.last().unwrap().clone();
        assert_eq!(issue(0).text, "#7");
        assert_eq!(
            issue(0).link.as_deref(),
            Some("https://github.com/o/a/issues/7")
        );
        assert_eq!(issue(1).text, "#7");
        assert_eq!(issue(1).link, None, "no page recorded for b");
    }

    #[test]
    fn prompts_and_questions_name_their_subject() {
        let note = Pick::Note {
            id: 3,
            text: "buy milk".into(),
        };
        assert_eq!(initial_input(Act::EditNote, &note), "buy milk");
        assert_eq!(question(Act::RemoveNote, &note), "remove note #3?");
        assert_eq!(
            question(Act::ApproveRuns, &Pick::Nothing),
            "approve every clean run?"
        );
    }
}
