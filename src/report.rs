//! Text output for `pma matrix` and `pma status`.

use crate::accept;
use crate::class::Class;
use crate::config::Config;
use crate::rank::{self, Placed, Project, Quadrant, Urgency};
use crate::scan::Ci;
use crate::store::{Attempt, Run, RunState};

/// Lays out rows as left-aligned columns separated by two spaces. The last
/// column is not padded.
pub fn table(rows: &[Vec<String>], indent: &str) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .filter_map(|r| r.get(c))
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for row in rows {
        let mut line = String::from(indent);
        for (c, cell) in row.iter().enumerate() {
            if c + 1 == row.len() {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<w$}  ", w = widths[c]));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Longest task text shown in the matrix, in characters.
const TEXT_WIDTH: usize = 100;

/// Longest group shown in the matrix, in characters.
const GROUP_WIDTH: usize = 24;

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 3).collect();
    out.push_str("...");
    out
}

/// Why a task is urgent, or how far off it is.
pub fn when(p: &Placed, today: i64) -> String {
    match (p.urgency, p.task.due) {
        (Some(Urgency::Due(d)), _) if d < 0 => format!("overdue {}d", -d),
        (Some(Urgency::Due(0)), _) => "due today".into(),
        (Some(Urgency::Due(d)), _) => format!("due in {d}d"),
        (Some(Urgency::Tagged), _) => "#urgent".into(),
        (Some(Urgency::Signal), _) => "signal".into(),
        (None, Some(due)) => format!("due in {}d", due - today),
        (None, None) => format!("open {}d", p.task.age_days),
    }
}

/// Open tasks by age, oldest first. Age used to make a task urgent, which in
/// a portfolio with no due dates meant "older than 30 days" and told the
/// queue nothing. It is a list to prune, not a reason to work.
pub fn stale(placed: &[Placed], limit: usize) -> String {
    let mut rows: Vec<&Placed> = placed.iter().filter(|p| p.task.line.is_some()).collect();
    if rows.is_empty() {
        return "no open items\n".into();
    }
    rows.sort_by_key(|p| std::cmp::Reverse(p.task.age_days));
    let shown = rows.len().min(limit);
    let cells: Vec<Vec<String>> = rows[..shown]
        .iter()
        .map(|p| {
            vec![
                format!("{}:{}", p.task.project, p.task.line.unwrap_or(0)),
                format!("T{}", p.task.tier),
                p.task.priority.name().into(),
                format!("open {}d", p.task.age_days),
                truncate(&p.task.text, TEXT_WIDTH),
            ]
        })
        .collect();
    let rest = rows.len() - shown;
    format!(
        "{}{}",
        table(&cells, "  "),
        match rest {
            0 => String::new(),
            n => format!("  and {n} more\n"),
        }
    )
}

/// `limit` caps the rows shown per quadrant; the count shows the rest.
pub fn matrix(
    placed: &[Placed],
    limit: Option<usize>,
    only: Option<Quadrant>,
    today: i64,
) -> String {
    let mut out = String::new();
    for q in Quadrant::ALL {
        if only.is_some_and(|o| o != q) {
            continue;
        }
        let tasks: Vec<&Placed> = placed.iter().filter(|p| p.quadrant == q).collect();
        let shown = limit.map_or(tasks.len(), |l| l.min(tasks.len()));
        let count = if shown < tasks.len() {
            format!("{}, top {shown}", tasks.len())
        } else {
            tasks.len().to_string()
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("{q:?} {}: {count}\n", q.title()));
        // The group column appears only when a shown task has a group.
        let grouped = tasks[..shown].iter().any(|p| p.task.group.is_some());
        let rows: Vec<Vec<String>> = tasks[..shown]
            .iter()
            .map(|p| {
                let t = &p.task;
                let place = match t.line {
                    Some(line) => format!("{}:{line}", t.project),
                    None => t.project.clone(),
                };
                let mut row = vec![place, format!("T{}", t.tier), t.priority.name().into()];
                row.push(when(p, today));
                if grouped {
                    row.push(truncate(t.group.as_deref().unwrap_or(""), GROUP_WIDTH));
                }
                row.push(truncate(&t.text, TEXT_WIDTH));
                row
            })
            .collect();
        out.push_str(&table(&rows, "  "));
    }
    out
}

pub struct StatusRow<'a> {
    pub project: &'a Project,
    pub has_todo: bool,
    pub lint_errors: i64,
    pub scan_error: Option<&'a str>,
}

pub fn status(cfg: &Config, rows: &[StatusRow], explain: bool) -> String {
    let mut scored: Vec<(f64, Vec<rank::Component>, &StatusRow)> = rows
        .iter()
        .map(|r| {
            let (score, parts) = rank::health(cfg, r.project);
            (score, parts, r)
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| a.2.project.name.cmp(&b.2.project.name))
    });

    let mut cells = vec![
        [
            "project", "tier", "health", "open", "idle", "ci", "deps", "local", "todo",
        ]
        .map(String::from)
        .to_vec(),
    ];
    for (score, _, r) in &scored {
        let p = r.project;
        let local = [
            (p.dirty > 0).then(|| format!("{} changed", p.dirty)),
            p.ahead.filter(|n| *n > 0).map(|n| format!("{n} ahead")),
            (p.leftover > 0).then(|| format!("{} leftover", p.leftover)),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
        cells.push(vec![
            p.name.clone(),
            p.tier.to_string(),
            format!("{score:.2}"),
            p.open.len().to_string(),
            p.idle_days.map_or("-".into(), |d| format!("{d}d")),
            match &p.ci {
                Ci::Failing(_) => "failing",
                Ci::Passing => "passing",
                Ci::NoRuns => "none",
                Ci::Unknown(_) => "unknown",
            }
            .into(),
            p.deps.map_or("-".into(), |(n, _)| n.to_string()),
            if local.is_empty() {
                "clean".into()
            } else {
                local
            },
            match (r.has_todo, r.lint_errors, r.scan_error) {
                (_, _, Some(e)) => format!("scan error: {e}"),
                (false, _, _) => "missing".into(),
                (true, 0, _) => "ok".into(),
                (true, n, _) => format!("{n} lint errors"),
            },
        ]);
    }
    let mut out = table(&cells, "");

    if explain {
        for (score, parts, r) in &scored {
            let p = r.project;
            out.push_str(&format!(
                "\n{}  tier {} (x{})  health {score:.2}\n",
                p.name,
                p.tier,
                cfg.tier(p.tier)
            ));
            let lines: Vec<Vec<String>> = parts
                .iter()
                .map(|c| match c.score {
                    Some(s) => vec![
                        c.signal.into(),
                        format!("s={s:.2}"),
                        format!("w={}", c.weight),
                        format!("+{:.2}", c.contribution),
                        c.detail.clone(),
                    ],
                    None => vec![
                        c.signal.into(),
                        "n/a".into(),
                        format!("w={}", c.weight),
                        String::new(),
                        c.detail.clone(),
                    ],
                })
                .collect();
            out.push_str(&table(&lines, "  "));
        }
    }
    out
}

fn verify_cell(run: &Run) -> String {
    match (run.verify.as_deref(), run.verify_ok) {
        (_, Some(true)) => "verify ok".into(),
        (_, Some(false)) => "verify FAILED".into(),
        (None, None) if run.state == RunState::Ready => "no verify".into(),
        _ => "-".into(),
    }
}

/// What the scope check made of the paths the run changed. An enumeration
/// failure is reported as such: it is not a clean run.
fn scope_cell(run: &Run, class: Class) -> String {
    let Some(paths) = &run.changed_paths else {
        return format!(
            "NOT CHECKED: {}",
            run.scope_error.as_deref().unwrap_or("paths not enumerated")
        );
    };
    match class.violations(&run.scope, paths) {
        v if v.is_empty() => format!("{} files, all permitted", paths.len()),
        v => format!(
            "{} of {} files OUTSIDE: {}",
            v.len(),
            paths.len(),
            v.join(" ")
        ),
    }
}

fn verify_state(ok: Option<bool>) -> &'static str {
    match ok {
        Some(true) => "passed",
        Some(false) => "FAILED",
        None => "not run",
    }
}

pub fn duration(seconds: Option<i64>) -> String {
    match seconds {
        None => "-".into(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) => format!("{}m{:02}s", s / 60, s % 60),
    }
}

fn run_cells(run: &Run) -> Vec<String> {
    vec![
        format!("#{}", run.id),
        run.state.name().into(),
        run.project.clone(),
        verify_cell(run),
        run.cost_usd.map_or("-".into(), |c| format!("${c:.2}")),
        duration(run.seconds),
        match accept::review_reasons(run).len() {
            0 => "clean".into(),
            n => format!("{n} to read"),
        },
        truncate(&run.text, 60),
    ]
}

/// One line per finished run, for `pma dispatch`.
pub fn run_line(run: &Run) -> String {
    let mut line = run_cells(run).join("  ");
    if let Some(e) = &run.error {
        line.push_str(&format!("\n  {e}"));
    }
    line
}

pub fn runs(runs: &[Run]) -> String {
    let rows: Vec<Vec<String>> = runs.iter().map(run_cells).collect();
    table(&rows, "")
}

/// One line per attempt, oldest first. A run with a single attempt says
/// nothing the rows above it do not, so it is left out.
fn attempt_lines(attempts: &[Attempt]) -> String {
    if attempts.len() < 2 {
        return String::new();
    }
    let rows: Vec<Vec<String>> = attempts
        .iter()
        .map(|a| {
            vec![
                format!("  {}", a.n),
                a.outcome.clone().unwrap_or_else(|| "?".into()),
                match a.verify_ok {
                    Some(true) => "verify passed".into(),
                    Some(false) => "verify FAILED".into(),
                    None => "verify not run".into(),
                },
                a.model.clone().unwrap_or_else(|| a.agent.clone()),
                a.cost_usd
                    .map_or("cost not reported".into(), |c| format!("${c:.2}")),
                duration(a.seconds),
            ]
        })
        .collect();
    format!("\nattempts:\n{}", table(&rows, ""))
}

pub fn run_detail(run: &Run, attempts: &[Attempt], diff: &str) -> String {
    let mut rows = vec![
        vec!["task".into(), run.text.clone()],
        vec![
            "agent".into(),
            format!(
                "{}{}, {}, {}",
                run.agent,
                run.model
                    .as_deref()
                    .map_or(String::new(), |m| format!(" {m}")),
                run.cost_usd
                    .map_or("cost not reported".into(), |c| format!("${c:.2}")),
                duration(run.seconds)
            ),
        ],
        vec![
            "branch".into(),
            format!("{} in {}", run.branch, run.worktree.display()),
        ],
        vec![
            "verify".into(),
            match &run.verify {
                // Base and head together separate a regression from a
                // repository that was already failing.
                Some(v) => format!(
                    "`{v}`: base {}, head {}",
                    verify_state(run.verify_base_ok),
                    verify_state(run.verify_ok)
                ),
                None => "none detected; set projects.<name>.verify".into(),
            },
        ],
    ];
    if let Some(c) = run.class {
        rows.push(vec![
            "class".into(),
            match run.scope.as_slice() {
                [] => format!("{}, any path", c.name()),
                globs => format!("{}, within {}", c.name(), globs.join(" ")),
            },
        ]);
        rows.push(vec!["scope".into(), scope_cell(run, c)]);
        if let (Some(rev), Some(name)) = (run.route_revision, run.route.as_deref()) {
            rows.push(vec![
                "route".into(),
                format!(
                    "{name} in revision {rev}, approval {}",
                    run.approval.map_or("-", |a| a.name())
                ),
            ]);
        }
        if let Some(n) = run.complexity {
            rows.push(vec![
                "complexity".into(),
                format!("{n} of 5 ({})", run.estimator.as_deref().unwrap_or("?")),
            ]);
        }
    }
    if let Some(stat) = &run.diffstat {
        rows.push(vec!["changes".into(), stat.clone()]);
    }
    if let Some(n) = run.commits.filter(|n| *n > 0) {
        rows.push(vec![
            "commits".into(),
            format!("{n} beyond the base; the agent was told not to commit"),
        ]);
    }
    if let Some(by) = &run.approved_by {
        rows.push(vec![
            "approved".into(),
            format!(
                "by {by}, tree {}",
                run.approved_tree
                    .as_deref()
                    .map_or("?", |t| &t[..t.len().min(12)])
            ),
        ]);
    }
    if let Some(s) = run.review_seconds {
        rows.push(vec!["reviewed".into(), duration(Some(s))]);
    }
    for (label, value) in [
        ("feedback", &run.feedback),
        ("error", &run.error),
        ("outcome", &run.outcome),
    ] {
        if let Some(v) = value {
            rows.push(vec![label.into(), v.clone()]);
        }
    }
    let mut out = format!(
        "#{} {}  {}{}\n",
        run.id,
        run.state.name(),
        run.project,
        run.quadrant
            .as_deref()
            .map_or(String::new(), |q| format!("  {q}"))
    );
    out.push_str(&table(&rows, "  "));
    if let Some(summary) = run.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str("\nsummary:\n");
        for line in summary.lines() {
            out.push_str(&format!("  {line}\n").replace("  \n", "\n"));
        }
    }
    let reasons = accept::review_reasons(run);
    if reasons.is_empty() {
        out.push_str("\naccept: every recorded gate is clean\n");
    } else {
        out.push_str("\nread this run because:\n");
        for r in &reasons {
            out.push_str(&format!("  - {r}\n"));
        }
    }
    out.push_str(&attempt_lines(attempts));
    out.push_str("\ndiff:\n");
    out.push_str(diff);
    if !diff.ends_with('\n') {
        out.push('\n');
    }
    out
}

pub fn ago(seconds: i64) -> String {
    match seconds {
        s if s < 90 => "just now".into(),
        s if s < 90 * 60 => format!("{} minutes ago", s / 60),
        s if s < 36 * 3600 => format!("{} hours ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rank::Task;
    use crate::todo::Priority;

    fn placed(
        project: &str,
        line: Option<i64>,
        q: Quadrant,
        urgency: Option<Urgency>,
        due: Option<i64>,
    ) -> Placed {
        Placed {
            task: Task {
                project: project.into(),
                tier: 1,
                priority: Priority::High,
                text: format!("text of {project}"),
                line,
                key: None,
                group: None,
                due,
                tagged_urgent: false,
                signal_urgent: false,
                eligible: false,
                age_days: 12,
            },
            importance: 0.5,
            urgency,
            quadrant: q,
        }
    }

    #[test]
    fn matrix_lists_quadrants_with_counts_and_limits() {
        let rows = vec![
            placed(
                "alpha",
                Some(5),
                Quadrant::Q1,
                Some(Urgency::Due(-3)),
                Some(97),
            ),
            placed("beta", None, Quadrant::Q1, Some(Urgency::Signal), None),
            placed("gamma", Some(9), Quadrant::Q2, None, Some(130)),
            placed("delta", Some(2), Quadrant::Q2, None, None),
        ];
        let out = matrix(&rows, Some(1), None, 100);
        assert_eq!(
            out,
            "Q1 Do right away: 2, top 1\n  alpha:5  T1  high  overdue 3d  text of alpha\n\
             \nQ2 Schedule for later: 2, top 1\n  gamma:9  T1  high  due in 30d  text of gamma\n\
             \nQ3 Delegate or avoid: 0\n\
             \nQ4 Remove: 0\n"
        );
        let q2 = matrix(&rows, None, Some(Quadrant::Q2), 100);
        assert!(q2.starts_with("Q2 Schedule for later: 2\n"), "{q2}");
        assert!(
            q2.contains("delta:2  T1  high  open 12d    text of delta"),
            "{q2}"
        );
        assert!(!q2.contains("Q1"), "{q2}");
    }

    #[test]
    fn group_column_appears_only_when_a_shown_task_has_one() {
        let mut a = placed("a", Some(1), Quadrant::Q2, None, None);
        a.task.group = Some("Security Hardening".into());
        let b = placed("b", Some(2), Quadrant::Q2, None, None);
        let c = placed("c", Some(3), Quadrant::Q4, None, None);
        let out = matrix(&[a, b, c], None, None, 0);
        assert!(
            out.contains("  a:1  T1  high  open 12d  Security Hardening  text of a\n"),
            "{out}"
        );
        assert!(
            out.contains("  b:2  T1  high  open 12d                      text of b\n"),
            "{out}"
        );
        assert!(
            out.contains("  c:3  T1  high  open 12d  text of c\n"),
            "{out}"
        );
    }

    #[test]
    fn long_text_is_truncated() {
        assert_eq!(truncate("abcdef", 6), "abcdef");
        assert_eq!(truncate("abcdefg", 6), "abc...");
        let mut long = placed("p", Some(1), Quadrant::Q1, Some(Urgency::Tagged), None);
        long.task.text = "x".repeat(150);
        let out = matrix(&[long], None, Some(Quadrant::Q1), 0);
        assert!(out.contains(&format!("{}...\n", "x".repeat(97))), "{out}");
    }

    #[test]
    fn status_sorts_by_health_and_explains() {
        let cfg = Config::default();
        let quiet = Project {
            name: "quiet".into(),
            tier: 1,
            open: vec![],
            idle_days: Some(1),
            ci: Ci::Passing,
            dirty: 0,
            leftover: 0,
            ahead: Some(0),
            deps: None,
        };
        let busy = Project {
            name: "busy".into(),
            open: vec![Priority::Critical],
            ci: Ci::Unknown("offline".into()),
            dirty: 2,
            ahead: Some(1),
            deps: Some((3, 0)),
            ..quiet.clone()
        };
        let rows = [
            StatusRow {
                project: &quiet,
                has_todo: true,
                lint_errors: 0,
                scan_error: None,
            },
            StatusRow {
                project: &busy,
                has_todo: true,
                lint_errors: 3,
                scan_error: None,
            },
        ];
        let out = status(&cfg, &rows, true);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            "project  tier  health  open  idle  ci       deps  local               todo"
        );
        assert!(
            lines[1].starts_with("busy     1     0.")
                && lines[1].ends_with("2 changed, 1 ahead  3 lint errors"),
            "{out}"
        );
        assert_eq!(
            lines[2],
            "quiet    1     0.00    0     1d    passing  -     clean               ok"
        );
        assert!(out.contains("\nbusy  tier 1 (x1)  health"), "{out}");
        assert!(
            out.contains("\n  ci        n/a     w=3         unknown: offline\n"),
            "{out}"
        );
        assert!(
            out.contains("last code commit 1 day ago, horizon 30"),
            "{out}"
        );
    }
}
