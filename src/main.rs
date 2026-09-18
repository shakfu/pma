//! pma: portfolio maintenance across many repositories.
//!
//! Design: `docs/dev/design.md`.

mod accept;
mod agent;
mod class;
mod config;
mod dates;
mod deps;
mod dispatch;
mod rank;
mod report;
mod scan;
mod ship;
mod store;
mod sync;
mod todo;
mod tui;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, Subcommand};

use config::Config;
use store::{Result, RunState, Session, Store};

#[derive(Parser)]
#[command(name = "pma", version, about = "Maintain many projects from one place")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check TODO.md files against the format.
    ///
    /// Each path is a TODO.md file or a directory containing one. Exits 1 when
    /// any file has errors or cannot be read; warnings alone exit 0.
    Lint {
        /// Files or directories; defaults to the current directory.
        paths: Vec<PathBuf>,
    },
    /// Remove finished items and `## Done` sections from TODO.md files. Dry
    /// run unless --apply.
    ///
    /// Each path is a TODO.md file or a directory containing one. A file with
    /// lint errors is skipped. Edits stay uncommitted.
    Prune {
        /// Files or directories; defaults to the current directory.
        paths: Vec<PathBuf>,
        /// Remove them instead of listing them.
        #[arg(long)]
        apply: bool,
    },
    /// List, add or remove the directories whose git repos are projects.
    Root {
        #[command(subcommand)]
        action: Option<RootAction>,
    },
    /// Show or set a project's tier: 1 (most important) to 5, or `none`.
    Tier {
        /// The project's directory name under a root.
        project: String,
        /// 1 to 5, or `none`; omit to show the current tier.
        tier: Option<String>,
    },
    /// List settings, show one, or set one.
    Config {
        /// A setting such as `tiers.2` or `weights.ci`; omit to list all.
        key: Option<String>,
        /// The new value; omit to show the current one.
        value: Option<String>,
        /// Restore the default for KEY.
        #[arg(long, requires = "key", conflicts_with = "value")]
        reset: bool,
    },
    /// Scan projects under the roots and record the results.
    ///
    /// With no names, scans every project and forgets projects no longer found.
    Scan {
        /// Project names; defaults to every project.
        projects: Vec<String>,
        /// Skip GitHub; CI is recorded as unknown.
        #[arg(long)]
        offline: bool,
        /// Also measure outdated dependencies (cargo, uv, go); seconds per
        /// project. Without it, the last measurement is kept.
        #[arg(long, conflicts_with = "offline")]
        deps: bool,
    },
    /// Show tasks of tiered projects in the Eisenhower matrix.
    Matrix {
        /// Limit to these projects.
        projects: Vec<String>,
        /// Show one quadrant: q1, q2, q3 or q4.
        #[arg(short, long, value_parser = parse_quadrant)]
        quadrant: Option<rank::Quadrant>,
        /// Show every task instead of the top `quadrant_limit`.
        #[arg(long)]
        all: bool,
    },
    /// Rank tiered projects by health.
    Status {
        /// Limit to these projects.
        projects: Vec<String>,
        /// Show each signal's contribution.
        #[arg(long)]
        explain: bool,
    },
    /// Run an agent on tasks, each in its own worktree of the remote default
    /// branch.
    ///
    /// A target is `project:line`, a TODO.md line from the last scan,
    /// `project:ci` for failing CI, or `project:deps` for outdated
    /// dependencies. Blocks until every agent has finished.
    Dispatch {
        /// Targets; with --auto, projects to draw from.
        targets: Vec<String>,
        /// Draw the top tasks from `dispatch_quadrants`, then
        /// `overflow_quadrants`.
        #[arg(long)]
        auto: bool,
        /// With --auto, how many tasks; defaults to `max_parallel`.
        #[arg(short = 'n', long, requires = "auto")]
        count: Option<usize>,
        /// Clear the named target's consumed attempts and dispatch it again.
        #[arg(long, conflicts_with = "auto")]
        retry: bool,
    },
    /// List runs that are not shipped or rejected, show one, or act on it.
    Review {
        /// A run id; omit to list runs.
        id: Option<i64>,
        /// Mark a ready run for `pma ship`.
        #[arg(long, requires = "id", conflicts_with_all = ["reject", "rework"])]
        approve: bool,
        /// Remove the run's worktree and branch.
        #[arg(long, requires = "id", conflicts_with = "rework")]
        reject: bool,
        /// Run the agent again in the same worktree with this feedback.
        #[arg(long, requires = "id", value_name = "FEEDBACK")]
        rework: Option<String>,
        /// Minutes spent reviewing this run, added to the run's total.
        #[arg(long, requires = "id", value_name = "N")]
        minutes: Option<u32>,
    },
    /// Commit and publish approved runs, then remove their worktrees.
    Ship {
        /// Limit to these projects.
        projects: Vec<String>,
    },
    /// List portfolio notes, or add, edit or remove one.
    Note {
        #[command(subcommand)]
        action: Option<NoteAction>,
    },
    /// Browse the matrix in the terminal. Reads the last scan.
    Tui,
    /// Sync `Critical` items with GitHub Issues. Dry run unless --apply.
    ///
    /// Opens an issue per Critical item and writes `gh:N` into its line,
    /// marks items done when their issue is closed, keeps issue titles and
    /// the `pma:critical` label in line with TODO.md, and lists open issues
    /// by other people that no item links. TODO.md edits stay uncommitted.
    Sync {
        /// Limit to these projects; defaults to every scanned project.
        projects: Vec<String>,
        /// Make the changes instead of listing them.
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand)]
enum NoteAction {
    /// Add a note; the words are joined by spaces.
    Add {
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Replace a note's text.
    Edit {
        id: i64,
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Remove a note.
    Rm { id: i64 },
}

#[derive(Subcommand)]
enum RootAction {
    /// Add a directory: the git repos directly under it, or the directory
    /// itself if it is one, become projects.
    Add { dir: PathBuf },
    /// Remove a directory; its projects are forgotten at the next full scan.
    Rm { dir: PathBuf },
}

fn parse_quadrant(s: &str) -> std::result::Result<rank::Quadrant, String> {
    rank::Quadrant::ALL
        .into_iter()
        .find(|q| format!("{q:?}").eq_ignore_ascii_case(s))
        .ok_or_else(|| "expected q1, q2, q3 or q4".into())
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Lint { paths } => return lint(&paths),
        Command::Prune { paths, apply } => return prune(&paths, apply),
        Command::Root { action } => root(action),
        Command::Tier { project, tier } => set_tier(&project, tier.as_deref()),
        Command::Config { key, value, reset } => configure(key.as_deref(), value.as_deref(), reset),
        Command::Scan {
            projects,
            offline,
            deps,
        } => run_scan(&projects, offline, deps),
        Command::Matrix {
            projects,
            quadrant,
            all,
        } => show_matrix(&projects, quadrant, all),
        Command::Status { projects, explain } => show_status(&projects, explain),
        Command::Dispatch {
            targets,
            auto,
            count,
            retry,
        } => run_dispatch(&targets, auto, count, retry),
        Command::Review {
            id,
            approve,
            reject,
            rework,
            minutes,
        } => run_review(id, approve, reject, rework.as_deref(), minutes),
        Command::Ship { projects } => run_ship(&projects),
        Command::Sync { projects, apply } => run_sync(&projects, apply),
        Command::Note { action } => note(action),
        Command::Tui => run_tui(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pma: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The TODO.md each path names: the file itself, or the one in a directory.
fn todo_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    if paths.is_empty() {
        return vec![PathBuf::from("./TODO.md")];
    }
    paths
        .iter()
        .map(|p| {
            if p.is_dir() {
                p.join("TODO.md")
            } else {
                p.clone()
            }
        })
        .collect()
}

fn lint(paths: &[PathBuf]) -> ExitCode {
    let (mut errors, mut warnings, mut failed) = (0, 0, false);
    for file in todo_files(paths) {
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(err) => {
                println!("{}: error: {err}", file.display());
                errors += 1;
                failed = true;
                continue;
            }
        };
        let mut parsed = todo::parse(&text);
        parsed.diagnostics.sort_by_key(|d| d.line);
        for d in &parsed.diagnostics {
            println!(
                "{}:{}: {}: {}",
                file.display(),
                d.line,
                d.severity,
                d.message
            );
            match d.severity {
                todo::Severity::Error => errors += 1,
                todo::Severity::Warning => warnings += 1,
            }
        }
        failed |= parsed.has_errors();
    }

    if errors + warnings > 0 {
        eprintln!("{errors} errors, {warnings} warnings");
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn prune(paths: &[PathBuf], apply: bool) -> ExitCode {
    let (mut count, mut failed) = (0, false);
    for file in todo_files(paths) {
        let fail = |why: String| println!("{}: error: {why}", file.display());
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(err) => {
                fail(err.to_string());
                failed = true;
                continue;
            }
        };
        if todo::parse(&text).has_errors() {
            println!("{}: skipped: lint errors; see `pma lint`", file.display());
            continue;
        }
        let pruned = todo::prune(&text);
        for done in &pruned.done_sections {
            println!(
                "{}:{}: remove `## Done`, lines {}-{}",
                file.display(),
                done.first,
                done.first,
                done.last
            );
            for (n, line) in &done.open {
                println!(
                    "{}:{n}: remove open item with `## Done`: `{line}`",
                    file.display()
                );
            }
        }
        for item in &pruned.items {
            println!("{}:{}: remove `{}`", file.display(), item.line, item.text);
        }
        let removals = pruned.done_sections.len() + pruned.items.len();
        if apply
            && removals > 0
            && let Err(err) = std::fs::write(&file, pruned.text)
        {
            fail(err.to_string());
            failed = true;
            continue;
        }
        count += removals;
    }

    if count == 0 {
        println!("nothing to prune");
    } else if apply {
        println!("{count} removals made; TODO.md edits are uncommitted");
    } else {
        println!("{count} removals; run `pma prune --apply` to make them");
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn root(action: Option<RootAction>) -> Result<()> {
    let store = Store::open_default()?;
    match action {
        None => {
            for r in store.roots()? {
                println!("{}", r.display());
            }
        }
        Some(RootAction::Add { dir }) => {
            let dir = dir
                .canonicalize()
                .map_err(|e| format!("{}: {e}", dir.display()))?;
            if !dir.is_dir() {
                return Err(format!("{} is not a directory", dir.display()).into());
            }
            if !store.add_root(&dir)? {
                println!("{} is already a root", dir.display());
            }
        }
        Some(RootAction::Rm { dir }) => {
            let canonical = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if !store.remove_root(&canonical)? && !store.remove_root(&dir)? {
                return Err(format!("{} is not a root", dir.display()).into());
            }
        }
    }
    Ok(())
}

fn set_tier(name: &str, tier: Option<&str>) -> Result<()> {
    let store = Store::open_default()?;
    let known = store.project(name)?;
    let Some(tier) = tier else {
        let row = known.ok_or_else(|| format!("unknown project `{name}`"))?;
        println!("{}", row.tier.map_or("none".into(), |t| t.to_string()));
        return Ok(());
    };
    let tier = match tier {
        "none" => None,
        t => Some(
            t.parse::<u8>()
                .ok()
                .filter(|t| (1..=5).contains(t))
                .ok_or("tier must be 1 to 5, or none")?,
        ),
    };
    let path = match known {
        Some(row) => row.path,
        None => find_project(&store, name)?,
    };
    store.set_tier(name, &path, tier)
}

fn find_project(store: &Store, name: &str) -> Result<PathBuf> {
    let (found, _) = scan::discover(&store.roots()?);
    found
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, p)| p)
        .ok_or_else(|| format!("no project `{name}` under the roots (pma root)").into())
}

fn configure(key: Option<&str>, value: Option<&str>, reset: bool) -> Result<()> {
    let store = Store::open_default()?;
    let rows = store.config_rows()?;
    match (key, value) {
        (None, _) => {
            let cfg = Config::with_overrides(rows.iter().map(|(k, v)| (k.as_str(), v.as_str())))?;
            for key in Config::keys() {
                let set = rows.iter().any(|(k, _)| *k == key);
                println!(
                    "{key} = {}{}",
                    cfg.get(&key).unwrap_or_default(),
                    if set { "  (set)" } else { "" }
                );
            }
            for (k, v) in rows.iter().filter(|(k, _)| k.starts_with("projects.")) {
                println!("{k} = {v}  (set)");
            }
            for (k, v) in rows
                .iter()
                .filter(|(k, _)| Config::default().get(k).is_none())
            {
                println!("{k} = {v}  (unknown; pma config {k} --reset)");
            }
        }
        (Some(key), _) if reset => {
            if !store.reset_config(key)? {
                println!("{key} was not set");
            }
        }
        (Some(key), None) => {
            let cfg = Config::with_overrides(rows.iter().map(|(k, v)| (k.as_str(), v.as_str())))?;
            println!(
                "{}",
                cfg.get(key)
                    .ok_or_else(|| format!("unknown setting `{key}`"))?
            );
        }
        (Some(key), Some(value)) => {
            Config::default()
                .set(key, value)
                .map_err(|e| format!("{key}: {e}"))?;
            store.set_config(key, value.trim())?;
        }
    }
    Ok(())
}

fn load_config(store: &Store) -> Result<Config> {
    let rows = store.config_rows()?;
    Ok(Config::with_overrides(
        rows.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    )?)
}

fn run_scan(names: &[String], offline: bool, deps: bool) -> Result<()> {
    let mut store = Store::open_default()?;
    let cfg = load_config(&store)?;
    let roots = store.roots()?;
    if roots.is_empty() {
        return Err("no roots; add one with `pma root add <dir>`".into());
    }
    let (found, warnings) = scan::discover(&roots);
    for w in warnings {
        eprintln!("warning: {w}");
    }
    let selected: Vec<(String, PathBuf)> = if names.is_empty() {
        found
    } else {
        names
            .iter()
            .map(|n| {
                found
                    .iter()
                    .find(|(f, _)| f == n)
                    .cloned()
                    .ok_or_else(|| format!("no project `{n}` under the roots"))
            })
            .collect::<std::result::Result<_, _>>()?
    };

    let started = Instant::now();
    let facts = scan::scan_all(&selected, &cfg.activity_ignore, offline, deps);
    for f in facts.iter().filter(|f| f.error.is_some()) {
        eprintln!(
            "warning: {}: {}",
            f.name,
            f.error.as_deref().unwrap_or_default()
        );
    }
    for (name, tier) in store.save_scan(&facts, names.is_empty(), dates::now())? {
        let lost = tier.map_or(String::new(), |t| format!("; its tier {t} is forgotten"));
        eprintln!("removed {name}: no longer under a root{lost}");
    }

    let with_todo = facts.iter().filter(|f| f.todo.is_some()).count();
    let items: usize = facts
        .iter()
        .filter_map(|f| f.todo.as_ref())
        .map(|t| t.items.len())
        .sum();
    let failing = facts
        .iter()
        .filter(|f| matches!(f.ci, scan::Ci::Failing(_)))
        .count();
    let mut summary = format!(
        "scanned {} projects in {:.1}s: {with_todo} with TODO.md, {items} open items",
        facts.len(),
        started.elapsed().as_secs_f64()
    );
    if !offline {
        let unknown = facts
            .iter()
            .filter(|f| matches!(f.ci, scan::Ci::Unknown(_)))
            .count();
        summary.push_str(&format!(", CI failing in {failing}, unknown in {unknown}"));
    }
    if deps {
        let outdated = facts
            .iter()
            .filter(|f| f.deps.as_ref().is_some_and(|d| d.outdated > Some(0)))
            .count();
        summary.push_str(&format!(", outdated dependencies in {outdated}"));
    }
    println!("{summary}");
    Ok(())
}

/// What the last scan recorded, for tiered projects.
struct Portfolio {
    cfg: Config,
    today: i64,
    projects: Vec<(rank::Project, store::ProjectRow)>,
    tasks: Vec<rank::Task>,
    untiered: usize,
    scanned_ago: i64,
}

fn portfolio(names: &[String]) -> Result<Portfolio> {
    let store = Store::open_default()?;
    let cfg = load_config(&store)?;
    let now = dates::now();
    let today = dates::day(now);
    let last = store
        .last_scan()?
        .ok_or("nothing scanned yet; run `pma scan`")?;

    let rows = store.projects()?;
    for n in names {
        if !rows.iter().any(|r| &r.name == n) {
            return Err(format!("unknown project `{n}`").into());
        }
    }
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|r| names.is_empty() || names.contains(&r.name))
        .collect();
    let untiered = rows.iter().filter(|r| r.tier.is_none()).count();
    let task_rows = store.tasks()?;

    let mut projects = Vec::new();
    let mut tasks = Vec::new();
    for row in rows {
        let Some(tier) = row.tier else { continue };
        let own: Vec<&store::TaskRow> =
            task_rows.iter().filter(|t| t.project == row.name).collect();
        let project = rank::Project {
            name: row.name.clone(),
            tier,
            open: own.iter().map(|t| t.priority).collect(),
            idle_days: row.last_activity.map(|t| (today - dates::day(t)).max(0)),
            ci: row.ci.clone(),
            dirty: row.dirty,
            leftover: row.leftover,
            ahead: row.ahead,
            deps: row
                .deps
                .map(|n| (n, row.deps_at.map_or(0, |t| (today - dates::day(t)).max(0)))),
        };
        tasks.extend(own.iter().map(|t| rank::Task {
            project: row.name.clone(),
            tier,
            priority: t.priority,
            text: t.text.clone(),
            line: Some(t.line),
            key: Some(t.key.clone()),
            group: t.group.clone(),
            due: t.due.as_deref().and_then(dates::parse),
            tagged_urgent: t.tags.iter().any(|g| g == "urgent"),
            signal_urgent: false,
            age_days:
                (today - dates::day(t.added_at.unwrap_or(t.first_seen).min(t.first_seen))).max(0),
        }));
        tasks.extend(rank::signal_tasks(&cfg, &project));
        projects.push((project, row));
    }
    Ok(Portfolio {
        cfg,
        today,
        projects,
        tasks,
        untiered,
        scanned_ago: now - last,
    })
}

fn header_text(p: &Portfolio) -> String {
    format!(
        "last scan {}; {} tiered projects, {} untiered (pma tier <project> <1-5>)",
        report::ago(p.scanned_ago),
        p.projects.len(),
        p.untiered
    )
}

fn header(p: &Portfolio) {
    println!("{}\n", header_text(p));
}

fn show_matrix(names: &[String], quadrant: Option<rank::Quadrant>, all: bool) -> Result<()> {
    let p = portfolio(names)?;
    header(&p);
    let limit = (!all).then_some(p.cfg.quadrant_limit as usize);
    let placed = rank::place(&p.cfg, p.tasks, p.today);
    print!("{}", report::matrix(&placed, limit, quadrant, p.today));
    Ok(())
}

fn show_status(names: &[String], explain: bool) -> Result<()> {
    let p = portfolio(names)?;
    header(&p);
    let rows: Vec<report::StatusRow> = p
        .projects
        .iter()
        .map(|(project, row)| report::StatusRow {
            project,
            has_todo: row.has_todo,
            lint_errors: row.lint_errors,
            scan_error: row.scan_error.as_deref(),
        })
        .collect();
    print!("{}", report::status(&p.cfg, &rows, explain));
    Ok(())
}

/// Whether a task already has a run that is not shipped or rejected. Text is
/// compared too, since sync may have added `gh:N` after dispatch.
fn has_run(runs: &[store::Run], project: &str, key: &str, text: &str) -> bool {
    runs.iter().any(|r| {
        !r.state.is_final()
            && r.project == project
            && (r.task_key == key || todo::normal_text(&r.text) == todo::normal_text(text))
    })
}

fn run_dispatch(targets: &[String], auto: bool, count: Option<usize>, retry: bool) -> Result<()> {
    let store = Store::open_default()?;
    let home = store::home()?;
    let session = Session::acquire(&home)?;
    store.fail_interrupted_runs(&session)?;
    settle_prs(&store)?;
    let active = store.runs()?;
    let rows = store.projects()?;
    let p = portfolio(if auto { targets } else { &[] })?;
    let placed = rank::place(&p.cfg, p.tasks, p.today);
    let quadrant = |project: &str, key: &str| {
        placed
            .iter()
            .find(|x| x.task.project == project && x.task.key.as_deref() == Some(key))
            .map(|x| format!("{:?}", x.quadrant))
    };
    let row = |project: &str| {
        rows.iter()
            .find(|r| r.name == project)
            .ok_or_else(|| format!("unknown project `{project}`").into())
            .map_err(|e: Box<dyn std::error::Error>| e)
    };

    // With --auto, every candidate in matrix order; `wanted` of them are
    // queued, so a refused candidate gives its place to the next.
    let mut picks = Vec::new();
    let wanted = match auto {
        true => count.unwrap_or(p.cfg.max_parallel as usize),
        false => targets.len(),
    };
    if auto {
        let lists = [&p.cfg.dispatch_quadrants, &p.cfg.overflow_quadrants];
        for list in lists {
            for x in placed.iter().filter(|x| list.contains(&x.quadrant)) {
                let Some(key) = &x.task.key else { continue };
                let taken = picks
                    .iter()
                    .any(|q: &dispatch::Pick| q.project == x.task.project && &q.key == key);
                if taken || has_run(&active, &x.task.project, key, &x.task.text) {
                    continue;
                }
                let spent =
                    store.consumed_attempts(&x.task.project, &dispatch::revision(&x.task.text))?;
                if spent >= dispatch::ATTEMPT_LIMIT {
                    continue;
                }
                let r = row(&x.task.project)?;
                picks.push(dispatch::Pick {
                    project: x.task.project.clone(),
                    repo: r.path.clone(),
                    key: key.clone(),
                    text: x.task.text.clone(),
                    gh: None,
                    tier: r.tier,
                    quadrant: Some(format!("{:?}", x.quadrant)),
                });
            }
        }
        let tasks = store.tasks()?;
        for pick in &mut picks {
            pick.gh = tasks
                .iter()
                .find(|t| t.project == pick.project && t.key == pick.key)
                .and_then(|t| t.gh);
        }
    } else {
        if targets.is_empty() {
            return Err("name a target such as `cynn:31`, or use --auto".into());
        }
        let tasks = store.tasks()?;
        for target in targets {
            let (project, what) = target.rsplit_once(':').ok_or_else(|| {
                format!("`{target}`: expected project:line, project:ci or project:deps")
            })?;
            let row = rows
                .iter()
                .find(|r| r.name == project)
                .ok_or_else(|| format!("unknown project `{project}`"))?;
            let (key, text, gh) = if what == "deps" {
                match row.deps.filter(|n| *n > 0) {
                    Some(n) => (
                        "deps".to_string(),
                        format!("update dependencies: {n} outdated"),
                        None,
                    ),
                    None => {
                        return Err(format!(
                            "{project}: no outdated dependencies at the last `pma scan --deps`"
                        )
                        .into());
                    }
                }
            } else if what == "ci" {
                match &row.ci {
                    scan::Ci::Failing(w) => {
                        ("ci".to_string(), format!("fix CI: {}", w.join(", ")), None)
                    }
                    _ => {
                        return Err(
                            format!("{project}: CI was not failing at the last scan").into()
                        );
                    }
                }
            } else {
                let line: i64 = what.parse().map_err(|_| {
                    format!("`{target}`: expected project:line, project:ci or project:deps")
                })?;
                let t = tasks
                    .iter()
                    .find(|t| t.project == project && t.line == line)
                    .ok_or_else(|| {
                        format!("{project}:{line} is not an open item at the last scan")
                    })?;
                (t.key.clone(), t.text.clone(), t.gh)
            };
            if has_run(&active, project, &key, &text) {
                return Err(format!("{target} already has a run; see `pma review`").into());
            }
            let rev = dispatch::revision(&text);
            if retry {
                store.reset_attempts(project, &rev)?;
            }
            let spent = store.consumed_attempts(project, &rev)?;
            if spent >= dispatch::ATTEMPT_LIMIT {
                return Err(format!(
                    "{target}: {spent} attempts on `{text}` were used without an accepted \
                     result; reword the task, or `pma dispatch {target} --retry`. \
                     Any worktree it left is removed by `pma review <id> --reject`"
                )
                .into());
            }
            picks.push(dispatch::Pick {
                project: project.into(),
                repo: row.path.clone(),
                tier: row.tier,
                quadrant: quadrant(project, &key),
                key,
                text,
                gh,
            });
        }
    }
    if picks.is_empty() || wanted == 0 {
        println!("nothing to dispatch");
        return Ok(());
    }

    let mut queued = Vec::new();
    // A project-level failure, such as a failed fetch, skips the project.
    let mut unreachable: Vec<&str> = Vec::new();
    for pick in &picks {
        if queued.len() == wanted {
            break;
        }
        if unreachable.contains(&pick.project.as_str()) {
            continue;
        }
        match dispatch::prepare(&store, &home, &p.cfg, pick) {
            Ok(dispatch::Prepared::Queued(run)) => {
                println!("#{} {}: {}", run.id, run.project, run.text);
                queued.push(*run);
            }
            Ok(dispatch::Prepared::Refused(why)) => eprintln!("warning: {why}"),
            Err(e) => {
                eprintln!("warning: {}: {e}", pick.project);
                unreachable.push(&pick.project);
            }
        }
    }
    if queued.is_empty() {
        return Err("no run started".into());
    }
    let finished = dispatch::execute(&store, &home, &p.cfg, queued, |run| {
        println!("{}", report::run_line(run));
    })?;
    let ready = finished
        .iter()
        .filter(|r| r.state == RunState::Ready)
        .count();
    let spent = finished
        .iter()
        .filter_map(|r| r.cost_usd)
        .fold(0.0, |a, c| a + c);
    println!(
        "{ready} ready, {} failed, ${spent:.2} spent; see `pma review`",
        finished.len() - ready
    );
    Ok(())
}

/// Reports runs whose pull request was merged or closed since the last check.
fn settle_prs(store: &Store) -> Result<()> {
    ship::settle(store, |run, outcome| match outcome {
        Ok(o) => println!("#{} {}: {o}", run.id, run.project),
        Err(e) => eprintln!(
            "warning: #{} {}: pull request state unknown: {e}",
            run.id, run.project
        ),
    })
}

fn run_review(
    id: Option<i64>,
    approve: bool,
    reject: bool,
    rework: Option<&str>,
    minutes: Option<u32>,
) -> Result<()> {
    let store = Store::open_default()?;
    let home = store::home()?;
    // Free, the lock proves that no session is running agents.
    let session = Session::try_acquire(&home)?;
    if let Some(s) = &session {
        store.fail_interrupted_runs(s)?;
    }
    // Reject and rework change the worktree, so they keep the lock. Others
    // release it now, so a starting dispatch does not wait on them.
    let session = match (session, reject || rework.is_some()) {
        (Some(s), true) => Some(s),
        (None, true) => Some(Session::acquire(&home)?),
        (_, false) => None,
    };
    let Some(id) = id else {
        settle_prs(&store)?;
        let runs: Vec<_> = store
            .runs()?
            .into_iter()
            .filter(|r| !r.state.is_final())
            .collect();
        if runs.is_empty() {
            println!("no runs to review");
        } else {
            print!("{}", report::runs(&runs));
        }
        return Ok(());
    };
    let mut run = store.run(id)?;
    // Added before the action, so a rework's own review time is not lost when
    // the run is reviewed again.
    if let Some(m) = minutes {
        run.review_seconds = Some(run.review_seconds.unwrap_or(0) + i64::from(m) * 60);
        store.update_run(&run)?;
    }
    if approve {
        dispatch::approve(&store, &mut run)?;
    } else if reject {
        dispatch::reject(&store, &mut run)?;
    } else if let Some(feedback) = rework {
        let cfg = load_config(&store)?;
        dispatch::rework(&store, &home, &cfg, &mut run, feedback)?;
        println!("{}", report::run_line(&run));
    } else {
        let diff = dispatch::diff(&run).unwrap_or_else(|e| format!("(no diff: {e})"));
        print!(
            "{}",
            report::run_detail(&run, &store.attempts(Some(run.id))?, &diff)
        );
    }
    drop(session);
    Ok(())
}

fn run_ship(projects: &[String]) -> Result<()> {
    let store = Store::open_default()?;
    let _session = Session::acquire(&store::home()?)?;
    let cfg = load_config(&store)?;
    let runs: Vec<_> = store
        .runs()?
        .into_iter()
        .filter(|r| r.state == RunState::Approved)
        .filter(|r| projects.is_empty() || projects.contains(&r.project))
        .collect();
    if runs.is_empty() {
        println!("nothing approved");
        return Ok(());
    }
    let mut failed = 0;
    ship::ship(&store, &cfg, runs, |run, outcome| match outcome {
        Ok(o) => println!("#{} {}: {o}", run.id, run.project),
        Err(e) => {
            failed += 1;
            println!("#{} {}: {e}", run.id, run.project);
        }
    })?;
    if failed > 0 {
        return Err(format!("{failed} runs not shipped; they stay approved").into());
    }
    Ok(())
}

fn run_sync(names: &[String], apply: bool) -> Result<()> {
    let store = Store::open_default()?;
    let rows = store.projects()?;
    for n in names {
        if !rows.iter().any(|r| &r.name == n) {
            return Err(format!("unknown project `{n}`").into());
        }
    }
    let me = sync::login()?;
    let (mut changes, mut failed, mut edited) = (0, 0, Vec::new());
    for row in rows
        .iter()
        .filter(|r| names.is_empty() || names.contains(&r.name))
    {
        let path = row.path.join("TODO.md");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let skip = |why: &str| println!("{}: skipped: {why}", row.name);
        let parsed = todo::parse(&text);
        if parsed.has_errors() {
            skip("TODO.md has lint errors; see `pma lint`");
            continue;
        }
        let Some(repo) = scan::git(&row.path, &["remote", "get-url", "origin"])
            .and_then(|url| scan::github_slug(url.trim()))
        else {
            if !names.is_empty() {
                skip("origin is not on GitHub");
            }
            continue;
        };
        let issues = match sync::issues(&repo) {
            Ok(i) => i,
            Err(e) => {
                skip(&e);
                failed += 1;
                continue;
            }
        };
        let actions = sync::plan(&parsed, &issues, &me);
        for a in &actions {
            match a.line() {
                Some(line) => println!("{}:{line}: {}", row.name, a.describe()),
                None => println!("{}: {}", row.name, a.describe()),
            }
        }
        let planned = actions.iter().filter(|a| a.is_change()).count();
        if !apply || planned == 0 {
            changes += planned;
            continue;
        }
        let before = text;
        match sync::apply(&repo, &path, &actions) {
            Ok(n) => changes += n,
            Err(e) => {
                println!("{}: stopped: {e}", row.name);
                failed += 1;
            }
        }
        if std::fs::read_to_string(&path).is_ok_and(|after| after != before) {
            edited.push(row.name.clone());
        }
    }
    if changes == 0 && failed == 0 {
        println!("in sync");
    } else if apply {
        println!("{changes} changes made");
    } else if changes > 0 {
        println!("{changes} changes; run `pma sync --apply` to make them");
    }
    if !edited.is_empty() {
        println!(
            "TODO.md changed, uncommitted, in: {}; commit, then `pma scan`",
            edited.join(", ")
        );
    }
    if failed > 0 {
        return Err(format!("{failed} projects not synced").into());
    }
    Ok(())
}

fn note(action: Option<NoteAction>) -> Result<()> {
    let store = Store::open_default()?;
    let text = |words: Vec<String>| -> Result<String> {
        let t = words.join(" ").trim().to_string();
        if t.is_empty() {
            return Err("a note needs text".into());
        }
        Ok(t)
    };
    let missing = |id: i64| format!("no note #{id}");
    match action {
        None => {
            let rows: Vec<Vec<String>> = store
                .notes()?
                .into_iter()
                .map(|n| {
                    vec![
                        format!("#{}", n.id),
                        dates::format(dates::day(n.updated_at)),
                        n.text,
                    ]
                })
                .collect();
            if rows.is_empty() {
                println!("no notes; add one with `pma note add <text>`");
            }
            print!("{}", report::table(&rows, ""));
        }
        Some(NoteAction::Add { text: words }) => {
            let id = store.add_note(&text(words)?, dates::now())?;
            println!("#{id}");
        }
        Some(NoteAction::Edit { id, text: words }) => {
            if !store.edit_note(id, &text(words)?, dates::now())? {
                return Err(missing(id).into());
            }
        }
        Some(NoteAction::Rm { id }) => {
            if !store.remove_note(id)? {
                return Err(missing(id).into());
            }
        }
    }
    Ok(())
}

fn run_tui() -> Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return Err("pma tui needs a terminal; use `pma matrix` otherwise".into());
    }
    let p = portfolio(&[])?;
    let header = header_text(&p);
    let placed = rank::place(&p.cfg, p.tasks, p.today);
    tui::run(tui::App::new(header, placed, p.today))?;
    Ok(())
}
