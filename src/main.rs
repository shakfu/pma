//! pma: portfolio maintenance across many repositories.
//!
//! Design: `docs/dev/design.md`.

mod accept;
mod agent;
mod choose;
mod class;
mod complexity;
mod config;
mod dates;
mod deps;
mod dispatch;
mod rank;
mod report;
mod report_runs;
mod route;
mod scan;
mod script;
mod ship;
mod store;
mod sync;
mod todo;
mod tui;
mod worker;
mod workflow;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, Subcommand};

use config::Config;
use store::{Result, RunState, Session, Store};
use todo::Priority;

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
    /// Remove finished items and `Done` sections from TODO.md files.
    ///
    /// Each path is a TODO.md file or a directory containing one. A file with
    /// lint errors is skipped. Edits stay uncommitted. Dry run unless --apply.
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
        /// Also select every project carrying this tag; repeatable.
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,

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
        /// Also select every project carrying this tag; repeatable.
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,

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
        /// Also select every project carrying this tag; repeatable.
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,

        /// Show each signal's contribution.
        #[arg(long)]
        explain: bool,
    },
    /// Run an agent on tasks, each in its own worktree of the remote branch.
    ///
    /// A target is `project:line`, a TODO.md line from the last scan,
    /// `project:ci` for failing CI, `project:deps` for outdated dependencies,
    /// `project:critical` and the other headings, or `project:q1` to
    /// `project:q4`. A bare `project` opens its tasks in a list. Blocks until
    /// every agent has finished.
    Dispatch {
        /// Targets; with --auto, projects to draw from.
        targets: Vec<String>,
        /// The agent to run; defaults to `pma config agent`.
        #[arg(short = 'a', long)]
        agent: Option<String>,
        /// The model to ask it for; defaults to the agent's own.
        #[arg(short = 'm', long)]
        model: Option<String>,
        /// With --auto, also draw from every project carrying this tag;
        /// repeatable.
        #[arg(long = "tag", value_name = "TAG", requires = "auto")]
        tags: Vec<String>,
        /// Draw eligible tasks in matrix order: the ci and deps signals,
        /// and items tagged #agent.
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
        /// Run ids; omit to list runs. Several are allowed with --approve.
        ids: Vec<i64>,
        /// Mark ready runs for `pma ship`.
        #[arg(long, requires = "ids", conflicts_with_all = ["reject", "rework"])]
        approve: bool,
        /// Remove the run's worktree and branch.
        #[arg(long, requires = "ids", conflicts_with = "rework")]
        reject: bool,
        /// Run the agent again in the same worktree with this feedback.
        #[arg(long, requires = "ids", value_name = "FEEDBACK")]
        rework: Option<String>,
        /// Minutes spent reviewing this run, added to the run's total.
        #[arg(long, requires = "ids", value_name = "N")]
        minutes: Option<u32>,
    },
    /// Open items by age, oldest first: a list to prune.
    Stale {
        /// Limit to these projects.
        projects: Vec<String>,
        /// Also select every project carrying this tag; repeatable.
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
        /// How many to show; defaults to `quadrant_limit`.
        #[arg(short = 'n', long)]
        count: Option<usize>,
    },
    /// One task definition across many repositories.
    Campaign {
        #[command(subcommand)]
        action: Option<CampaignAction>,
    },
    /// Routing policy: propose a revision, put one into effect, or replay
    /// a candidate over the runs already recorded.
    Route {
        #[command(subcommand)]
        action: Option<RouteAction>,
    },
    /// Workflow documents: read one, store a revision, put one into effect.
    Workflow {
        #[command(subcommand)]
        action: Option<WorkflowAction>,
    },
    /// List the workers `pma dispatch` can run, or change one.
    Agent {
        #[command(subcommand)]
        action: Option<AgentAction>,
    },
    /// What dispatching has produced: outcomes, attempts, cost and time.
    Report {
        /// Group by `project`, `class` or `agent`. Default: class.
        #[arg(long, value_name = "DIMENSION")]
        by: Option<String>,
    },
    /// Commit and publish approved runs, then remove their worktrees.
    Ship {
        /// Limit to these projects.
        projects: Vec<String>,
        /// Also select every project carrying this tag; repeatable.
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
    },
    /// List portfolio notes, or add, edit or remove one.
    Note {
        #[command(subcommand)]
        action: Option<NoteAction>,
    },
    /// Delete an absent project's record: its tasks, tags, attempt counters
    /// and campaign memberships.
    ///
    /// Only a project that no full scan can find is forgotten, since one that
    /// is still under a root returns on the next scan. Its runs are kept:
    /// their worktrees may still exist. Dry run unless --apply.
    Forget {
        /// The project's name, as `pma status` shows it.
        project: String,
        /// Delete it instead of reporting what would go.
        #[arg(long)]
        apply: bool,
    },
    /// Group projects with private tags. A project may carry several.
    ///
    /// Tags are local to this database and are never read from or written to
    /// GitHub. Commands that take project names also take `--tag`.
    Tag {
        #[command(subcommand)]
        action: Option<TagAction>,
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
        /// Also select every project carrying this tag; repeatable.
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,

        /// Make the changes instead of listing them.
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand)]
enum TagAction {
    /// Give projects a tag.
    Add {
        tag: String,
        #[arg(required = true)]
        projects: Vec<String>,
    },
    /// Take a tag off projects.
    Rm {
        tag: String,
        #[arg(required = true)]
        projects: Vec<String>,
    },
    /// List the projects carrying a tag.
    Show { tag: String },
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

#[derive(Subcommand)]
enum CampaignAction {
    /// Define a campaign over a fixed set of projects.
    Add {
        name: String,
        /// The task, as every repository's agent will read it.
        text: String,
        /// The projects to apply it to.
        #[arg(long, value_delimiter = ',', required = true)]
        projects: Vec<String>,
        /// Extra prompt lines: acceptance, constraints, an example.
        #[arg(long, value_name = "TEXT")]
        describe: Option<String>,
        /// Maintenance class; default B. A- for `.github/**` work.
        #[arg(long)]
        class: Option<String>,
    },
    /// Show a campaign's members and what took them.
    Show { name: String },
    /// Dispatch the members that have no run.
    Run {
        name: String,
        /// How many to start; defaults to every remaining member.
        #[arg(short = 'n', long)]
        count: Option<usize>,
    },
    /// Remove a campaign. Its runs are untouched.
    Rm { name: String },
}

#[derive(Subcommand)]
enum RouteAction {
    /// Store a policy document as a draft revision.
    Propose {
        /// A JSON policy file, or `-` for stdin.
        file: String,
        /// Who proposed it; defaults to the local user.
        #[arg(long)]
        by: Option<String>,
    },
    /// Put a revision into effect.
    Activate {
        revision: i64,
        /// Compute and record the route without applying it.
        #[arg(long)]
        shadow: bool,
        /// Who approved it; defaults to the local user.
        #[arg(long)]
        by: Option<String>,
    },
    /// Show what a candidate would have routed differently.
    Replay {
        /// A JSON policy file, `-` for stdin, or a stored revision number.
        file: String,
    },
    /// Print a revision's document.
    Show { revision: Option<i64> },
}

#[derive(Subcommand)]
enum WorkflowAction {
    /// Store a document as a draft revision, printing its worst case.
    Propose {
        /// A workflow file: JSON, or a Rhai script (`.rhai`) that builds one.
        file: String,
        /// Who proposed it; defaults to the local user.
        #[arg(long)]
        by: Option<String>,
    },
    /// Put a revision into effect, unless its worst case exceeds
    /// `workflow_budget`.
    Activate {
        revision: i64,
        /// Who approved it; defaults to the local user.
        #[arg(long)]
        by: Option<String>,
    },
    /// Print a revision's document, as `pma` read it.
    Show { revision: Option<i64> },
    /// Read a document and print the worst case each workflow can cost.
    /// Nothing is stored and nothing runs.
    Check {
        /// A workflow file: JSON, or a Rhai script (`.rhai`) that builds one.
        /// `-` reads stdin as JSON.
        file: String,
        /// Size of the argument bag the estimate assumes.
        #[arg(long, default_value_t = 1, value_name = "N")]
        units: i64,
        /// Print the document a script built, instead of the estimate.
        #[arg(long)]
        emit_json: bool,
    },
}

#[derive(Subcommand)]
enum AgentAction {
    /// Set one field, creating the worker when `command` is set first.
    Set {
        name: String,
        /// One of: command, args, allow, parse, reports-cost,
        /// enforces-budget, sandbox, resumes.
        field: String,
        /// The value; for `args`, a JSON array such as `["-p","{prompt}"]`.
        value: String,
    },
    /// Remove a worker.
    Rm { name: String },
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
            tags,
            offline,
            deps,
        } => run_scan(&projects, &tags, offline, deps),
        Command::Matrix {
            projects,
            tags,
            quadrant,
            all,
        } => show_matrix(&projects, &tags, quadrant, all),
        Command::Status {
            projects,
            tags,
            explain,
        } => show_status(&projects, &tags, explain),
        Command::Dispatch {
            targets,
            agent,
            model,
            tags,
            auto,
            count,
            retry,
        } => run_dispatch(
            &targets,
            &tags,
            auto,
            count,
            retry,
            &dispatch::Overrides { agent, model },
        ),
        Command::Review {
            ids,
            approve,
            reject,
            rework,
            minutes,
        } => run_review(&ids, approve, reject, rework.as_deref(), minutes),
        Command::Stale {
            projects,
            tags,
            count,
        } => show_stale(&projects, &tags, count),
        Command::Campaign { action } => campaign_command(action),
        Command::Route { action } => route_command(action),
        Command::Workflow { action } => workflow_command(action),
        Command::Agent { action } => agent_command(action),
        Command::Report { by } => run_report(by.as_deref()),
        Command::Ship { projects, tags } => run_ship(&projects, &tags),
        Command::Sync {
            projects,
            tags,
            apply,
        } => run_sync(&projects, &tags, apply),
        Command::Note { action } => note(action),
        Command::Forget { project, apply } => forget(&project, apply),
        Command::Tag { action } => tag(action),
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

fn forget(name: &str, apply: bool) -> Result<()> {
    let mut store = Store::open_default()?;
    let row = store
        .project(name)?
        .ok_or_else(|| format!("unknown project `{name}`"))?;
    if row.absent_since.is_none() {
        return Err(format!(
            "{name} is still under a root at {}; `pma root rm` its root or delete the \
             checkout, run `pma scan`, then forget it",
            row.path.display()
        )
        .into());
    }
    let f = store.project_footprint(name)?;
    if !f.open_runs.is_empty() {
        let ids: Vec<String> = f.open_runs.iter().map(|id| format!("#{id}")).collect();
        return Err(format!(
            "{name} has runs that are not shipped or rejected: {}. Each may own a worktree; \
             settle them with `pma review` first",
            ids.join(" ")
        )
        .into());
    }

    println!(
        "{name}: {} tasks, {} tags, {} campaign memberships",
        f.tasks, f.tags, f.campaigns
    );
    if f.runs > 0 {
        println!("{name}: {} finished runs are kept", f.runs);
    }
    if !apply {
        println!("run `pma forget {name} --apply` to delete the record");
        return Ok(());
    }
    store.forget_project(name)?;
    println!("{name} is forgotten");
    Ok(())
}

fn tag(action: Option<TagAction>) -> Result<()> {
    let store = Store::open_default()?;
    let known = |names: &[String]| -> Result<()> {
        let rows = store.projects()?;
        for n in names {
            if !rows.iter().any(|r| &r.name == n) {
                return Err(format!("unknown project `{n}`; run `pma scan`").into());
            }
        }
        Ok(())
    };
    match action {
        None => {
            let pairs = store.project_tags()?;
            if pairs.is_empty() {
                println!("no tags; add one with `pma tag add <tag> <project>...`");
                return Ok(());
            }
            let mut counts: Vec<(String, usize)> = Vec::new();
            for (_, t) in pairs {
                match counts.iter_mut().find(|(name, _)| name == &t) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((t, 1)),
                }
            }
            counts.sort();
            for (t, n) in counts {
                println!("{t}  {n}");
            }
        }
        Some(TagAction::Add { tag, projects }) => {
            let tag = normal_tag(&tag)?;
            known(&projects)?;
            for p in &projects {
                if !store.add_project_tag(p, &tag)? {
                    println!("{p} already carries `{tag}`");
                }
            }
        }
        Some(TagAction::Rm { tag, projects }) => {
            let tag = normal_tag(&tag)?;
            for p in &projects {
                if !store.remove_project_tag(p, &tag)? {
                    println!("{p} does not carry `{tag}`");
                }
            }
        }
        Some(TagAction::Show { tag }) => {
            let tag = normal_tag(&tag)?;
            for p in store.projects_tagged(&[tag])? {
                println!("{p}");
            }
        }
    }
    Ok(())
}

/// Tags are matched exactly, so they are lowercased once here rather than
/// leaving `ai` and `AI` as two groups.
fn normal_tag(tag: &str) -> Result<String> {
    let tag = tag.trim().to_lowercase();
    if tag.is_empty() || tag.split_whitespace().count() > 1 {
        return Err("a tag is one word".into());
    }
    Ok(tag)
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

fn run_scan(names: &[String], tags: &[String], offline: bool, deps: bool) -> Result<()> {
    let mut store = Store::open_default()?;
    let cfg = load_config(&store)?;
    let names = &select(&store, names, tags)?;
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
    for name in store.save_scan(&facts, names.is_empty(), dates::now())? {
        eprintln!("absent: {name} is no longer under a root; its record is kept");
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

/// The projects named outright together with every project carrying one of
/// `tags`. Empty means all of them, so a `--tag` that matches nothing is an
/// error rather than a silent selection of the whole portfolio.
fn select(store: &Store, names: &[String], tags: &[String]) -> Result<Vec<String>> {
    if tags.is_empty() {
        return Ok(names.to_vec());
    }
    let mut out = names.to_vec();
    for project in store.projects_tagged(tags)? {
        if !out.contains(&project) {
            out.push(project);
        }
    }
    if out.is_empty() {
        return Err(format!("no project carries `{}`", tags.join("` or `")).into());
    }
    Ok(out)
}

fn portfolio(names: &[String], tags: &[String]) -> Result<Portfolio> {
    let store = Store::open_default()?;
    let cfg = load_config(&store)?;
    let now = dates::now();
    let today = dates::day(now);
    let last = store
        .last_scan()?
        .ok_or("nothing scanned yet; run `pma scan`")?;

    let names = select(&store, names, tags)?;
    let rows = store.projects()?;
    for n in &names {
        if !rows.iter().any(|r| &r.name == n) {
            return Err(format!("unknown project `{n}`").into());
        }
    }
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|r| names.is_empty() || names.contains(&r.name))
        .collect();
    let fallback = cfg.default_tier.and_then(|t| u8::try_from(t).ok());
    let untiered = rows
        .iter()
        .filter(|r| r.tier.is_none() && fallback.is_none())
        .count();
    let task_rows = store.tasks()?;

    let mut projects = Vec::new();
    let mut tasks = Vec::new();
    for row in rows {
        let Some(tier) = row.tier.or(fallback) else {
            continue;
        };
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
            eligible: t.tags.iter().any(|g| g == "agent"),
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

fn show_matrix(
    names: &[String],
    tags: &[String],
    quadrant: Option<rank::Quadrant>,
    all: bool,
) -> Result<()> {
    let p = portfolio(names, tags)?;
    header(&p);
    let limit = (!all).then_some(p.cfg.quadrant_limit as usize);
    let placed = rank::place(&p.cfg, p.tasks, p.today);
    print!("{}", report::matrix(&placed, limit, quadrant, p.today));
    Ok(())
}

fn show_status(names: &[String], tags: &[String], explain: bool) -> Result<()> {
    let p = portfolio(names, tags)?;
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

fn run_dispatch(
    targets: &[String],
    tags: &[String],
    auto: bool,
    count: Option<usize>,
    retry: bool,
    over: &dispatch::Overrides,
) -> Result<()> {
    let store = Store::open_default()?;
    let home = store::home()?;
    if let Some(name) = &over.agent {
        let known = store.agents()?;
        if !known.iter().any(|w| &w.name == name) {
            return Err(format!(
                "unknown agent `{name}`; `pma agent` lists {}",
                known
                    .iter()
                    .map(|w| w.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .into());
        }
    }
    let session = Session::acquire(&home)?;
    store.fail_interrupted_runs(&session)?;
    settle_prs(&store)?;
    let active = store.runs()?;
    let rows = store.projects()?;
    let p = portfolio(
        if auto { targets } else { &[] },
        if auto { tags } else { &[] },
    )?;
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
    // Whether any target named a task at all, which decides whether running
    // nothing is a quiet result or a failure.
    let mut named_any = auto;
    let wanted = match auto {
        true => count.unwrap_or(p.cfg.max_parallel as usize),
        false => targets.len(),
    };
    if auto {
        // Importance orders the queue; eligibility decides what is taken from
        // it. A quadrant is a view, not a dispatch policy: `Medium` and `Low`
        // are never important at any tier, so quadrant gating hid the
        // maintenance work agents are best at.
        for x in placed.iter().filter(|x| x.task.eligible) {
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
            if r.absent_since.is_some() {
                continue;
            }
            picks.push(dispatch::Pick {
                project: x.task.project.clone(),
                repo: r.path.clone(),
                key: key.clone(),
                text: x.task.text.clone(),
                gh: None,
                tier: r.tier,
                class: None,
                details: None,
                quadrant: Some(format!("{:?}", x.quadrant)),
            });
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
            return Err(
                "name a project such as `cynn`, a target such as `cynn:31`, \
                        or use --auto"
                    .into(),
            );
        }
        let tasks = store.tasks()?;
        for target in targets {
            let (project, what) = dispatch::target(target)?;
            let row = rows
                .iter()
                .find(|r| r.name == project)
                .ok_or_else(|| format!("unknown project `{project}`"))?;
            if row.absent_since.is_some() {
                return Err(format!(
                    "{project} is not under a root; it was last seen at {}. \
                     Clone it back and run `pma scan`",
                    row.path.display()
                )
                .into());
            }
            let named = match &what {
                dispatch::Target::Project => {
                    let mut all = task_rows(&tasks, project, None);
                    all.extend(signal_rows(row));
                    // `#manual` is already on the row, from the last scan.
                    // The list is a view: it does not fetch to confirm it.
                    for r in &mut all {
                        if r.blocked.is_none() {
                            r.blocked = blocked(&store, &active, project, r, retry)?;
                        }
                    }
                    match chosen(project, all)? {
                        Some(rows) => rows,
                        // Cancelled: the other targets are left alone too.
                        None => return Ok(()),
                    }
                }
                dispatch::Target::Line(line) => {
                    let t = tasks
                        .iter()
                        .find(|t| t.project == project && t.line == *line)
                        .ok_or_else(|| {
                            format!("{project}:{line} is not an open item at the last scan")
                        })?;
                    vec![task_row(t)]
                }
                dispatch::Target::Signal(which) => {
                    let found = signal_rows(row).into_iter().find(|r| &r.key == which);
                    match (found, which.as_str()) {
                        (Some(r), _) => vec![r],
                        (None, "ci") => {
                            return Err(
                                format!("{project}: CI was not failing at the last scan").into()
                            );
                        }
                        (None, _) => {
                            return Err(format!(
                                "{project}: no outdated dependencies at the last \
                                 `pma scan --deps`"
                            )
                            .into());
                        }
                    }
                }
                dispatch::Target::Priority(want) => {
                    let named = task_rows(&tasks, project, Some(*want));
                    if named.is_empty() {
                        return Err(format!(
                            "{project}: no open {} items at the last scan",
                            want.name()
                        )
                        .into());
                    }
                    named
                }
                // Quadrants are a ranking of tiered projects, so an untiered
                // one has none rather than an empty one.
                dispatch::Target::Quadrant(want) => {
                    if row.tier.is_none() && p.cfg.default_tier.is_none() {
                        return Err(format!(
                            "{project} has no tier, so it is in no quadrant; \
                             `pma tier {project} <1-5>`, or name the tasks"
                        )
                        .into());
                    }
                    let named: Vec<choose::Row> = placed
                        .iter()
                        .filter(|x| x.task.project == project && x.quadrant == *want)
                        .filter_map(placed_row)
                        .collect();
                    if named.is_empty() {
                        return Err(format!(
                            "{project}: no tasks in {want:?} at the last scan; `pma matrix`"
                        )
                        .into());
                    }
                    named
                }
            };
            named_any |= !named.is_empty();
            // One named task that cannot run is an error; one of many is
            // passed over, so a selector is not held up by a single task.
            let one = what.is_one();
            for candidate in named {
                let choose::Row { key, text, gh, .. } = candidate.clone();
                if let Some(why) = blocked(&store, &active, project, &candidate, retry)? {
                    let at = at(project, &candidate);
                    if one {
                        return Err(format!("{at}: {why}").into());
                    }
                    eprintln!("warning: {at}: {why}");
                    continue;
                }
                if retry {
                    store.reset_attempts(project, &dispatch::revision(&text))?;
                }
                picks.push(dispatch::Pick {
                    project: project.into(),
                    repo: row.path.clone(),
                    tier: row.tier,
                    class: None,
                    details: None,
                    quadrant: quadrant(project, &key),
                    key,
                    text,
                    gh,
                });
            }
        }
    }
    // A selector expands to as many tasks as it names; `max_parallel` and
    // `batch_budget` bound what runs at once, not what is queued.
    let wanted = match auto {
        true => wanted,
        false => picks.len(),
    };
    if picks.is_empty() || wanted == 0 {
        // A draw that finds nothing is a quiet day, and so is a list nothing
        // was checked in. A target that named tasks and ran none of them did
        // not do what was asked, and the warnings above say why.
        if !named_any {
            println!("nothing to dispatch");
            return Ok(());
        }
        return Err("nothing to dispatch: every task named was passed over".into());
    }
    run_picks(&store, &home, &p.cfg, over, &picks, wanted).map(|_| ())
}

/// The project's open items from the last scan, one priority or all of them.
fn task_rows(tasks: &[store::TaskRow], project: &str, want: Option<Priority>) -> Vec<choose::Row> {
    tasks
        .iter()
        .filter(|t| t.project == project && want.is_none_or(|p| t.priority == p))
        .map(task_row)
        .collect()
}

fn task_row(t: &store::TaskRow) -> choose::Row {
    choose::Row {
        key: t.key.clone(),
        text: t.text.clone(),
        line: Some(t.line),
        gh: t.gh,
        priority: t.priority.name().into(),
        blocked: t
            .tags
            .iter()
            .any(|g| g == "manual")
            .then(|| "tagged #manual, which is class D and never dispatched".to_string()),
    }
}

/// A placed task, dropped when it is a signal no agent can act on.
fn placed_row(x: &rank::Placed) -> Option<choose::Row> {
    Some(choose::Row {
        key: x.task.key.clone()?,
        text: x.task.text.clone(),
        line: x.task.line,
        gh: None,
        priority: x.task.priority.name().into(),
        blocked: None,
    })
}

/// The `ci` and `deps` signals the last scan left on a project. The text is
/// the task an agent is given, so it is built in one place only.
fn signal_rows(row: &store::ProjectRow) -> Vec<choose::Row> {
    let signal = |key: &str, text: String| choose::Row {
        key: key.into(),
        text,
        line: None,
        gh: None,
        priority: "signal".into(),
        blocked: None,
    };
    let mut rows = Vec::new();
    if let scan::Ci::Failing(w) = &row.ci {
        rows.push(signal("ci", format!("fix CI: {}", w.join(", "))));
    }
    if let Some(n) = row.deps.filter(|n| *n > 0) {
        rows.push(signal("deps", format!("update dependencies: {n} outdated")));
    }
    rows
}

/// Where a task is named on the command line: `project:line`, or
/// `project:ci` and the other signals.
fn at(project: &str, row: &choose::Row) -> String {
    match row.line {
        Some(line) => format!("{project}:{line}"),
        None => format!("{project}:{}", row.key),
    }
}

/// Why a task cannot be dispatched now, as the store knows it. `#manual` is
/// not here: the tags that decide a class are read from origin at dispatch,
/// not from the last local scan.
fn blocked(
    store: &Store,
    active: &[store::Run],
    project: &str,
    row: &choose::Row,
    retry: bool,
) -> Result<Option<String>> {
    if has_run(active, project, &row.key, &row.text) {
        return Ok(Some("already has a run; see `pma review`".into()));
    }
    let spent = store.consumed_attempts(project, &dispatch::revision(&row.text))?;
    if !retry && spent >= dispatch::ATTEMPT_LIMIT {
        let at = at(project, row);
        return Ok(Some(format!(
            "{spent} attempts on `{}` were used without an accepted result; \
             reword the task, or `pma dispatch {at} --retry`. Any worktree it \
             left is removed by `pma review <id> --reject`",
            row.text
        )));
    }
    Ok(None)
}

/// Opens the project's tasks in a list. `None` when the list was cancelled.
fn chosen(project: &str, rows: Vec<choose::Row>) -> Result<Option<Vec<choose::Row>>> {
    use std::io::IsTerminal;
    if rows.is_empty() {
        return Err(format!("{project}: nothing open at the last scan; run `pma scan`").into());
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(format!(
            "`pma dispatch {project}` opens a list, which needs a terminal; \
             name tasks such as `{project}:31`, or `{project}:critical`"
        )
        .into());
    }
    let mut app = choose::App::new(project.to_string(), rows);
    Ok(choose::run(&mut app)?)
}

/// Prepares up to `wanted` picks and runs them, reporting each. Returns the
/// finished runs, so a caller that owns a set of tasks can record them.
fn run_picks(
    store: &Store,
    home: &std::path::Path,
    cfg: &config::Config,
    over: &dispatch::Overrides,
    picks: &[dispatch::Pick],
    wanted: usize,
) -> Result<Vec<store::Run>> {
    let mut queued = Vec::new();
    // A project-level failure, such as a failed fetch, skips the project.
    let mut unreachable: Vec<&str> = Vec::new();
    for pick in picks {
        if queued.len() == wanted {
            break;
        }
        if unreachable.contains(&pick.project.as_str()) {
            continue;
        }
        match dispatch::prepare(store, home, cfg, over, pick) {
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
    let finished = dispatch::execute(store, home, cfg, over, queued, |run| {
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
    Ok(finished)
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
    ids: &[i64],
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
    if ids.is_empty() {
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
    }
    if ids.len() > 1 {
        if !approve {
            return Err("name one run, or several ids with --approve".into());
        }
        return approve_many(&store, ids);
    }
    let id = ids[0];
    let mut run = store.run(id)?;
    // Added before the action, so a rework's own review time is not lost when
    // the run is reviewed again.
    if let Some(m) = minutes {
        run.review_seconds = Some(run.review_seconds.unwrap_or(0) + i64::from(m) * 60);
        store.update_run(&run)?;
    }
    if approve {
        dispatch::approve(&store, &mut run, &whoami())?;
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

fn show_stale(names: &[String], tags: &[String], count: Option<usize>) -> Result<()> {
    let p = portfolio(names, tags)?;
    header(&p);
    let limit = count.unwrap_or(p.cfg.quadrant_limit as usize);
    let placed = rank::place(&p.cfg, p.tasks, p.today);
    print!("{}", report::stale(&placed, limit));
    Ok(())
}

fn campaign_command(action: Option<CampaignAction>) -> Result<()> {
    let store = Store::open_default()?;
    let Some(action) = action else {
        let all = store.campaigns()?;
        if all.is_empty() {
            println!("no campaigns; `pma campaign add <name> <text> --projects a,b` defines one");
            return Ok(());
        }
        let rows: Vec<Vec<String>> = all
            .iter()
            .map(|c| {
                let taken = c.members.iter().filter(|(_, r)| r.is_some()).count();
                vec![
                    c.name.clone(),
                    format!("class {}", c.class.name()),
                    format!("{taken}/{} dispatched", c.members.len()),
                    report::truncate(&c.text, 50),
                ]
            })
            .collect();
        print!("{}", report::table(&rows, ""));
        return Ok(());
    };
    match action {
        CampaignAction::Add {
            name,
            text,
            projects,
            describe,
            class,
        } => {
            let known = store.projects()?;
            for p in &projects {
                if !known.iter().any(|r| &r.name == p) {
                    return Err(format!("unknown project `{p}`; run `pma scan`").into());
                }
            }
            let class = match class.as_deref() {
                None => class::Class::Specified,
                Some(c) => class::Class::parse(c)
                    .ok_or_else(|| format!("unknown class `{c}`; expected A, A-, B, C or D"))?,
            };
            if !class.dispatchable() {
                return Err(format!("class {} is never dispatched", class.name()).into());
            }
            let mut members: Vec<(String, Option<i64>)> =
                projects.iter().map(|p| (p.clone(), None)).collect();
            members.sort();
            members.dedup();
            store.add_campaign(&store::Campaign {
                name: name.clone(),
                text,
                description: describe.unwrap_or_default(),
                class,
                created_at: dates::now(),
                members,
            })?;
            println!("campaign `{name}` defined; `pma campaign run {name}` starts it");
        }
        CampaignAction::Rm { name } => {
            if !store.remove_campaign(&name)? {
                return Err(format!("unknown campaign `{name}`").into());
            }
        }
        CampaignAction::Show { name } => {
            let c = store.campaign(&name)?;
            let runs = store.runs()?;
            println!("{}: {}", c.name, c.text);
            let rows: Vec<Vec<String>> = c
                .members
                .iter()
                .map(|(project, run)| {
                    let state = match run.and_then(|id| runs.iter().find(|r| r.id == id)) {
                        None => "not dispatched".to_string(),
                        Some(r) => format!(
                            "#{} {}{}",
                            r.id,
                            r.state.name(),
                            r.outcome
                                .as_deref()
                                .map_or(String::new(), |o| format!("  {o}"))
                        ),
                    };
                    vec![format!("  {project}"), state]
                })
                .collect();
            print!("{}", report::table(&rows, ""));
        }
        CampaignAction::Run { name, count } => return run_campaign(&name, count),
    }
    Ok(())
}

/// Dispatches the members that have no live run. Repeating it is safe: a
/// member whose run is still open is passed over, so a restart after a
/// failure does not open a second pull request for it.
fn run_campaign(name: &str, count: Option<usize>) -> Result<()> {
    let store = Store::open_default()?;
    let home = store::home()?;
    let session = Session::acquire(&home)?;
    store.fail_interrupted_runs(&session)?;
    let cfg = load_config(&store)?;
    let campaign = store.campaign(name)?;
    let rows = store.projects()?;
    let active = store.runs()?;

    let mut picks = Vec::new();
    for (project, run) in &campaign.members {
        // A member with a run that is not final is left alone, whatever its
        // state. Dispatching over a failed run would leave its worktree
        // behind and could open a second pull request for the same work;
        // rejecting or reworking it is the reviewer's call, not this one.
        if let Some(open) = run
            .and_then(|id| active.iter().find(|r| r.id == id))
            .filter(|r| !r.state.is_final())
        {
            println!(
                "{project}: #{} is {}; reject or rework it to dispatch again",
                open.id,
                open.state.name()
            );
            continue;
        }
        let Some(row) = rows
            .iter()
            .find(|r| &r.name == project && r.absent_since.is_none())
        else {
            eprintln!("warning: {project} is not under a root; skipped");
            continue;
        };
        picks.push(dispatch::Pick {
            project: project.clone(),
            repo: row.path.clone(),
            key: format!("campaign:{name}"),
            text: campaign.text.clone(),
            gh: None,
            tier: row.tier,
            class: Some(campaign.class),
            details: (!campaign.description.trim().is_empty())
                .then(|| format!("{}\n", campaign.description.trim_end())),
            quadrant: None,
        });
    }
    if picks.is_empty() {
        println!("every member of `{name}` has a run; see `pma campaign show {name}`");
        return Ok(());
    }
    let wanted = count.unwrap_or(picks.len());
    let finished = run_picks(&store, &home, &cfg, &Default::default(), &picks, wanted)?;
    for run in &finished {
        store.set_campaign_run(name, &run.project, run.id)?;
    }
    println!("`pma campaign show {name}` lists what each repository did");
    Ok(())
}

/// A policy document from a file, stdin, or a stored revision number.
/// A document from a file: a script builds one, JSON is read as one. Both end
/// up as the same `Document`, so everything downstream is one path.
fn workflow_document(file: &str) -> Result<(workflow::Document, Option<String>)> {
    let text = if file == "-" {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
        text
    } else {
        std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?
    };
    if file.ends_with(".rhai") {
        Ok((workflow::Document::from_script(&text, file)?, Some(text)))
    } else {
        Ok((workflow::Document::parse(&text)?, None))
    }
}

/// Per node, and the total. `units` is the argument bag the estimate assumes.
fn workflow_estimate(
    doc: &workflow::Document,
    w: &workflow::Workflow,
    units: i64,
    budget: f64,
) -> Result<workflow::Estimate> {
    let estimate = doc.estimate(&w.name, units, budget)?;
    let effects = match w.effects.names().as_slice() {
        [] => "pure".to_string(),
        names => names.join(" "),
    };
    println!(
        "{}(in: [{}]{}) -> [{}]  {effects}",
        w.name,
        w.input,
        w.params
            .iter()
            .map(|(k, p)| format!(", {k} = {}", p.default))
            .collect::<String>(),
        w.output.as_deref().unwrap_or("")
    );
    let mut rows = vec![
        ["node", "op", "in", "out", "runs", "agent runs"]
            .iter()
            .map(|s| (*s).to_string())
            .collect::<Vec<_>>(),
    ];
    for b in &estimate.per_node {
        let op = match w.node(&b.node).map(|n| &n.op) {
            Some(workflow::Op::Map(m)) => format!("map {}", m.out.name()),
            Some(op) => op.name().to_string(),
            None => String::new(),
        };
        rows.push(vec![
            b.node.clone(),
            op,
            b.units_in.to_string(),
            b.units_out.to_string(),
            b.runs.to_string(),
            b.agent_runs.to_string(),
        ]);
    }
    print!("{}", report::table(&rows, "  "));
    println!(
        "  worst case: {} agent runs, {} edits, ${:.2} at ${:.2} per run\n",
        estimate.agent_runs, estimate.edits, estimate.cost, budget
    );
    Ok(estimate)
}

fn workflow_command(action: Option<WorkflowAction>) -> Result<()> {
    let Some(action) = action else {
        let store = Store::open_default()?;
        let revisions = store.workflow_revisions()?;
        if revisions.is_empty() {
            println!(
                "no workflow revisions. `pma workflow check <file>` reads a document, \n\
                 `pma workflow propose <file>` stores one."
            );
            return Ok(());
        }
        let rows: Vec<Vec<String>> = revisions
            .iter()
            .map(|r| {
                let state = match r.activated_at {
                    None => "draft".to_string(),
                    Some(_) => format!("active, by {}", r.activated_by.as_deref().unwrap_or("?")),
                };
                let names = r
                    .document()
                    .map(|d| {
                        d.workflows
                            .iter()
                            .map(|w| w.name.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                vec![
                    format!("{}", r.revision),
                    state,
                    names,
                    match (r.worst_case_runs, r.worst_case_cost) {
                        (Some(runs), Some(cost)) => format!("<= {runs} runs, ${cost:.2} per unit"),
                        _ => String::new(),
                    },
                    format!("proposed by {}", r.proposed_by),
                    report::ago(dates::now() - r.created_at),
                ]
            })
            .collect();
        print!("{}", report::table(&rows, ""));
        return Ok(());
    };
    match action {
        WorkflowAction::Check {
            file,
            units,
            emit_json,
        } => {
            if emit_json {
                if !file.ends_with(".rhai") {
                    return Err(
                        "--emit-json prints what a script built; this is already JSON".into(),
                    );
                }
                let text = std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&script::script_json(&text, &file)?)?
                );
                return Ok(());
            }
            let (doc, _) = workflow_document(&file)?;
            // A document is read without a store when there is none; the
            // budget only scales the estimate.
            let budget = Store::open_default()
                .and_then(|s| load_config(&s))
                .map_or(Config::default().agent_budget, |c| c.agent_budget);
            for w in &doc.workflows {
                workflow_estimate(&doc, w, units, budget)?;
            }
            Ok(())
        }
        WorkflowAction::Propose { file, by } => {
            let store = Store::open_default()?;
            let cfg = load_config(&store)?;
            let (doc, source) = workflow_document(&file)?;
            // Over one unit of input: the argument bag is not known until a
            // pass names its target, so the stored figure is per unit and a
            // pass multiplies it.
            let mut runs = 0;
            let mut cost = 0.0f64;
            for w in &doc.workflows {
                let estimate = workflow_estimate(&doc, w, 1, cfg.agent_budget)?;
                runs = runs.max(estimate.agent_runs);
                cost = cost.max(estimate.cost);
            }
            let n = store.add_workflow_revision(
                &doc,
                source.as_deref(),
                &by.unwrap_or_else(whoami),
                runs,
                cost,
            )?;
            println!(
                "revision {n} stored as a draft: at most ${cost:.2} per unit of input.\n\
                 `pma workflow activate {n}` puts it in effect."
            );
            Ok(())
        }
        WorkflowAction::Activate { revision, by } => {
            let store = Store::open_default()?;
            let cfg = load_config(&store)?;
            let found = store
                .workflow_revisions()?
                .into_iter()
                .find(|r| r.revision == revision)
                .ok_or_else(|| format!("no workflow revision {revision}"))?;
            // The ceiling is per unit of input, and it is checked here rather
            // than at parse: the document is not wrong, the budget is a
            // setting.
            if let Some(cost) = found.worst_case_cost
                && cost > cfg.workflow_budget
            {
                return Err(format!(
                    "revision {revision} could cost ${cost:.2} per unit of input, over \
                     workflow_budget of ${:.2}. Lower a cap, or raise the budget with \
                     `pma config workflow_budget <n>`.",
                    cfg.workflow_budget
                )
                .into());
            }
            store.activate_workflow(revision, &by.unwrap_or_else(whoami))?;
            println!("revision {revision} is in effect");
            Ok(())
        }
        WorkflowAction::Show { revision } => {
            let store = Store::open_default()?;
            let found = match revision {
                Some(n) => store
                    .workflow_revisions()?
                    .into_iter()
                    .find(|r| r.revision == n),
                None => store.active_workflow()?,
            }
            .ok_or("no such workflow revision; `pma workflow` lists them")?;
            println!("{}", found.document);
            Ok(())
        }
    }
}

fn policy_text(store: &Store, from: &str) -> Result<String> {
    if from == "-" {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
        return Ok(text);
    }
    if let Ok(n) = from.parse::<i64>() {
        return store
            .route_revisions()?
            .into_iter()
            .find(|r| r.revision == n)
            .map(|r| r.document)
            .ok_or_else(|| format!("no policy revision {n}").into());
    }
    std::fs::read_to_string(from).map_err(|e| format!("{from}: {e}").into())
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

fn route_command(action: Option<RouteAction>) -> Result<()> {
    let store = Store::open_default()?;
    let Some(action) = action else {
        let revisions = store.route_revisions()?;
        if revisions.is_empty() {
            println!(
                "no routing policy; dispatch uses `pma config agent` and each task's class.\n\
                 `pma route propose <file>` stores one."
            );
            return Ok(());
        }
        let rows: Vec<Vec<String>> = revisions
            .iter()
            .map(|r| {
                let state = match (r.activated_at, r.shadow) {
                    (None, _) => "draft".to_string(),
                    (Some(_), true) => {
                        format!("shadow, by {}", r.activated_by.as_deref().unwrap_or("?"))
                    }
                    (Some(_), false) => {
                        format!("active, by {}", r.activated_by.as_deref().unwrap_or("?"))
                    }
                };
                let routes = r.policy().map(|p| p.routes.len()).unwrap_or(0);
                vec![
                    format!("{}", r.revision),
                    state,
                    format!("{routes} routes"),
                    format!("proposed by {}", r.proposed_by),
                    report::ago(dates::now() - r.created_at),
                ]
            })
            .collect();
        print!("{}", report::table(&rows, ""));
        return Ok(());
    };
    match action {
        RouteAction::Propose { file, by } => {
            let text = policy_text(&store, &file)?;
            let n = store.add_route_revision(&text, &by.unwrap_or_else(whoami))?;
            println!("revision {n} stored as a draft; `pma route activate {n}` puts it in effect");
        }
        RouteAction::Activate {
            revision,
            shadow,
            by,
        } => {
            store.activate_route(revision, &by.unwrap_or_else(whoami), shadow)?;
            println!(
                "revision {revision} is {}",
                if shadow {
                    "in shadow: routes are recorded, not applied"
                } else {
                    "in effect"
                }
            );
        }
        RouteAction::Show { revision } => {
            let doc = match revision {
                Some(n) => policy_text(&store, &n.to_string())?,
                None => {
                    store
                        .active_route()?
                        .ok_or("no revision is in effect; name one")?
                        .document
                }
            };
            print!("{doc}");
            if !doc.ends_with('\n') {
                println!();
            }
        }
        RouteAction::Replay { file } => {
            let policy = route::Policy::parse(&policy_text(&store, &file)?)?;
            let runs = store.runs()?;
            let (differences, compared) = route::replay(&policy, &runs);
            if compared == 0 {
                println!("no run carries a class and a complexity to replay against");
                return Ok(());
            }
            println!(
                "{compared} runs replayed, {} routed differently",
                differences.len()
            );
            if !differences.is_empty() {
                let rows: Vec<Vec<String>> = differences
                    .iter()
                    .map(|d| {
                        vec![
                            format!("  #{}", d.run),
                            d.was.clone(),
                            "->".into(),
                            d.would_be.clone(),
                        ]
                    })
                    .collect();
                print!("{}", report::table(&rows, ""));
            }
            println!(
                "\nCost is not projected. What a different model would spend, or whether it \n\
                 would succeed, is not in this data; only a canary settles that."
            );
        }
    }
    Ok(())
}

fn agent_command(action: Option<AgentAction>) -> Result<()> {
    let store = Store::open_default()?;
    let Some(action) = action else {
        let cfg = load_config(&store)?;
        let rows: Vec<Vec<String>> = store
            .agents()?
            .iter()
            .map(|w| {
                let mut flags: Vec<&str> = Vec::new();
                if w.reports_cost {
                    flags.push("cost");
                }
                if w.enforces_budget {
                    flags.push("budget");
                }
                if w.sandbox {
                    flags.push("sandbox");
                }
                if w.allow.is_some() {
                    flags.push("allowlist");
                }
                if w.resumes {
                    flags.push("resumes");
                }
                vec![
                    if w.name == cfg.agent {
                        format!("* {}", w.name)
                    } else {
                        format!("  {}", w.name)
                    },
                    w.command.clone(),
                    w.parse.name().into(),
                    if flags.is_empty() {
                        "-".into()
                    } else {
                        flags.join(",")
                    },
                    serde_json::to_string(&w.args).unwrap_or_default(),
                ]
            })
            .collect();
        if rows.is_empty() {
            println!("no agents; `pma agent set <name> command <program>` adds one");
        } else {
            print!("{}", report::table(&rows, ""));
            println!("\n* is `pma config agent`; reports cost, enforces budget, has a sandbox");
        }
        return Ok(());
    };
    match action {
        AgentAction::Rm { name } => {
            if !store.remove_agent(&name)? {
                return Err(format!("unknown agent `{name}`").into());
            }
        }
        AgentAction::Set { name, field, value } => {
            let mut w = store.agent(&name).unwrap_or_else(|_| worker::Worker {
                name: name.clone(),
                command: String::new(),
                args: Vec::new(),
                allow: None,
                parse: worker::Parser::TextTail,
                reports_cost: false,
                enforces_budget: false,
                sandbox: false,
                resumes: false,
            });
            let flag = |v: &str| match v {
                "true" | "yes" => Ok(true),
                "false" | "no" => Ok(false),
                _ => Err(format!("expected true or false, not `{v}`")),
            };
            match field.as_str() {
                "command" => w.command = value,
                "args" => {
                    w.args = serde_json::from_str(&value)
                        .map_err(|e| format!("args must be a JSON array of strings: {e}"))?;
                }
                "allow" => w.allow = (!value.is_empty()).then_some(value),
                "parse" => {
                    w.parse = worker::Parser::parse(&value).ok_or_else(|| {
                        format!(
                            "unknown parser `{value}`; expected {}",
                            worker::Parser::names()
                        )
                    })?;
                }
                "reports-cost" => w.reports_cost = flag(&value)?,
                "enforces-budget" => w.enforces_budget = flag(&value)?,
                "sandbox" => w.sandbox = flag(&value)?,
                "resumes" => w.resumes = flag(&value)?,
                _ => {
                    let names: Vec<&str> = worker::FIELDS.iter().map(|(n, _)| *n).collect();
                    return Err(
                        format!("unknown field `{field}`; expected {}", names.join(", ")).into(),
                    );
                }
            }
            if w.command.is_empty() {
                return Err(format!("set `command` for `{name}` first").into());
            }
            store.set_agent(&w)?;
        }
    }
    Ok(())
}

fn run_report(by: Option<&str>) -> Result<()> {
    let store = Store::open_default()?;
    let by = match by {
        None => report_runs::By::Class,
        Some(s) => report_runs::By::parse(s).ok_or_else(|| {
            let names: Vec<&str> = report_runs::By::ALL.iter().map(|(n, _)| *n).collect();
            format!("unknown dimension `{s}`; expected {}", names.join(", "))
        })?,
    };
    print!(
        "{}",
        report_runs::report(&store.runs()?, &store.attempts(None)?, by)
    );
    Ok(())
}

/// A batch approval. Every named run is checked before any is approved, so
/// a list with one bad id changes nothing.
fn approve_many(store: &Store, ids: &[i64]) -> Result<()> {
    let mut runs = Vec::new();
    for id in ids {
        let run = store.run(*id)?;
        let reasons = accept::review_reasons(&run);
        if !reasons.is_empty() {
            return Err(format!(
                "#{id} is not clean, so it is not a batch approval: {}. \
                 Approve it on its own after reading it.",
                reasons.join("; ")
            )
            .into());
        }
        runs.push(run);
    }
    let by = whoami();
    for mut run in runs {
        dispatch::approve(store, &mut run, &by)?;
        println!("#{} {}: approved", run.id, run.project);
    }
    Ok(())
}

fn run_ship(projects: &[String], tags: &[String]) -> Result<()> {
    let store = Store::open_default()?;
    let _session = Session::acquire(&store::home()?)?;
    let cfg = load_config(&store)?;
    let projects = &select(&store, projects, tags)?;
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
    ship::ship(
        &store,
        &store::home()?,
        &cfg,
        runs,
        |run, outcome| match outcome {
            Ok(o) => println!("#{} {}: {o}", run.id, run.project),
            Err(e) => {
                failed += 1;
                println!("#{} {}: {e}", run.id, run.project);
            }
        },
    )?;
    if failed > 0 {
        return Err(format!("{failed} runs not shipped; they stay approved").into());
    }
    Ok(())
}

fn run_sync(names: &[String], tags: &[String], apply: bool) -> Result<()> {
    let store = Store::open_default()?;
    let names = &select(&store, names, tags)?;
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
        if row.absent_since.is_some() {
            if !names.is_empty() {
                println!("{}: skipped: not under a root", row.name);
            }
            continue;
        }
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
        let Some(repo) = row.slug.as_deref() else {
            if !names.is_empty() {
                skip("no GitHub origin recorded; run `pma scan`");
            }
            continue;
        };
        let issues = match sync::issues(repo) {
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
        match sync::apply(repo, &path, &actions) {
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
    let p = portfolio(&[], &[])?;
    let header = header_text(&p);
    let placed = rank::place(&p.cfg, p.tasks, p.today);
    tui::run(tui::App::new(header, placed, p.today))?;
    Ok(())
}
