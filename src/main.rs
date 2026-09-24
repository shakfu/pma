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
mod pass;
mod progress;
mod projects;
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

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use config::Config;
use store::{Result, RunState, Session, Store};
use todo::Priority;

/// The commands, in the order someone reaching for one would look. Generated
/// against clap's own list, so a command missing from here is a test failure
/// rather than a command missing from the help.
const GROUPS: [(&str, &[&str]); 6] = [
    (
        "The loop",
        &["scan", "matrix", "dispatch", "review", "ship"],
    ),
    ("Tasks", &["lint", "prune", "stale", "sync", "note"]),
    ("Reading", &["status", "report", "tui"]),
    ("Setup", &["root", "project", "config", "verify"]),
    ("Agents", &["agent", "preset", "route"]),
    ("Many at once", &["campaign", "workflow"]),
];

/// The template that leaves the command list to `grouped_commands`.
const HELP: &str = "\
{about-with-newline}
{usage-heading} {usage}{after-help}
Options:
{options}";

/// One section per group, each command with the first line of its own help, so
/// there is one place a description is written.
fn grouped_commands(cmd: &clap::Command) -> String {
    let width = cmd
        .get_subcommands()
        .map(|s| s.get_name().len())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for (heading, names) in GROUPS {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(heading);
        out.push('\n');
        for name in names {
            let Some(sub) = cmd.get_subcommands().find(|s| s.get_name() == *name) else {
                continue;
            };
            let about = sub
                .get_about()
                .map(|a| a.to_string())
                .unwrap_or_default()
                .lines()
                .next()
                .unwrap_or_default()
                .to_string();
            out.push_str(&format!("  {name:width$}  {about}\n"));
        }
    }
    out.push_str("\n`pma help <command>` explains one.\n");
    out
}

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
    /// Remove finished items and `Done` sections.
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
    /// Directories whose git repos are projects.
    Root {
        #[command(subcommand)]
        action: Option<RootAction>,
    },
    /// A project's record: its tier, its tags, or its removal.
    Project {
        #[command(subcommand)]
        action: Option<ProjectAction>,
    },
    /// List a setting, show one, or set one.
    Config {
        /// A setting such as `tiers.2` or `weights.ci`; omit to list all.
        key: Option<String>,
        /// The new value; omit to show the current one.
        value: Option<String>,
        /// Restore the default for KEY.
        #[arg(long, requires = "key", conflicts_with = "value")]
        reset: bool,
    },
    /// Read TODO.md, git state and CI into the database.
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
    /// Every tiered project's tasks, ranked, in one view.
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
        /// Include untiered projects, ranked as tier 5.
        #[arg(long)]
        all: bool,
    },
    /// Run an agent on tasks, each in its own worktree.
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
        /// A named preset; `-a` or `-m` beside it wins.
        #[arg(short = 'p', long)]
        preset: Option<String>,
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
    /// Runs awaiting a decision; show one, or act on it.
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
    /// Open items by age, oldest first.
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
    /// One task across many repositories.
    Campaign {
        #[command(subcommand)]
        action: Option<CampaignAction>,
    },
    /// Which worker and how much autonomy, per kind of task.
    ///
    /// Propose a revision, put one into effect, or replay a candidate over the
    /// runs already recorded.
    Route {
        #[command(subcommand)]
        action: Option<RouteAction>,
    },
    /// A graph of agents over a project.
    ///
    /// Read a document, store a revision, or put one into effect.
    Workflow {
        #[command(subcommand)]
        action: Option<WorkflowAction>,
    },
    /// A named worker, model and configuration.
    ///
    /// List them, name one, or make one the default.
    Preset {
        #[command(subcommand)]
        action: Option<PresetAction>,
    },
    /// The workers `pma dispatch` can run.
    Agent {
        #[command(subcommand)]
        action: Option<AgentAction>,
    },
    /// What dispatching produced: outcomes, cost, time.
    Report {
        /// Group by `project`, `class` or `agent`. Default: class.
        #[arg(long, value_name = "DIMENSION")]
        by: Option<String>,
    },
    /// Run each project's check where a dispatch would.
    ///
    /// Checks out the head of the remote default branch in a fresh worktree
    /// and runs the verify command there, under the agent's environment. The
    /// result is what the next dispatch at that commit reads as its base.
    Verify {
        /// Project names.
        projects: Vec<String>,
        /// Also select every project carrying this tag; repeatable.
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
    },
    /// Commit and publish approved runs.
    Ship {
        /// Limit to these projects.
        projects: Vec<String>,
        /// Also select every project carrying this tag; repeatable.
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
    },
    /// Portfolio notes, which belong to no one project.
    Note {
        #[command(subcommand)]
        action: Option<NoteAction>,
    },
    /// Browse the matrix in the terminal.
    Tui,
    /// Mirror `Critical` items to GitHub Issues.
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
enum ProjectAction {
    /// Set the tier of one project or many: 1 (most important) to 5, or `none`.
    ///
    /// `pma project` lists every project's tier.
    Tier {
        /// 1 to 5, or `none`.
        tier: String,
        /// Project directory names under a root.
        #[arg(required = true)]
        projects: Vec<String>,
    },
    /// Write every project's tier and tags to a file to edit in bulk.
    ///
    /// The extension picks the format: `.csv` holds one project per line as
    /// `name,tier,tag,tag`; `.json` holds a list of objects.
    Export {
        /// The file to write, ending `.csv` or `.json`.
        file: PathBuf,
    },
    /// Read tiers and tags back from an edited `export` file.
    ///
    /// A project the file leaves out keeps the tier and tags it has; a
    /// project it names gets exactly the tags in its row. Dry run unless
    /// --apply.
    Import {
        /// The file to read, ending `.csv` or `.json`.
        file: PathBuf,
        /// Make the changes instead of listing them.
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
enum PresetAction {
    /// Name a worker, a model and the arguments that configure it, replacing
    /// one of that name. An empty model leaves the worker its own.
    Set {
        name: String,
        agent: String,
        model: Option<String>,
        /// Arguments the worker's `{extra}` stands in for, such as
        /// `--thinking high`. Leading dashes are taken literally.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Make a preset the default, as `pma config preset` does.
    Use { name: String },
    /// Forget a preset. The worker it named is untouched.
    Rm { name: String },
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
    /// Advance one pass of a workflow over a target, then exit. Nodes a rule
    /// decides run; nodes an agent decides are planned, priced and left for
    /// you to approve with `--yes`.
    ///
    /// An instance is frozen at the revision it started under, so activating
    /// another does not change a pass already under way.
    Run {
        /// A workflow named by the revision in effect.
        name: String,
        /// What to run it over, in `pma dispatch`'s target syntax: `cynn`,
        /// `cynn:31`, `cynn:critical`, `cynn:ci`. The element type must be
        /// the one the workflow reads.
        projects: Vec<String>,
        /// Add every project carrying this tag.
        #[arg(long)]
        tag: Option<String>,
        /// Resume an instance instead of starting one.
        #[arg(long, value_name = "ID")]
        instance: Option<i64>,
        /// Set a declared parameter: `--set severity=high`. Repeatable. An
        /// instance records what it was given, so a replay reads the same.
        #[arg(long = "set", value_name = "NAME=VALUE")]
        set: Vec<String>,
        /// Plan and price the pass without running anything at all.
        #[arg(long)]
        dry_run: bool,
        /// Approve the spend this pass plans.
        #[arg(long)]
        yes: bool,
        /// Run agent nodes with this worker, whatever a route names.
        #[arg(short = 'a', long)]
        agent: Option<String>,
        /// And this model. A cheap one is how to try a workflow out.
        #[arg(short = 'm', long)]
        model: Option<String>,
        /// A named preset; `-a` or `-m` beside it wins.
        #[arg(short = 'p', long)]
        preset: Option<String>,
    },
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
    /// Print every field of one worker.
    Show { name: String },
    /// Remove a worker.
    Rm { name: String },
}

fn main() -> ExitCode {
    // The command list is grouped rather than alphabetical, which needs the
    // template and the list built before parsing.
    let command = Cli::command();
    let help = grouped_commands(&command);
    let matches = command.help_template(HELP).after_help(help).get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };
    let result = match cli.command {
        Command::Lint { paths } => return lint(&paths),
        Command::Prune { paths, apply } => return prune(&paths, apply),
        Command::Root { action } => root(action),
        Command::Project { action } => project_command(action),
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
            all,
        } => show_status(&projects, &tags, explain, all),
        Command::Dispatch {
            targets,
            agent,
            model,
            preset,
            tags,
            auto,
            count,
            retry,
        } => overrides(agent, model, preset)
            .and_then(|over| run_dispatch(&targets, &tags, auto, count, retry, &over)),
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
        Command::Preset { action } => preset_command(action),
        Command::Agent { action } => agent_command(action),
        Command::Report { by } => run_report(by.as_deref()),
        Command::Ship { projects, tags } => run_ship(&projects, &tags),
        Command::Verify { projects, tags } => run_verify(&projects, &tags),
        Command::Sync {
            projects,
            tags,
            apply,
        } => run_sync(&projects, &tags, apply),
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
            let tag = projects::normal_tag(&tag)?;
            known(&projects)?;
            for p in &projects {
                if !store.add_project_tag(p, &tag)? {
                    println!("{p} already carries `{tag}`");
                }
            }
        }
        Some(TagAction::Rm { tag, projects }) => {
            let tag = projects::normal_tag(&tag)?;
            for p in &projects {
                if !store.remove_project_tag(p, &tag)? {
                    println!("{p} does not carry `{tag}`");
                }
            }
        }
        Some(TagAction::Show { tag }) => {
            let tag = projects::normal_tag(&tag)?;
            for p in store.projects_tagged(&[tag])? {
                println!("{p}");
            }
        }
    }
    Ok(())
}

/// Tags are matched exactly, so they are lowercased once here rather than
/// leaving `ai` and `AI` as two groups.
fn set_tier(tier: &str, names: &[String]) -> Result<()> {
    let store = Store::open_default()?;
    let tier = match tier {
        "none" => None,
        t => Some(
            t.parse::<u8>()
                .ok()
                .filter(|t| (1..=5).contains(t))
                .ok_or("tier must be 1 to 5, or none")?,
        ),
    };
    // Every project is resolved before any is written, so a name typed wrong
    // does not leave half the list retiered.
    let mut paths = Vec::with_capacity(names.len());
    for name in names {
        paths.push(match store.project(name)? {
            Some(row) => row.path,
            None => find_project(&store, name)?,
        });
    }
    for (name, path) in names.iter().zip(paths) {
        store.set_tier(name, &path, tier)?;
    }
    Ok(())
}

fn export_projects(file: &Path) -> Result<()> {
    let store = Store::open_default()?;
    let format = projects::Format::of(file)?;
    let tags = store.project_tags()?;
    let entries: Vec<projects::Entry> = store
        .projects()?
        .into_iter()
        .map(|p| projects::Entry {
            tier: p.tier,
            tags: tags
                .iter()
                .filter(|(project, _)| *project == p.name)
                .map(|(_, tag)| tag.clone())
                .collect(),
            name: p.name,
        })
        .collect();
    if entries.is_empty() {
        return Err("no projects; `pma root add <dir>` then `pma scan`".into());
    }
    std::fs::write(file, projects::write(&entries, format)?)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    println!(
        "{} projects to {}; edit it, then `pma project import {} --apply`",
        entries.len(),
        file.display(),
        file.display()
    );
    Ok(())
}

fn import_projects(file: &Path, apply: bool) -> Result<()> {
    let store = Store::open_default()?;
    let format = projects::Format::of(file)?;
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let entries = projects::read(&text, format)?;
    let rows = store.projects()?;
    let tags = store.project_tags()?;

    // The whole file is checked first: a name that is not a project usually
    // means the wrong file, and half of it applied is worse than none.
    let unknown: Vec<&str> = entries
        .iter()
        .map(|e| e.name.as_str())
        .filter(|n| !rows.iter().any(|r| r.name == *n))
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "not projects: {}; `pma project` lists them, `pma scan` finds new ones",
            unknown.join(", ")
        )
        .into());
    }

    let mut changes = 0;
    for entry in &entries {
        let row = rows.iter().find(|r| r.name == entry.name).expect("checked");
        let was: Vec<String> = tags
            .iter()
            .filter(|(project, _)| *project == entry.name)
            .map(|(_, tag)| tag.clone())
            .collect();
        if row.tier != entry.tier {
            changes += 1;
            println!(
                "{}: tier {} -> {}",
                entry.name,
                show_tier(row.tier),
                show_tier(entry.tier)
            );
            if apply {
                store.set_tier(&entry.name, &row.path, entry.tier)?;
            }
        }
        let gone: Vec<&String> = was.iter().filter(|t| !entry.tags.contains(t)).collect();
        let new: Vec<&String> = entry.tags.iter().filter(|t| !was.contains(t)).collect();
        if !gone.is_empty() || !new.is_empty() {
            changes += 1;
            println!(
                "{}: tags {} -> {}",
                entry.name,
                show_tags(&was),
                show_tags(&entry.tags)
            );
            if apply {
                for tag in gone {
                    store.remove_project_tag(&entry.name, tag)?;
                }
                for tag in new {
                    store.add_project_tag(&entry.name, tag)?;
                }
            }
        }
    }

    let absent = rows.len() - entries.len();
    let kept = match absent {
        0 => String::new(),
        n => format!("; {n} projects the file leaves out keep what they have"),
    };
    println!(
        "{}",
        match (changes, apply) {
            (0, _) => format!("{} projects, nothing to change{kept}", entries.len()),
            (n, true) => format!("{n} changes applied{kept}"),
            (n, false) => format!("{n} changes; --apply to make them{kept}"),
        }
    );
    Ok(())
}

fn show_tier(tier: Option<u8>) -> String {
    tier.map_or("none".into(), |t| t.to_string())
}

fn show_tags<T: std::fmt::Display>(tags: &[T]) -> String {
    match tags.is_empty() {
        true => "none".into(),
        false => tags
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<String>>()
            .join(","),
    }
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
    let bar = progress::Bar::new(selected.len());
    let facts = scan::scan_all(&selected, &cfg.activity_ignore, offline, deps, &|name| {
        bar.done(name)
    });
    bar.finish();
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
    /// Whether the untiered projects are in `projects`, ranked as tier 5.
    all: bool,
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
    portfolio_with(names, tags, false)
}

/// The portfolio; with `all`, untiered projects are ranked as tier 5.
fn portfolio_with(names: &[String], tags: &[String], all: bool) -> Result<Portfolio> {
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
        let Some(tier) = row.tier.or(fallback).or(all.then_some(5)) else {
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
        all,
        scanned_ago: now - last,
    })
}

fn header_text(p: &Portfolio) -> String {
    if p.all {
        return format!(
            "last scan {}; {} projects, {} untiered ranked as tier 5",
            report::ago(p.scanned_ago),
            p.projects.len(),
            p.untiered
        );
    }
    format!(
        "last scan {}; {} tiered projects, {} untiered (pma project tier <1-5> <project>...)",
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

fn show_status(names: &[String], tags: &[String], explain: bool, all: bool) -> Result<()> {
    let p = portfolio_with(names, tags, all)?;
    header(&p);
    let rows: Vec<report::StatusRow> = p
        .projects
        .iter()
        .map(|(project, row)| report::StatusRow {
            project,
            tiered: row.tier.is_some() || p.cfg.default_tier.is_some(),
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
                workflow: None,
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
                             `pma project tier <1-5> {project}`, or name the tasks"
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
                    workflow: None,
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
            workflow: None,
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
/// The flags, with a named preset read. A flag beside one wins: it is the more
/// specific statement of intent.
fn overrides(
    agent: Option<String>,
    model: Option<String>,
    preset: Option<String>,
) -> Result<dispatch::Overrides> {
    let preset = match preset {
        None => None,
        Some(name) => Some(Store::open_default()?.preset(&name)?),
    };
    Ok(dispatch::Overrides {
        agent,
        model,
        preset,
    })
}

/// A project's record. Bare, it lists the projects: what each is called, its
/// tier, its tags and when it was last scanned.
fn project_command(action: Option<ProjectAction>) -> Result<()> {
    let Some(action) = action else {
        let store = Store::open_default()?;
        let projects = store.projects()?;
        if projects.is_empty() {
            println!("no projects; `pma root add <dir>` then `pma scan`");
            return Ok(());
        }
        let tags = store.project_tags()?;
        let rows: Vec<Vec<String>> = projects
            .iter()
            .map(|p| {
                let mine: Vec<&str> = tags
                    .iter()
                    .filter(|(project, _)| *project == p.name)
                    .map(|(_, tag)| tag.as_str())
                    .collect();
                vec![
                    p.name.clone(),
                    match p.tier {
                        Some(t) => format!("tier {t}"),
                        None => "untiered".into(),
                    },
                    mine.join(","),
                    match p.absent_since {
                        Some(_) => "absent".into(),
                        None => String::new(),
                    },
                    match p.scanned_at {
                        Some(at) => report::ago(dates::now() - at),
                        None => "never scanned".into(),
                    },
                ]
            })
            .collect();
        print!("{}", report::table(&rows, ""));
        println!(
            "\n`pma project tier <1-5> <project>...` sets tiers; \
             `pma project export\n  <file.csv>` writes them all to edit at \
             once, `import` reads it back."
        );
        return Ok(());
    };
    match action {
        ProjectAction::Tier { tier, projects } => set_tier(&tier, &projects),
        ProjectAction::Export { file } => export_projects(&file),
        ProjectAction::Import { file, apply } => import_projects(&file, apply),
        ProjectAction::Tag { action } => tag(action),
        ProjectAction::Forget { project, apply } => forget(&project, apply),
    }
}

fn preset_command(action: Option<PresetAction>) -> Result<()> {
    let store = Store::open_default()?;
    let cfg = load_config(&store)?;
    let Some(action) = action else {
        let presets = store.presets()?;
        if presets.is_empty() {
            println!(
                "no presets. `pma preset set <name> <agent> [model] [args...]` names one, \n\
                 and `pma agent` lists each worker and its own default model."
            );
            return Ok(());
        }
        let rows: Vec<Vec<String>> = presets
            .iter()
            .map(|p| {
                vec![
                    format!(
                        "{} {}",
                        if cfg.preset.as_deref() == Some(&p.name) {
                            "*"
                        } else {
                            " "
                        },
                        p.name
                    ),
                    p.agent.clone(),
                    p.model.clone().unwrap_or_else(|| "its own default".into()),
                    p.args.join(" "),
                    match store.agent(&p.agent) {
                        Ok(_) => String::new(),
                        Err(_) => "no such worker".into(),
                    },
                ]
            })
            .collect();
        print!("{}", report::table(&rows, ""));
        println!(
            "\n* is `pma config preset`. `-p <name>` uses one for a command; \
             `pma preset use <name>`\n  makes it the default, and `-a` or `-m` beside \
             either one wins."
        );
        return Ok(());
    };
    match action {
        PresetAction::Set {
            name,
            agent,
            model,
            args,
        } => {
            let p = store::Preset {
                name: name.clone(),
                agent: agent.clone(),
                model: model.clone().filter(|m| !m.is_empty()),
                args,
            };
            store.set_preset(&p)?;
            println!(
                "preset `{name}`: {agent}{}{}",
                p.model.map(|m| format!(" at {m}")).unwrap_or_default(),
                if p.args.is_empty() {
                    String::new()
                } else {
                    format!(" with {}", p.args.join(" "))
                }
            );
        }
        PresetAction::Use { name } => {
            // Read first, so naming one that does not exist changes nothing.
            store.preset(&name)?;
            store.set_config("preset", &name)?;
            println!("`{name}` is the default preset");
        }
        PresetAction::Rm { name } => {
            if !store.remove_preset(&name)? {
                return Err(format!("no preset `{name}`").into());
            }
            if cfg.preset.as_deref() == Some(name.as_str()) {
                store.set_config("preset", "")?;
            }
        }
    }
    Ok(())
}

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
    // The rows are the flat graph's nodes, so the op each one applies is read
    // from there rather than from the call site the document wrote.
    let flat = doc.flatten(&w.name)?;
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
        let op = match flat.node(&b.node).map(|n| &n.op) {
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
        let instances = store.workflow_instances()?;
        if !instances.is_empty() {
            let rows: Vec<Vec<String>> = instances
                .iter()
                .map(|i| {
                    vec![
                        format!("instance {}", i.id),
                        i.workflow.clone(),
                        i.target.clone(),
                        i.outcome.clone().unwrap_or_else(|| "open".into()),
                        report::ago(dates::now() - i.started_at),
                    ]
                })
                .collect();
            println!();
            print!("{}", report::table(&rows, ""));
        }
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
        WorkflowAction::Run {
            name,
            projects,
            tag,
            instance,
            set,
            dry_run,
            yes,
            agent,
            model,
            preset,
        } => {
            let store = Store::open_default()?;
            // A pass runs agents and edits worktrees, so it holds the session
            // lock for its whole run, as `pma dispatch` does.
            let _session = Session::acquire(&store::home()?)?;
            let cfg = load_config(&store)?;
            // The flags cover the whole pass and are not stored: a pass
            // resolves its worker exactly as a dispatch does.
            let over = overrides(agent, model, preset)?;
            // An instance is frozen at the revision it started under. Reading
            // the active revision instead would walk units one graph wrote
            // with another graph's edge indexes.
            let revision = match instance {
                Some(id) => {
                    let found = store.workflow_instance(id)?.ok_or_else(|| {
                        format!("no workflow instance {id}; `pma workflow` lists them")
                    })?;
                    if found.workflow != name {
                        return Err(format!(
                            "instance {id} runs `{}`, not `{name}`",
                            found.workflow
                        )
                        .into());
                    }
                    // An instance that stopped short records why and has no
                    // finish time. A finished one is re-derived like any
                    // other: the frontier is evidence, not a cursor (W9).
                    if let (Some(outcome), None) = (&found.outcome, found.finished_at) {
                        return Err(format!(
                            "instance {id} stopped: {outcome}. It cannot be resumed."
                        )
                        .into());
                    }
                    if !projects.is_empty() || tag.is_some() || !set.is_empty() {
                        return Err(format!(
                            "instance {id} holds the target and arguments it started with; \
                             `--instance` takes no projects, no `--tag` and no `--set`"
                        )
                        .into());
                    }
                    store.workflow_revision(found.revision)?.ok_or_else(|| {
                        format!(
                            "instance {id} ran under revision {}, which is gone",
                            found.revision
                        )
                    })?
                }
                None => store
                    .active_workflow()?
                    .ok_or("no workflow revision is in effect; `pma workflow activate <rev>`")?,
            };
            let doc = revision.document()?;
            if doc.workflow(&name).is_none() {
                return Err(
                    format!("revision {} has no workflow `{name}`", revision.revision).into(),
                );
            }
            // A pass walks the flat graph: a call is resolved before anything
            // runs, so the frontier, the caps and the edge indexes a move is
            // keyed by are all one graph's.
            let w = &doc.flatten(&name)?;

            let (instance, args) = match instance {
                Some(id) => (
                    id,
                    serde_json::from_str(&store.workflow_instance(id)?.expect("loaded above").args)
                        .unwrap_or(serde_json::Value::Null),
                ),
                None => {
                    let mut names = projects.clone();
                    if let Some(tag) = &tag {
                        for (project, t) in store.project_tags()? {
                            if t == *tag && !names.contains(&project) {
                                names.push(project);
                            }
                        }
                    }
                    let args = serde_json::to_value(w.bind(&set)?)?;
                    let units = pass::root_units(&store, &doc, &name, &names)?;
                    // The total a pass may spend scales with the argument bag,
                    // which is only known now.
                    let estimate = doc.estimate(&name, units.len() as i64, cfg.agent_budget)?;
                    if estimate.cost > cfg.workflow_budget {
                        return Err(format!(
                            "over {} unit(s) this pass could cost ${:.2}, over workflow_budget \
                             of ${:.2}",
                            units.len(),
                            estimate.cost,
                            cfg.workflow_budget
                        )
                        .into());
                    }
                    // A dry run plans and prices and writes nothing: an
                    // instance it left behind would be an open instance
                    // nobody meant to start.
                    if dry_run {
                        let moves: Vec<(String, usize, bool)> = units
                            .iter()
                            .flat_map(|u| {
                                pass::entry_moves(w, u)
                                    .into_iter()
                                    .map(move |(edge, taken, _)| (u.id.clone(), edge, taken))
                            })
                            .collect();
                        let plan = pass::plan_over(&store, &cfg, &over, w, &units, &moves)?;
                        println!(
                            "`{name}` over {} unit(s), at most ${:.2}",
                            units.len(),
                            estimate.cost
                        );
                        print!("{}", pass::describe(&plan, "would run: "));
                        return Ok(());
                    }
                    let target = match &tag {
                        Some(t) => format!("--tag {t}"),
                        None => names.join(" "),
                    };
                    let id = store.add_workflow_instance(
                        &name,
                        revision.revision,
                        &args.to_string(),
                        &target,
                    )?;
                    for unit in &units {
                        store.add_workflow_unit(id, unit)?;
                        pass::enter(&store, id, w, unit)?;
                    }
                    println!(
                        "instance {id} of `{name}`: {} unit(s), at most ${:.2}",
                        units.len(),
                        estimate.cost
                    );
                    (id, args)
                }
            };

            if dry_run {
                let plan = pass::plan(&store, &cfg, &over, w, instance)?;
                print!("{}", pass::describe(&plan, "would run: "));
                return Ok(());
            }

            let home = store::home()?;
            let given = pass::Invocation {
                home: &home,
                args: &args,
                approved: yes,
            };
            let plan = pass::advance(&store, &cfg, &over, &doc, w, instance, &given)?;
            if plan.is_empty() {
                store.finish_instance(instance, "finished")?;
                println!("instance {instance}: nothing left to run");
                return Ok(());
            }
            let cost: f64 = plan.iter().map(|p| p.cost).sum();
            print!("{}", pass::describe(&plan, "next: "));
            println!(
                "\nat most ${cost:.2}. Nothing was spent. Approve it with \
                 `pma workflow run {name} --instance {instance} --yes`."
            );
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
                    w.parse.name(),
                    if flags.is_empty() {
                        "-".into()
                    } else {
                        flags.join(",")
                    },
                ]
            })
            .collect();
        if rows.is_empty() {
            println!("no agents; `pma agent set <name> command <program>` adds one");
        } else {
            print!("{}", report::table(&rows, ""));
            println!(
                "\n* is `pma config agent`. A worker is how to run a program; which model \
                 it runs\n  at is a preset (`pma preset`). Flags are cost reported, budget \
                 enforced, sandbox,\n  allowlist and resume; `pma agent show <name>` prints \
                 the rest."
            );
        }
        return Ok(());
    };
    match action {
        AgentAction::Show { name } => {
            let w = store.agent(&name)?;
            let rows = vec![
                vec!["command".into(), w.command.clone()],
                vec![
                    "args".into(),
                    serde_json::to_string(&w.args).unwrap_or_default(),
                ],
                vec![
                    "allow".into(),
                    w.allow.clone().unwrap_or_else(|| "none".into()),
                ],
                vec!["parse".into(), w.parse.name()],
                vec!["reports-cost".into(), w.reports_cost.to_string()],
                vec!["enforces-budget".into(), w.enforces_budget.to_string()],
                vec!["sandbox".into(), w.sandbox.to_string()],
                vec!["resumes".into(), w.resumes.to_string()],
                vec![
                    "env".into(),
                    serde_json::to_string(&w.env).unwrap_or_default(),
                ],
            ];
            print!("{}", report::table(&rows, ""));
        }
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
                env: std::collections::BTreeMap::new(),
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
                "model" => {
                    return Err(format!(
                        "a worker no longer names a model: \
                         `pma preset set <name> {name} {value}`"
                    )
                    .into());
                }
                "env" => {
                    w.env = serde_json::from_str(&value)
                        .map_err(|e| format!("env must be a JSON object of name to value: {e}"))?;
                }
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

fn run_verify(projects: &[String], tags: &[String]) -> Result<()> {
    let store = Store::open_default()?;
    let home = store::home()?;
    // It adds and removes a worktree, as a dispatch does.
    let _session = Session::acquire(&home)?;
    let cfg = load_config(&store)?;
    let names = select(&store, projects, tags)?;
    if names.is_empty() {
        return Err("name a project, or --tag".into());
    }
    let rows = store.projects()?;
    let mut failed = 0;
    for name in &names {
        let row = rows
            .iter()
            .find(|r| &r.name == name)
            .ok_or_else(|| format!("unknown project `{name}`"))?;
        if row.absent_since.is_some() {
            return Err(format!("{name} is not under a root; run `pma scan`").into());
        }
        let p = dispatch::preflight(&store, &home, &cfg, name, &row.path)?;
        let at = &p.base[..p.base.len().min(7)];
        let log = p
            .log
            .as_ref()
            .map_or(String::new(), |l| format!("; log: {}", l.display()));
        let line = match (&p.command, p.ok) {
            (None, _) => {
                format!("no verify command; set `pma config projects.{name}.verify <command>`")
            }
            (Some(c), Some(true)) => format!("`{c}` passed at {at} in {}s", p.seconds),
            (Some(c), Some(false)) => format!("`{c}` FAILED at {at} in {}s{log}", p.seconds),
            (Some(c), None) => {
                format!("`{c}` did not finish at {at}: timed out or could not start{log}")
            }
        };
        if p.ok != Some(true) {
            failed += 1;
        }
        println!("{name}: {line}");
    }
    match failed {
        0 => Ok(()),
        n => Err(format!("{n} of {} projects did not pass their check", names.len()).into()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The grouped listing is the only listing, so a command missing from
    /// `GROUPS` would be a command missing from the help. This is what makes
    /// hand-written grouping safe.
    #[test]
    fn every_command_is_in_exactly_one_group() {
        let command = Cli::command();
        let grouped: Vec<&str> = GROUPS
            .iter()
            .flat_map(|(_, names)| *names)
            .copied()
            .collect();
        for sub in command.get_subcommands() {
            let name = sub.get_name();
            if name == "help" {
                continue;
            }
            let count = grouped.iter().filter(|n| **n == name).count();
            assert_eq!(count, 1, "`{name}` appears in {count} groups, not 1");
        }
        for name in &grouped {
            assert!(
                command.get_subcommands().any(|s| s.get_name() == *name),
                "`{name}` is grouped but is not a command"
            );
        }
    }

    /// Every line fits a narrow terminal, which is why the one-liners are
    /// short and the long form lives in each command's own help.
    #[test]
    fn the_listing_does_not_wrap() {
        let command = Cli::command();
        for line in grouped_commands(&command).lines() {
            assert!(line.len() <= 72, "{} chars: {line}", line.len());
        }
    }
}
