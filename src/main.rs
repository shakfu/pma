//! pma: portfolio maintenance across many repositories.
//!
//! Design: `docs/dev/design.md`.

mod config;
mod dates;
mod rank;
mod report;
mod scan;
mod store;
mod todo;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, Subcommand};

use config::Config;
use store::{Result, Store};

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
    /// List, add or remove the directories whose git repos are projects.
    Root {
        #[command(subcommand)]
        action: Option<RootAction>,
    },
    /// Show or set a project's tier: 1 (most important) to 5, or `none`.
    Tier {
        project: String,
        tier: Option<String>,
    },
    /// List settings, show one, or set one.
    Config {
        key: Option<String>,
        value: Option<String>,
        /// Restore the default for KEY.
        #[arg(long, requires = "key", conflicts_with = "value")]
        reset: bool,
    },
    /// Scan projects under the roots and record the results.
    ///
    /// With no names, scans every project and forgets projects no longer found.
    Scan {
        projects: Vec<String>,
        /// Skip GitHub; CI is recorded as unknown.
        #[arg(long)]
        offline: bool,
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
}

#[derive(Subcommand)]
enum RootAction {
    Add { dir: PathBuf },
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
        Command::Root { action } => root(action),
        Command::Tier { project, tier } => set_tier(&project, tier.as_deref()),
        Command::Config { key, value, reset } => configure(key.as_deref(), value.as_deref(), reset),
        Command::Scan { projects, offline } => run_scan(&projects, offline),
        Command::Matrix {
            projects,
            quadrant,
            all,
        } => show_matrix(&projects, quadrant, all),
        Command::Status { projects, explain } => show_status(&projects, explain),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pma: {e}");
            ExitCode::FAILURE
        }
    }
}

fn lint(paths: &[PathBuf]) -> ExitCode {
    let default = [PathBuf::from(".")];
    let paths = if paths.is_empty() {
        &default[..]
    } else {
        paths
    };

    let (mut errors, mut warnings, mut failed) = (0, 0, false);
    for path in paths {
        let file = if path.is_dir() {
            path.join("TODO.md")
        } else {
            path.to_path_buf()
        };
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

fn run_scan(names: &[String], offline: bool) -> Result<()> {
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
    let facts = scan::scan_all(&selected, &cfg.activity_ignore, offline);
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
            ahead: row.ahead,
        };
        tasks.extend(own.iter().map(|t| rank::Task {
            project: row.name.clone(),
            tier,
            priority: t.priority,
            text: t.text.clone(),
            line: Some(t.line),
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

fn header(p: &Portfolio) {
    println!(
        "last scan {}; {} tiered projects, {} untiered (pma tier <project> <1-5>)\n",
        report::ago(p.scanned_ago),
        p.projects.len(),
        p.untiered
    );
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
