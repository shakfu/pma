//! One pass over a workflow instance: write the argument bag, derive what is
//! runnable, say what it would cost, and advance only what was approved.
//!
//! A pass holds no state of its own. What has run is the units a node wrote and
//! the moves recorded against them, so the frontier is re-derived on every
//! invocation and a killed pass resumes by recomputing rather than by trusting
//! a cursor.
//!
//! Nothing here spends money without being told to. A node backed by a rule is
//! free and deterministic, and runs as soon as its units arrive; a node backed
//! by an agent is planned, priced and left for the developer to approve.
//! Design: `docs/dev/workflows.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use crate::config::Config;
use crate::dispatch::Overrides;
use crate::store::{Result, Store, WorkflowUnit};
use crate::todo;
use crate::workflow::{Action, Document, From, Node, Op, Out, Sink, To, Via, Workflow};

/// What a pass reads while it runs one node: the store it records into, the
/// settings, and the graph it is walking.
struct Ctx<'a> {
    store: &'a Store,
    cfg: &'a Config,
    doc: &'a Document,
    w: &'a Workflow,
    instance: i64,
}

/// What one node of the frontier would do.
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub node: String,
    pub op: String,
    pub units: usize,
    /// `None` for a node a rule decides, which costs nothing.
    pub agent: Option<String>,
    pub model: Option<String>,
    /// Upper bound on what running it now would spend.
    pub cost: f64,
}

impl Planned {
    pub fn free(&self) -> bool {
        self.agent.is_none()
    }
}

/// The argument bag, from a target the command line named. Frozen when the
/// instance starts: a rescan must not move work under a pass already running.
pub fn root_units(
    store: &Store,
    doc: &Document,
    workflow: &str,
    projects: &[String],
) -> Result<Vec<WorkflowUnit>> {
    let w = doc
        .workflow(workflow)
        .ok_or_else(|| format!("unknown workflow `{workflow}`"))?;
    if w.input != "project" {
        return Err(format!(
            "`{workflow}` reads `{}`; a pass can take a project target only, for now",
            w.input
        )
        .into());
    }
    let rows = store.projects()?;
    let mut units = Vec::new();
    for (i, name) in projects.iter().enumerate() {
        let row = rows
            .iter()
            .find(|p| p.name == *name)
            .ok_or_else(|| format!("no project `{name}`; `pma scan` first"))?;
        let data = serde_json::json!({
            "name": row.name,
            "tier": row.tier.map(i64::from).unwrap_or_default().to_string(),
            "repo": row.path.to_string_lossy(),
            "owner": row.slug.clone().unwrap_or_default(),
            "default_branch": "",
            "ci": "",
            "deps": row.deps.unwrap_or_default().to_string(),
            "tags": "",
        });
        let id = format!("u{}", i + 1);
        units.push(WorkflowUnit {
            id: id.clone(),
            ty: "project".into(),
            node: "@input".into(),
            parent: None,
            root: id,
            depth: 0,
            lap: 0,
            project: Some(row.name.clone()),
            data: data.to_string(),
        });
    }
    if units.is_empty() {
        return Err("the target names no project".into());
    }
    Ok(units)
}

/// Admits a root unit into the graph: one move per edge leaving `@input`.
pub fn enter(store: &Store, instance: i64, w: &Workflow, unit: &WorkflowUnit) -> Result<()> {
    let data: Value = serde_json::from_str(&unit.data).unwrap_or(Value::Null);
    let system = system_fields(unit, &BTreeMap::new());
    for (i, e) in w.edges.iter().enumerate() {
        if e.from != From::Input {
            continue;
        }
        match &e.when {
            None => store.add_workflow_move(instance, &unit.id, i, true, None)?,
            Some(guard) => match crate::workflow::holds(guard, &data, &system) {
                Ok(()) => store.add_workflow_move(instance, &unit.id, i, true, None)?,
                Err(why) => store.add_workflow_move(instance, &unit.id, i, false, Some(&why))?,
            },
        }
    }
    Ok(())
}

/// The frontier, priced, without running anything.
pub fn plan(
    store: &Store,
    cfg: &Config,
    over: &Overrides,
    w: &Workflow,
    instance: i64,
) -> Result<Vec<Planned>> {
    let chosen = crate::dispatch::choose_worker(store, cfg, over, None)?;
    let (agent, model) = (chosen.agent, chosen.model);
    let units = store.workflow_units(instance)?;
    let moves = store.workflow_moves(instance)?;
    Ok(waiting(w, &units, &moves)
        .into_iter()
        .filter_map(|(name, units)| {
            let node = w.node(&name)?;
            let free = free(node);
            Some(Planned {
                node: name.clone(),
                op: node.op.name().to_string(),
                units: units.len(),
                agent: (!free).then(|| agent.clone()),
                model: (!free).then(|| model.clone()).flatten(),
                cost: if free {
                    0.0
                } else {
                    units.len() as f64 * cfg.agent_budget
                },
            })
        })
        .collect())
}

/// One line per node of the frontier, priced. What the developer approves.
pub fn describe(plan: &[Planned], lead: &str) -> String {
    if plan.is_empty() {
        return "nothing is runnable\n".to_string();
    }
    let mut rows = Vec::new();
    for p in plan {
        rows.push(vec![
            format!("{lead}{}", p.node),
            p.op.clone(),
            format!("{} unit(s)", p.units),
            match (&p.agent, &p.model) {
                (None, _) => "a rule, free".to_string(),
                (Some(a), None) => a.clone(),
                (Some(a), Some(m)) => format!("{a}/{m}"),
            },
            if p.free() {
                "$0.00".to_string()
            } else {
                format!("<= ${:.2}", p.cost)
            },
        ]);
    }
    crate::report::table(&rows, "")
}

/// Whether a rule decides this node, which is what makes it free.
fn free(node: &Node) -> bool {
    match &node.op {
        Op::Check { .. } | Op::Emit(_) => true,
        Op::Map(m) => m.via == Via::Rule,
        Op::Reduce(r) => r.via == Via::Rule,
        Op::Edit(_) | Op::Call(_) => false,
    }
}

/// Units waiting at each node: those an incoming edge admitted and this node
/// has not consumed. A node whose units have all been consumed is done, which
/// is how a second pass advances nothing.
fn waiting(
    w: &Workflow,
    units: &[WorkflowUnit],
    moves: &[(String, usize, bool)],
) -> BTreeMap<String, Vec<WorkflowUnit>> {
    let mut out: BTreeMap<String, Vec<WorkflowUnit>> = BTreeMap::new();
    for (unit, edge, taken) in moves {
        if !taken {
            continue;
        }
        let Some(e) = w.edges.get(*edge) else {
            continue;
        };
        let To::Node(target) = &e.to else { continue };
        let Some(u) = units.iter().find(|u| u.id == *unit) else {
            continue;
        };
        if handled(w, target, &u.id, moves) {
            continue;
        }
        let bag = out.entry(target.clone()).or_default();
        if !bag.iter().any(|other| other.id == u.id) {
            bag.push(u.clone());
        }
    }
    out
}

/// The moves a unit makes when it leaves `node`, and the guard that refused it
/// where it goes nowhere. Recorded whatever happens, so a unit that stopped is
/// answerable for.
fn route_unit(
    store: &Store,
    instance: i64,
    w: &Workflow,
    from: &str,
    unit: &WorkflowUnit,
    verdicts: &BTreeMap<(String, String), String>,
) -> Result<()> {
    let data: Value = serde_json::from_str(&unit.data).unwrap_or(Value::Null);
    let system = system_fields(unit, verdicts);
    let mut matched = false;
    let mut defaults = Vec::new();
    for (i, e) in w.edges.iter().enumerate() {
        if e.from != From::Node(from.to_string()) {
            continue;
        }
        if e.default {
            defaults.push(i);
            continue;
        }
        match &e.when {
            None => {
                store.add_workflow_move(instance, &unit.id, i, true, None)?;
                matched = true;
            }
            Some(guard) => match crate::workflow::holds(guard, &data, &system) {
                Ok(()) => {
                    store.add_workflow_move(instance, &unit.id, i, true, None)?;
                    matched = true;
                }
                Err(why) => store.add_workflow_move(instance, &unit.id, i, false, Some(&why))?,
            },
        }
    }
    if !matched {
        for i in defaults {
            store.add_workflow_move(instance, &unit.id, i, true, Some("no guard accepted it"))?;
            matched = true;
        }
    }
    if !matched {
        // Settled here. The refusals above say why.
        store.add_workflow_move(
            instance,
            &unit.id,
            settled_at(w, from),
            false,
            Some("settled"),
        )?;
    }
    Ok(())
}

/// The `@` fields a guard may read: the unit's own, plus every verdict written
/// against it. A guard reads what is recorded rather than recomputing it.
fn system_fields(
    unit: &WorkflowUnit,
    verdicts: &BTreeMap<(String, String), String>,
) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    out.insert("id".into(), Value::from(unit.id.clone()));
    out.insert("type".into(), Value::from(unit.ty.clone()));
    out.insert("node".into(), Value::from(unit.node.clone()));
    out.insert("root".into(), Value::from(unit.root.clone()));
    out.insert("depth".into(), Value::from(unit.depth));
    out.insert("lap".into(), Value::from(unit.lap));
    if let Some(parent) = &unit.parent {
        out.insert("parent".into(), Value::from(parent.clone()));
    }
    if let Some(project) = &unit.project {
        out.insert("project".into(), Value::from(project.clone()));
    }
    for ((u, check), verdict) in verdicts {
        if *u == unit.id {
            out.insert(check.clone(), Value::from(verdict.clone()));
        }
    }
    out
}

/// Where a unit's "it went nowhere from here" move is recorded. Moves are
/// keyed by edge, and a unit that leaves a node by no edge has no edge to key
/// on, so each node gets one index past the edge list. Per node rather than per
/// unit, because a fork admits one unit into two nodes and both must be
/// answerable for it.
fn settled_at(w: &Workflow, node: &str) -> usize {
    let index = w.nodes.iter().position(|n| n.name == node).unwrap_or(0);
    w.edges.len() + index
}

/// Whether this node has already handled the unit: a move on one of its own
/// outgoing edges, or its settled marker.
fn handled(w: &Workflow, node: &str, unit: &str, moves: &[(String, usize, bool)]) -> bool {
    let settled = settled_at(w, node);
    moves.iter().any(|(u, edge, _)| {
        u == unit
            && (*edge == settled
                || w.edges
                    .get(*edge)
                    .is_some_and(|e| e.from == From::Node(node.to_string())))
    })
}

fn next_id(units: &[WorkflowUnit], node: &str, n: usize) -> String {
    format!("{node}-{}", units.len() + n + 1)
}

/// Runs every node of the frontier a rule decides, and plans the rest. The
/// plan is what the developer approves before anything is spent.
pub fn advance(
    store: &Store,
    cfg: &Config,
    over: &Overrides,
    doc: &Document,
    w: &Workflow,
    instance: i64,
) -> Result<Vec<Planned>> {
    let chosen = crate::dispatch::choose_worker(store, cfg, over, None)?;
    let (agent, model) = (chosen.agent, chosen.model);
    let mut plan = Vec::new();
    // One iteration per node run. A graph's worst case bounds the units, so a
    // pass that exceeds this is a bug rather than a long job, and saying so is
    // better than spinning.
    let ceiling = (w.caps.max_units.max(1) as usize + 1) * (w.nodes.len() + 1);
    for _ in 0..ceiling {
        let units = store.workflow_units(instance)?;
        let moves = store.workflow_moves(instance)?;
        let verdicts: BTreeMap<(String, String), String> = store
            .workflow_verdicts(instance)?
            .into_iter()
            .map(|(u, c, v)| ((u, c), v))
            .collect();
        let frontier = waiting(w, &units, &moves);
        let mut ran = false;
        plan.clear();
        for (name, waiting) in &frontier {
            let Some(node) = w.node(name) else { continue };
            if free(node) {
                run_free(
                    &Ctx {
                        store,
                        cfg,
                        doc,
                        w,
                        instance,
                    },
                    node,
                    waiting,
                    &verdicts,
                )?;
                ran = true;
                break;
            }
            plan.push(Planned {
                node: name.clone(),
                op: node.op.name().to_string(),
                units: waiting.len(),
                agent: Some(agent.clone()),
                model: model.clone(),
                cost: waiting.len() as f64 * cfg.agent_budget,
            });
        }
        if !ran {
            return Ok(plan);
        }
    }
    Err(format!(
        "workflow `{}` ran {ceiling} nodes without settling, which is a bug in the pass",
        w.name
    )
    .into())
}

/// A node a rule decides. It reads what is already recorded, writes units or a
/// verdict or a file `pma` owns, and costs nothing.
fn run_free(
    ctx: &Ctx<'_>,
    node: &Node,
    waiting: &[WorkflowUnit],
    verdicts: &BTreeMap<(String, String), String>,
) -> Result<()> {
    let Ctx {
        store,
        cfg,
        doc,
        w,
        instance,
    } = *ctx;
    let mut verdicts = verdicts.clone();
    match &node.op {
        Op::Map(m) => {
            let rule = m.rule.as_deref().unwrap_or_default();
            let all = store.workflow_units(instance)?;
            let mut minted = 0;
            for unit in waiting {
                let produced = rule_map(store, doc, rule, m.out, unit)?;
                for data in produced {
                    let id = next_id(&all, &node.name, minted);
                    minted += 1;
                    store.add_workflow_unit(
                        instance,
                        &WorkflowUnit {
                            id: id.clone(),
                            ty: m.emits.clone(),
                            node: node.name.clone(),
                            parent: Some(unit.id.clone()),
                            root: unit.root.clone(),
                            depth: unit.depth + 1,
                            lap: unit.lap,
                            project: unit.project.clone(),
                            data: data.to_string(),
                        },
                    )?;
                    let child = store
                        .workflow_units(instance)?
                        .into_iter()
                        .find(|u| u.id == id)
                        .expect("just written");
                    route_unit(store, instance, w, &node.name, &child, &verdicts)?;
                }
                // The unit itself stops here: its children carry on.
                store.add_workflow_move(
                    instance,
                    &unit.id,
                    settled_at(w, &node.name),
                    false,
                    Some("expanded"),
                )?;
            }
        }
        Op::Reduce(r) => {
            // One unit per group, keeping the first, which is `dedupe`.
            let all = store.workflow_units(instance)?;
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut minted = 0;
            for unit in waiting {
                let data: Value = serde_json::from_str(&unit.data).unwrap_or(Value::Null);
                let key: String = r
                    .group_by
                    .iter()
                    .map(|f| {
                        todo::normal_text(data.get(f).and_then(Value::as_str).unwrap_or_default())
                    })
                    .collect::<Vec<_>>()
                    .join("\u{0}");
                if !seen.insert(key) {
                    store.add_workflow_move(
                        instance,
                        &unit.id,
                        settled_at(w, &node.name),
                        false,
                        Some("dropped as a duplicate"),
                    )?;
                    continue;
                }
                let id = next_id(&all, &node.name, minted);
                minted += 1;
                store.add_workflow_unit(
                    instance,
                    &WorkflowUnit {
                        id: id.clone(),
                        ty: r.emits.clone(),
                        node: node.name.clone(),
                        parent: Some(unit.id.clone()),
                        root: unit.root.clone(),
                        depth: unit.depth,
                        lap: unit.lap,
                        project: unit.project.clone(),
                        data: unit.data.clone(),
                    },
                )?;
                let child = store
                    .workflow_units(instance)?
                    .into_iter()
                    .find(|u| u.id == id)
                    .expect("just written");
                route_unit(store, instance, w, &node.name, &child, &verdicts)?;
            }
        }
        Op::Check { rule } => {
            for unit in waiting {
                let (verdict, detail) = run_check(store, rule, unit, waiting.len())?;
                store.add_workflow_verdict(
                    instance,
                    &unit.id,
                    rule,
                    &verdict,
                    detail.as_deref(),
                )?;
                verdicts.insert((unit.id.clone(), rule.clone()), verdict);
                route_unit(store, instance, w, &node.name, unit, &verdicts)?;
                mark_handled(store, instance, w, &node.name, &unit.id)?;
            }
        }
        Op::Emit(e) => {
            for unit in waiting {
                let data: Value = serde_json::from_str(&unit.data).unwrap_or(Value::Null);
                emit(store, cfg, e, unit, &data)?;
                route_unit(store, instance, w, &node.name, unit, &verdicts)?;
                mark_handled(store, instance, w, &node.name, &unit.id)?;
            }
        }
        Op::Edit(_) | Op::Call(_) => {
            return Err(format!("node `{}` is not decided by a rule", node.name).into());
        }
    }
    Ok(())
}

/// A node that passes a unit on rather than replacing it still has to record
/// that it handled it, or the frontier would offer the same unit again.
fn mark_handled(store: &Store, instance: i64, w: &Workflow, node: &str, unit: &str) -> Result<()> {
    store.add_workflow_move(instance, unit, settled_at(w, node), false, Some("handled"))
}

/// The rules a `map` may name. Each reads what `pma` already knows.
fn rule_map(
    store: &Store,
    doc: &Document,
    rule: &str,
    out: Out,
    unit: &WorkflowUnit,
) -> Result<Vec<Value>> {
    let data: Value = serde_json::from_str(&unit.data).unwrap_or(Value::Null);
    let project = unit.project.clone().unwrap_or_default();
    if let Some(ty) = rule.strip_prefix("as:") {
        // A projection by field name: keep what the target type declares.
        let target = doc
            .resolve(ty)
            .ok_or_else(|| format!("rule `{rule}`: unknown type `{ty}`"))?;
        let mut kept = serde_json::Map::new();
        for field in target.fields.keys() {
            if let Some(v) = data.get(field) {
                kept.insert(field.clone(), v.clone());
            }
        }
        return Ok(vec![Value::Object(kept)]);
    }
    if let Some(condition) = rule.strip_prefix("where:") {
        let (field, want) = condition
            .split_once('=')
            .ok_or_else(|| format!("rule `{rule}`: expected field=value"))?;
        let holds = data.get(field).and_then(Value::as_str) == Some(want);
        return Ok(if holds { vec![data] } else { Vec::new() });
    }
    if let Some(glob) = rule.strip_prefix("path-present:") {
        return Ok(path_holds(store, &project, glob, true)?
            .then_some(data)
            .into_iter()
            .collect());
    }
    if let Some(glob) = rule.strip_prefix("path-absent:") {
        return Ok(path_holds(store, &project, glob, false)?
            .then_some(data)
            .into_iter()
            .collect());
    }
    match rule {
        "todo-items" => {
            if out != Out::Grows {
                return Err(format!("rule `{rule}` produces many units; it needs `0..n`").into());
            }
            let items = store
                .tasks()?
                .into_iter()
                .filter(|t| t.project == project)
                .map(|t| {
                    serde_json::json!({
                        "key": t.key,
                        "text": t.text,
                        "priority": t.priority.name(),
                        "tags": t.tags.join(" "),
                        "due": t.due.clone().unwrap_or_default(),
                        "gh": t.gh.map(|n| n.to_string()).unwrap_or_default(),
                        "group": t.group.clone().unwrap_or_default(),
                        "description": "",
                        "done": "false",
                    })
                })
                .collect();
            Ok(items)
        }
        other => Err(format!("rule `{other}` is not built yet").into()),
    }
}

/// Whether the project's working tree holds a path matching `glob`.
fn path_holds(store: &Store, project: &str, glob: &str, want: bool) -> Result<bool> {
    let row = store
        .project(project)?
        .ok_or_else(|| format!("no project `{project}`"))?;
    let found = walk(&row.path, &row.path, glob);
    Ok(found == want)
}

fn walk(root: &Path, dir: &Path, glob: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy();
        if crate::scan::glob_match(glob, &relative) {
            return true;
        }
        if path.is_dir() && !path.ends_with(".git") && walk(root, &path, glob) {
            return true;
        }
    }
    false
}

/// A check writes `passed`, `failed` or `unknown`. An unknown is a result: a
/// check that could not run is not evidence either way.
fn run_check(
    store: &Store,
    rule: &str,
    unit: &WorkflowUnit,
    bag: usize,
) -> Result<(String, Option<String>)> {
    match rule {
        "nonempty" => Ok(if bag > 0 {
            ("passed".into(), None)
        } else {
            ("failed".into(), None)
        }),
        "lint-todo" => {
            let Some(project) = &unit.project else {
                return Ok(("unknown".into(), Some("the unit names no project".into())));
            };
            let Some(row) = store.project(project)? else {
                return Ok(("unknown".into(), Some(format!("no project `{project}`"))));
            };
            let Ok(text) = std::fs::read_to_string(row.path.join("TODO.md")) else {
                return Ok(("unknown".into(), Some("no TODO.md".into())));
            };
            let parsed = todo::parse(&text);
            Ok(if parsed.has_errors() {
                ("failed".into(), Some("TODO.md has lint errors".into()))
            } else {
                ("passed".into(), None)
            })
        }
        other => Ok((
            "unknown".into(),
            Some(format!("check `{other}` is not built yet")),
        )),
    }
}

/// Writes a unit where `pma` owns the file. A `todo` edit is uncommitted in the
/// user's clone, as `pma sync` writes one: `scripts/commit_todo.py` commits it.
fn emit(
    store: &Store,
    _cfg: &Config,
    e: &crate::workflow::EmitNode,
    unit: &WorkflowUnit,
    data: &Value,
) -> Result<()> {
    let fill = |template: &str| -> String {
        let mut out = template.to_string();
        if let Value::Object(map) = data {
            for (k, v) in map {
                let text = v
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| v.to_string());
                out = out.replace(&format!("{{{k}}}"), &text);
            }
        }
        out
    };
    match e.sink {
        Sink::Note => {
            let text = fill(e.map.get("text").map(String::as_str).unwrap_or_default());
            store.add_note(&text, crate::dates::now())?;
            Ok(())
        }
        Sink::Todo => {
            let Some(project) = &unit.project else {
                return Err("a todo sink needs a unit that names a project".into());
            };
            let row = store
                .project(project)?
                .ok_or_else(|| format!("no project `{project}`"))?;
            let file = row.path.join("TODO.md");
            let text =
                std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            if todo::parse(&text).has_errors() {
                return Err(format!(
                    "{}: lint errors, so an item cannot be identified; `pma lint` first",
                    file.display()
                )
                .into());
            }
            let item = fill(e.map.get("text").map(String::as_str).unwrap_or_default());
            let updated = match e.action {
                Action::Add => {
                    let priority = e
                        .map
                        .get("priority")
                        .map(|p| fill(p))
                        .and_then(|p| todo::Priority::parse(&p))
                        .ok_or("adding an item needs a priority the sections name")?;
                    let description = e.map.get("description").map(|d| fill(d));
                    todo::insert(&text, priority, &item, description.as_deref())
                }
                Action::Tick => todo::mark_done(&text, &item, &item),
                Action::Remove => Some(todo::prune(&text).text),
            };
            match updated {
                Some(new) if new != text => {
                    std::fs::write(&file, new).map_err(|e| format!("{}: {e}", file.display()))?;
                    Ok(())
                }
                _ => Ok(()),
            }
        }
        Sink::Doc => Ok(()),
    }
}
