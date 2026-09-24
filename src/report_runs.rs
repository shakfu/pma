//! `pma report`: what dispatching has actually produced. Reads `runs` and
//! `attempts` only, so it describes work that was attempted. It says nothing
//! about repositories where nothing was ever dispatched; `pma status` is
//! still what covers those.

use crate::report::{duration, table};
use crate::store::{Attempt, Run, RunState};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum By {
    Project,
    Class,
    Agent,
}

impl By {
    pub const ALL: [(&'static str, By); 3] = [
        ("project", By::Project),
        ("class", By::Class),
        ("agent", By::Agent),
    ];

    pub fn parse(s: &str) -> Option<By> {
        Self::ALL.iter().find(|(n, _)| *n == s).map(|(_, b)| *b)
    }

    fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, b)| *b == self)
            .map_or("", |(n, _)| n)
    }

    fn key(self, run: &Run) -> String {
        match self {
            By::Project => run.project.clone(),
            By::Class => run.class.map_or("?".into(), |c| c.name().to_string()),
            By::Agent => run.agent.clone(),
        }
    }
}

#[derive(Default)]
struct Tally {
    runs: i64,
    first_pass: i64,
    decided: i64,
    accepted: i64,
    /// Runs whose change reached the default branch: pushed, or merged.
    landed: i64,
    open: i64,
    attempts: i64,
    cost: f64,
    /// Runs whose agent reported no cost, so `cost` is a floor.
    cost_unknown: i64,
    seconds: i64,
    review_seconds: i64,
}

impl Tally {
    fn add(&mut self, run: &Run, attempts: &[Attempt]) {
        self.runs += 1;
        let mine: Vec<&Attempt> = attempts.iter().filter(|a| a.run_id == run.id).collect();
        self.attempts += mine.len() as i64;
        if mine.iter().any(|a| a.n == 1 && a.verify_ok == Some(true)) {
            self.first_pass += 1;
        }
        match run.state {
            RunState::Approved | RunState::Pushed | RunState::Merged => {
                self.decided += 1;
                self.accepted += 1;
            }
            RunState::Rejected | RunState::Closed => self.decided += 1,
            // Neither accepted nor refused until someone merges or closes it,
            // so it is in no share.
            RunState::PrOpen => self.open += 1,
            // Failed and never reviewed: no one has judged it.
            _ => {}
        }
        if run.state.landed() {
            self.landed += 1;
        }
        match run.cost_usd {
            Some(c) => self.cost += c,
            None => self.cost_unknown += 1,
        }
        self.seconds += run.seconds.unwrap_or(0);
        self.review_seconds += run.review_seconds.unwrap_or(0);
    }

    fn cells(&self, label: String) -> Vec<String> {
        vec![
            label,
            self.runs.to_string(),
            self.first_pass.to_string(),
            self.accepted.to_string(),
            self.landed.to_string(),
            self.open.to_string(),
            self.attempts.to_string(),
            match self.cost_unknown {
                0 => format!("${:.2}", self.cost),
                n => format!("${:.2}+{n}?", self.cost),
            },
            duration(Some(self.seconds)),
            duration(Some(self.review_seconds)),
        ]
    }
}

const HEADINGS: [&str; 9] = [
    "runs", "1st pass", "accepted", "landed", "open", "attempts", "cost", "agent", "review",
];

/// A run still queued or running has not produced a result yet, so it is left
/// out rather than counted as a failure.
pub fn report(runs: &[Run], attempts: &[Attempt], by: By) -> String {
    let done: Vec<&Run> = runs
        .iter()
        .filter(|r| !matches!(r.state, RunState::Queued | RunState::Running))
        .collect();
    if done.is_empty() {
        return "no finished runs; dispatch some tasks first\n".into();
    }

    let mut total = Tally::default();
    let mut groups: Vec<(String, Tally)> = Vec::new();
    for run in &done {
        total.add(run, attempts);
        let key = by.key(run);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, t)) => t.add(run, attempts),
            None => {
                let mut t = Tally::default();
                t.add(run, attempts);
                groups.push((key, t));
            }
        }
    }
    groups.sort_by(|a, b| b.1.runs.cmp(&a.1.runs).then_with(|| a.0.cmp(&b.0)));

    let mut rows = vec![
        std::iter::once(format!("by {}", by.name()))
            .chain(HEADINGS.iter().map(|h| (*h).to_string()))
            .collect(),
    ];
    rows.extend(groups.iter().map(|(k, t)| t.cells(k.clone())));
    rows.push(total.cells("all".into()));

    let share = match total.decided {
        0 => "nothing decided yet".to_string(),
        d => format!(
            "{}% of {d} decided run{} accepted",
            (total.accepted * 100 + d / 2) / d,
            if d == 1 { "" } else { "s" }
        ),
    };
    let pending = match total.open {
        0 => String::new(),
        1 => "; 1 pull request open, in no share".into(),
        n => format!("; {n} pull requests open, in no share"),
    };
    format!("{share}{pending}\n\n{}", table(&rows, ""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::Class;

    fn run(id: i64, class: Class, state: RunState) -> Run {
        Run {
            id,
            project: "alpha".into(),
            agent: "claude".into(),
            class: Some(class),
            state,
            cost_usd: Some(0.10),
            seconds: Some(60),
            review_seconds: Some(120),
            ..Run::blank()
        }
    }

    fn attempt(run_id: i64, n: i64, verify_ok: Option<bool>) -> Attempt {
        Attempt {
            id: 0,
            run_id,
            n,
            agent: "claude".into(),
            prompt: String::new(),
            feedback: None,
            started_at: 0,
            seconds: Some(30),
            cost_usd: Some(0.05),
            summary: None,
            verify: None,
            verify_ok,
            error: None,
            outcome: None,
            model: None,
        }
    }

    #[test]
    fn nothing_dispatched_says_so() {
        assert!(report(&[], &[], By::Class).starts_with("no finished runs"));
    }

    /// A run that has not finished is not a failure, and an open pull request
    /// is in no share.
    #[test]
    fn unfinished_runs_and_open_pull_requests_are_excluded() {
        let runs = [
            run(1, Class::Mechanical, RunState::Merged),
            run(2, Class::Mechanical, RunState::Running),
            run(3, Class::Mechanical, RunState::PrOpen),
        ];
        let out = report(&runs, &[], By::Class);
        assert!(out.contains("100% of 1 decided run accepted"), "{out}");
        assert!(out.contains("1 pull request open, in no share"), "{out}");
        let cells: Vec<&str> = out
            .lines()
            .find(|l| l.starts_with("A "))
            .unwrap()
            .split_whitespace()
            .collect();
        // The running run is not counted at all; the open one only as open.
        assert_eq!(&cells[..6], ["A", "2", "0", "1", "1", "1"], "{out}");
    }

    /// Passing on the first attempt and being accepted after rework are
    /// different outcomes, so they are counted apart.
    #[test]
    fn a_rework_does_not_become_a_first_pass() {
        let runs = [
            run(1, Class::Specified, RunState::Merged),
            run(2, Class::Specified, RunState::Merged),
        ];
        let attempts = [
            attempt(1, 1, Some(true)),
            attempt(2, 1, Some(false)),
            attempt(2, 2, Some(true)),
        ];
        let out = report(&runs, &attempts, By::Class);
        let row = out.lines().find(|l| l.starts_with("B ")).unwrap();
        let cells: Vec<&str> = row.split_whitespace().collect();
        // class, runs, 1st pass, accepted, landed, open, attempts
        assert_eq!(&cells[..7], ["B", "2", "1", "2", "2", "0", "3"], "{row}");
    }

    /// A worker that reports no cost must not read as free.
    #[test]
    fn unknown_cost_is_marked_not_summed_as_zero() {
        let mut a = run(1, Class::Mechanical, RunState::Merged);
        a.cost_usd = None;
        let runs = [a, run(2, Class::Mechanical, RunState::Merged)];
        let out = report(&runs, &[], By::Class);
        assert!(out.contains("$0.10+1?"), "{out}");
    }

    #[test]
    fn grouping_follows_the_dimension() {
        let mut b = run(2, Class::Specified, RunState::Merged);
        b.project = "beta".into();
        b.agent = "codex".into();
        let runs = [run(1, Class::Mechanical, RunState::Merged), b];
        for (by, labels) in [
            (By::Project, ["alpha", "beta"]),
            (By::Class, ["A", "B"]),
            (By::Agent, ["claude", "codex"]),
        ] {
            let out = report(&runs, &[], by);
            for l in labels {
                assert!(out.lines().any(|x| x.starts_with(l)), "{by:?}: {out}");
            }
        }
    }
}
