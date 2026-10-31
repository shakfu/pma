//! Text output for `pma matrix` and `pma status`.

use crate::accept;
use crate::class::Class;
use crate::config::Config;
use crate::history;
use crate::rank::{self, Placed, Project, Quadrant, State, Urgency};
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
    /// False for a project ranked at `default_tier`, or at tier 5 by `--all`.
    pub tiered: bool,
    pub has_todo: bool,
    pub lint_errors: i64,
    pub faults: &'a [(crate::scan::Fault, String)],
}

/// Projects worst first: by the worst state each is in, then by tier, then by
/// name. The state column names the worst, with a count of any others;
/// `explain` lists every state and what put the project in it.
/// The projects in status order, worst state first, each with its states.
fn placed<'a>(
    cfg: &Config,
    rows: &'a [StatusRow<'a>],
) -> Vec<(Vec<(State, String)>, &'a StatusRow<'a>)> {
    let mut placed: Vec<(Vec<(State, String)>, &StatusRow)> = rows
        .iter()
        .map(|r| {
            let facts = rank::Facts { faults: r.faults };
            (rank::states(cfg, r.project, facts), r)
        })
        .collect();
    placed.sort_by(|a, b| {
        a.0[0]
            .0
            .cmp(&b.0[0].0)
            .then(a.1.project.tier.cmp(&b.1.project.tier))
            .then_with(|| a.1.project.name.cmp(&b.1.project.name))
    });

    placed
}

/// The cells of one status row, as `status` prints them.
fn status_cells(states: &[(State, String)], r: &StatusRow) -> Vec<String> {
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
    vec![
        p.name.clone(),
        if r.tiered {
            p.tier.to_string()
        } else {
            "-".into()
        },
        match states.len() {
            1 => states[0].0.name().to_string(),
            n => format!("{} +{}", states[0].0.name(), n - 1),
        },
        p.open.len().to_string(),
        p.idle_days.map_or("-".into(), |d| format!("{d}d")),
        match &p.ci {
            Ci::Failing(_) => "failing".to_string(),
            Ci::Passing => "passing".into(),
            Ci::NoRuns => "none".into(),
            Ci::Unknown(_) => "unknown".into(),
        },
        p.deps.map_or("-".into(), |d| d.moves.to_string()),
        if local.is_empty() {
            "clean".into()
        } else {
            local
        },
        match (r.has_todo, r.lint_errors, todo_unread(r)) {
            (_, _, true) => "unreadable".into(),
            (false, _, _) => "missing".into(),
            (true, 0, _) => "ok".into(),
            (true, n, _) => format!("{n} lint errors"),
        },
    ]
}

fn todo_unread(r: &StatusRow) -> bool {
    r.faults
        .iter()
        .any(|(f, _)| *f == crate::scan::Fault::TodoUnreadable)
}

pub const STATUS_HEADINGS: [&str; 9] = [
    "project", "tier", "state", "open", "idle", "ci", "deps", "local", "todo",
];

/// A project's states, worst first, each with why; then its status cells.
pub type StatusLine = (Vec<(State, String)>, Vec<String>);

pub fn status_rows(cfg: &Config, rows: &[StatusRow]) -> Vec<StatusLine> {
    placed(cfg, rows)
        .into_iter()
        .map(|(states, r)| {
            let cells = status_cells(&states, r);
            (states, cells)
        })
        .collect()
}

/// The terminal's narrower cells for `local` and `todo`, so a row fits 80
/// columns for a name of up to 15 characters. Unpushed commits and leftover
/// branches already place a project in `unpublished`, and `--explain` counts
/// them, so `local` keeps changed files alone, as `dirty`.
fn terse(cells: &mut [String], r: &StatusRow) {
    cells[7] = match r.project.dirty {
        0 => "-".into(),
        n => n.to_string(),
    };
    cells[8] = match (r.has_todo, r.lint_errors, todo_unread(r)) {
        (_, _, true) => "unread".into(),
        (true, n, false) if n > 0 => format!("{n} lint"),
        _ => std::mem::take(&mut cells[8]),
    };
}

pub fn status(cfg: &Config, rows: &[StatusRow], explain: bool) -> String {
    let placed = placed(cfg, rows);
    let mut headings = STATUS_HEADINGS.map(String::from).to_vec();
    headings[7] = "dirty".into();
    let mut cells = vec![headings];
    for (states, r) in &placed {
        let mut row = status_cells(states, r);
        terse(&mut row, r);
        cells.push(row);
    }
    let mut out = table(&cells, "");

    if explain {
        for (states, r) in &placed {
            let p = r.project;
            out.push_str(&format!(
                "\n{}  tier {}{}\n",
                p.name,
                p.tier,
                if r.tiered { "" } else { ", untiered" },
            ));
            let lines: Vec<Vec<String>> = states
                .iter()
                .map(|(state, why)| vec![state.name().to_string(), why.clone()])
                .collect();
            out.push_str(&table(&lines, "  "));
            if let Ci::Unknown(why) = &p.ci {
                out.push_str(&format!("  CI not measured: {why}\n"));
            }
            match p.deps {
                None => out.push_str("  deps not measured; `pma scan --deps`\n"),
                Some(d) if d.measured > cfg.deps_days => out.push_str(&format!(
                    "  deps measured {}, over deps.days; `pma scan --deps`\n",
                    rank::days_ago(d.measured)
                )),
                Some(_) => {}
            }
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
/// One row per attempt: number, outcome, verify, model, cost, time.
pub fn attempt_rows(attempts: &[Attempt]) -> Vec<Vec<String>> {
    attempts
        .iter()
        .map(|a| {
            vec![
                a.n.to_string(),
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
        .collect()
}

fn attempt_lines(attempts: &[Attempt]) -> String {
    if attempts.len() < 2 {
        return String::new();
    }
    let mut rows = attempt_rows(attempts);
    for r in &mut rows {
        r[0] = format!("  {}", r[0]);
    }
    format!("\nattempts:\n{}", table(&rows, ""))
}

/// `changelog` is the run's drafted entries: `None` when its project keeps
/// no `CHANGELOG.md`, empty when the agent drafted none.
/// A run's facts as label and value, in the order `run_detail` prints them.
pub fn run_fields(run: &Run) -> Vec<Vec<String>> {
    let mut rows = vec![
        vec!["task".into(), run.text.clone()],
        vec![
            "issue".into(),
            run.gh.map_or("none".into(), |n| format!("#{n}")),
        ],
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
        match run.workspace {
            crate::config::Workspace::Clone => vec![
                "edited".into(),
                format!("in clone {} on {}", run.repo.display(), run.branch),
            ],
            crate::config::Workspace::Worktree => vec![
                "branch".into(),
                format!("{} in {}", run.branch, run.worktree.display()),
            ],
        },
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
        // A revision with no approval was in shadow, which records a route,
        // or none, and applies nothing.
        if let Some(rev) = run.route_revision {
            let name = run.route.as_deref().unwrap_or("no route");
            rows.push(vec![
                "route".into(),
                match run.approval {
                    Some(a) => format!("{name} in revision {rev}, approval {}", a.name()),
                    None => format!("{name} in revision {rev}, in shadow"),
                },
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
    if let Some(pane) = &run.pane {
        let state = run.agent_state.as_deref().unwrap_or("-");
        rows.push(vec!["herdr".into(), format!("{pane}, {state}")]);
    }
    if let Some(s) = run.review_time() {
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
    rows
}

/// A bundle's items, numbered as its prompt and `--drop` number them, one
/// per line.
pub fn item_lines(run: &Run, indent: &str) -> String {
    run.items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let open = if item.dropped { "  (left open)" } else { "" };
            format!("{indent}{}. {}{open}\n", i + 1, item.text)
        })
        .collect()
}

pub fn run_detail(run: &Run, attempts: &[Attempt], changelog: Option<&str>, diff: &str) -> String {
    let rows = run_fields(run);
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
    if !run.items.is_empty() {
        out.push_str("\nitems:\n");
        out.push_str(&item_lines(run, "  "));
    }
    if let Some(summary) = run.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str("\nsummary:\n");
        for line in summary.lines() {
            out.push_str(&format!("  {line}\n").replace("  \n", "\n"));
        }
    }
    match changelog.map(str::trim) {
        Some("") => out.push_str("\nchangelog: none drafted\n"),
        Some(entries) => {
            out.push_str("\nchangelog:\n");
            for line in entries.lines() {
                out.push_str(&format!("  {line}\n").replace("  \n", "\n"));
            }
        }
        None => {}
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

/// Recorded scans as a table, in the order given. A fault no listed scan
/// found has no column.
pub fn scans(scans: &[history::Scan], now: i64) -> String {
    let found: Vec<usize> = (0..crate::scan::Fault::ALL.len())
        .filter(|&i| scans.iter().any(|s| s.faults[i] > 0))
        .collect();
    let mut head: Vec<String> = ["scan", "when", "kind", "projects", "failing"]
        .map(String::from)
        .to_vec();
    head.extend(
        found
            .iter()
            .map(|&i| crate::scan::Fault::ALL[i].name().to_string()),
    );
    head.extend(["open", "opened", "closed", "other"].map(String::from));
    let mut rows = vec![head];
    for s in scans {
        let mut row = vec![
            s.id.to_string(),
            ago(now - s.at),
            if s.full { "full" } else { "partial" }.into(),
            s.projects.to_string(),
            s.failing.to_string(),
        ];
        row.extend(found.iter().map(|&i| s.faults[i].to_string()));
        row.extend([s.open, s.opened, s.closed, s.other].map(|n| n.to_string()));
        rows.push(row);
    }
    table(&rows, "")
}

/// One line per change.
pub fn changes(changes: &[history::ProjectChange]) -> String {
    let rows: Vec<Vec<String>> = changes
        .iter()
        .map(|c| {
            vec![
                c.project.clone(),
                c.change.name().into(),
                truncate(&c.change.detail(), TEXT_WIDTH),
            ]
        })
        .collect();
    table(&rows, "  ")
}

/// One line per change, with a project's items opened and closed counted
/// on one line rather than listed.
pub fn changes_brief(changes: &[history::ProjectChange]) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut items: Vec<(&str, usize, usize)> = Vec::new();
    for c in changes {
        match &c.change {
            history::Change::Opened { .. } | history::Change::Closed { .. } => {
                let opened = matches!(c.change, history::Change::Opened { .. }) as usize;
                match items.iter_mut().find(|(p, ..)| *p == c.project) {
                    Some(e) => {
                        e.1 += opened;
                        e.2 += 1 - opened;
                    }
                    None => items.push((&c.project, opened, 1 - opened)),
                }
            }
            other => rows.push(vec![
                c.project.clone(),
                other.name().into(),
                truncate(&other.detail(), TEXT_WIDTH),
            ]),
        }
    }
    for (project, opened, closed) in items {
        rows.push(vec![
            project.into(),
            "items".into(),
            format!("{opened} opened, {closed} closed"),
        ]);
    }
    rows.sort_by(|a, b| a[0].cmp(&b[0]));
    table(&rows, "  ")
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
                manual: false,
                gh: None,
                age_days: 12,
            },
            importance: 0.5,
            urgency,
            quadrant: q,
        }
    }

    /// A fault no listed scan found has no column; one any scan found does.
    #[test]
    fn scans_list_only_the_faults_found() {
        let scan = |id, lint| history::Scan {
            id,
            at: 0,
            full: true,
            projects: 3,
            failing: 0,
            faults: [0, 0, 0, 0, 0, lint, 0],
            open: 0,
            opened: 0,
            closed: 0,
            other: 0,
        };
        let head = |t: String| t.lines().next().unwrap().to_string();
        let none = head(scans(&[scan(1, 0)], 0));
        assert!(
            !none.contains("lint") && !none.contains("unknown"),
            "{none}"
        );
        let some = head(scans(&[scan(2, 1), scan(1, 0)], 0));
        assert!(
            some.contains("TODO lint") && !some.contains("CI unknown"),
            "{some}"
        );
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
             \nQ4 Later: 0\n"
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

    /// Worst state first, then tier: a tier-2 project with failing CI comes
    /// before a tier-1 project with unpublished work, which comes before a
    /// clear one. The state column names the worst and counts the rest.
    #[test]
    fn status_sorts_by_the_worst_state_then_tier() {
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
            deps: Some(rank::Deps {
                moves: 3,
                measured: 0,
                lock: Some(31),
                held_back: None,
            }),
            ..quiet.clone()
        };
        let red = Project {
            name: "red".into(),
            tier: 2,
            ci: Ci::Failing(vec!["test".into()]),
            deps: Some(rank::Deps {
                moves: 3,
                measured: 31,
                lock: Some(90),
                held_back: None,
            }),
            ..quiet.clone()
        };
        let row = |project| StatusRow {
            project,
            tiered: true,
            has_todo: true,
            lint_errors: 0,
            faults: &[],
        };
        let out = status(&cfg, &[row(&quiet), row(&busy), row(&red)], true);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[..4],
            [
                "project  tier  state           open  idle  ci       deps  dirty  todo",
                "red      2     failing CI      0     1d    failing  3     -      ok",
                "busy     1     unpublished +2  1     1d    unknown  3     2      ok",
                "quiet    1     clear           0     1d    passing  -     -      ok",
            ]
        );
        assert!(
            out.contains(
                "\nbusy  tier 1\n  unpublished  1 unpushed commit\n  critical     1 open item\n  \
                 stale deps   3 to move, lock 31d old, measured today\n  CI not measured: offline\n"
            ),
            "{out}"
        );
        assert!(
            out.contains("\n  deps measured 31 days ago, over deps.days; `pma scan --deps`\n"),
            "a measurement past the window places red in no deps state: {out}"
        );
        assert!(
            out.contains("\nquiet  tier 1\n  clear\n  deps not measured; `pma scan --deps`\n"),
            "{out}"
        );
    }

    /// A 15-character name with the widest cells a state, a lint count and
    /// three-digit counts produce still fits a terminal of 80 columns.
    #[test]
    fn a_status_row_fits_80_columns() {
        let cfg = Config::default();
        let wide = Project {
            name: "buildpy-bundler".into(),
            tier: 1,
            open: vec![Priority::Low; 120],
            idle_days: Some(999),
            ci: Ci::Failing(vec!["test".into()]),
            dirty: 120,
            leftover: 2,
            ahead: Some(3),
            deps: Some(rank::Deps {
                moves: 153,
                measured: 0,
                lock: Some(336),
                held_back: Some(18),
            }),
        };
        let row = StatusRow {
            project: &wide,
            tiered: true,
            has_todo: true,
            lint_errors: 146,
            faults: &[],
        };
        let out = status(&cfg, &[row], true);
        let long: Vec<&str> = out.lines().filter(|l| l.chars().count() > 80).collect();
        assert!(long.is_empty(), "{long:?}");
    }
}
