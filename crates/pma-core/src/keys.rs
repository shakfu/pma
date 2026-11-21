//! The one key table every front end reads: which key opens which tab, moves
//! the selection, or does which action. Keys are DOM `KeyboardEvent.key`
//! names (`j`, `Enter`, `Escape`, `ArrowDown`), so the web uses the table as
//! it is and the TUI maps its key codes to the same names. Plain keys only:
//! a browser or screen reader reserves most combinations with a modifier.
//! `BackTab` is the one name the DOM lacks: it is Shift+Tab, which the web
//! maps from `Tab` and `shiftKey`.

use crate::api::{Action, Query, Selection};
use crate::report_runs::By;
use crate::screen::{self, Pick};
use crate::store::RunState;

/// A screen: one query, rendered as a list or as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    /// What waits on you, what is under way, and notes.
    Inbox,
    /// Every project, worst first, and failing CI.
    Projects,
    /// Jobs, running agents and workflow instances, newest first.
    Activity,
    /// The run report and the latest scans: what happened.
    History,
    /// Agents, settings, roots, presets, routes, workflow revisions and
    /// campaigns.
    Setup,
    Matrix,
    /// One project, opened from a row that names it. It has no key of its
    /// own: which project is the front end's to remember.
    Project,
}

impl Tab {
    pub const ALL: [Tab; 7] = [
        Tab::Inbox,
        Tab::Projects,
        Tab::Activity,
        Tab::History,
        Tab::Setup,
        Tab::Matrix,
        Tab::Project,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Inbox => "inbox",
            Tab::Projects => "projects",
            Tab::Activity => "activity",
            Tab::History => "history",
            Tab::Setup => "setup",
            Tab::Matrix => "matrix",
            Tab::Project => "project",
        }
    }

    /// What the tab shows. Lists are not cut short: a front end scrolls.
    /// `of` is the front end's scope, the tags its views are narrowed to;
    /// `project` names the project the project tab shows; `finished`, the
    /// state the activity tab's finished runs are filtered to.
    pub fn query(self, of: Selection, by: By, project: &str, finished: Option<RunState>) -> Query {
        match self {
            Tab::Inbox => Query::Inbox { of },
            // Every project, untiered ones too: it is the list a project is
            // opened from.
            Tab::Projects => Query::Status {
                of,
                explain: false,
                all: true,
            },
            Tab::Activity => Query::Activity { finished },
            Tab::Matrix => Query::Matrix {
                of,
                quadrant: None,
                all: true,
            },
            Tab::History => Query::History { by, of },
            Tab::Setup => Query::Settings,
            Tab::Project => Query::Project {
                name: project.to_string(),
            },
        }
    }
}

/// An action by kind, without its arguments: what a key names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Act {
    ApproveRun,
    ApproveRuns,
    RejectRun,
    AddReviewTime,
    SettlePrs,
    StopJob,
    ApproveJob,
    StopWorkflow,
    AddNote,
    EditNote,
    RemoveNote,
    Scan,
    /// Scans the project the project view shows, and nothing else.
    ScanProject,
    Dispatch,
    /// Dispatches, then approves and pushes each run whose gates are clean.
    DispatchPush,
    /// Dispatches, for a person to drive the agent in a herdr pane.
    DispatchInteractive,
    /// Brings the run's herdr pane to the front.
    OpenPane,
    /// Opens an interactive agent on the run's directory, in herdr.
    TakeOver,
    /// A shell in the project's clone, for a fix by hand; the project is
    /// re-read when it ends.
    Shell,
    /// Rejects a run and leaves the clone's files as they are.
    ForgetRun,
    /// Deletes the record of a project whose checkout is gone.
    ForgetProject,
    /// Resumes the selected instance.
    ResumeWorkflow,
    /// Starts an instance from a typed `<workflow> <target> [flags]`.
    StartWorkflow,
    ReworkRun,
    /// A failed run: rejects it and dispatches its task again.
    RetryRun,
    Stage,
    Commit,
    Pr,
    Push,
    /// Pushes every approved, staged or committed run in the list.
    PushApproved,
    /// Merges the run's open pull request.
    Merge,
    /// Has an agent resolve the conflicts of the run's open pull request.
    FixConflicts,
    RunCampaign,
    Sync,
    Verify,
    /// `pma run <name>` over the project in view or the selected row's,
    /// from a typed `<name> [--review] [--worktree] [--push]`.
    Run,
    // Configuring.
    EditSetting,
    ResetSetting,
    AddRoot,
    RemoveRoot,
    /// `pma agent set <name> <field> <value>`, from a typed `field value`.
    EditAgent,
    RemoveAgent,
    /// Makes the selected agent `pma config agent`.
    UseAgent,
    EditPreset,
    AddPreset,
    RemovePreset,
    /// Makes the selected preset `pma config preset`.
    UsePreset,
    ActivateRoute,
    ProposeRoute,
    AddCampaign,
    RemoveCampaign,
    /// The project in view: its tier, a tag, its verify command or its
    /// sandbox, from a typed `tier 3`, `tag +name`, `verify make test` or
    /// `sandbox on`.
    Configure,
    /// Tidies the project in view's TODO.md: prunes finished items, or
    /// rewrites its mechanical errors, by choice.
    Prune,
    /// Dispatches a typed prompt over the project in view: work that is no
    /// TODO.md item.
    Prompt,
    /// Queues a typed `pma` command line as a job: what no key does.
    Cli,
}

impl Act {
    pub const ALL: [Act; 55] = [
        Act::ApproveRun,
        Act::ApproveRuns,
        Act::RejectRun,
        Act::AddReviewTime,
        Act::SettlePrs,
        Act::StopJob,
        Act::ApproveJob,
        Act::StopWorkflow,
        Act::AddNote,
        Act::EditNote,
        Act::RemoveNote,
        Act::Scan,
        Act::ScanProject,
        Act::Dispatch,
        Act::DispatchPush,
        Act::DispatchInteractive,
        Act::OpenPane,
        Act::TakeOver,
        Act::Shell,
        Act::ResumeWorkflow,
        Act::StartWorkflow,
        Act::ReworkRun,
        Act::RetryRun,
        Act::Stage,
        Act::Commit,
        Act::Pr,
        Act::Push,
        Act::PushApproved,
        Act::Merge,
        Act::FixConflicts,
        Act::RunCampaign,
        Act::Sync,
        Act::Verify,
        Act::Run,
        Act::EditSetting,
        Act::ResetSetting,
        Act::AddRoot,
        Act::RemoveRoot,
        Act::EditAgent,
        Act::RemoveAgent,
        Act::UseAgent,
        Act::EditPreset,
        Act::AddPreset,
        Act::RemovePreset,
        Act::UsePreset,
        Act::ActivateRoute,
        Act::ProposeRoute,
        Act::AddCampaign,
        Act::RemoveCampaign,
        Act::Configure,
        Act::Prune,
        Act::Prompt,
        Act::ForgetRun,
        Act::ForgetProject,
        Act::Cli,
    ];

    /// Whether the act settles a run a reviewer has open, so the next one to
    /// read can follow. Not stage or commit: those leave a next step on this
    /// run, which their notice offers to open.
    pub fn decides(self) -> bool {
        matches!(
            self,
            Act::ApproveRun
                | Act::RejectRun
                | Act::ForgetRun
                | Act::ReworkRun
                | Act::RetryRun
                | Act::Pr
                | Act::Push
        )
    }

    pub fn name(self) -> String {
        format!("{self:?}")
    }

    /// Whether the act needs a selected row; one that does not acts on the
    /// whole tab or on nothing in particular.
    pub fn on_row(self) -> bool {
        !matches!(
            self,
            Act::Scan
                | Act::ScanProject
                | Act::Sync
                | Act::AddNote
                | Act::SettlePrs
                | Act::ApproveRuns
                | Act::PushApproved
                | Act::StartWorkflow
                | Act::Run
                | Act::Shell
                | Act::AddRoot
                | Act::AddPreset
                | Act::ProposeRoute
                | Act::AddCampaign
                | Act::Configure
                | Act::Prune
                | Act::Prompt
                | Act::Cli
        )
    }

    /// How much an act weighs: `primary` moves work forward, `danger`
    /// discards or publishes something that cannot be called back.
    pub fn weight(self) -> &'static str {
        match self {
            Act::ApproveRun
            | Act::ApproveJob
            | Act::Dispatch
            | Act::ActivateRoute
            | Act::RetryRun => "primary",
            Act::RejectRun
            | Act::StopJob
            | Act::StopWorkflow
            | Act::RemoveNote
            | Act::Push
            | Act::PushApproved
            | Act::DispatchPush
            | Act::Merge
            | Act::ResetSetting
            | Act::RemoveRoot
            | Act::RemoveAgent
            | Act::RemovePreset
            | Act::RemoveCampaign
            | Act::ForgetProject
            | Act::Prune
            | Act::Cli => "danger",
            _ => "default",
        }
    }

    /// The kinds an action can be. A new `Action` variant fails to compile
    /// here until it has a kind, and the tests then require it a key.
    pub fn of(a: &Action) -> &'static [Act] {
        match a {
            Action::ApproveRun { .. } => &[Act::ApproveRun],
            Action::ApproveRuns { .. } => &[Act::ApproveRuns],
            Action::RejectRun { .. } => &[Act::RejectRun, Act::ForgetRun],
            Action::CloseRuns { .. } => &[Act::ForgetRun],
            Action::AddReviewTime { .. } => &[Act::AddReviewTime],
            Action::SettlePrs => &[Act::SettlePrs],
            Action::MergePr { .. } => &[Act::Merge],
            Action::FixConflicts { .. } => &[Act::FixConflicts],
            Action::StopJob { .. } => &[Act::StopJob],
            Action::ApproveJob { .. } => &[Act::ApproveJob],
            Action::StopWorkflow { .. } => &[Act::StopWorkflow],
            Action::AddNote { .. } => &[Act::AddNote],
            Action::EditNote { .. } => &[Act::EditNote],
            Action::RemoveNote { .. } => &[Act::RemoveNote],
            Action::Scan { .. } => &[Act::Scan, Act::ScanProject],
            Action::Dispatch { .. } => &[
                Act::Dispatch,
                Act::DispatchPush,
                Act::DispatchInteractive,
                Act::Prompt,
            ],
            Action::FocusPane { .. } => &[Act::OpenPane],
            Action::TakeOver { .. } => &[Act::TakeOver],
            Action::Shell { .. } => &[Act::Shell],
            Action::WorkflowRun { .. } => &[Act::ResumeWorkflow, Act::StartWorkflow],
            Action::ReworkRun { .. } => &[Act::ReworkRun],
            Action::RetryRun { .. } => &[Act::RetryRun],
            Action::Run { .. } => &[Act::Run],
            Action::Publish { .. } => &[
                Act::Stage,
                Act::Commit,
                Act::Pr,
                Act::Push,
                Act::PushApproved,
            ],
            Action::RunCampaign { .. } => &[Act::RunCampaign],
            Action::Sync { .. } => &[Act::Sync],
            Action::Verify { .. } => &[Act::Verify],
            Action::SetConfig { .. } => &[
                Act::EditSetting,
                Act::UseAgent,
                Act::UsePreset,
                Act::Configure,
            ],
            Action::ResetConfig { .. } => &[Act::ResetSetting],
            Action::AddRoot { .. } => &[Act::AddRoot],
            Action::RemoveRoot { .. } => &[Act::RemoveRoot],
            Action::SetAgentField { .. } => &[Act::EditAgent],
            Action::RemoveAgent { .. } => &[Act::RemoveAgent],
            Action::SetPreset { .. } => &[Act::EditPreset, Act::AddPreset],
            Action::RemovePreset { .. } => &[Act::RemovePreset],
            Action::ActivateRoute { .. } => &[Act::ActivateRoute],
            Action::ProposeRoute { .. } => &[Act::ProposeRoute],
            Action::AddCampaign { .. } => &[Act::AddCampaign],
            Action::RemoveCampaign { .. } => &[Act::RemoveCampaign],
            Action::ForgetProject { .. } => &[Act::ForgetProject],
            Action::SetTier { .. } | Action::Tag { .. } => &[Act::Configure],
            Action::Prune { .. } | Action::FixTodo { .. } => &[Act::Prune],
            Action::RefreshDeps { .. } => &[Act::ScanProject],
            Action::Cli { .. } => &[Act::Cli],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    Tab(Tab),
    Act(Act),
    Down,
    Up,
    First,
    Last,
    /// The next list on a tab with several.
    NextPane,
    PrevPane,
    /// The selected item in full: a run's diff, a job's log; a task's
    /// project.
    Open,
    /// Closes what `Open` or a prompt opened.
    Back,
    /// The report's next dimension.
    ReportBy,
    /// Cycles the state the activity tab's finished runs are filtered to,
    /// through `FINISHED`.
    Finished,
    /// The project of the selected row.
    OpenProject,
    /// A project by name, from a fuzzy chooser over every project's name and
    /// tags: the `cd` of working by hand.
    Jump,
    /// The scope: a tag every view is narrowed to and every act over the
    /// portfolio acts within, chosen from the tags in use. `--tag` in the
    /// front ends.
    Scope,
    /// Filters the tab's tables by a typed fuzzy match over each row's
    /// cells; Escape clears it.
    Filter,
    /// The selected run's diff in `$PAGER`, in the TUI; the web scrolls its
    /// detail to the diff. Reading is the long step, and a pager has search.
    ViewDiff,
    /// The selected row's project, or the one in view, in `$VISUAL` or
    /// `$EDITOR`, in the TUI: a fix by hand is sometimes quicker than a run.
    Edit,
    /// Marks the selected task, project or run, or unmarks it, for an act on several.
    Mark,
    /// Marks every row the focused list shows, or unmarks them all.
    MarkAll,
    /// A page of rows down or up.
    PageDown,
    PageUp,
    Help,
    /// The TUI only; a browser tab is closed by the browser.
    Quit,
    /// A command typed by name, as `:` does in vim: `resolve` reads it.
    Line,
}

impl Command {
    /// The name `:` takes for the command. Typed by people, so these are
    /// spelled out rather than derived from the variant.
    pub fn name(self) -> String {
        let s = match self {
            Command::Tab(t) => t.title(),
            Command::Act(a) => return kebab(&a.name()),
            Command::Down => "down",
            Command::Up => "up",
            Command::First => "first",
            Command::Last => "last",
            Command::NextPane => "next-pane",
            Command::PrevPane => "prev-pane",
            Command::Open => "open",
            Command::Back => "back",
            Command::ReportBy => "report-by",
            Command::Finished => "finished",
            Command::OpenProject => "open-project",
            Command::Jump => "project",
            Command::Scope => "scope",
            Command::Filter => "filter",
            Command::ViewDiff => "diff",
            Command::Edit => "edit",
            Command::Mark => "mark",
            Command::MarkAll => "mark-all",
            Command::PageDown => "page-down",
            Command::PageUp => "page-up",
            Command::Help => "help",
            Command::Quit => "quit",
            Command::Line => "command",
        };
        s.to_string()
    }
}

/// `ApproveRun` as `approve-run`.
fn kebab(camel: &str) -> String {
    let mut out = String::new();
    for (i, c) in camel.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('-');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// What a front end asks before it acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    Nothing,
    /// Yes or no: the action cannot be undone from here.
    Confirm,
    /// A line of text, with this label.
    Text(&'static str),
    /// One of a list `api::choices` gives, plus its flags, with this label.
    Choose(&'static str),
    /// Rows `api::checklist` gives to check or uncheck, with this label. The
    /// input is the 1-based numbers left unchecked, comma-separated. An
    /// empty list asks nothing.
    Check(&'static str),
    /// As `Check`, but an empty list asks yes or no.
    CheckOrConfirm(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub key: &'static str,
    pub command: Command,
    /// Where the key applies; empty is every tab.
    pub tabs: &'static [Tab],
    pub ask: Ask,
    pub label: &'static str,
}

const fn bind(key: &'static str, command: Command, label: &'static str) -> Binding {
    Binding {
        key,
        command,
        tabs: &[],
        ask: Ask::Nothing,
        label,
    }
}

const fn on(
    key: &'static str,
    tabs: &'static [Tab],
    act: Act,
    ask: Ask,
    label: &'static str,
) -> Binding {
    Binding {
        key,
        command: Command::Act(act),
        tabs,
        ask,
        label,
    }
}

const TASKS: &[Tab] = &[Tab::Inbox, Tab::Matrix, Tab::Project, Tab::Projects];
const MARKS: &[Tab] = &[Tab::Inbox, Tab::Matrix, Tab::Project, Tab::Projects];
const PANES: &[Tab] = &[
    Tab::Inbox,
    Tab::Projects,
    Tab::Activity,
    Tab::Matrix,
    Tab::History,
    Tab::Setup,
    Tab::Project,
];
const RUNS: &[Tab] = &[Tab::Inbox, Tab::Project];
const JOBS: &[Tab] = &[Tab::Inbox, Tab::Activity, Tab::Project];

pub static BINDINGS: &[Binding] = &[
    bind("1", Command::Tab(Tab::Inbox), "inbox"),
    bind("2", Command::Tab(Tab::Projects), "projects"),
    bind("3", Command::Tab(Tab::Activity), "activity"),
    bind("4", Command::Tab(Tab::History), "history"),
    bind("5", Command::Tab(Tab::Setup), "setup"),
    bind("6", Command::Tab(Tab::Matrix), "matrix"),
    bind("j", Command::Down, "down"),
    bind("ArrowDown", Command::Down, "down"),
    bind("k", Command::Up, "up"),
    bind("ArrowUp", Command::Up, "up"),
    bind("g", Command::First, "first"),
    bind("Home", Command::First, "first"),
    bind("G", Command::Last, "last"),
    bind("End", Command::Last, "last"),
    bind("PageDown", Command::PageDown, "page down"),
    bind("PageUp", Command::PageUp, "page up"),
    bind("/", Command::Filter, "filter"),
    bind("O", Command::Jump, "go to project"),
    bind("#", Command::Scope, "scope to a tag"),
    bind("E", Command::Edit, "edit project in $EDITOR"),
    Binding {
        tabs: PANES,
        ..bind("Tab", Command::NextPane, "next list")
    },
    Binding {
        tabs: PANES,
        ..bind("BackTab", Command::PrevPane, "previous list")
    },
    Binding {
        tabs: PANES,
        ..bind("l", Command::NextPane, "next list")
    },
    Binding {
        tabs: PANES,
        ..bind("h", Command::PrevPane, "previous list")
    },
    // A run, a job, a scan, a campaign or a route in full; a task's
    // project, or in its project the task itself.
    Binding {
        tabs: &[
            Tab::Inbox,
            Tab::Projects,
            Tab::Activity,
            Tab::Matrix,
            Tab::History,
            Tab::Setup,
            Tab::Project,
        ],
        ..bind("Enter", Command::Open, "open")
    },
    Binding {
        tabs: &[
            Tab::Inbox,
            Tab::Projects,
            Tab::Activity,
            Tab::Matrix,
            Tab::History,
        ],
        ..bind("o", Command::OpenProject, "open project")
    },
    bind("Escape", Command::Back, "back"),
    Binding {
        tabs: &[Tab::History],
        ..bind("b", Command::ReportBy, "by")
    },
    Binding {
        tabs: &[Tab::Activity],
        ..bind("b", Command::Finished, "finished state")
    },
    bind("?", Command::Help, "keys"),
    bind(":", Command::Line, "command"),
    bind("q", Command::Quit, "quit"),
    // Anywhere.
    on("c", &[], Act::Scan, Ask::Nothing, "scan"),
    on("i", &[], Act::Sync, Ask::Confirm, "sync issues"),
    on("n", &[], Act::AddNote, Ask::Text("note"), "add note"),
    on(
        "!",
        &[],
        Act::Cli,
        Ask::Text("pma command"),
        "run pma command",
    ),
    on(
        "r",
        &[Tab::Project],
        Act::ScanProject,
        Ask::Choose("rescan"),
        "rescan project",
    ),
    // On the projects tab it takes the selected project, or the marked ones.
    on(
        "e",
        &[Tab::Project, Tab::Projects],
        Act::Configure,
        Ask::Text("tier 1-5|none, tag +name|-name, verify <command>, or sandbox on|off"),
        "project settings",
    ),
    on(
        "P",
        &[Tab::Project],
        Act::Prompt,
        Ask::Text("prompt for an agent, over the whole project"),
        "prompt an agent",
    ),
    on(
        "f",
        &[Tab::Project],
        Act::Prune,
        Ask::Choose("tidy TODO.md"),
        "tidy TODO.md",
    ),
    // A project: the one in view, or the selected row's.
    on(
        "z",
        &[Tab::Project, Tab::Projects, Tab::Inbox, Tab::Matrix],
        Act::Run,
        Ask::Choose("run"),
        "run",
    ),
    // A task.
    Binding {
        tabs: MARKS,
        ..bind(" ", Command::Mark, "mark")
    },
    Binding {
        tabs: MARKS,
        ..bind("*", Command::MarkAll, "mark all shown")
    },
    on("d", TASKS, Act::Dispatch, Ask::Confirm, "dispatch"),
    on(
        "D",
        TASKS,
        Act::DispatchPush,
        Ask::Confirm,
        "dispatch and push",
    ),
    on("v", TASKS, Act::Verify, Ask::Nothing, "verify project"),
    // On a run `v` is the diff: `lookup` tells the rows apart.
    Binding {
        tabs: RUNS,
        ..bind("v", Command::ViewDiff, "diff in pager")
    },
    on(
        "I",
        TASKS,
        Act::DispatchInteractive,
        Ask::Nothing,
        "dispatch to drive yourself",
    ),
    // A run.
    on(
        "a",
        RUNS,
        Act::ApproveRun,
        Ask::Check("items done"),
        "approve",
    ),
    on("b", RUNS, Act::ApproveRuns, Ask::Confirm, "approve clean"),
    on("x", RUNS, Act::RejectRun, Ask::Confirm, "reject"),
    on("w", RUNS, Act::ReworkRun, Ask::Text("feedback"), "rework"),
    on("R", RUNS, Act::RetryRun, Ask::Confirm, "retry as it was"),
    on(
        "t",
        RUNS,
        Act::AddReviewTime,
        Ask::Text("minutes"),
        "review time",
    ),
    // Staging is undone by `git reset`, so it asks nothing.
    on("s", RUNS, Act::Stage, Ask::Nothing, "stage"),
    on(
        "m",
        RUNS,
        Act::Commit,
        Ask::Text("commit subject"),
        "commit",
    ),
    on("p", RUNS, Act::Pr, Ask::Confirm, "pull request"),
    on(
        "u",
        RUNS,
        Act::Push,
        Ask::CheckOrConfirm("items done"),
        "push",
    ),
    on("U", RUNS, Act::PushApproved, Ask::Confirm, "push approved"),
    on("y", RUNS, Act::SettlePrs, Ask::Nothing, "check PRs"),
    on("M", RUNS, Act::Merge, Ask::Confirm, "merge PR"),
    on("F", RUNS, Act::FixConflicts, Ask::Confirm, "fix conflicts"),
    on("H", RUNS, Act::OpenPane, Ask::Nothing, "open in herdr"),
    // Opening a pane publishes nothing and spends nothing until it is used.
    on("T", RUNS, Act::TakeOver, Ask::Nothing, "take over"),
    // A project: the selected row's, or the one in view.
    on(
        "S",
        &[Tab::Inbox, Tab::Projects, Tab::Matrix, Tab::Project],
        Act::Shell,
        Ask::Nothing,
        "shell in project",
    ),
    on(
        "X",
        RUNS,
        Act::ForgetRun,
        Ask::Confirm,
        "close, leave files",
    ),
    // `api::forgettable` refuses a project still on disk.
    on(
        "x",
        &[Tab::Projects],
        Act::ForgetProject,
        Ask::Confirm,
        "forget deleted project",
    ),
    // A job. `a` and `s` are a run's keys too: a key means one thing for
    // the row it is pressed on, and `lookup` tells them apart by the row.
    on("a", JOBS, Act::ApproveJob, Ask::Nothing, "approve plan"),
    on("s", JOBS, Act::StopJob, Ask::Confirm, "stop"),
    // A workflow instance.
    on(
        "r",
        &[Tab::Activity],
        Act::ResumeWorkflow,
        Ask::Nothing,
        "resume",
    ),
    on(
        "f",
        &[Tab::Activity],
        Act::StartWorkflow,
        Ask::Text("workflow target [--local] [--unattended]"),
        "start workflow",
    ),
    on(
        "s",
        &[Tab::Activity],
        Act::StopWorkflow,
        Ask::Confirm,
        "stop",
    ),
    // A campaign. `r`, `a` and `f` are setup's add-root, use and propose,
    // which fit any row, so a campaign takes `z`, as a run does elsewhere.
    on(
        "z",
        &[Tab::Setup],
        Act::RunCampaign,
        Ask::Confirm,
        "run campaign",
    ),
    on(
        "C",
        &[Tab::Setup],
        Act::AddCampaign,
        Ask::Text("name project,project text... [--class=A]"),
        "add campaign",
    ),
    on(
        "x",
        &[Tab::Setup],
        Act::RemoveCampaign,
        Ask::Confirm,
        "remove",
    ),
    // A note.
    on("e", &[Tab::Inbox], Act::EditNote, Ask::Text("note"), "edit"),
    on("x", &[Tab::Inbox], Act::RemoveNote, Ask::Confirm, "remove"),
    // Settings. `e`, `x` and `a` each mean one thing per row kind.
    on(
        "e",
        &[Tab::Setup],
        Act::EditSetting,
        Ask::Text("value"),
        "edit",
    ),
    on(
        "e",
        &[Tab::Setup],
        Act::EditAgent,
        Ask::Text("field value"),
        "set field",
    ),
    on(
        "e",
        &[Tab::Setup],
        Act::EditPreset,
        Ask::Text("agent[:model] [args...]"),
        "edit",
    ),
    on("x", &[Tab::Setup], Act::ResetSetting, Ask::Confirm, "reset"),
    on("x", &[Tab::Setup], Act::RemoveRoot, Ask::Confirm, "remove"),
    on("x", &[Tab::Setup], Act::RemoveAgent, Ask::Confirm, "remove"),
    on(
        "x",
        &[Tab::Setup],
        Act::RemovePreset,
        Ask::Confirm,
        "remove",
    ),
    on(
        "a",
        &[Tab::Setup],
        Act::UseAgent,
        Ask::Nothing,
        "make default",
    ),
    on(
        "a",
        &[Tab::Setup],
        Act::UsePreset,
        Ask::Nothing,
        "make default",
    ),
    on(
        "a",
        &[Tab::Setup],
        Act::ActivateRoute,
        Ask::Confirm,
        "activate",
    ),
    on(
        "r",
        &[Tab::Setup],
        Act::AddRoot,
        Ask::Text("directory"),
        "add root",
    ),
    on(
        "p",
        &[Tab::Setup],
        Act::AddPreset,
        Ask::Text("name agent[:model] [args...]"),
        "add preset",
    ),
    on(
        "f",
        &[Tab::Setup],
        Act::ProposeRoute,
        Ask::Text("policy file"),
        "propose route",
    ),
];

/// The binding `key` has on `tab` for a row about `pick`. One applying to
/// that tab alone wins over one applying everywhere; among those, an act
/// that fits the row wins, then a command that is not an act, then the
/// first, so a key for the wrong row says what it needs.
pub fn lookup(key: &str, tab: Tab, pick: &Pick) -> Option<&'static Binding> {
    let fits = |b: &&Binding| b.key == key;
    let on_tab: Vec<&Binding> = BINDINGS
        .iter()
        .filter(fits)
        .filter(|b| b.tabs.contains(&tab))
        .collect();
    let candidates = match on_tab.is_empty() {
        false => on_tab,
        true => BINDINGS
            .iter()
            .filter(fits)
            .filter(|b| b.tabs.is_empty())
            .collect(),
    };
    candidates
        .iter()
        .find(|b| matches!(b.command, Command::Act(a) if screen::fits(a, pick)))
        .or_else(|| {
            candidates
                .iter()
                .find(|b| !matches!(b.command, Command::Act(_)))
        })
        .or_else(|| candidates.first())
        .copied()
}

/// The bindings that apply on `tab`, for a help screen.
pub fn on_tab(tab: Tab) -> Vec<&'static Binding> {
    BINDINGS
        .iter()
        .filter(|b| b.tabs.is_empty() || b.tabs.contains(&tab))
        .collect()
}

/// A key as a person reads it. The table names keys as `KeyboardEvent.key`
/// does, and that name for the space bar is a blank.
pub fn shown(key: &str) -> &str {
    match key {
        " " => "Space",
        k => k,
    }
}

/// Names vim users type that a prefix alone would not settle.
const ALIASES: [(&str, Command); 2] = [("q", Command::Quit), ("h", Command::Help)];

/// What a `pma` command acts on without a project named: the kinds of
/// `docs/dev/cli-context.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every project, narrowed by the context.
    Portfolio,
    /// The targets named, else the context project.
    Targets,
    /// One project: the context's, or an error.
    One,
    /// A TODO.md: the context project's, else `./TODO.md`.
    Files,
    /// Run ids.
    Runs,
    /// No project.
    None,
}

/// `pma`'s commands that a `:` line queues as a job, with clap's one-line
/// help and their scope. The pma crate's tests hold this and
/// `CLI_TERMINAL` to clap's list.
pub const CLI_TABLE: &[(&str, &str, Scope)] = &[
    (
        "next",
        "Tasks for agents, and what waits on you. The default",
        Scope::Portfolio,
    ),
    (
        "run",
        "Run a review, a CI fix or an upgrade over projects, by name",
        Scope::Targets,
    ),
    (
        "scan",
        "Read TODO.md, git state and CI into the database",
        Scope::Portfolio,
    ),
    (
        "matrix",
        "Every tiered project's tasks, ranked, in one view",
        Scope::Portfolio,
    ),
    (
        "dispatch",
        "Run an agent on tasks, each in its own worktree",
        Scope::Targets,
    ),
    (
        "review",
        "Runs awaiting a decision; show one, or act on it",
        Scope::Runs,
    ),
    (
        "take-over",
        "Open an interactive agent on a finished run's directory",
        Scope::Runs,
    ),
    (
        "close",
        "End a run and leave your files as they are",
        Scope::Runs,
    ),
    (
        "stage",
        "Write each run named into its clone's index, for review",
        Scope::Runs,
    ),
    (
        "commit",
        "Commit each run named in its clone, staging it first",
        Scope::Runs,
    ),
    (
        "push",
        "Push each run named to the default branch, committing first",
        Scope::Runs,
    ),
    (
        "pr",
        "Open a pull request for each approved run named",
        Scope::Runs,
    ),
    (
        "merge",
        "Merge a run's open pull request, or fix its conflicts",
        Scope::Runs,
    ),
    (
        "task",
        "One task in full, as Enter opens it in the TUI and web",
        Scope::One,
    ),
    (
        "lint",
        "Check TODO.md files against the format",
        Scope::Files,
    ),
    (
        "prune",
        "Remove finished items and `Done` sections",
        Scope::Files,
    ),
    ("stale", "Open items by age, oldest first", Scope::Portfolio),
    (
        "sync",
        "Mirror `Critical` items to GitHub Issues",
        Scope::Portfolio,
    ),
    (
        "note",
        "Portfolio notes, which belong to no one project",
        Scope::None,
    ),
    ("deps", "Dependency locks", Scope::One),
    (
        "shell",
        "A shell in a project's clone, for a fix by hand",
        Scope::One,
    ),
    (
        "status",
        "Projects worst first, by what needs attention",
        Scope::Portfolio,
    ),
    (
        "history",
        "Past scans, or what one scan changed",
        Scope::None,
    ),
    (
        "report",
        "What dispatching produced: outcomes, cost, time",
        Scope::None,
    ),
    (
        "usage",
        "Which tabs, panes and acts the TUI and web were used for",
        Scope::None,
    ),
    (
        "root",
        "Directories whose git repos are projects",
        Scope::None,
    ),
    (
        "project",
        "A project's record: its tier, its tags, or its removal",
        Scope::One,
    ),
    (
        "config",
        "List a setting, show one, or set one, in `settings.toml`",
        Scope::None,
    ),
    (
        "verify",
        "Run each project's check where a dispatch would",
        Scope::Targets,
    ),
    (
        "integrate",
        "Where agents run: `herdr`, `herdr-app`, `minos` or `none`",
        Scope::None,
    ),
    ("agent", "The workers `pma dispatch` can run", Scope::None),
    (
        "preset",
        "A named worker, model and configuration",
        Scope::None,
    ),
    (
        "route",
        "Which worker and how much autonomy, per kind of task",
        Scope::None,
    ),
    ("campaign", "One task across many repositories", Scope::None),
    (
        "workflow",
        "A graph of agents over a project",
        Scope::Targets,
    ),
    (
        "job",
        "Commands queued with `--detach`, newest first",
        Scope::None,
    ),
];

/// The names in `CLI_TABLE`.
pub const CLI: &[&str] = &{
    let mut out = [""; CLI_TABLE.len()];
    let mut i = 0;
    while i < CLI_TABLE.len() {
        out[i] = CLI_TABLE[i].0;
        i += 1;
    }
    out
};

/// `command`'s scope; `None` for one not in `CLI_TABLE`.
pub fn scope_of(command: &str) -> Option<Scope> {
    CLI_TABLE.iter().find(|c| c.0 == command).map(|c| c.2)
}

/// The `pma` commands that act on something on this page, for `:help`:
/// each as typed, what it acts on, and its one-liner. `project` is the
/// project page's, empty elsewhere; `%` stands for the selected row.
pub fn commands_here(project: &str, pick: &Pick) -> Vec<(String, String, &'static str)> {
    let p = (!project.is_empty()).then_some(project);
    let row = screen::selection(pick);
    let run = matches!(pick, Pick::Run { .. });
    let task = matches!(
        pick,
        Pick::Task { .. }
            | Pick::Project {
                target: Some(_),
                ..
            }
    );
    CLI_TABLE
        .iter()
        .filter_map(|&(name, about, scope)| {
            let (typed, on) = match (scope, p) {
                (Scope::Portfolio, Some(p)) => {
                    (name.to_string(), format!("{p}; -A for every project"))
                }
                (Scope::Portfolio, None) => (name.to_string(), "every project".into()),
                (Scope::Targets, _) if task => (format!("{name} %"), row.clone()?),
                (Scope::Targets, Some(p)) => {
                    (name.to_string(), format!("{p}, unless a target is named"))
                }
                (Scope::One, Some(p)) => (name.to_string(), p.to_string()),
                (Scope::Files, Some(p)) => (name.to_string(), format!("{p}'s TODO.md")),
                (Scope::Runs, _) if run => (format!("{name} %"), format!("run #{}", row.clone()?)),
                _ => return None,
            };
            Some((format!(":{typed}"), on, about))
        })
        .collect()
}

/// What `command` acts on with no project named, given the context
/// `(project, how it was found)`: the line its help starts with.
pub fn here(command: &str, ctx: Option<(&str, &str)>) -> Option<String> {
    Some(match (scope_of(command)?, ctx) {
        (Scope::Portfolio, Some((p, how))) => {
            format!("Here: acts on {p} ({how}); -A for every project.")
        }
        (Scope::Portfolio, None) => "Here: no project context; acts on every project.".into(),
        (Scope::Targets, Some((p, how))) => {
            format!("Here: acts on {p} ({how}) when no target is named; `:12` is {p}:12.")
        }
        (Scope::Targets, None) => "Here: no project context; name a target.".into(),
        (Scope::One, Some((p, how))) => {
            format!("Here: acts on {p} ({how}) when no project is named.")
        }
        (Scope::One, None) => "Here: no project context; name a project.".into(),
        (Scope::Files, Some((p, how))) => {
            format!("Here: {p}'s TODO.md ({how}) when no path is named.")
        }
        (Scope::Files, None) => "Here: no project context; ./TODO.md.".into(),
        (Scope::Runs | Scope::None, _) => return None,
    })
}

/// Commands with `-d`, whose plan a front end shows before it queues one.
/// `-d` on a subcommand without it is a parse error, which changes nothing.
pub const CLI_PLANS: &[&str] = &[
    "push", "pr", "merge", "close", "sync", "prune", "lint", "run", "review", "root", "agent",
    "preset", "campaign", "project",
];

/// Commands that hold a terminal of their own, which a job has none of.
pub const CLI_TERMINAL: &[&str] = &["tui", "web", "watch"];

/// `pma project`'s subcommands: `:project tier 2 x` runs the CLI, where
/// `:project x` opens a project.
pub const PROJECT_SUBCOMMANDS: &[&str] =
    &["show", "tier", "export", "import", "tag", "forget", "clone"];

fn cli_binding() -> &'static Binding {
    BINDINGS
        .iter()
        .find(|b| b.command == Command::Act(Act::Cli))
        .expect("`Act::Cli` has a key")
}

/// The commands `:` offers on `tab` for a row about `pick`, one binding
/// each, in table order. An act on a row that does not fit it is left out.
pub fn commands(tab: Tab, pick: &Pick) -> Vec<(String, &'static Binding)> {
    let mut out: Vec<(String, &'static Binding)> = Vec::new();
    for b in on_tab(tab) {
        let fits = match b.command {
            Command::Line => false,
            Command::Act(a) => !a.on_row() || screen::fits(a, pick),
            _ => true,
        };
        let name = b.command.name();
        if fits && !out.iter().any(|(n, _)| *n == name) {
            out.push((name, b));
        }
    }
    out
}

/// The names in `commands` that begin with `word`, for Tab to complete.
pub fn complete(word: &str, tab: Tab, pick: &Pick) -> Vec<String> {
    let mut names: Vec<String> = commands(tab, pick).into_iter().map(|(n, _)| n).collect();
    for c in CLI {
        if !names.iter().any(|n| n == c) {
            names.push(c.to_string());
        }
    }
    names.retain(|n| n.starts_with(word));
    names
}

/// The binding a typed line names, and the argument after the name. A name
/// matches exactly, as an alias, or as the prefix of one command alone. A
/// `pma` command's name resolves to `Act::Cli` with the whole line as its
/// argument: when nothing here has that name, or when arguments follow it,
/// as the front end's command of that name acts on the selection instead.
/// `help` with an argument is help on that `pma` command.
pub fn resolve<'a>(
    line: &'a str,
    tab: Tab,
    pick: &Pick,
) -> Result<(&'static Binding, &'a str), String> {
    let line = line.trim();
    let (word, arg) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let arg = arg.trim();
    if CLI_TERMINAL.contains(&word) {
        return Err(format!(
            "`{word}` needs a terminal of its own: run `pma {word}`."
        ));
    }
    let jump = word == "project"
        && !PROJECT_SUBCOMMANDS.contains(&arg.split_whitespace().next().unwrap_or(""));
    if CLI.contains(&word) && !arg.is_empty() && !jump {
        return Ok((cli_binding(), line));
    }
    let offered = commands(tab, pick);
    let alias = ALIASES
        .iter()
        .find(|(a, _)| *a == word)
        .map(|(_, c)| c.name());
    let exact = alias.as_deref().unwrap_or(word);
    if let Some((_, b)) = offered.iter().find(|(n, _)| n == exact) {
        return Ok((b, arg));
    }
    // A full name is never read as the prefix of another: `approve-run`
    // off a run must not become `approve-runs`.
    if BINDINGS.iter().any(|b| b.command.name() == exact) {
        return Err(match CLI.contains(&word) {
            true => format!("`{word}` does not apply here; `:{word} <args>` runs `pma {word}`."),
            false => format!("`{word}` does not apply here."),
        });
    }
    if CLI.contains(&word) {
        return Ok((cli_binding(), line));
    }
    let prefixed: Vec<&(String, &Binding)> = offered
        .iter()
        .filter(|(n, _)| n.starts_with(word))
        .collect();
    match prefixed.as_slice() {
        [(_, b)] => Ok((b, arg)),
        [] if word.is_empty() => Err("Type a command; Tab completes.".into()),
        [] => Err(format!("No command `{word}` here.")),
        many => Err(format!(
            "`{word}` could be {}.",
            many.iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every action a front end may take has a key, so no front end can do
    /// what the other has no key for.
    #[test]
    fn every_act_has_a_key() {
        for act in Act::ALL {
            assert!(
                BINDINGS.iter().any(|b| b.command == Command::Act(act)),
                "{act:?} has no key"
            );
        }
        for tab in Tab::ALL.into_iter().filter(|t| *t != Tab::Project) {
            assert!(
                BINDINGS.iter().any(|b| b.command == Command::Tab(tab)),
                "{tab:?} has no key"
            );
        }
        assert!(
            BINDINGS.iter().any(|b| b.command == Command::OpenProject),
            "a project opens from a row"
        );
    }

    /// One key means one thing for a row: two bindings for the same key on
    /// one tab must be acts on rows, and no row can fit both. A tab binding
    /// hiding an everywhere binding would make it ambiguous too.
    #[test]
    fn no_key_means_two_things_for_one_row() {
        let samples = [
            Pick::Nothing,
            Pick::Task {
                project: "p".into(),
                target: "p:1".into(),
                retry: false,
            },
            Pick::Manual {
                project: "p".into(),
            },
            Pick::Project {
                project: "p".into(),
                target: Some("p:ci".into()),
                gone: true,
            },
            Pick::Run {
                id: 1,
                clean: true,
                fixing: false,
                empty: false,
                state: "ready".into(),
                project: "p".into(),
            },
            Pick::Job {
                id: 1,
                awaiting: true,
            },
            Pick::Instance {
                id: 1,
                workflow: "w".into(),
                open: true,
            },
            Pick::Note {
                id: 1,
                text: "t".into(),
            },
            Pick::Campaign { name: "c".into() },
            Pick::Setting {
                key: "k".into(),
                value: "v".into(),
            },
            Pick::Root { path: "/r".into() },
            Pick::Agent { name: "a".into() },
            Pick::Preset {
                name: "p".into(),
                agent: "a".into(),
                model: None,
                args: vec![],
            },
            Pick::Route {
                revision: 1,
                active: false,
            },
            Pick::Scan { id: 1 },
        ];
        for tab in Tab::ALL {
            let on = on_tab(tab);
            let keys: Vec<&str> = on.iter().map(|b| b.key).collect();
            for k in &keys {
                let same: Vec<&&Binding> = on.iter().filter(|b| b.key == *k).collect();
                if same.len() == 1 {
                    continue;
                }
                assert!(
                    same.iter().all(|b| b.tabs.contains(&tab)),
                    "`{k}` on {tab:?} is bound for the tab and for everywhere"
                );
                for pick in &samples {
                    let fitting = same
                        .iter()
                        .filter(|b| {
                            matches!(b.command, Command::Act(a) if a.on_row() && screen::fits(a, pick))
                        })
                        .count();
                    assert!(
                        fitting <= 1,
                        "`{k}` on {tab:?} means two things for {pick:?}"
                    );
                }
            }
        }
    }

    /// Every command has its own name, and no alias hides one.
    #[test]
    fn command_names_are_unique() {
        let mut names: Vec<String> = BINDINGS.iter().map(|b| b.command.name()).collect();
        names.sort();
        names.dedup();
        let distinct: std::collections::HashSet<Command> =
            BINDINGS.iter().map(|b| b.command).collect();
        assert_eq!(names.len(), distinct.len(), "two commands share a name");
        for (alias, _) in ALIASES {
            assert!(!names.iter().any(|n| n == alias), "`{alias}` is a name");
        }
        assert_eq!(Command::Act(Act::ApproveRun).name(), "approve-run");
        assert_eq!(Command::Act(Act::Pr).name(), "pr");
    }

    /// A `pma` command's name runs the CLI when arguments follow it or
    /// nothing here has that name; bare, the front end's command keeps it.
    #[test]
    fn a_line_names_a_pma_command() {
        let got = |line: &str, tab: Tab| {
            resolve(line, tab, &Pick::Nothing).map(|(b, arg)| (b.command, arg.to_string()))
        };
        let cli = |line: &str| Ok((Command::Act(Act::Cli), line.to_string()));
        assert_eq!(got(" prune  argdec ", Tab::Inbox), cli("prune  argdec"));
        assert_eq!(got("prune argdec -y", Tab::Project), cli("prune argdec -y"));
        assert_eq!(
            got("prune", Tab::Project),
            Ok((Command::Act(Act::Prune), String::new())),
            "bare, the open project's"
        );
        assert_eq!(
            got("prune", Tab::Inbox),
            Err("`prune` does not apply here; `:prune <args>` runs `pma prune`.".into())
        );
        assert_eq!(got("status", Tab::Inbox), cli("status"));
        assert_eq!(got("stale --tag x", Tab::Inbox), cli("stale --tag x"));
        assert_eq!(
            got("history", Tab::Inbox),
            Ok((Command::Tab(Tab::History), String::new()))
        );
        assert_eq!(
            got("project argdec", Tab::Inbox),
            Ok((Command::Jump, "argdec".into())),
            "a project's name opens it"
        );
        assert_eq!(
            got("project tier 2 argdec", Tab::Inbox),
            cli("project tier 2 argdec")
        );
        assert_eq!(
            got("help prune", Tab::Inbox),
            Ok((Command::Help, "prune".into()))
        );
        assert!(
            got("tui", Tab::Inbox)
                .unwrap_err()
                .contains("terminal of its own")
        );
        assert!(complete("sta", Tab::Inbox, &Pick::Nothing).contains(&"status".to_string()));
    }

    /// `:help` names what each command acts on here: the project page's
    /// project, the selected row as `%`, else every project.
    #[test]
    fn help_lists_what_each_command_acts_on_here() {
        let find = |rows: &[(String, String, &str)], typed: &str| {
            rows.iter().find(|r| r.0 == typed).map(|r| r.1.clone())
        };
        let page = commands_here("argdec", &Pick::Nothing);
        assert_eq!(find(&page, ":prune").as_deref(), Some("argdec's TODO.md"));
        assert_eq!(
            find(&page, ":status").as_deref(),
            Some("argdec; -A for every project")
        );
        assert_eq!(find(&page, ":shell").as_deref(), Some("argdec"));
        assert_eq!(find(&page, ":push %"), None, "no run selected");
        assert_eq!(find(&page, ":config"), None, "no project");
        let inbox = commands_here("", &Pick::Nothing);
        assert_eq!(find(&inbox, ":status").as_deref(), Some("every project"));
        assert_eq!(find(&inbox, ":prune"), None, "no TODO.md here");
        let run = Pick::Run {
            id: 51,
            clean: true,
            fixing: false,
            empty: false,
            state: "ready".into(),
            project: "argdec".into(),
        };
        assert_eq!(
            find(&commands_here("", &run), ":push %").as_deref(),
            Some("run #51")
        );
        assert_eq!(
            here("prune", Some(("argdec", "cwd"))).as_deref(),
            Some("Here: argdec's TODO.md (cwd) when no path is named.")
        );
        assert_eq!(here("config", None), None);
    }

    #[test]
    fn a_line_resolves_by_name_alias_or_unique_prefix() {
        let run = Pick::Run {
            id: 1,
            clean: true,
            fixing: false,
            empty: false,
            state: "ready".into(),
            project: "p".into(),
        };
        let got = |line: &str, tab: Tab, pick: &Pick| {
            resolve(line, tab, pick).map(|(b, arg)| (b.command, arg.to_string()))
        };
        assert_eq!(
            got("q", Tab::Inbox, &Pick::Nothing),
            Ok((Command::Quit, String::new()))
        );
        assert_eq!(
            got("push", Tab::Inbox, &run),
            Ok((Command::Act(Act::Push), String::new())),
            "exact beats `push-approved`"
        );
        assert_eq!(
            got("  rework  try again ", Tab::Inbox, &run),
            Ok((Command::Act(Act::ReworkRun), "try again".into()))
        );
        assert_eq!(
            got("proj", Tab::Inbox, &run).map(|(c, _)| c),
            Err("`proj` could be projects, project.".into())
        );
        assert_eq!(
            got("approve-run", Tab::Inbox, &Pick::Nothing),
            Err("`approve-run` does not apply here.".into()),
            "a row act needs its row"
        );
        assert_eq!(
            got("history", Tab::Inbox, &Pick::Nothing),
            Ok((Command::Tab(Tab::History), String::new()))
        );
        assert!(got("", Tab::Inbox, &Pick::Nothing).is_err());
        assert!(!complete("", Tab::Inbox, &Pick::Nothing).contains(&"command".to_string()));
        assert_eq!(
            complete(
                "dispatch",
                Tab::Inbox,
                &Pick::Task {
                    project: "p".into(),
                    target: "p:1".into(),
                    retry: false,
                }
            ),
            ["dispatch", "dispatch-push", "dispatch-interactive"]
        );
    }

    #[test]
    fn stage_and_commit_stay_on_their_run() {
        assert!(Act::Push.decides() && Act::ApproveRun.decides());
        assert!(!Act::Stage.decides() && !Act::Commit.decides());
    }

    #[test]
    fn keys_are_plain() {
        for b in BINDINGS {
            assert!(
                b.key.chars().count() == 1 || b.key.chars().all(|c| c.is_ascii_alphabetic()),
                "{}",
                b.key
            );
        }
    }

    #[test]
    fn lookup_prefers_the_tab_then_the_row() {
        let job = Pick::Job {
            id: 1,
            awaiting: true,
        };
        let run = Pick::Run {
            id: 1,
            clean: true,
            fixing: false,
            empty: false,
            state: "ready".into(),
            project: "p".into(),
        };
        assert_eq!(
            lookup("a", Tab::Activity, &Pick::Nothing).map(|b| b.command),
            Some(Command::Act(Act::ApproveJob))
        );
        assert_eq!(
            lookup("a", Tab::Inbox, &run).map(|b| b.command),
            Some(Command::Act(Act::ApproveRun))
        );
        assert_eq!(
            lookup("a", Tab::Inbox, &job).map(|b| b.command),
            Some(Command::Act(Act::ApproveJob))
        );
        assert_eq!(
            lookup("a", Tab::Inbox, &Pick::Nothing).map(|b| b.command),
            Some(Command::Act(Act::ApproveRun)),
            "the first, so the error names a run"
        );
        assert!(lookup("a", Tab::History, &run).is_none());
        assert_eq!(
            lookup("c", Tab::History, &Pick::Nothing).map(|b| b.command),
            Some(Command::Act(Act::Scan))
        );
    }
}
