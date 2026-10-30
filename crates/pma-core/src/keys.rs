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
    /// What waits on you, what is under way, and what agents can take.
    Inbox,
    /// Every project, worst first.
    Projects,
    /// Jobs, running agents and workflow instances, newest first.
    Activity,
    Matrix,
    /// Workflow revisions and campaigns: what can be run.
    Library,
    Report,
    Notes,
    /// Settings, roots, agents, presets and route revisions.
    Settings,
    /// One project, opened from a row that names it. It has no key of its
    /// own: which project is the front end's to remember.
    Project,
}

impl Tab {
    pub const ALL: [Tab; 9] = [
        Tab::Inbox,
        Tab::Projects,
        Tab::Activity,
        Tab::Matrix,
        Tab::Library,
        Tab::Report,
        Tab::Notes,
        Tab::Settings,
        Tab::Project,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Inbox => "inbox",
            Tab::Projects => "projects",
            Tab::Activity => "activity",
            Tab::Matrix => "matrix",
            Tab::Library => "library",
            Tab::Report => "report",
            Tab::Notes => "notes",
            Tab::Settings => "settings",
            Tab::Project => "project",
        }
    }

    /// What the tab shows. Lists are not cut short: a front end scrolls.
    /// `project` names the project the project tab shows; `finished`, the
    /// state the activity tab's finished runs are filtered to.
    pub fn query(self, by: By, project: &str, finished: Option<RunState>) -> Query {
        let of = Selection::default();
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
            Tab::Library => Query::Library,
            Tab::Report => Query::Report { by },
            Tab::Notes => Query::Notes,
            Tab::Settings => Query::Settings,
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
}

impl Act {
    pub const ALL: [Act; 46] = [
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
        Act::ResumeWorkflow,
        Act::StartWorkflow,
        Act::ReworkRun,
        Act::RetryRun,
        Act::Stage,
        Act::Commit,
        Act::Pr,
        Act::Push,
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
    ];

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
                | Act::StartWorkflow
                | Act::Run
                | Act::AddRoot
                | Act::AddPreset
                | Act::ProposeRoute
                | Act::AddCampaign
                | Act::Configure
                | Act::Prune
                | Act::Prompt
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
            | Act::Merge
            | Act::ResetSetting
            | Act::RemoveRoot
            | Act::RemoveAgent
            | Act::RemovePreset
            | Act::RemoveCampaign
            | Act::Prune => "danger",
            _ => "default",
        }
    }

    /// The kinds an action can be. A new `Action` variant fails to compile
    /// here until it has a kind, and the tests then require it a key.
    pub fn of(a: &Action) -> &'static [Act] {
        match a {
            Action::ApproveRun { .. } => &[Act::ApproveRun],
            Action::ApproveRuns { .. } => &[Act::ApproveRuns],
            Action::RejectRun { .. } => &[Act::RejectRun],
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
            Action::Dispatch { .. } => &[Act::Dispatch, Act::Prompt],
            Action::WorkflowRun { .. } => &[Act::ResumeWorkflow, Act::StartWorkflow],
            Action::ReworkRun { .. } => &[Act::ReworkRun],
            Action::RetryRun { .. } => &[Act::RetryRun],
            Action::Run { .. } => &[Act::Run],
            Action::Publish { .. } => &[Act::Stage, Act::Commit, Act::Pr, Act::Push],
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
            Action::SetTier { .. } | Action::Tag { .. } => &[Act::Configure],
            Action::Prune { .. } | Action::FixTodo { .. } => &[Act::Prune],
            Action::RefreshDeps { .. } => &[Act::ScanProject],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Marks the selected task, or unmarks it, for a dispatch of several.
    Mark,
    Help,
    /// The TUI only; a browser tab is closed by the browser.
    Quit,
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

const TASKS: &[Tab] = &[Tab::Inbox, Tab::Matrix, Tab::Project];
const PANES: &[Tab] = &[
    Tab::Inbox,
    Tab::Activity,
    Tab::Matrix,
    Tab::Library,
    Tab::Project,
];
const RUNS: &[Tab] = &[Tab::Inbox, Tab::Project];
const JOBS: &[Tab] = &[Tab::Inbox, Tab::Activity, Tab::Project];

pub const BINDINGS: &[Binding] = &[
    bind("1", Command::Tab(Tab::Inbox), "inbox"),
    bind("2", Command::Tab(Tab::Projects), "projects"),
    bind("3", Command::Tab(Tab::Activity), "activity"),
    bind("4", Command::Tab(Tab::Matrix), "matrix"),
    bind("5", Command::Tab(Tab::Library), "library"),
    bind("6", Command::Tab(Tab::Report), "report"),
    bind("7", Command::Tab(Tab::Notes), "notes"),
    bind("8", Command::Tab(Tab::Settings), "settings"),
    bind("j", Command::Down, "down"),
    bind("ArrowDown", Command::Down, "down"),
    bind("k", Command::Up, "up"),
    bind("ArrowUp", Command::Up, "up"),
    bind("g", Command::First, "first"),
    bind("Home", Command::First, "first"),
    bind("G", Command::Last, "last"),
    bind("End", Command::Last, "last"),
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
            Tab::Library,
            Tab::Settings,
            Tab::Project,
        ],
        ..bind("Enter", Command::Open, "open")
    },
    Binding {
        tabs: &[Tab::Inbox, Tab::Projects, Tab::Activity, Tab::Matrix],
        ..bind("o", Command::OpenProject, "open project")
    },
    bind("Escape", Command::Back, "back"),
    Binding {
        tabs: &[Tab::Report],
        ..bind("b", Command::ReportBy, "by")
    },
    Binding {
        tabs: &[Tab::Activity],
        ..bind("b", Command::Finished, "finished state")
    },
    bind("?", Command::Help, "keys"),
    bind("q", Command::Quit, "quit"),
    // Anywhere.
    on("c", &[], Act::Scan, Ask::Nothing, "scan"),
    on("i", &[], Act::Sync, Ask::Confirm, "sync issues"),
    on("n", &[], Act::AddNote, Ask::Text("note"), "add note"),
    on(
        "r",
        &[Tab::Project],
        Act::ScanProject,
        Ask::Choose("rescan"),
        "rescan project",
    ),
    on(
        "e",
        &[Tab::Project],
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
        tabs: TASKS,
        ..bind(" ", Command::Mark, "mark")
    },
    on("d", TASKS, Act::Dispatch, Ask::Confirm, "dispatch"),
    on("v", TASKS, Act::Verify, Ask::Nothing, "verify project"),
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
    on("s", RUNS, Act::Stage, Ask::Confirm, "stage"),
    on(
        "m",
        RUNS,
        Act::Commit,
        Ask::Text("commit subject"),
        "commit",
    ),
    on("p", RUNS, Act::Pr, Ask::Confirm, "pull request"),
    on("u", RUNS, Act::Push, Ask::Confirm, "push"),
    on("y", RUNS, Act::SettlePrs, Ask::Nothing, "check PRs"),
    on("M", RUNS, Act::Merge, Ask::Confirm, "merge PR"),
    on("F", RUNS, Act::FixConflicts, Ask::Confirm, "fix conflicts"),
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
        &[Tab::Activity, Tab::Library],
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
    // A campaign.
    on(
        "r",
        &[Tab::Library],
        Act::RunCampaign,
        Ask::Confirm,
        "run campaign",
    ),
    on(
        "a",
        &[Tab::Library],
        Act::AddCampaign,
        Ask::Text("name project,project text... [--class=A]"),
        "add campaign",
    ),
    on(
        "x",
        &[Tab::Library],
        Act::RemoveCampaign,
        Ask::Confirm,
        "remove",
    ),
    // A note.
    on("e", &[Tab::Notes], Act::EditNote, Ask::Text("note"), "edit"),
    on("x", &[Tab::Notes], Act::RemoveNote, Ask::Confirm, "remove"),
    // Settings. `e`, `x` and `a` each mean one thing per row kind.
    on(
        "e",
        &[Tab::Settings],
        Act::EditSetting,
        Ask::Text("value"),
        "edit",
    ),
    on(
        "e",
        &[Tab::Settings],
        Act::EditAgent,
        Ask::Text("field value"),
        "set field",
    ),
    on(
        "e",
        &[Tab::Settings],
        Act::EditPreset,
        Ask::Text("agent[:model] [args...]"),
        "edit",
    ),
    on(
        "x",
        &[Tab::Settings],
        Act::ResetSetting,
        Ask::Confirm,
        "reset",
    ),
    on(
        "x",
        &[Tab::Settings],
        Act::RemoveRoot,
        Ask::Confirm,
        "remove",
    ),
    on(
        "x",
        &[Tab::Settings],
        Act::RemoveAgent,
        Ask::Confirm,
        "remove",
    ),
    on(
        "x",
        &[Tab::Settings],
        Act::RemovePreset,
        Ask::Confirm,
        "remove",
    ),
    on(
        "a",
        &[Tab::Settings],
        Act::UseAgent,
        Ask::Nothing,
        "make default",
    ),
    on(
        "a",
        &[Tab::Settings],
        Act::UsePreset,
        Ask::Nothing,
        "make default",
    ),
    on(
        "a",
        &[Tab::Settings],
        Act::ActivateRoute,
        Ask::Confirm,
        "activate",
    ),
    on(
        "r",
        &[Tab::Settings],
        Act::AddRoot,
        Ask::Text("directory"),
        "add root",
    ),
    on(
        "p",
        &[Tab::Settings],
        Act::AddPreset,
        Ask::Text("name agent[:model] [args...]"),
        "add preset",
    ),
    on(
        "f",
        &[Tab::Settings],
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
            Pick::Run {
                id: 1,
                clean: true,
                fixing: false,
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
        assert!(lookup("a", Tab::Notes, &run).is_none());
        assert_eq!(
            lookup("c", Tab::Notes, &Pick::Nothing).map(|b| b.command),
            Some(Command::Act(Act::Scan))
        );
    }
}
