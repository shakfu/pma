//! `pma run`: a named job over projects, in one command. A front end over
//! `pma workflow run` and `pma dispatch`: it adds short names, paths as
//! targets, a default model per job and a gate on bulk runs, and nothing the
//! two commands do not already do. Design: `docs/dev/run.md`.

use std::path::{Path, PathBuf};

use pma_core::catalog::{CATALOG, Entry, LIBRARY_BY, Runs};
use pma_core::{api, dispatch, scan, store, workflow};
use store::{Result, Store, WorkflowRevision};

use super::{WorkflowAction, run_dispatch, workflow_command};

/// The worker a catalog model belongs to.
const HINTED_AGENT: &str = "claude";

pub struct Request {
    pub name: Option<String>,
    pub targets: Vec<String>,
    pub all: bool,
    pub tags: Vec<String>,
    pub worker: Option<String>,
    pub set: Vec<String>,
    pub confirm: super::Confirm,
    /// For a worktree run: leave each edit in `pma review` rather than
    /// writing it into the clone. A clone run waits in review either way.
    pub review: bool,
    /// Edit in a worktree, whatever the setting says.
    pub worktree: bool,
    /// Approve and push each run whose gates are clean.
    pub push: bool,
}

pub fn run(req: Request) -> Result<()> {
    let Some(name) = req.name.clone() else {
        return list();
    };
    let store = Store::open_default()?;
    let cfg = api::config(&store)?;
    let entry = CATALOG.iter().find(|e| e.name == name);
    let runs = match entry {
        Some(e) => e.runs,
        // Any other workflow of the revision in effect runs by its own name.
        None => Runs::Workflow(&name),
    };
    if req.all && !req.targets.is_empty() {
        return Err("`--all` takes no targets".into());
    }
    if !req.all && req.tags.is_empty() && req.targets.is_empty() {
        return Err(format!(
            "name a project or a path, or pass `--all`: `pma run {name} <project>`"
        )
        .into());
    }
    let bulk = req.all || !req.tags.is_empty();

    let (agent, model) = match &req.worker {
        Some(w) => {
            let (a, m) = match w.split_once(':') {
                Some((a, m)) => (a.to_string(), Some(m.to_string())),
                None => (w.clone(), None),
            };
            if !store.agents()?.iter().any(|k| k.name == a) {
                return Err(format!("unknown agent `{a}`; `pma agent` lists them").into());
            }
            (Some(a), m)
        }
        None => (None, hinted(&store, &cfg, entry)?),
    };
    if let (Some(m), None, Some(e)) = (&model, &req.worker, entry) {
        println!(
            "model {m}: `{}`'s default; `-w agent:model` names another",
            e.name
        );
    }

    let rows = store.projects()?;
    let tagged: Vec<String> = store
        .project_tags()?
        .into_iter()
        .filter(|(_, t)| req.tags.contains(t))
        .map(|(p, _)| p)
        .collect();
    // Present and scanned: a project a full scan no longer finds is not one
    // to run anything over.
    let pool = rows.iter().filter(|r| {
        r.absent_since.is_none() && r.scanned_at.is_some() && (req.all || tagged.contains(&r.name))
    });

    match runs {
        Runs::Signal(signal) => {
            let targets: Vec<String> = match bulk {
                true => pool
                    .filter(|r| match signal {
                        "ci" => matches!(r.ci, scan::Ci::Failing(_)),
                        _ => r.deps.unwrap_or(0) > 0,
                    })
                    .map(|r| format!("{}:{signal}", r.name))
                    .collect(),
                false => req
                    .targets
                    .iter()
                    .map(|t| signal_target(&rows, t, signal))
                    .collect::<Result<_>>()?,
            };
            if targets.is_empty() {
                println!("{}", nothing_for(signal));
                return Ok(());
            }
            let local = !req.review;
            let in_clone = !req.worktree && cfg.workspace == pma_core::config::Workspace::Clone;
            if req.confirm.dry_run || bulk {
                let goes = match (in_clone, req.push, local) {
                    (true, true, _) => "each edit made in its clone and pushed when clean",
                    (true, false, _) => "each edit made in its clone, waiting for review",
                    (false, true, _) => "each edit pushed when clean",
                    (false, false, true) => {
                        "each edit that passes verify written into its clone, unstaged"
                    }
                    (false, false, false) => "each edit left in `pma review`",
                };
                println!("would dispatch {}, {goes}:", targets.len());
                for t in &targets {
                    println!("  {t}");
                }
                if !req
                    .confirm
                    .approve(&format!("dispatch {}", targets.len()))?
                {
                    return Ok(());
                }
            }
            let over = dispatch::Overrides {
                agent,
                model,
                preset: None,
            };
            run_dispatch(
                &targets,
                &[],
                false,
                None,
                false,
                super::dispatch_flags(local && !req.push, req.worktree, req.push),
                &over,
                None,
            )
        }
        Runs::Workflow(workflow_name) => {
            let revision = effective_revision(&store, &cfg)?;
            let doc = revision.document()?;
            let Some(w) = doc.workflow(workflow_name) else {
                return Err(match entry {
                    Some(_) => format!(
                        "revision {} in effect has no `{workflow_name}`; `pma workflow library` holds it",
                        revision.revision
                    ),
                    None => format!("no `{name}`; `pma run` lists what runs"),
                }
                .into());
            };
            let projects: Vec<String> = match bulk {
                true if w.input != "project" => {
                    return Err(format!(
                        "`{name}` reads {}, not projects, so `--all` and `--tag` do not apply",
                        w.input
                    )
                    .into());
                }
                true => pool.map(|r| r.name.clone()).collect(),
                false => req
                    .targets
                    .iter()
                    .map(|t| match is_path(t) {
                        true => project_at(&rows, t),
                        false => Ok(t.clone()),
                    })
                    .collect::<Result<_>>()?,
            };
            if projects.is_empty() {
                println!("no project to run `{name}` over");
                return Ok(());
            }
            let go = |dry_run: bool| {
                workflow_command(Some(WorkflowAction::Run {
                    name: workflow_name.to_string(),
                    projects: projects.clone(),
                    tag: None,
                    instance: None,
                    set: req.set.clone(),
                    dry_run,
                    approve: None,
                    unattended: !dry_run,
                    local: false,
                    worktree: req.worktree,
                    detach: false,
                    cap: Vec::new(),
                    agent: agent.clone(),
                    model: model.clone(),
                    preset: None,
                }))
            };
            // A bulk run shows its plan and worst case before it is asked for.
            let asked = !req.confirm.yes && pma_core::jobs::current().is_none();
            if req.confirm.dry_run || (bulk && asked) {
                go(true)?;
                println!();
                if !req.confirm.approve("run it unattended")? {
                    return Ok(());
                }
            }
            let before = latest_instance(&store)?;
            go(false)?;
            if let Some(id) = latest_instance(&store)?.filter(|id| Some(*id) != before) {
                print_outputs(&store, &doc, workflow_name, id)?;
                print_documents(id)?;
            }
            Ok(())
        }
    }
}

/// The catalog, then every other workflow of the revision in effect, or of
/// the bundled library before one is. A workflow whose input no target can
/// name is left out, and said so.
fn list() -> Result<()> {
    let store = Store::open_default()?;
    let (choices, unreachable) = pma_core::catalog::choices(&store)?;
    let mut rows = vec![["name", "reads", "what"].map(String::from).to_vec()];
    rows.extend(choices.into_iter().map(|c| vec![c.name, c.reads, c.about]));
    println!("pma run <name> <project|path>... [--all | --tag T] [-w agent[:model]]\n");
    print!("{}", pma_core::report::table(&rows, "  "));
    if !unreachable.is_empty() {
        println!(
            "\nnot runnable from here, no target names their input: {}",
            unreachable.join(", ")
        );
    }
    Ok(())
}

/// The catalog's model for `entry`, when nothing else would name one: no
/// route policy applies, no default preset is set, and the default worker
/// is the one the model belongs to.
fn hinted(
    store: &Store,
    cfg: &pma_core::config::Config,
    entry: Option<&Entry>,
) -> Result<Option<String>> {
    let Some(model) = entry.and_then(|e| e.model) else {
        return Ok(None);
    };
    let routed = store.active_route()?.is_some_and(|r| !r.shadow);
    Ok((!routed && cfg.preset.is_none() && cfg.agent == HINTED_AGENT).then(|| model.to_string()))
}

/// The revision a run uses: one you activated, else the bundled library,
/// proposed and activated on first use and again when `pma` ships another.
/// The library goes in only within `workflow_budget`, as any revision does.
fn effective_revision(store: &Store, cfg: &pma_core::config::Config) -> Result<WorkflowRevision> {
    let library = workflow::Document::parse(workflow::LIBRARY)?;
    let text = library.to_json();
    if let Some(r) = store.active_workflow()?
        && (r.proposed_by != LIBRARY_BY || r.document == text)
    {
        return Ok(r);
    }
    let mut runs = 0;
    let mut cost = 0.0f64;
    for w in &library.workflows {
        let e = library.estimate(&w.name, 1, cfg.agent_budget)?;
        runs = runs.max(e.agent_runs);
        cost = cost.max(e.cost);
    }
    if cost > cfg.workflow_budget {
        return Err(format!(
            "the bundled workflows could cost ${cost:.2} per project, over workflow_budget of \
             ${:.2}. Allow it once with `pma config workflow_budget {}`; each run still \
             spends what its agents do.",
            cfg.workflow_budget,
            cost.ceil()
        )
        .into());
    }
    let n = store.add_workflow_revision(&library, None, LIBRARY_BY, runs, cost)?;
    store.activate_workflow(n, LIBRARY_BY)?;
    println!("revision {n}: the bundled workflows, now in effect");
    Ok(store
        .workflow_revisions()?
        .into_iter()
        .find(|r| r.revision == n)
        .expect("just stored"))
}

fn is_path(t: &str) -> bool {
    t.contains('/') || t.starts_with('~') || t == "."
}

/// The project at a path, by its canonical form.
fn project_at(rows: &[store::ProjectRow], t: &str) -> Result<String> {
    let path = match t.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?).join(rest),
        None => PathBuf::from(t),
    };
    let want = path.canonicalize().map_err(|e| format!("{t}: {e}"))?;
    let same = |p: &Path| p.canonicalize().is_ok_and(|p| p == want);
    rows.iter()
        .find(|r| same(&r.path))
        .map(|r| r.name.clone())
        .ok_or_else(|| {
            format!(
                "{t} is no project pma knows; `pma root add {}`, then `pma scan`",
                want.parent().unwrap_or(&want).display()
            )
            .into()
        })
}

/// `project:<signal>` for a project, a path or that target written out.
fn signal_target(rows: &[store::ProjectRow], t: &str, signal: &str) -> Result<String> {
    let project = match t.rsplit_once(':') {
        Some((p, s)) if s == signal => p.to_string(),
        Some(_) => {
            return Err(format!("`{t}`: this runs over `project:{signal}` targets only").into());
        }
        None if is_path(t) => project_at(rows, t)?,
        None => t.to_string(),
    };
    Ok(format!("{project}:{signal}"))
}

fn nothing_for(signal: &str) -> &'static str {
    match signal {
        "ci" => "no project has failing CI at the last scan",
        _ => "no project has outdated dependencies at the last `pma scan --deps`",
    }
}

fn latest_instance(store: &Store) -> Result<Option<i64>> {
    Ok(store.workflow_instances()?.last().map(|i| i.id))
}

/// The prose documents the instance's agents wrote, such as `REVIEW.md`,
/// kept under `artifacts/` once their trees are gone.
fn print_documents(instance: i64) -> Result<()> {
    let base = store::state_home()?
        .join("artifacts")
        .join(instance.to_string());
    let mut docs = Vec::new();
    let mut dirs = vec![base];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.flatten().map(|e| e.path()) {
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|x| x == "md") {
                docs.push(path);
            }
        }
    }
    docs.sort();
    for d in docs {
        println!("written: {}", d.display());
    }
    Ok(())
}

/// What reached the workflow's output, one line per unit: its severity or
/// verdict, its project, its title or text, and the reason it was kept.
fn print_outputs(store: &Store, doc: &workflow::Document, name: &str, instance: i64) -> Result<()> {
    let w = doc.flatten(name)?;
    let moves = store.workflow_moves(instance)?;
    let out: Vec<store::WorkflowUnit> = store
        .workflow_units(instance)?
        .into_iter()
        .filter(|u| {
            moves.iter().any(|(id, edge, taken)| {
                *taken
                    && *id == u.id
                    && w.edges
                        .get(*edge)
                        .is_some_and(|e| e.to == workflow::To::Output)
            })
        })
        .collect();
    if out.is_empty() {
        println!("\ninstance {instance}: nothing reached the output");
        return Ok(());
    }
    println!(
        "\ninstance {instance}: {} {}(s)",
        out.len(),
        w.output.as_deref().unwrap_or("unit")
    );
    let field = |d: &serde_json::Value, keys: &[&str]| -> String {
        keys.iter()
            .find_map(|k| d.get(*k).and_then(|v| v.as_str()))
            .unwrap_or_default()
            .to_string()
    };
    let mut rows = Vec::new();
    for u in &out {
        let d: serde_json::Value = serde_json::from_str(&u.data).unwrap_or_default();
        rows.push(vec![
            field(&d, &["severity", "verdict"]),
            u.project.clone().unwrap_or_default(),
            field(&d, &["title", "text", "summary"]),
        ]);
        let reason = field(&d, &["reason"]);
        if !reason.is_empty() {
            rows.push(vec![String::new(), String::new(), format!("  {reason}")]);
        }
    }
    print!("{}", pma_core::report::table(&rows, "  "));
    Ok(())
}
