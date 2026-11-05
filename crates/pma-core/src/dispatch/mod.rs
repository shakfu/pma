//! Dispatch and review: a worktree per task, agent runs within the batch
//! budget, verification by `pma` itself, and the review actions.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use crate::agent;
use crate::class::Class;
use crate::complexity::{self, Features};
use crate::config::{Config, Workspace};
use crate::rank::Quadrant;
use crate::route::{Policy, Subject};
use crate::scan;
use crate::store::{Attempt, Result, Run, RunItem, RunState, Store};
use crate::todo::{self, Priority};
use crate::worker::Worker;

mod attempt;
mod git;
mod prepare;
mod review;
mod verify;

pub use attempt::*;
pub use git::*;
pub use prepare::*;
pub use review::*;
pub use verify::*;

/// A task chosen for dispatch.
#[derive(Debug, Clone)]
pub struct Pick {
    /// The workflow unit this dispatch serves, where one does. A route
    /// matches on the node (W13) and a report groups by it.
    pub workflow: Option<UnitRef>,
    pub project: String,
    pub repo: PathBuf,
    /// The item's key, or a signal: `ci` or `deps`.
    pub key: String,
    pub text: String,
    pub gh: Option<i64>,
    /// The project's tier, recorded on the run because tiers change.
    pub tier: Option<u8>,
    /// Stated by a campaign, which has no item to read tags from.
    pub class: Option<Class>,
    /// Extra prompt lines a campaign carries.
    pub details: Option<String>,
    pub quadrant: Option<String>,
    /// Start from the clone's `HEAD` rather than the fetched default branch.
    /// A local run never reaches the remote.
    pub local: bool,
    /// A person drives the agent in a herdr pane.
    pub interactive: bool,
    /// Where the agent edits. `Clone` ignores `local`: a clone run always
    /// starts from the working tree, and waits there for review.
    pub workspace: Workspace,
    /// A bundle's items, when `key` is `BUNDLE`; empty otherwise.
    pub items: Vec<RunItem>,
}

/// The key of a bundle: one run over several TODO.md items of a project
/// (`docs/dev/bundled-dispatch.md`).
pub const BUNDLE: &str = "bundle";

/// The key of a run over a prompt the developer typed, which names no
/// TODO.md item.
pub const PROMPT: &str = "prompt";

/// What a prompt's run is called: its first line, at most 80 characters.
pub fn prompt_title(prompt: &str) -> String {
    let line = prompt.trim().lines().next().unwrap_or("");
    match line.char_indices().nth(80) {
        Some((at, _)) => format!("{}...", line[..at].trim_end()),
        None => line.to_string(),
    }
}

/// Items one bundle may hold. Its budget and timeout are those of one task.
pub const BUNDLE_MAX: usize = 8;

/// What a bundle's run is called: its items' texts.
pub fn bundle_text(items: &[RunItem]) -> String {
    items
        .iter()
        .map(|i| i.text.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

/// The TODO.md items a run marks done when published, as key, text and
/// issue: a bundle's items the approver did not drop, or the run's own item.
pub fn closes(run: &Run) -> Vec<(&str, &str, Option<i64>)> {
    if run.task_key == BUNDLE {
        return run
            .items
            .iter()
            .filter(|i| !i.dropped)
            .map(|i| (i.key.as_str(), i.text.as_str(), i.gh))
            .collect();
    }
    match without_item(&run.task_key) {
        true => Vec::new(),
        false => vec![(run.task_key.as_str(), run.text.as_str(), run.gh)],
    }
}

/// Which workflow unit a dispatch serves.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UnitRef {
    pub instance: i64,
    /// The qualified node name.
    pub node: String,
    pub unit: String,
    pub lap: i64,
}

/// An agent and a model named on the command line. Both override the
/// configuration and an applied route: a flag is the most recent statement of
/// intent there is, and it covers the whole invocation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overrides {
    pub agent: Option<String>,
    pub model: Option<String>,
    /// A preset named on the command line. Its fields sit below the flags and
    /// above an applied route, because naming one is a statement of intent
    /// about this invocation and a route is policy about the work.
    pub preset: Option<crate::store::Preset>,
}

/// What a target names after the colon.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// No colon: the project's open tasks, chosen from a list.
    Project,
    /// A TODO.md line from the last scan.
    Line(i64),
    /// `ci` or `deps`.
    Signal(String),
    /// Every open item under that heading.
    Priority(Priority),
    /// Every task the last scan placed in that quadrant.
    Quadrant(Quadrant),
}

impl Target {
    /// Whether it names at most one task, so a task it cannot take is an
    /// error rather than something to pass over.
    pub fn is_one(&self) -> bool {
        matches!(self, Target::Line(_) | Target::Signal(_))
    }
}

/// Splits `project:what`, or reads a bare project name.
pub fn target(s: &str) -> Result<(&str, Target)> {
    let Some((project, what)) = s.rsplit_once(':') else {
        return match s.is_empty() {
            true => Err("empty target; name a project such as `cynn`".into()),
            false => Ok((s, Target::Project)),
        };
    };
    if project.is_empty() {
        return Err(format!("`{s}`: no project before the colon").into());
    }
    let target = match what {
        "ci" | "deps" => Target::Signal(what.to_string()),
        _ => {
            if let Ok(line) = what.parse::<i64>() {
                Target::Line(line)
            } else if let Some(p) = Priority::parse(what) {
                Target::Priority(p)
            } else if let Some(q) = Quadrant::parse(what) {
                Target::Quadrant(q)
            } else {
                return Err(format!(
                    "`{s}`: after the colon expected a TODO.md line, ci, deps, \
                     critical, high, medium, low, or q1 to q4"
                )
                .into());
            }
        }
    };
    Ok((project, target))
}

/// Whether a task key names a signal rather than a TODO.md item.
pub fn is_signal(key: &str) -> bool {
    matches!(key, "ci" | "deps")
}

/// A task with no line in any `TODO.md`: a signal, or a campaign applying one
/// definition across repositories. There is nothing to check on origin before
/// dispatch and nothing to tick when it is published.
pub fn without_item(key: &str) -> bool {
    is_signal(key)
        || key == BUNDLE
        || key == PROMPT
        || key.starts_with("campaign:")
        || key.starts_with("workflow:")
}

/// Consumed attempts after which a task revision is not dispatched again.
/// Two independent negative signals: an agent that cannot pass the check, or
/// one that passed it and produced work a reviewer refused.
pub const ATTEMPT_LIMIT: i64 = 2;

/// A task's identity for the attempt counter. The normalised text, so a
/// reworded specification starts fresh and a `ci` incident is identified by
/// the workflows it names rather than by the word `ci`, which recurs.
pub fn revision(text: &str) -> String {
    todo::normal_text(text)
}

/// What a run's attempts count against. A workflow unit counts against its
/// lineage, `workflow:<instance>:<root>`, so a retry and a lap draw from one
/// budget (W23); anything else counts against its task text.
pub fn attempt_key(run: &Run) -> String {
    match run.task_key.starts_with("workflow:") {
        true => run.task_key.clone(),
        // A refused refresh says an update broke the check, which is the
        // agent's task, not that an agent cannot do it.
        false if run.agent == crate::worker::REFRESH => format!("{} #refresh", revision(&run.text)),
        false => revision(&run.text),
    }
}

/// Whether this attempt counts against the limit. A run that never reached
/// the agent, or whose agent could not start or was killed at the timeout,
/// says nothing about the task's suitability.
fn consumes(run: &Run) -> bool {
    run.state == RunState::Ready && run.verify_ok == Some(false)
}

/// The tail of the check's output from the project's last `refresh` run,
/// when that check failed. `None` when the last refresh passed, or there was
/// none.
fn failed_refresh(store: &Store, home: &Path, project: &str) -> Result<Option<String>> {
    let last = store
        .runs()?
        .into_iter()
        .filter(|r| {
            r.project == project && r.task_key == "deps" && r.agent == crate::worker::REFRESH
        })
        .max_by_key(|r| r.id);
    let Some(run) = last.filter(|r| r.verify_ok == Some(false)) else {
        return Ok(None);
    };
    let logs = home.join("runs").join(run.id.to_string());
    let newest = (1..)
        .map(|n| logs.join(format!("verify-{n}.log")))
        .take_while(|p| p.is_file())
        .last();
    Ok(newest
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| crate::agent::tail(&text, CI_LOG_LINES)))
}

/// Lines of failed CI log carried in a fix-CI prompt.
const CI_LOG_LINES: usize = 200;

/// Lowercase words of the task text joined by `-`, at most 40 bytes.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for word in text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if out.len() + word.len() + 1 > 40 {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&word.to_ascii_lowercase());
    }
    if out.is_empty() { "task".into() } else { out }
}

/// Why `run` must be approved on its own before it is published: a ready
/// bundle, whose approver has not yet said which items it finished.
/// Publishing it as it is would tick every item.
pub fn needs_approval(run: &Run) -> Option<String> {
    (run.state == RunState::Ready && !run.items.is_empty()).then(|| {
        format!(
            "run #{} is a bundle; approve it first, leaving open the items it did \
             not finish (`pma review {} --approve --drop N`)",
            run.id, run.id
        )
    })
}

/// Refuses a list of bundle items to leave open, numbered from 1, that
/// names an item the run lacks or every item it has.
pub fn check_drops(run: &Run, drop: &[usize]) -> Result<()> {
    if drop.is_empty() {
        return Ok(());
    }
    let n = run.items.len();
    if n == 0 {
        return Err(format!("run #{} is not a bundle; it has no items to drop", run.id).into());
    }
    if let Some(bad) = drop.iter().find(|i| **i == 0 || **i > n) {
        return Err(format!("run #{} has items 1 to {n}; there is no item {bad}", run.id).into());
    }
    if (1..=n).all(|i| drop.contains(&i)) {
        return Err(format!(
            "every item of run #{} is dropped; reject the run instead",
            run.id
        )
        .into());
    }
    Ok(())
}

pub fn path_arg(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_names_a_project_a_line_a_signal_a_heading_or_a_quadrant() {
        let parse = |s| {
            let (project, what) = target(s).unwrap();
            (project.to_string(), what)
        };
        assert_eq!(parse("cynn"), ("cynn".into(), Target::Project));
        assert_eq!(parse("cynn:31"), ("cynn".into(), Target::Line(31)));
        assert_eq!(
            parse("cynn:ci"),
            ("cynn".into(), Target::Signal("ci".into()))
        );
        assert_eq!(
            parse("cynn:critical"),
            ("cynn".into(), Target::Priority(Priority::Critical))
        );
        assert_eq!(
            parse("cynn:q1"),
            ("cynn".into(), Target::Quadrant(Quadrant::Q1))
        );
        assert_eq!(
            parse("cynn:Q4"),
            ("cynn".into(), Target::Quadrant(Quadrant::Q4))
        );
    }

    /// Only a target naming one task fails the batch when it cannot run.
    #[test]
    fn one_task_or_many() {
        assert!(Target::Line(3).is_one());
        assert!(Target::Signal("deps".into()).is_one());
        assert!(!Target::Priority(Priority::Low).is_one());
        assert!(!Target::Quadrant(Quadrant::Q2).is_one());
        assert!(!Target::Project.is_one());
    }

    #[test]
    fn an_unreadable_target_says_what_is_accepted() {
        let err = |s| target(s).unwrap_err().to_string();
        assert!(err("cynn:soon").contains("expected a TODO.md line, ci, deps, critical"));
        assert!(err("cynn:q5").contains("expected a TODO.md line"));
        assert!(err(":31").contains("no project before the colon"));
        assert!(err("").contains("name a project"));
    }

    #[test]
    fn slugs_are_short_ascii_words() {
        assert_eq!(
            slug("Fix the `Lexer` crash on Ctrl-C!"),
            "fix-the-lexer-crash-on-ctrl-c"
        );
        assert_eq!(slug("::"), "task");
        assert_eq!(
            slug("add a context manager for every network class and its trainer"),
            "add-a-context-manager-for-every-network"
        );
        assert_eq!(slug(&"x".repeat(60)), "task");
    }

    pub(super) fn bundle(dropped: &[usize]) -> Run {
        let item = |n: usize, key: &str, gh| RunItem {
            key: key.into(),
            text: format!("task {key}"),
            gh,
            dropped: dropped.contains(&n),
        };
        Run {
            id: 4,
            task_key: BUNDLE.into(),
            text: "task a; task b; task c".into(),
            items: vec![
                item(1, "a", Some(7)),
                item(2, "b", None),
                item(3, "c", Some(9)),
            ],
            ..Run::blank()
        }
    }

    #[test]
    fn a_bundle_closes_what_its_approver_kept() {
        let run = bundle(&[2]);
        assert_eq!(
            closes(&run),
            [("a", "task a", Some(7)), ("c", "task c", Some(9))]
        );
        let one = Run {
            gh: Some(3),
            ..Run::blank()
        };
        assert_eq!(closes(&one), [("fix it", "Fix it", Some(3))]);
        let signal = Run {
            task_key: "ci".into(),
            ..Run::blank()
        };
        assert!(closes(&signal).is_empty());
    }

    /// A ready bundle is published only after an approval that says which
    /// items it finished; an approved one, or a plain ready run, goes on.
    #[test]
    fn a_ready_bundle_needs_its_own_approval() {
        let ready = |run: Run| Run {
            state: RunState::Ready,
            ..run
        };
        assert!(needs_approval(&ready(bundle(&[]))).is_some());
        assert!(needs_approval(&ready(Run::blank())).is_none());
        let approved = Run {
            state: RunState::Approved,
            ..bundle(&[])
        };
        assert!(needs_approval(&approved).is_none());
    }

    #[test]
    fn drops_name_items_the_bundle_has_and_leave_one() {
        let run = bundle(&[]);
        assert!(check_drops(&run, &[]).is_ok());
        assert!(check_drops(&run, &[1, 3]).is_ok());
        let err = check_drops(&run, &[4]).unwrap_err().to_string();
        assert_eq!(err, "run #4 has items 1 to 3; there is no item 4");
        assert!(check_drops(&run, &[0]).is_err());
        let err = check_drops(&run, &[1, 2, 3]).unwrap_err().to_string();
        assert!(err.contains("reject the run instead"), "{err}");
        let err = check_drops(&Run::blank(), &[1]).unwrap_err().to_string();
        assert!(err.contains("is not a bundle"), "{err}");
    }

    /// Only a run that reached the agent and failed its check says anything
    /// about the task's suitability.
    #[test]
    fn an_attempt_counts_only_when_the_check_decided_against_it() {
        let ready = |verify_ok| Run {
            state: RunState::Ready,
            verify_ok,
            ..Run::blank()
        };
        assert!(consumes(&ready(Some(false))));
        assert!(!consumes(&ready(Some(true))), "an accepted check");
        assert!(!consumes(&ready(None)), "verify never ran");
        for error in [
            "not started: batch budget $5 reached",
            "timed out",
            "claude: no such file",
        ] {
            let run = Run {
                state: RunState::Failed,
                error: Some(error.into()),
                ..Run::blank()
            };
            assert!(!consumes(&run), "{error}");
        }
    }

    /// A workflow unit's attempts count against its lineage, so every node and
    /// lap spending on it draws from one counter; a task's count against its
    /// text.
    #[test]
    fn a_workflow_unit_counts_attempts_against_its_lineage() {
        let unit = Run {
            task_key: "workflow:3:u1".into(),
            text: "Fix the parser".into(),
            ..Run::default()
        };
        assert_eq!(attempt_key(&unit), "workflow:3:u1");
        let task = Run {
            task_key: "fix the parser".into(),
            text: "Fix  the Parser".into(),
            ..Run::default()
        };
        assert_eq!(attempt_key(&task), "fix the parser");
        let refresh = Run {
            agent: crate::worker::REFRESH.into(),
            ..task.clone()
        };
        assert_eq!(attempt_key(&refresh), "fix the parser #refresh");
    }

    #[test]
    fn a_revision_ignores_wording_that_does_not_change_the_task() {
        assert_eq!(revision("Fix  the PARSER"), revision("fix the parser"));
        assert_ne!(revision("fix CI: build"), revision("fix CI: build, test"));
    }
}
