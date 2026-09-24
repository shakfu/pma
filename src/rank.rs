//! Places tasks in the Eisenhower matrix and orders projects by what needs
//! attention.
//!
//! Importance is `tier multiplier * priority weight` against a threshold.
//! Urgency is sequencing, not decay: a deadline, a signal that blocks other
//! work, or an explicit mark. Age orders the queue and fills `pma stale`; it
//! does not make a task urgent. Formulas are in `docs/dev/design.md`.

use std::cmp::Ordering;

use crate::config::Config;
use crate::scan::Ci;
use crate::todo::Priority;

/// Tolerance for comparing a product of weights against the threshold.
const EPSILON: f64 = 1e-9;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quadrant {
    Q1,
    Q2,
    Q3,
    Q4,
}

impl Quadrant {
    pub const ALL: [Quadrant; 4] = [Quadrant::Q1, Quadrant::Q2, Quadrant::Q3, Quadrant::Q4];

    pub fn title(self) -> &'static str {
        match self {
            Quadrant::Q1 => "Do right away",
            Quadrant::Q2 => "Schedule for later",
            Quadrant::Q3 => "Delegate or avoid",
            Quadrant::Q4 => "Later",
        }
    }

    /// `q1` to `q4`, in either case.
    pub fn parse(s: &str) -> Option<Quadrant> {
        Quadrant::ALL
            .into_iter()
            .find(|q| format!("{q:?}").eq_ignore_ascii_case(s))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    /// Days until due; negative when overdue.
    Due(i64),
    Tagged,
    /// A failing check blocks everything else in that repository.
    Signal,
}

/// A project as ranking sees it: what the last scan recorded, plus its tier.
#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub tier: u8,
    pub open: Vec<Priority>,
    /// Days since the last commit that counts as activity; `None` if none found.
    pub idle_days: Option<i64>,
    pub ci: Ci,
    pub dirty: i64,
    /// `pma/` branches that no open run owns.
    pub leftover: i64,
    pub ahead: Option<i64>,
    /// Outdated dependencies and days since measured; `None` if never measured
    /// or not applicable.
    pub deps: Option<(i64, i64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub project: String,
    pub tier: u8,
    pub priority: Priority,
    pub text: String,
    /// TODO.md line; `None` for signal tasks.
    pub line: Option<i64>,
    /// What `pma dispatch` works on: an item's key, or `ci`. `None` for
    /// signals agents cannot act on.
    pub key: Option<String>,
    /// The nearest `###` heading above the item.
    pub group: Option<String>,
    /// Day number of `due:`.
    pub due: Option<i64>,
    pub tagged_urgent: bool,
    pub signal_urgent: bool,
    pub age_days: i64,
    /// Whether an agent may take it: the `ci` and `deps` signals at any tier,
    /// and items tagged `#agent`. Importance orders the queue; this decides
    /// what is dispatched from it.
    pub eligible: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub task: Task,
    pub importance: f64,
    pub urgency: Option<Urgency>,
    pub quadrant: Quadrant,
}

pub fn urgency(cfg: &Config, t: &Task, today: i64) -> Option<Urgency> {
    if let Some(due) = t.due
        && due - today <= cfg.urgent_within
    {
        return Some(Urgency::Due(due - today));
    }
    if t.tagged_urgent {
        return Some(Urgency::Tagged);
    }
    t.signal_urgent.then_some(Urgency::Signal)
}

/// Places every task, ordered by quadrant, then importance, then days to due
/// (dated first), then age.
pub fn place(cfg: &Config, tasks: Vec<Task>, today: i64) -> Vec<Placed> {
    let mut placed: Vec<Placed> = tasks
        .into_iter()
        .map(|task| {
            let importance = cfg.tier(task.tier) * cfg.priority(task.priority);
            let urgency = urgency(cfg, &task, today);
            let quadrant = match (
                importance + EPSILON >= cfg.important_threshold,
                urgency.is_some(),
            ) {
                (true, true) => Quadrant::Q1,
                (true, false) => Quadrant::Q2,
                (false, true) => Quadrant::Q3,
                (false, false) => Quadrant::Q4,
            };
            Placed {
                task,
                importance,
                urgency,
                quadrant,
            }
        })
        .collect();
    placed.sort_by(|a, b| {
        a.quadrant
            .cmp(&b.quadrant)
            .then(b.importance.total_cmp(&a.importance))
            .then(due_order(a.task.due, b.task.due))
            .then(b.task.age_days.cmp(&a.task.age_days))
            .then_with(|| (&a.task.project, a.task.line).cmp(&(&b.task.project, b.task.line)))
    });
    placed
}

fn due_order(a: Option<i64>, b: Option<i64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Tasks for project conditions: failing CI, outdated dependencies, local
/// changes, and inactivity beyond the tier's horizon.
pub fn signal_tasks(cfg: &Config, p: &Project) -> Vec<Task> {
    let task = |priority, text: String, urgent, age_days| Task {
        project: p.name.clone(),
        tier: p.tier,
        priority,
        text,
        line: None,
        key: None,
        group: None,
        due: None,
        tagged_urgent: false,
        signal_urgent: urgent,
        age_days,
        eligible: false,
    };
    let mut tasks = Vec::new();
    if let Ci::Failing(workflows) = &p.ci {
        tasks.push(Task {
            key: Some("ci".into()),
            eligible: true,
            ..task(
                cfg.signals.ci,
                format!("fix CI: {}", workflows.join(", ")),
                true,
                0,
            )
        });
    }
    if let Some((n, _)) = p.deps.filter(|(n, _)| *n > 0) {
        tasks.push(Task {
            key: Some("deps".into()),
            eligible: true,
            ..task(
                cfg.signals.deps,
                format!("update dependencies: {n} outdated"),
                false,
                0,
            )
        });
    }
    let local = local_changes(p);
    if !local.is_empty() {
        tasks.push(task(
            cfg.signals.hygiene,
            format!("resolve local changes: {local}"),
            false,
            0,
        ));
    }
    let horizon = cfg.activity_horizon[usize::from(p.tier) - 1];
    match p.idle_days {
        Some(days) if days > horizon => tasks.push(task(
            cfg.signals.activity,
            format!("review project: no code commits in {days} days"),
            false,
            // The task exists from the day the horizon was crossed.
            days - horizon,
        )),
        None => tasks.push(task(
            cfg.signals.activity,
            "review project: no code commits found".into(),
            false,
            0,
        )),
        Some(_) => {}
    }
    tasks
}

/// `n` with the noun that agrees with it.
fn count(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub fn local_changes(p: &Project) -> String {
    let mut parts = Vec::new();
    if p.dirty > 0 {
        parts.push(count(p.dirty, "changed file", "changed files"));
    }
    if let Some(n) = p.ahead.filter(|n| *n > 0) {
        parts.push(count(n, "unpushed commit", "unpushed commits"));
    }
    if p.leftover > 0 {
        parts.push(count(
            p.leftover,
            "leftover pma branch",
            "leftover pma branches",
        ));
    }
    parts.join(", ")
}

/// What places a project in `pma status`, worst first. Each is a condition
/// with an action, so the order says what to do first: a failing check blocks
/// the repository, broken facts make the rest unreliable, unpublished work is
/// at risk, then the work itself, then upkeep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// CI fails on the default branch.
    FailingCi,
    /// `TODO.md` has lint errors, or the scan could not read the project.
    Broken,
    /// Commits not pushed, or `pma/` branches no open run owns.
    Unpublished,
    /// Open items under `## Critical`.
    Critical,
    /// Outdated dependencies at the last measurement.
    StaleDeps,
    /// No commit that counts as activity within the tier's horizon.
    Idle,
    /// None of the above.
    Clear,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::FailingCi => "failing CI",
            State::Broken => "broken",
            State::Unpublished => "unpublished",
            State::Critical => "critical",
            State::StaleDeps => "stale deps",
            State::Idle => "idle",
            State::Clear => "clear",
        }
    }
}

/// What the scan could not read about a project, which `rank::Project` does
/// not carry.
#[derive(Debug, Clone, Copy, Default)]
pub struct Facts<'a> {
    pub lint_errors: i64,
    pub scan_error: Option<&'a str>,
}

/// Every state a project is in, worst first, each with what put it there.
/// `Clear` alone when it is in none. A signal that was not measured, such as
/// CI offline or dependencies never counted, puts a project in no state.
pub fn states(cfg: &Config, p: &Project, facts: Facts<'_>) -> Vec<(State, String)> {
    let mut out = Vec::new();
    if let Ci::Failing(workflows) = &p.ci {
        out.push((State::FailingCi, workflows.join(", ")));
    }
    let mut broken = Vec::new();
    if let Some(e) = facts.scan_error {
        broken.push(format!("scan error: {e}"));
    }
    if facts.lint_errors > 0 {
        broken.push(format!(
            "{} in TODO.md",
            count(facts.lint_errors, "lint error", "lint errors")
        ));
    }
    if !broken.is_empty() {
        out.push((State::Broken, broken.join("; ")));
    }
    let mut unpublished = Vec::new();
    if let Some(n) = p.ahead.filter(|n| *n > 0) {
        unpublished.push(count(n, "unpushed commit", "unpushed commits"));
    }
    if p.leftover > 0 {
        unpublished.push(count(
            p.leftover,
            "leftover pma branch",
            "leftover pma branches",
        ));
    }
    if !unpublished.is_empty() {
        out.push((State::Unpublished, unpublished.join(", ")));
    }
    let critical = p.open.iter().filter(|&&x| x == Priority::Critical).count() as i64;
    if critical > 0 {
        out.push((State::Critical, count(critical, "open item", "open items")));
    }
    if let Some((n, age)) = p.deps.filter(|(n, _)| *n > 0) {
        let when = match age {
            0 => "today".to_string(),
            1 => "1 day ago".to_string(),
            d => format!("{d} days ago"),
        };
        out.push((State::StaleDeps, format!("{n} outdated, measured {when}")));
    }
    let horizon = cfg.activity_horizon[usize::from(p.tier) - 1];
    match p.idle_days {
        Some(d) if d > horizon => out.push((
            State::Idle,
            format!("no code commit in {d} days, horizon {horizon}"),
        )),
        None => out.push((State::Idle, "no code commits found".into())),
        Some(_) => {}
    }
    if out.is_empty() {
        out.push((State::Clear, String::new()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODAY: i64 = 20_000;

    fn task(tier: u8, priority: Priority) -> Task {
        Task {
            project: "p".into(),
            tier,
            priority,
            text: "t".into(),
            line: Some(1),
            key: Some("t".into()),
            group: None,
            due: None,
            tagged_urgent: false,
            signal_urgent: false,
            age_days: 0,
            eligible: false,
        }
    }

    fn quadrant(t: Task) -> Quadrant {
        place(&Config::default(), vec![t], TODAY)[0].quadrant
    }

    fn project(tier: u8) -> Project {
        Project {
            name: "p".into(),
            tier,
            open: vec![],
            idle_days: Some(0),
            ci: Ci::Passing,
            dirty: 0,
            leftover: 0,
            ahead: Some(0),
            deps: None,
        }
    }

    #[test]
    fn importance_follows_the_design_table() {
        use Priority::*;
        let important = |tier, p| quadrant(task(tier, p)) == Quadrant::Q2;
        for tier in 1..=4 {
            assert!(important(tier, Critical), "critical, tier {tier}");
        }
        assert!(!important(5, Critical));
        assert!(
            important(1, High) && important(2, High),
            "0.8 * 0.5 meets 0.4 exactly"
        );
        assert!(!important(3, High));
        for tier in 1..=5 {
            assert!(!important(tier, Medium) && !important(tier, Low));
        }
    }

    #[test]
    fn urgency_sources() {
        let cfg = Config::default();
        let mut t = task(4, Priority::Low);

        t.due = Some(TODAY + 7);
        assert_eq!(urgency(&cfg, &t, TODAY), Some(Urgency::Due(7)));
        t.due = Some(TODAY - 2);
        assert_eq!(urgency(&cfg, &t, TODAY), Some(Urgency::Due(-2)));
        t.due = Some(TODAY + 8);
        assert_eq!(urgency(&cfg, &t, TODAY), None);

        t.tagged_urgent = true;
        assert_eq!(urgency(&cfg, &t, TODAY), Some(Urgency::Tagged));
        t.tagged_urgent = false;
        t.signal_urgent = true;
        assert_eq!(urgency(&cfg, &t, TODAY), Some(Urgency::Signal));
    }

    /// Age no longer makes a task urgent. With no due dates in a portfolio,
    /// the old rule reduced to "older than 30 days", which within a month
    /// admitted most tier-1 tasks and carried no ordering information the
    /// queue did not already have.
    #[test]
    fn age_alone_is_not_urgency() {
        let cfg = Config::default();
        let aged = |tier, age_days| Task {
            age_days,
            ..task(tier, Priority::Low)
        };
        for (tier, age) in [(1, 31), (1, 400), (3, 91), (4, 5000)] {
            assert_eq!(urgency(&cfg, &aged(tier, age), TODAY), None, "{tier} {age}");
        }
        // It still orders the queue, oldest first among equals.
        let placed = place(&cfg, vec![aged(1, 10), aged(1, 90)], TODAY);
        assert_eq!(
            placed.iter().map(|p| p.task.age_days).collect::<Vec<_>>(),
            [90, 10]
        );
    }

    #[test]
    fn ordering_within_and_across_quadrants() {
        let mut crit = task(1, Priority::Critical);
        crit.text = "crit".into();
        let mut high_due_late = task(1, Priority::High);
        high_due_late.text = "late".into();
        high_due_late.due = Some(TODAY + 60);
        let mut high_due_soon = task(1, Priority::High);
        high_due_soon.text = "soon".into();
        high_due_soon.due = Some(TODAY + 30);
        let mut high_old = task(1, Priority::High);
        high_old.text = "old".into();
        high_old.age_days = 20;
        let mut urgent_low = task(1, Priority::Low);
        urgent_low.text = "q3".into();
        urgent_low.tagged_urgent = true;
        let mut fire = task(2, Priority::Critical);
        fire.text = "q1".into();
        fire.due = Some(TODAY);

        let order: Vec<_> = place(
            &Config::default(),
            vec![
                urgent_low,
                high_old,
                high_due_late,
                crit,
                high_due_soon,
                fire,
            ],
            TODAY,
        )
        .into_iter()
        .map(|p| (p.quadrant, p.task.text))
        .collect();
        assert_eq!(
            order,
            [
                (Quadrant::Q1, "q1".to_string()),
                (Quadrant::Q2, "crit".to_string()),
                (Quadrant::Q2, "soon".to_string()),
                (Quadrant::Q2, "late".to_string()),
                (Quadrant::Q2, "old".to_string()),
                (Quadrant::Q3, "q3".to_string()),
            ]
        );
    }

    #[test]
    fn signal_tasks_for_ci_local_changes_and_idleness() {
        let cfg = Config::default();
        assert!(signal_tasks(&cfg, &project(1)).is_empty());

        let p = Project {
            ci: Ci::Failing(vec!["test".into(), "wheels".into()]),
            dirty: 1,
            ahead: Some(2),
            leftover: 1,
            idle_days: Some(31),
            deps: Some((3, 2)),
            ..project(1)
        };
        let tasks = signal_tasks(&cfg, &p);
        let ages: Vec<i64> = tasks.iter().map(|t| t.age_days).collect();
        assert_eq!(
            ages,
            [0, 0, 0, 1],
            "an idle task is as old as the time past its horizon"
        );
        let got: Vec<_> = tasks
            .iter()
            .map(|t| (t.priority, t.text.as_str(), t.signal_urgent))
            .collect();
        assert_eq!(
            got,
            [
                (Priority::High, "fix CI: test, wheels", true),
                (Priority::Medium, "update dependencies: 3 outdated", false),
                (
                    Priority::Medium,
                    "resolve local changes: 1 changed file, 2 unpushed commits, 1 leftover pma branch",
                    false
                ),
                (
                    Priority::Low,
                    "review project: no code commits in 31 days",
                    false
                ),
            ]
        );
        let placed = place(&cfg, tasks, TODAY);
        assert_eq!(
            placed[0].quadrant,
            Quadrant::Q1,
            "failing CI in tier 1 is important and urgent"
        );

        let tier3 = Project {
            ci: Ci::Failing(vec!["t".into()]),
            ..project(3)
        };
        assert_eq!(
            place(&cfg, signal_tasks(&cfg, &tier3), TODAY)[0].quadrant,
            Quadrant::Q3
        );

        let quiet = Project {
            idle_days: None,
            ..project(5)
        };
        assert_eq!(
            signal_tasks(&cfg, &quiet)[0].text,
            "review project: no code commits found"
        );
    }

    #[test]
    fn a_project_is_in_each_state_it_meets_worst_first() {
        let cfg = Config::default();
        let p = Project {
            open: vec![Priority::Critical, Priority::High, Priority::Critical],
            idle_days: Some(31),
            ci: Ci::Failing(vec!["test".into()]),
            dirty: 4,
            ahead: Some(1),
            leftover: 2,
            deps: Some((3, 1)),
            ..project(1)
        };
        let facts = Facts {
            lint_errors: 1,
            scan_error: None,
        };
        let got = states(&cfg, &p, facts);
        assert_eq!(
            got,
            [
                (State::FailingCi, "test".to_string()),
                (State::Broken, "1 lint error in TODO.md".to_string()),
                (
                    State::Unpublished,
                    "1 unpushed commit, 2 leftover pma branches".to_string()
                ),
                (State::Critical, "2 open items".to_string()),
                (
                    State::StaleDeps,
                    "3 outdated, measured 1 day ago".to_string()
                ),
                (
                    State::Idle,
                    "no code commit in 31 days, horizon 30".to_string()
                ),
            ]
        );
        assert!(got.windows(2).all(|w| w[0].0 < w[1].0), "worst first");
    }

    /// A clean project is clear; changed files alone, CI not measured and
    /// dependencies never counted place it nowhere worse.
    #[test]
    fn what_was_not_measured_places_a_project_in_no_state() {
        let cfg = Config::default();
        let quiet = Project {
            dirty: 3,
            ci: Ci::Unknown("offline".into()),
            ..project(1)
        };
        assert_eq!(
            states(&cfg, &quiet, Facts::default()),
            [(State::Clear, String::new())]
        );
        let scan = Facts {
            lint_errors: 0,
            scan_error: Some("git status failed"),
        };
        assert_eq!(
            states(&cfg, &project(1), scan)[0],
            (State::Broken, "scan error: git status failed".to_string())
        );
        let never = Project {
            idle_days: None,
            ..project(5)
        };
        assert_eq!(states(&cfg, &never, Facts::default())[0].0, State::Idle);
    }
}
