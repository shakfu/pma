//! Places tasks in the Eisenhower matrix and scores project health.
//!
//! Importance is `tier multiplier * priority weight` against a threshold.
//! Urgency is computed from due dates, tags, signals and age, so a quadrant
//! is never stored. Formulas are described in `docs/dev/design.md`.

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
            Quadrant::Q4 => "Remove",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    /// Days until due; negative when overdue.
    Due(i64),
    Tagged,
    Signal,
    /// Days open, beyond the tier's `stale_after`.
    Stale(i64),
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
    if t.signal_urgent {
        return Some(Urgency::Signal);
    }
    match cfg.stale_after[usize::from(t.tier) - 1] {
        Some(days) if t.due.is_none() && t.age_days > days => Some(Urgency::Stale(t.age_days)),
        _ => None,
    }
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
    };
    let mut tasks = Vec::new();
    if let Ci::Failing(workflows) = &p.ci {
        tasks.push(Task {
            key: Some("ci".into()),
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

pub fn local_changes(p: &Project) -> String {
    let plural = |n: i64, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    if p.dirty > 0 {
        parts.push(plural(p.dirty, "changed file"));
    }
    if let Some(n) = p.ahead.filter(|n| *n > 0) {
        parts.push(plural(n, "unpushed commit"));
    }
    parts.join(", ")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Component {
    pub signal: &'static str,
    /// 0..1, where 1 needs attention; `None` when not measured.
    pub score: Option<f64>,
    pub weight: f64,
    /// This component's share of the health score.
    pub contribution: f64,
    pub detail: String,
}

/// Saturation constant for the tasks signal: this much summed priority weight
/// gives a score of 1 - 1/e.
const TASKS_SCALE: f64 = 3.0;

/// Outdated dependencies at which the deps signal reaches 1.
const DEPS_SCALE: f64 = 10.0;

/// `tier * sum(w_i * s_i) / sum(w_i)` over the measured signals.
pub fn health(cfg: &Config, p: &Project) -> (f64, Vec<Component>) {
    let load: f64 = p.open.iter().map(|&pr| cfg.priority(pr)).sum();
    let counts: Vec<String> = Priority::ALL
        .iter()
        .filter_map(|&pr| {
            let n = p.open.iter().filter(|&&x| x == pr).count();
            (n > 0).then(|| format!("{n} {}", pr.name()))
        })
        .collect();
    let horizon = cfg.activity_horizon[usize::from(p.tier) - 1];

    let mut parts = vec![
        (
            "tasks",
            Some(1.0 - (-load / TASKS_SCALE).exp()),
            cfg.weights.tasks,
            if counts.is_empty() {
                "no open items".into()
            } else {
                format!("open: {}", counts.join(", "))
            },
        ),
        (
            "activity",
            Some(
                p.idle_days
                    .map_or(1.0, |d| (d as f64 / horizon as f64).min(1.0)),
            ),
            cfg.weights.activity,
            match p.idle_days {
                Some(d) => format!(
                    "last code commit {d} day{} ago, horizon {horizon}",
                    if d == 1 { "" } else { "s" }
                ),
                None => "no code commits found".into(),
            },
        ),
        match &p.ci {
            Ci::Failing(w) => (
                "ci",
                Some(1.0),
                cfg.weights.ci,
                format!("failing: {}", w.join(", ")),
            ),
            Ci::Passing => ("ci", Some(0.0), cfg.weights.ci, "passing".into()),
            Ci::NoRuns => (
                "ci",
                Some(0.5),
                cfg.weights.ci,
                "no runs on the default branch".into(),
            ),
            Ci::Unknown(why) => ("ci", None, cfg.weights.ci, format!("unknown: {why}")),
        },
        match p.deps {
            Some((n, age)) => (
                "deps",
                Some((n as f64 / DEPS_SCALE).min(1.0)),
                cfg.weights.deps,
                format!(
                    "{n} outdated, measured {}",
                    match age {
                        0 => "today".to_string(),
                        1 => "1 day ago".to_string(),
                        d => format!("{d} days ago"),
                    }
                ),
            ),
            None => (
                "deps",
                None,
                cfg.weights.deps,
                "not measured; `pma scan --deps`".into(),
            ),
        },
        {
            let local = local_changes(p);
            let score = 0.5 * f64::from(u8::from(p.dirty > 0))
                + 0.5 * f64::from(u8::from(p.ahead.unwrap_or(0) > 0));
            (
                "hygiene",
                Some(score),
                cfg.weights.hygiene,
                if local.is_empty() {
                    "clean".into()
                } else {
                    local
                },
            )
        },
    ];

    let total_weight: f64 = parts.iter().filter(|p| p.1.is_some()).map(|p| p.2).sum();
    let tier = cfg.tier(p.tier);
    let components: Vec<Component> = parts
        .drain(..)
        .map(|(signal, score, weight, detail)| Component {
            signal,
            score,
            weight,
            contribution: match score {
                Some(s) if total_weight > 0.0 => tier * weight * s / total_weight,
                _ => 0.0,
            },
            detail,
        })
        .collect();
    (components.iter().map(|c| c.contribution).sum(), components)
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

    #[test]
    fn age_makes_undated_tasks_urgent_per_tier() {
        let cfg = Config::default();
        let aged = |tier, age_days, due| Task {
            age_days,
            due,
            ..task(tier, Priority::Low)
        };
        assert_eq!(
            urgency(&cfg, &aged(1, 30, None), TODAY),
            None,
            "30 days is not beyond 30"
        );
        assert_eq!(
            urgency(&cfg, &aged(1, 31, None), TODAY),
            Some(Urgency::Stale(31))
        );
        assert_eq!(
            urgency(&cfg, &aged(3, 91, None), TODAY),
            Some(Urgency::Stale(91))
        );
        assert_eq!(
            urgency(&cfg, &aged(4, 5000, None), TODAY),
            None,
            "tier 4 never ages"
        );
        assert_eq!(
            urgency(&cfg, &aged(1, 400, Some(TODAY + 30)), TODAY),
            None,
            "a due date overrides age"
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
                    "resolve local changes: 1 changed file, 2 unpushed commits",
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
    fn health_sums_contributions_and_skips_unmeasured_signals() {
        let cfg = Config::default();
        let p = Project {
            open: vec![Priority::Critical, Priority::High, Priority::High],
            idle_days: Some(15),
            ci: Ci::Failing(vec!["test".into()]),
            dirty: 4,
            ahead: None,
            ..project(2)
        };
        let (score, parts) = health(&cfg, &p);

        let tasks = 1.0 - (-2.0_f64 / 3.0).exp();
        let activity = 15.0 / 60.0;
        // deps is unmeasured, so its weight is left out: 5 + 1 + 3 + 2.
        let expected = 0.8 * (5.0 * tasks + activity + 3.0 * 1.0 + 2.0 * 0.5) / 11.0;
        assert!((score - expected).abs() < 1e-12, "{score} vs {expected}");
        assert!((parts.iter().map(|c| c.contribution).sum::<f64>() - score).abs() < 1e-12);

        let deps = parts.iter().find(|c| c.signal == "deps").unwrap();
        assert_eq!((deps.score, deps.contribution), (None, 0.0));

        let measured = Project {
            deps: Some((4, 1)),
            ..p.clone()
        };
        let (with_deps, parts) = health(&cfg, &measured);
        let expected = 0.8 * (5.0 * tasks + activity + 3.0 + 2.0 * 0.5 + 0.4) / 12.0;
        assert!(
            (with_deps - expected).abs() < 1e-12,
            "{with_deps} vs {expected}"
        );
        assert_eq!(parts[3].detail, "4 outdated, measured 1 day ago");
        assert_eq!(parts[0].detail, "open: 1 critical, 2 high");
        assert_eq!(parts[4].detail, "4 changed files");

        let (clean, _) = health(&cfg, &project(1));
        assert_eq!(
            clean, 0.0,
            "no tasks, recent activity, passing CI, clean tree"
        );
    }
}
