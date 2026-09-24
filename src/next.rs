//! `pma next`: what should happen next, and who does it. One ordered list for
//! agents, in the order `pma dispatch --auto` takes it, and one for you: runs
//! waiting on a decision, then tasks no agent may take that are critical or
//! urgent.
//!
//! Nothing here ranks anew. The order is the matrix's, and what an agent may
//! take is what dispatch decides, so this view and `--auto` cannot disagree.

use crate::rank::Placed;
use crate::report;
use crate::store::{Run, RunState};
use crate::todo::Priority;

/// Where an eligible task stands with dispatch, as `--auto` sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taken {
    /// Nothing holds it: `--auto` would take it.
    Free,
    /// A run that is not shipped or rejected holds it. It shows as that run.
    Held,
    /// Its attempts were used without an accepted result.
    Exhausted(i64),
    /// No scan finds its project, so nothing can be dispatched against it.
    Absent,
}

/// One row of either list: its cells, and the command that acts on it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub cells: Vec<String>,
    pub command: Option<String>,
}

#[derive(Debug, Default)]
pub struct Next {
    /// Tasks an agent may take, in dispatch order.
    pub agents: Vec<Row>,
    /// Runs waiting on you: to review, to ship, or a pull request to merge.
    pub runs: Vec<Row>,
    /// Tasks no agent may take, or may take no more, that are critical or
    /// urgent.
    pub tasks: Vec<Row>,
}

/// The two lists. `placed` is in matrix order; `taken` says what dispatch
/// would do with an eligible task.
pub fn build(
    placed: &[Placed],
    runs: &[Run],
    today: i64,
    taken: impl Fn(&Placed) -> Taken,
) -> Next {
    let mut next = Next::default();
    for p in placed {
        let t = &p.task;
        let at = match t.line {
            Some(line) => format!("{}:{line}", t.project),
            None => format!("{}:{}", t.project, t.key.as_deref().unwrap_or("")),
        };
        let cells = |why: &str| {
            let mut cells = vec![
                at.clone(),
                format!("T{}", t.tier),
                t.priority.name().to_string(),
                report::when(p, today),
            ];
            if !why.is_empty() {
                cells.push(why.to_string());
            }
            cells.push(report::truncate(&t.text, 80));
            cells
        };
        let needs_you = t.priority == Priority::Critical || p.urgency.is_some();
        match (t.eligible && t.key.is_some(), taken(p)) {
            (_, Taken::Absent | Taken::Held) => {}
            (true, Taken::Free) => next.agents.push(Row {
                cells: cells(""),
                command: Some(format!("pma dispatch {at}")),
            }),
            (true, Taken::Exhausted(n)) if needs_you => next.tasks.push(Row {
                cells: cells(&format!("{n} attempts used")),
                command: Some(format!("pma dispatch {at} --retry")),
            }),
            (false, _) if needs_you && t.line.is_some() => next.tasks.push(Row {
                cells: cells("not #agent"),
                command: None,
            }),
            _ => {}
        }
    }

    // A run you decide blocks its task, so the ones that cost least to settle
    // come first: read and decide, then ship, then merge.
    let mut waiting: Vec<&Run> = runs
        .iter()
        .filter(|r| {
            matches!(
                r.state,
                RunState::Ready | RunState::Failed | RunState::Approved | RunState::PrOpen
            )
        })
        .collect();
    let order = |r: &Run| match r.state {
        RunState::Ready | RunState::Failed => 0,
        RunState::Approved => 1,
        _ => 2,
    };
    waiting.sort_by_key(|r| (order(r), r.id));
    for r in waiting {
        let (what, command) = match r.state {
            RunState::Approved => ("to ship", "pma ship".to_string()),
            RunState::PrOpen => (
                "to merge",
                r.outcome
                    .clone()
                    .unwrap_or_else(|| "its pull request".into()),
            ),
            _ => ("to review", format!("pma review {}", r.id)),
        };
        next.runs.push(Row {
            cells: vec![
                format!("#{}", r.id),
                r.state.name().to_string(),
                what.to_string(),
                r.project.clone(),
                report::truncate(&r.text, 80),
            ],
            command: Some(command),
        });
    }
    next
}

/// The two lists as text. `limit` caps the rows of each, and the count says
/// what was left out.
pub fn render(next: &Next, limit: Option<usize>) -> String {
    let section = |rows: &[Row], indent: &str| -> String {
        let shown = limit.map_or(rows.len(), |l| l.min(rows.len()));
        let cells: Vec<Vec<String>> = rows[..shown].iter().map(|r| r.cells.clone()).collect();
        let mut out = report::table(&cells, indent);
        if shown < rows.len() {
            out.push_str(&format!("{indent}and {} more\n", rows.len() - shown));
        }
        out
    };
    let mut out = String::new();
    out.push_str(&format!("For agents: {}\n", next.agents.len()));
    match next.agents.is_empty() {
        true => out.push_str("  nothing an agent may take; tag an item #agent\n"),
        false => {
            out.push_str(&section(&next.agents, "  "));
            out.push_str("  `pma dispatch --auto` takes them in this order\n");
        }
    }
    out.push_str(&format!(
        "\nFor you: {}\n",
        next.runs.len() + next.tasks.len()
    ));
    if next.runs.is_empty() && next.tasks.is_empty() {
        out.push_str("  nothing waits on you\n");
    }
    if !next.runs.is_empty() {
        out.push_str(&format!("  runs: {}\n", next.runs.len()));
        let rows: Vec<Row> = next
            .runs
            .iter()
            .map(|r| Row {
                cells: r.cells.iter().cloned().chain(r.command.clone()).collect(),
                command: None,
            })
            .collect();
        out.push_str(&section(&rows, "    "));
    }
    if !next.tasks.is_empty() {
        out.push_str(&format!("  tasks: {}\n", next.tasks.len()));
        out.push_str(&section(&next.tasks, "    "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rank::{Quadrant, Task, Urgency};

    fn placed(project: &str, line: Option<i64>, priority: Priority, eligible: bool) -> Placed {
        Placed {
            task: Task {
                project: project.into(),
                tier: 1,
                priority,
                text: format!("text of {project}"),
                line,
                key: Some(match line {
                    Some(_) => format!("text of {project}"),
                    None => "ci".into(),
                }),
                group: None,
                due: None,
                tagged_urgent: false,
                signal_urgent: line.is_none(),
                eligible,
                age_days: 3,
            },
            importance: 0.5,
            urgency: line.is_none().then_some(Urgency::Signal),
            quadrant: Quadrant::Q2,
        }
    }

    fn run(id: i64, state: RunState) -> Run {
        Run {
            id,
            state,
            project: "p".into(),
            text: format!("run {id}"),
            outcome: (state == RunState::PrOpen).then(|| "https://x/pull/1".into()),
            ..Run::default()
        }
    }

    #[test]
    fn agents_take_the_free_eligible_tasks_in_matrix_order() {
        let tasks = [
            placed("ci", None, Priority::High, true),
            placed("free", Some(5), Priority::Medium, true),
            placed("held", Some(6), Priority::Medium, true),
            placed("gone", Some(7), Priority::Critical, true),
            placed("spent", Some(8), Priority::Critical, true),
            placed("tired", Some(9), Priority::Low, true),
            placed("mine", Some(10), Priority::Critical, false),
            placed("later", Some(11), Priority::Low, false),
        ];
        let next = build(&tasks, &[], 100, |p| match p.task.project.as_str() {
            "held" => Taken::Held,
            "gone" => Taken::Absent,
            "spent" | "tired" => Taken::Exhausted(2),
            _ => Taken::Free,
        });
        let at: Vec<&str> = next.agents.iter().map(|r| r.cells[0].as_str()).collect();
        assert_eq!(at, ["ci:ci", "free:5"]);
        assert_eq!(
            next.agents[1].command.as_deref(),
            Some("pma dispatch free:5")
        );

        // A spent task needs you only when it is critical or urgent, as any
        // other does; one no agent may take says so.
        let yours: Vec<(&str, &str)> = next
            .tasks
            .iter()
            .map(|r| (r.cells[0].as_str(), r.cells[4].as_str()))
            .collect();
        assert_eq!(
            yours,
            [("spent:8", "2 attempts used"), ("mine:10", "not #agent")]
        );
        assert_eq!(
            next.tasks[0].command.as_deref(),
            Some("pma dispatch spent:8 --retry")
        );
    }

    #[test]
    fn runs_waiting_on_you_come_review_first() {
        let runs = [
            run(1, RunState::PrOpen),
            run(2, RunState::Approved),
            run(3, RunState::Ready),
            run(4, RunState::Running),
            run(5, RunState::Shipped),
            run(6, RunState::Failed),
        ];
        let next = build(&[], &runs, 100, |_| Taken::Free);
        let got: Vec<(&str, &str)> = next
            .runs
            .iter()
            .map(|r| (r.cells[0].as_str(), r.command.as_deref().unwrap_or("")))
            .collect();
        assert_eq!(
            got,
            [
                ("#3", "pma review 3"),
                ("#6", "pma review 6"),
                ("#2", "pma ship"),
                ("#1", "https://x/pull/1"),
            ]
        );
    }

    #[test]
    fn a_limit_counts_what_it_leaves_out() {
        let tasks: Vec<Placed> = (1..=4)
            .map(|n| placed(&format!("p{n}"), Some(n), Priority::Low, true))
            .collect();
        let next = build(&tasks, &[run(3, RunState::Ready)], 100, |_| Taken::Free);
        let out = render(&next, Some(2));
        assert!(out.starts_with("For agents: 4\n  p1:1"), "{out}");
        assert!(out.contains("  and 2 more\n"), "{out}");
        assert!(
            out.contains(
                "For you: 1\n  runs: 1\n    #3  ready  to review  p  run 3  pma review 3\n"
            ),
            "{out}"
        );
        let empty = render(&Next::default(), None);
        assert!(
            empty.contains("nothing an agent may take") && empty.contains("nothing waits on you")
        );
    }
}
