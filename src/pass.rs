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

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::config::Config;
use crate::dispatch::{Chosen, Overrides};
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
    /// Where artifacts, logs and the scratch trees a node works in live.
    home: &'a Path,
    /// The flags this pass was given, which an `edit` resolves its worker
    /// with exactly as a dispatch does.
    over: &'a Overrides,
    /// Every parameter's value for this instance: the declared default,
    /// replaced by the argument the run was given. What `{$name}` reads.
    params: BTreeMap<String, Value>,
}

/// What one node's turn did: the batch budget it committed, and whether it
/// moved any unit. A turn that committed nothing and moved nothing is the
/// budget saying stop, not a node that has finished, so the pass leaves the
/// node on the frontier and the next invocation picks it up.
struct Turn {
    spent: f64,
    advanced: bool,
}

/// What one invocation of a pass was told: where to work, the arguments the
/// instance was given, and whether its spend was approved.
pub struct Invocation<'a> {
    pub home: &'a Path,
    pub args: &'a Value,
    pub approved: bool,
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

/// The argument bag, from the targets the command line named. Frozen when the
/// instance starts: a rescan must not move work under a pass already running.
///
/// A target's element type must be the one the workflow reads, which is the
/// type error section 4.3 names at the call site.
pub fn root_units(
    store: &Store,
    doc: &Document,
    workflow: &str,
    targets: &[String],
) -> Result<Vec<WorkflowUnit>> {
    use crate::dispatch::Target;
    let w = doc
        .workflow(workflow)
        .ok_or_else(|| format!("unknown workflow `{workflow}`"))?;
    let projects = store.projects()?;
    let tasks = store.tasks()?;
    let mut data: Vec<(String, Option<String>, Value)> = Vec::new();
    for spec in targets {
        let (name, what) = crate::dispatch::target(spec)?;
        let row = projects
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| format!("no project `{name}`; `pma scan` first"))?;
        match &what {
            Target::Project => {
                data.push(("project".into(), Some(row.name.clone()), project_data(row)))
            }
            Target::Line(line) => {
                let t = tasks
                    .iter()
                    .find(|t| t.project == name && t.line == *line)
                    .ok_or_else(|| format!("{spec} is not an open item at the last scan"))?;
                data.push(("item".into(), Some(row.name.clone()), item_data(t)));
            }
            Target::Priority(want) => {
                let found: Vec<&crate::store::TaskRow> = tasks
                    .iter()
                    .filter(|t| t.project == name && t.priority == *want)
                    .collect();
                if found.is_empty() {
                    return Err(
                        format!("{name}: no open {} items at the last scan", want.name()).into(),
                    );
                }
                for t in found {
                    data.push(("item".into(), Some(row.name.clone()), item_data(t)));
                }
            }
            Target::Signal(kind) => {
                let detail = match kind.as_str() {
                    "ci" => match &row.ci {
                        crate::scan::Ci::Failing(workflows) => workflows.join(" "),
                        _ => {
                            return Err(
                                format!("{name}: CI was not failing at the last scan").into()
                            );
                        }
                    },
                    _ => match row.deps.unwrap_or_default() {
                        0 => {
                            return Err(format!(
                                "{name}: no outdated dependencies at the last `pma scan --deps`"
                            )
                            .into());
                        }
                        _ => row.deps_detail.clone(),
                    },
                };
                data.push((
                    "signal".into(),
                    Some(row.name.clone()),
                    serde_json::json!({"kind": kind, "detail": detail}),
                ));
            }
            // A quadrant holds items and signals at once, so it names no one
            // type and cannot be an argument (section 4.3).
            Target::Quadrant(_) => {
                return Err(format!(
                    "`{spec}` holds items and signals together, so it is not an argument; \
                     name a heading, a line, or the project"
                )
                .into());
            }
        }
    }
    if data.is_empty() {
        return Err("the target names nothing".into());
    }
    if let Some((ty, _, _)) = data.iter().find(|(ty, _, _)| *ty != w.input) {
        return Err(format!(
            "`{workflow}` reads `{}`, and the target yields `{ty}`",
            w.input
        )
        .into());
    }
    Ok(data
        .into_iter()
        .enumerate()
        .map(|(i, (ty, project, data))| {
            let id = format!("u{}", i + 1);
            WorkflowUnit {
                id: id.clone(),
                ty,
                node: "@input".into(),
                parent: None,
                root: id,
                depth: 0,
                lap: 0,
                project,
                data: data.to_string(),
            }
        })
        .collect())
}

fn project_data(row: &crate::store::ProjectRow) -> Value {
    serde_json::json!({
        "name": row.name,
        "tier": row.tier.map(i64::from).unwrap_or_default().to_string(),
        "repo": row.path.to_string_lossy(),
        "owner": row.slug.clone().unwrap_or_default(),
        "default_branch": "",
        "ci": "",
        "deps": row.deps.unwrap_or_default().to_string(),
        "tags": "",
    })
}

fn item_data(t: &crate::store::TaskRow) -> Value {
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
}

/// The moves a root unit makes leaving `@input`, as `(edge, taken, why not)`.
/// Derived, so a dry run can price a bag it never wrote.
pub fn entry_moves(w: &Workflow, unit: &WorkflowUnit) -> Vec<(usize, bool, Option<String>)> {
    let data: Value = serde_json::from_str(&unit.data).unwrap_or(Value::Null);
    let system = system_fields(unit, &BTreeMap::new());
    w.edges
        .iter()
        .enumerate()
        .filter(|(_, e)| e.from == From::Input)
        .map(|(i, e)| match &e.when {
            None => (i, true, None),
            Some(guard) => match crate::workflow::holds(guard, &data, &system) {
                Ok(()) => (i, true, None),
                Err(why) => (i, false, Some(why)),
            },
        })
        .collect()
}

/// Admits a root unit into the graph: one move per edge leaving `@input`.
pub fn enter(store: &Store, instance: i64, w: &Workflow, unit: &WorkflowUnit) -> Result<()> {
    for (edge, taken, why) in entry_moves(w, unit) {
        store.add_workflow_move(instance, &unit.id, edge, taken, why.as_deref())?;
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
    let units = store.workflow_units(instance)?;
    let moves = store.workflow_moves(instance)?;
    plan_over(store, cfg, over, w, &units, &moves)
}

/// The frontier of a bag, priced, whether or not the store holds it. What a
/// dry run reads: it plans and prices without starting an instance, because
/// the help says it runs nothing and an abandoned instance is something.
pub fn plan_over(
    store: &Store,
    cfg: &Config,
    over: &Overrides,
    w: &Workflow,
    units: &[WorkflowUnit],
    moves: &[(String, usize, bool)],
) -> Result<Vec<Planned>> {
    let chosen = crate::dispatch::choose_worker(store, cfg, over, None)?;
    let (agent, model) = (chosen.agent, chosen.model);
    Ok(waiting(w, units, moves)
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

/// Writes a unit a node produced, once the instance cap admits it, and routes
/// it. `held` is the instance's unit count, which this advances.
///
/// The caps are what `activate` weighed against the budget, so the runtime has
/// to hold them or the approved figure bounds nothing. Exhaustion stops the
/// pass and is recorded on the instance: a bag silently short of its input
/// cannot be told from a complete one by anything downstream.
fn mint(
    ctx: &Ctx<'_>,
    node: &Node,
    unit: WorkflowUnit,
    held: &mut i64,
    verdicts: &BTreeMap<(String, String), String>,
) -> Result<()> {
    if *held >= ctx.w.caps.max_units {
        return Err(capped(
            ctx,
            &node.name,
            format!(
                "instance {} holds {held} units, its `caps.max_units`",
                ctx.instance
            ),
        ));
    }
    *held += 1;
    let id = unit.id.clone();
    ctx.store.add_workflow_unit(ctx.instance, &unit)?;
    let child = ctx
        .store
        .workflow_units(ctx.instance)?
        .into_iter()
        .find(|u| u.id == id)
        .expect("just written");
    route_unit(ctx.store, ctx.instance, ctx.w, &node.name, &child, verdicts)
}

/// Stops the pass at a cap, naming the node and what it exceeded, and records
/// the instance as capped so a resume does not walk into the same wall.
fn capped(ctx: &Ctx<'_>, node: &str, why: String) -> Box<dyn std::error::Error> {
    if let Err(e) = ctx.store.note_instance_outcome(ctx.instance, "capped") {
        return e;
    }
    format!(
        "node `{node}`: {why}. Nothing further was written; raise the cap and \
         propose another revision."
    )
    .into()
}

/// What one input unit may yield at a `0..n` map: the declared width, read at
/// its parameter's maximum, which is the figure the worst case used.
fn width(w: &Workflow, node: &Node) -> Option<i64> {
    match &node.op {
        Op::Map(m) if m.out == Out::Grows => {
            Some(m.max_units.as_ref().map_or(0, |c| c.ceiling(&w.params)))
        }
        _ => None,
    }
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
    given: &Invocation<'_>,
) -> Result<Vec<Planned>> {
    let Invocation {
        home,
        args,
        approved,
    } = *given;
    let chosen = crate::dispatch::choose_worker(store, cfg, over, None)?;
    let (agent, model) = (chosen.agent.clone(), chosen.model.clone());
    let mut plan = Vec::new();
    // One iteration per node run. A graph's worst case bounds the units, so a
    // pass that exceeds this is a bug rather than a long job, and saying so is
    // better than spinning.
    let ceiling = (w.caps.max_units.max(1) as usize + 1) * (w.nodes.len() + 1);
    // What this pass has committed against `batch_budget`, over every node it
    // has run. Held here because a pass is the unit the budget bounds, not a
    // node's turn: `advance` re-enters a node as its frontier refills.
    let mut committed = 0.0;
    let ctx = Ctx {
        store,
        cfg,
        doc,
        w,
        instance,
        home,
        over,
        params: w.arguments(args),
    };
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
        // Free nodes first, every time: a rule costs nothing and may settle
        // units an agent node would otherwise be priced for.
        for free_first in [true, false] {
            for (name, waiting) in &frontier {
                let Some(node) = w.node(name) else { continue };
                if free(node) != free_first {
                    continue;
                }
                if free_first {
                    run_free(&ctx, node, waiting, &verdicts)?;
                    ran = true;
                    break;
                }
                let priced = Planned {
                    node: name.clone(),
                    op: node.op.name().to_string(),
                    units: waiting.len(),
                    agent: Some(agent.clone()),
                    model: model.clone(),
                    cost: waiting.len() as f64 * cfg.agent_budget,
                };
                if !approved {
                    plan.push(priced);
                    continue;
                }
                let turn = match node.op {
                    Op::Edit(_) => {
                        run_edit(&ctx, node, waiting, &verdicts, cfg.batch_budget - committed)?
                    }
                    _ => run_agent(
                        &ctx,
                        node,
                        waiting,
                        &verdicts,
                        &chosen,
                        cfg.batch_budget - committed,
                    )?,
                };
                committed += turn.spent;
                // The batch budget admitted nothing, so the node keeps its
                // place: the pass ends and the next one resumes here. A pass
                // is not a daemon, and stopping at a budget is not an error.
                if !turn.advanced {
                    plan.push(priced);
                    continue;
                }
                ran = true;
                break;
            }
            if ran {
                break;
            }
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
        doc,
        w,
        instance,
        ..
    } = *ctx;
    let mut verdicts = verdicts.clone();
    match &node.op {
        Op::Map(m) => {
            let rule = m.rule.as_deref().unwrap_or_default();
            let all = store.workflow_units(instance)?;
            let mut held = all.len() as i64;
            let mut minted = 0;
            for unit in waiting {
                let produced = rule_map(store, doc, rule, m.out, unit)?;
                if let Some(width) = width(w, node)
                    && produced.len() as i64 > width
                {
                    return Err(capped(
                        ctx,
                        &node.name,
                        format!(
                            "`{}` yielded {} units from `{}`, over its `max_units` of {width}",
                            rule,
                            produced.len(),
                            unit.id
                        ),
                    ));
                }
                store.atomically(|| {
                    for data in produced {
                        let id = next_id(&all, &node.name, minted);
                        minted += 1;
                        mint(
                            ctx,
                            node,
                            WorkflowUnit {
                                id,
                                ty: m.emits.clone(),
                                node: node.name.clone(),
                                parent: Some(unit.id.clone()),
                                root: unit.root.clone(),
                                depth: unit.depth + 1,
                                lap: unit.lap,
                                project: unit.project.clone(),
                                data: data.to_string(),
                            },
                            &mut held,
                            &verdicts,
                        )?;
                    }
                    // The unit itself stops here: its children carry on.
                    store.add_workflow_move(
                        instance,
                        &unit.id,
                        settled_at(w, &node.name),
                        false,
                        Some("expanded"),
                    )
                })?;
            }
        }
        Op::Reduce(r) => {
            let rule = r.rule.as_deref().unwrap_or("dedupe");
            let all = store.workflow_units(instance)?;
            let mut held = all.len() as i64;
            let mut minted = 0;
            // A reduce decides per group, so the groups are formed first and
            // the rule says which of each group's units carry on.
            let mut groups: Vec<(String, Vec<&WorkflowUnit>)> = Vec::new();
            for unit in waiting {
                let key = group_key(r, unit);
                match groups.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, bag)) => bag.push(unit),
                    None => groups.push((key, vec![unit])),
                }
            }
            for (_, bag) in &groups {
                let (kept, dropped) = rule_reduce(rule, bag)?;
                for unit in dropped {
                    store.add_workflow_move(
                        instance,
                        &unit.id,
                        settled_at(w, &node.name),
                        false,
                        Some(match rule {
                            "dedupe" => "dropped as a duplicate",
                            _ => "dropped by the group's limit",
                        }),
                    )?;
                }
                for unit in kept {
                    let id = next_id(&all, &node.name, minted);
                    minted += 1;
                    store.atomically(|| {
                        mint(
                            ctx,
                            node,
                            WorkflowUnit {
                                id,
                                ty: r.emits.clone(),
                                node: node.name.clone(),
                                parent: Some(unit.id.clone()),
                                root: unit.root.clone(),
                                depth: unit.depth,
                                lap: unit.lap,
                                project: unit.project.clone(),
                                data: unit.data.clone(),
                            },
                            &mut held,
                            &verdicts,
                        )?;
                        // The input stops here and its replacement carries
                        // on. Without this the node would be offered the same
                        // unit on the next turn of the frontier.
                        store.add_workflow_move(
                            instance,
                            &unit.id,
                            settled_at(w, &node.name),
                            false,
                            Some("reduced"),
                        )
                    })?;
                }
            }
        }
        Op::Check { rule } => {
            for unit in waiting {
                let (verdict, detail) = run_check(ctx, rule, unit, waiting.len())?;
                verdicts.insert((unit.id.clone(), rule.clone()), verdict.clone());
                store.atomically(|| {
                    store.add_workflow_verdict(
                        instance,
                        &unit.id,
                        rule,
                        &verdict,
                        detail.as_deref(),
                    )?;
                    route_unit(store, instance, w, &node.name, unit, &verdicts)?;
                    mark_handled(store, instance, w, &node.name, &unit.id)
                })?;
            }
        }
        Op::Emit(e) => {
            for unit in waiting {
                let data: Value = serde_json::from_str(&unit.data).unwrap_or(Value::Null);
                store.atomically(|| {
                    emit(ctx, e, unit, &data, &verdicts)?;
                    route_unit(store, instance, w, &node.name, unit, &verdicts)?;
                    mark_handled(store, instance, w, &node.name, &unit.id)
                })?;
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
    if out != Out::Grows {
        return Err(format!("rule `{rule}` produces many units; it needs `0..n`").into());
    }
    let row = || {
        store
            .project(&project)?
            .ok_or_else(|| format!("no project `{project}`; `pma scan` first").into())
            as Result<crate::store::ProjectRow>
    };
    match rule {
        "todo-items" => Ok(store
            .tasks()?
            .iter()
            .filter(|t| t.project == project)
            .map(item_data)
            .collect()),
        // A run that is shipped or rejected no longer holds its task or its
        // worktree, so it is not work a workflow can act on.
        "open-runs" => Ok(store
            .runs()?
            .iter()
            .filter(|r| r.project == project && !r.state.is_final())
            .map(|r| {
                serde_json::json!({
                    "id": r.id.to_string(),
                    "task": r.text,
                    "state": r.state.name(),
                    "branch": r.branch,
                    "pr": r.outcome.clone().unwrap_or_default(),
                    "verify": r.verify_ok.map(|v| v.to_string()).unwrap_or_default(),
                    "cost": r.cost_usd.map(|c| format!("{c:.2}")).unwrap_or_default(),
                })
            })
            .collect()),
        "outdated-deps" => {
            let row = row()?;
            Ok(row
                .deps_detail
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::json!({"kind": "deps", "detail": l}))
                .collect())
        }
        // The document declares the shape it wants back, so the fields are
        // `gh`'s own names and the projection is the document's job.
        "open-issues" => {
            let row = row()?;
            let out = std::process::Command::new("gh")
                .args(["issue", "list", "--state", "open", "--limit", "100"])
                .args(["--json", "number,title,body,labels"])
                .current_dir(&row.path)
                .output()
                .map_err(|e| format!("gh: {e}"))?;
            if !out.status.success() {
                return Err(String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("gh issue list failed")
                    .to_string()
                    .into());
            }
            let listed: Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout))
                .map_err(|e| format!("gh issue list: {e}"))?;
            Ok(listed
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|v| {
                    serde_json::json!({
                        "gh": v.get("number").map(|n| n.to_string()).unwrap_or_default(),
                        "text": v.get("title").and_then(Value::as_str).unwrap_or_default(),
                        "description": v.get("body").and_then(Value::as_str).unwrap_or_default(),
                        "tags": v
                            .get("labels")
                            .and_then(Value::as_array)
                            .map(|l| {
                                l.iter()
                                    .filter_map(|x| x.get("name").and_then(Value::as_str))
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect())
        }
        other => Err(format!("rule `{other}` is not built yet").into()),
    }
}

/// The rules a `reduce` may name, applied to one group: which units carry on,
/// and which stop here. Ordering is the project's priority order, which is why
/// `rank` is refused on a type that declares no `priority`.
fn rule_reduce<'a>(
    rule: &str,
    group: &[&'a WorkflowUnit],
) -> Result<(Vec<&'a WorkflowUnit>, Vec<&'a WorkflowUnit>)> {
    let mut ordered: Vec<&WorkflowUnit> = group.to_vec();
    if rule == "rank" {
        let key = |u: &WorkflowUnit| -> usize {
            let data: Value = serde_json::from_str(&u.data).unwrap_or(Value::Null);
            data.get("priority")
                .and_then(Value::as_str)
                .and_then(todo::Priority::parse)
                .map_or(usize::MAX, |p| p as usize)
        };
        ordered.sort_by_key(|u| key(u));
    }
    let keep = match rule {
        "dedupe" => 1,
        "rank" => ordered.len(),
        other => other
            .strip_prefix("limit:")
            .and_then(|n| n.parse::<usize>().ok())
            .ok_or_else(|| format!("rule `{other}` is not built yet"))?,
    };
    let dropped = ordered.split_off(keep.min(ordered.len()));
    Ok((ordered, dropped))
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
    ctx: &Ctx<'_>,
    rule: &str,
    unit: &WorkflowUnit,
    bag: usize,
) -> Result<(String, Option<String>)> {
    let store = ctx.store;
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
        // The rest read a run an `edit` left. A unit that has none has no
        // evidence either way, which is what `unknown` says.
        other => match run_of(ctx, unit)? {
            None => Ok((
                "unknown".into(),
                Some(format!("`{other}` reads a run, and this unit has none")),
            )),
            Some(run) => Ok(verdict_of(&run, other)),
        },
    }
}

/// The run an `edit` node recorded against this unit: the latest, so a lap
/// reads its own attempt rather than the first. This is what `@run` names.
fn run_of(ctx: &Ctx<'_>, unit: &WorkflowUnit) -> Result<Option<crate::store::Run>> {
    Ok(ctx
        .store
        .runs()?
        .into_iter()
        .rev()
        .find(|r| r.workflow_instance == Some(ctx.instance) && r.unit.as_deref() == Some(&unit.id)))
}

/// A check about a run. `passed`, `failed` or `unknown`, and `unknown` is a
/// result: a check that could not run is not evidence either way.
fn verdict_of(run: &crate::store::Run, rule: &str) -> (String, Option<String>) {
    let verdict = |ok: bool| -> String {
        match ok {
            true => "passed".into(),
            false => "failed".into(),
        }
    };
    match rule {
        "verify" => match (&run.verify, run.verify_ok) {
            (None, _) => (
                "unknown".into(),
                Some(format!("`{}` states no verify command", run.project)),
            ),
            (Some(_), None) => (
                "unknown".into(),
                Some(
                    run.error
                        .clone()
                        .unwrap_or_else(|| "verify did not run".into()),
                ),
            ),
            (Some(_), Some(ok)) => (verdict(ok), (!ok).then(|| "verify failed".to_string())),
        },
        "scope-clean" => {
            let reasons = crate::accept::review_reasons(run);
            let scope: Vec<&String> = reasons
                .iter()
                .filter(|r| r.contains("outside") || r.contains("scope"))
                .collect();
            match (&run.changed_paths, scope.as_slice()) {
                (None, _) => (
                    "unknown".into(),
                    Some(
                        run.scope_error
                            .clone()
                            .unwrap_or_else(|| "the changed paths are not known".into()),
                    ),
                ),
                (Some(_), []) => ("passed".into(), None),
                (Some(_), why) => (
                    "failed".into(),
                    Some(
                        why.iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join("; "),
                    ),
                ),
            }
        }
        "ci-green" => match &run.outcome {
            None => (
                "unknown".into(),
                Some("the run has not been published".into()),
            ),
            Some(_) => match gh_json(
                &run.worktree,
                &["pr", "view", "--json", "statusCheckRollup"],
            ) {
                Err(e) => ("unknown".into(), Some(e)),
                Ok(v) => {
                    let checks = v
                        .get("statusCheckRollup")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let decisive = |c: &Value| -> Option<bool> {
                        let state = c
                            .get("conclusion")
                            .or_else(|| c.get("state"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_ascii_uppercase();
                        match state.as_str() {
                            "SUCCESS" | "NEUTRAL" | "SKIPPED" => Some(true),
                            "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE" | "ERROR" => Some(false),
                            _ => None,
                        }
                    };
                    let verdicts: Vec<Option<bool>> = checks.iter().map(decisive).collect();
                    if verdicts.contains(&Some(false)) {
                        ("failed".into(), Some("a required check failed".into()))
                    } else if verdicts.iter().any(Option::is_none) {
                        ("unknown".into(), Some("a check has not finished".into()))
                    } else if verdicts.is_empty() {
                        ("unknown".into(), Some("no checks are reported".into()))
                    } else {
                        ("passed".into(), None)
                    }
                }
            },
        },
        "pr-merged" => match &run.outcome {
            None => (
                "unknown".into(),
                Some("the run has not been published".into()),
            ),
            Some(_) => match gh_json(&run.worktree, &["pr", "view", "--json", "state"]) {
                Err(e) => ("unknown".into(), Some(e)),
                Ok(v) => match v.get("state").and_then(Value::as_str) {
                    Some("MERGED") => ("passed".into(), None),
                    Some("CLOSED") => (
                        "failed".into(),
                        Some("the pull request was closed without merging".into()),
                    ),
                    Some(other) => (
                        "unknown".into(),
                        Some(format!("the pull request is {other}")),
                    ),
                    None => ("unknown".into(), Some("gh reported no state".into())),
                },
            },
        },
        other => (
            "unknown".into(),
            Some(format!("check `{other}` is not built yet")),
        ),
    }
}

/// `gh` in the run's tree, so it finds the repository from the git remote.
fn gh_json(dir: &Path, args: &[&str]) -> std::result::Result<Value, String> {
    if !dir.is_dir() {
        return Err(format!("{} no longer exists", dir.display()));
    }
    let out = std::process::Command::new("gh")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("gh failed")
            .to_string());
    }
    serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).map_err(|e| format!("gh: {e}"))
}

/// Writes a unit where `pma` owns the file. A `todo` edit is uncommitted in the
/// user's clone, as `pma sync` writes one: `scripts/commit_todo.py` commits it.
fn emit(
    ctx: &Ctx<'_>,
    e: &crate::workflow::EmitNode,
    unit: &WorkflowUnit,
    data: &Value,
    verdicts: &BTreeMap<(String, String), String>,
) -> Result<()> {
    let store = ctx.store;
    let system = system_fields(unit, verdicts);
    let fill = |template: &str| -> String { fill(template, data, &system, &ctx.params, &[]) };
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

/// A node a model decides. One run per unit for a `map`, one per group for a
/// `reduce`, which is what the worst case prices.
///
/// The model reads `in.json` and writes `out.json` under the instance's
/// artifact directory, and it works in a detached worktree rather than in the
/// user's clone: a node that is not an `edit` may read the code and must not
/// be able to change it (W14).
///
/// Runs go `max_parallel` at a time. The store is a single connection, so it
/// stays on this thread: a worker is handed everything it needs as files and
/// paths, and what comes back is recorded here, in the order it arrives.
fn run_agent(
    ctx: &Ctx<'_>,
    node: &Node,
    waiting: &[WorkflowUnit],
    verdicts: &BTreeMap<(String, String), String>,
    chosen: &Chosen,
    left: f64,
) -> Result<Turn> {
    let batches: Vec<Vec<WorkflowUnit>> = match &node.op {
        Op::Reduce(r) => {
            let mut groups: Vec<(String, Vec<WorkflowUnit>)> = Vec::new();
            for unit in waiting {
                let key = group_key(r, unit);
                match groups.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, bag)) => bag.push(unit.clone()),
                    None => groups.push((key, vec![unit.clone()])),
                }
            }
            groups.into_iter().map(|(_, bag)| bag).collect()
        }
        _ => waiting.iter().map(|u| vec![u.clone()]).collect(),
    };
    let worker = ctx
        .store
        .agents()?
        .into_iter()
        .find(|w| w.name == chosen.agent)
        .ok_or_else(|| format!("unknown agent `{}`; see `pma agent`", chosen.agent))?;

    // What the batch budget admits, decided before anything is staged: a run
    // refused after its worktree exists would leave the worktree behind. The
    // rest wait for the next pass, which is not an error.
    let room = match ctx.cfg.agent_budget > 0.0 {
        true => (left / ctx.cfg.agent_budget).floor().max(0.0) as usize,
        false => batches.len(),
    };
    let mut staged = Vec::new();
    for batch in batches.iter().take(room) {
        staged.push(stage(ctx, node, batch, verdicts, chosen)?);
    }
    if staged.is_empty() {
        return Ok(Turn {
            spent: 0.0,
            advanced: false,
        });
    }
    // Each staged run is committed against the budget at its ceiling. What it
    // actually cost is recorded on its row; the budget admits on the ceiling,
    // as a dispatch does, because the cost is not known until it has run.
    let committed = staged.len() as f64 * ctx.cfg.agent_budget;

    let queue = std::sync::Mutex::new(std::collections::VecDeque::from(staged));
    let (tx, rx) = std::sync::mpsc::channel::<(Staged, Ran)>();
    let mut held = ctx.store.workflow_units(ctx.instance)?.len() as i64;
    let mut failure = None;
    // The store is one connection and is not shared between threads, so a
    // worker is given the settings and nothing else.
    let cfg = ctx.cfg;
    std::thread::scope(|scope| {
        for _ in 0..(cfg.max_parallel as usize).max(1) {
            let tx = tx.clone();
            let queue = &queue;
            let worker = &worker;
            scope.spawn(move || {
                loop {
                    let Some(job) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    let ran = work(cfg, worker, chosen, &job);
                    let _ = tx.send((job, ran));
                }
            });
        }
        drop(tx);
        for (job, ran) in rx {
            if let Err(e) = record(ctx, node, job, ran, &mut held, verdicts) {
                failure.get_or_insert(e.to_string());
            }
        }
    });
    match failure {
        Some(e) => Err(e.into()),
        None => Ok(Turn {
            spent: committed,
            advanced: true,
        }),
    }
}

/// One agent run, staged: everything the store and the repository had to say
/// about it, so a worker thread needs nothing but the filesystem.
struct Staged {
    run: crate::store::Run,
    batch: Vec<WorkflowUnit>,
    repo: std::path::PathBuf,
    tree: std::path::PathBuf,
    out: std::path::PathBuf,
    doc: Option<std::path::PathBuf>,
    log: std::path::PathBuf,
    /// An empty directory outside the worktree, which `agent::restrict` hands
    /// the child as its `gh` configuration. Inside the tree the agent could
    /// write credentials back into it.
    empty: std::path::PathBuf,
}

/// What a worker thread produced: no store, no repository, just the run's own
/// numbers and the units it wrote.
struct Ran {
    seconds: i64,
    cost_usd: Option<f64>,
    summary: String,
    produced: std::result::Result<Vec<Value>, String>,
}

/// Everything a run needs before a thread can take it: the files it reads and
/// writes, the worktree it works in, and the `runs` row that records it.
fn stage(
    ctx: &Ctx<'_>,
    node: &Node,
    batch: &[WorkflowUnit],
    verdicts: &BTreeMap<(String, String), String>,
    chosen: &Chosen,
) -> Result<Staged> {
    let lead = batch.first().expect("a batch holds at least one unit");
    let project = lead
        .project
        .clone()
        .ok_or_else(|| format!("node `{}`: unit `{}` names no project", node.name, lead.id))?;
    let row = ctx
        .store
        .project(&project)?
        .ok_or_else(|| format!("no project `{project}`; `pma scan` first"))?;

    let dir = artifacts(ctx, node)?;
    let input: Vec<Value> = batch.iter().map(|u| exported(u, verdicts)).collect();
    let in_file = dir.join("in.json");
    let out_file = dir.join("out.json");
    let doc_file = node_doc(ctx, node).map(|name| dir.join(name));
    write(&in_file, &serde_json::to_string_pretty(&input)?)?;

    let data: Value = serde_json::from_str(&lead.data).unwrap_or(Value::Null);
    let task = task_of(node).ok_or_else(|| format!("node `{}` states no task", node.name))?;
    let prompt = fill(
        task,
        &data,
        &system_fields(lead, verdicts),
        &ctx.params,
        &[
            ("in", in_file.to_string_lossy().into_owned()),
            ("out", out_file.to_string_lossy().into_owned()),
            (
                "doc",
                doc_file
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
        ],
    );

    // A read-only node still gets a tree of its own. An agent that writes to
    // the clone would put a change past every review this tool has. The trees
    // are added from this thread: two `git worktree add` in one repository
    // contend for its index lock.
    let tree = ctx
        .home
        .join("workflow-trees")
        .join(ctx.instance.to_string())
        .join(dir.file_name().expect("a numbered directory"));
    let _ = std::fs::remove_dir_all(&tree);
    if let Some(parent) = tree.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    crate::dispatch::git(
        &row.path,
        &[
            "worktree",
            "add",
            "--detach",
            "--quiet",
            &tree.to_string_lossy(),
            "HEAD",
        ],
    )?;

    let mut run = crate::store::Run {
        project: project.clone(),
        task_key: format!("workflow:{}:{}", ctx.instance, lead.root),
        text: node.name.clone(),
        agent: chosen.agent.clone(),
        model: chosen.model.clone(),
        repo: row.path.clone(),
        branch: String::new(),
        worktree: tree.clone(),
        base: String::new(),
        prompt,
        state: crate::store::RunState::Running,
        agent_budget: Some(ctx.cfg.agent_budget),
        timeout_minutes: Some(ctx.cfg.timeout),
        tier: row.tier,
        workflow_instance: Some(ctx.instance),
        node: Some(node.name.clone()),
        unit: Some(lead.id.clone()),
        lap: lead.lap,
        preset: chosen.preset.clone(),
        extra_args: chosen.args.clone(),
        ..crate::store::Run::default()
    };
    ctx.store.insert_run(&mut run)?;
    let log = ctx
        .home
        .join("runs")
        .join(run.id.to_string())
        .join("agent-1.log");
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let empty = ctx.home.join("empty");
    std::fs::create_dir_all(&empty).map_err(|e| format!("{}: {e}", empty.display()))?;
    Ok(Staged {
        run,
        batch: batch.to_vec(),
        repo: row.path,
        tree,
        out: out_file,
        doc: doc_file,
        log,
        empty,
    })
}

/// The agent itself. No store and no `&Ctx`: this is the half that runs on a
/// worker thread.
fn work(cfg: &Config, worker: &crate::worker::Worker, chosen: &Chosen, job: &Staged) -> Ran {
    let mut cmd = worker.build(
        &job.run.prompt,
        &job.tree,
        chosen.model.as_deref(),
        cfg.agent_budget,
        &chosen.args,
    );
    cmd.current_dir(&job.tree);
    crate::agent::restrict(&mut cmd, &job.empty);
    let finished = match crate::agent::run_limited(
        cmd,
        &job.log,
        std::time::Duration::from_secs(cfg.timeout as u64 * 60),
    ) {
        Ok(f) => f,
        Err(e) => {
            return Ran {
                seconds: 0,
                cost_usd: None,
                summary: String::new(),
                produced: Err(format!("{}: {e}", worker.command)),
            };
        }
    };
    let report = worker.parse.report(
        &std::fs::read_to_string(&job.log).unwrap_or_default(),
        finished.success == Some(true),
    );
    let produced = match finished.success {
        None => Err(format!("timed out after {} minutes", cfg.timeout)),
        Some(_) if !report.ok => Err(format!("the agent failed; log: {}", job.log.display())),
        _ => read_out(&job.out).and_then(|units| match &job.doc {
            Some(f) => doc_written(f).map(|()| units),
            None => Ok(units),
        }),
    };
    Ran {
        seconds: finished.seconds,
        cost_usd: report.cost_usd,
        summary: report.summary,
        produced,
    }
}

/// What a finished run leaves behind: the row, the units it wrote, and the
/// worktree it no longer needs. Every store write of a pass happens here.
fn record(
    ctx: &Ctx<'_>,
    node: &Node,
    job: Staged,
    ran: Ran,
    held: &mut i64,
    verdicts: &BTreeMap<(String, String), String>,
) -> Result<()> {
    let Staged {
        mut run,
        batch,
        repo,
        tree,
        ..
    } = job;
    run.seconds = Some(ran.seconds);
    run.cost_usd = ran.cost_usd;
    run.summary = Some(ran.summary);
    run.state = match &ran.produced {
        Ok(_) => crate::store::RunState::Ready,
        Err(_) => crate::store::RunState::Failed,
    };
    run.error = ran.produced.as_ref().err().cloned();
    run.ready_at = Some(crate::dates::now());
    ctx.store.update_run(&run)?;
    // The tree was the agent's scratch space, and a node that is not an
    // `edit` publishes nothing from it.
    let _ = crate::dispatch::remove_worktree(&repo, &tree, "");
    match ran.produced {
        Ok(units) => accept(ctx, node, &batch, units, held, verdicts),
        // A run that was not clean settles its input by the guards, which is
        // where a default edge catches it. Nothing is minted.
        Err(why) => {
            for unit in &batch {
                ctx.store.add_workflow_verdict(
                    ctx.instance,
                    &unit.id,
                    &node.name,
                    "failed",
                    Some(&why),
                )?;
                route_unit(ctx.store, ctx.instance, ctx.w, &node.name, unit, verdicts)?;
                mark_handled(ctx.store, ctx.instance, ctx.w, &node.name, &unit.id)?;
            }
            Ok(())
        }
    }
}

fn group_key(r: &crate::workflow::ReduceNode, unit: &WorkflowUnit) -> String {
    let data: Value = serde_json::from_str(&unit.data).unwrap_or(Value::Null);
    r.group_by
        .iter()
        .map(|f| todo::normal_text(data.get(f).and_then(Value::as_str).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("\u{0}")
}

/// `out.json` as a list of units, or why it could not be read. An agent that
/// wrote nothing produced nothing, which is a clean empty result for a filter
/// and a failure for nothing else to distinguish here.
fn read_out(file: &Path) -> std::result::Result<Vec<Value>, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(_) => return Ok(Vec::new()),
    };
    let v: Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: not JSON: {e}", file.display()))?;
    match v {
        Value::Array(units) => Ok(units),
        Value::Object(_) => Ok(vec![v]),
        _ => Err(format!("{}: expected a list of units", file.display())),
    }
}

/// A prose document the node also writes, checked as section 10 states.
fn doc_written(file: &Path) -> std::result::Result<(), String> {
    const MAX: u64 = 1 << 20;
    match std::fs::metadata(file) {
        Err(_) => Err(format!("{}: the document was not written", file.display())),
        Ok(m) if !m.is_file() => Err(format!("{}: not a regular file", file.display())),
        Ok(m) if m.len() == 0 => Err(format!("{}: the document is empty", file.display())),
        Ok(m) if m.len() > MAX => Err(format!("{}: over 1 MiB", file.display())),
        Ok(_) => Ok(()),
    }
}

/// What a model returned, admitted into the graph only where it matches the
/// declared type and the node's own contract (section 10).
fn accept(
    ctx: &Ctx<'_>,
    node: &Node,
    batch: &[WorkflowUnit],
    produced: Vec<Value>,
    held: &mut i64,
    verdicts: &BTreeMap<(String, String), String>,
) -> Result<()> {
    let (emits, out, writes) = match &node.op {
        Op::Map(m) => (m.emits.clone(), Some(m.out), m.writes.clone()),
        Op::Reduce(r) => (r.emits.clone(), None, Vec::new()),
        _ => return Err(format!("node `{}` is not decided by a model", node.name).into()),
    };
    let ty = ctx
        .doc
        .resolve(&emits)
        .ok_or_else(|| format!("node `{}`: unknown type `{emits}`", node.name))?;

    let mut reasons = Vec::new();
    if let (Some(Out::Grows), Some(width)) = (out, width(ctx.w, node))
        && produced.len() as i64 > width
    {
        reasons.push(format!(
            "{} units returned, over `max_units` of {width}",
            produced.len()
        ));
    }
    if out == Some(Out::Shrinks) && produced.len() > batch.len() {
        reasons.push("a filter returned more units than it was given".into());
    }
    if out == Some(Out::Same) && produced.len() != batch.len() {
        reasons.push(format!(
            "an annotation returned {} units for {}",
            produced.len(),
            batch.len()
        ));
    }
    for (i, v) in produced.iter().enumerate() {
        if let Err(e) = ty.check(v) {
            reasons.push(format!("unit {}: {e}", i + 1));
        }
    }
    // An annotation and a filter return the unit they were given, so the id
    // has to be one of them and every field outside `writes` has to be
    // untouched. A model that could rewrite the text could launder work past
    // whoever reads it (W17).
    if matches!(out, Some(Out::Same) | Some(Out::Shrinks)) {
        for v in &produced {
            match kept_id(v).and_then(|id| batch.iter().find(|u| u.id == id)) {
                None => reasons.push("a returned unit names no `@id` it was given".into()),
                Some(source) => {
                    if let Err(e) = unchanged(source, v, &writes) {
                        reasons.push(e);
                    }
                }
            }
        }
    }
    if !reasons.is_empty() {
        let why = reasons.join("; ");
        for unit in batch {
            ctx.store.add_workflow_verdict(
                ctx.instance,
                &unit.id,
                &node.name,
                "failed",
                Some(&why),
            )?;
            route_unit(ctx.store, ctx.instance, ctx.w, &node.name, unit, verdicts)?;
            mark_handled(ctx.store, ctx.instance, ctx.w, &node.name, &unit.id)?;
        }
        return Ok(());
    }

    let all = ctx.store.workflow_units(ctx.instance)?;
    for (i, v) in produced.into_iter().enumerate() {
        let source = kept_id(&v)
            .and_then(|id| batch.iter().find(|u| u.id == id))
            .unwrap_or(&batch[0]);
        let mut data = v;
        if let Value::Object(map) = &mut data {
            map.retain(|k, _| !k.starts_with('@'));
        }
        mint(
            ctx,
            node,
            WorkflowUnit {
                id: next_id(&all, &node.name, i),
                ty: emits.clone(),
                node: node.name.clone(),
                parent: Some(source.id.clone()),
                root: source.root.clone(),
                depth: match out {
                    Some(Out::Grows) => source.depth + 1,
                    _ => source.depth,
                },
                lap: source.lap,
                project: source.project.clone(),
                data: data.to_string(),
            },
            held,
            verdicts,
        )?;
    }
    // The input stops here whatever it produced: its children carry on, and a
    // filter's dropped units are answerable for by the moves recorded.
    for unit in batch {
        ctx.store.add_workflow_move(
            ctx.instance,
            &unit.id,
            settled_at(ctx.w, &node.name),
            false,
            Some("handed to the node's output"),
        )?;
    }
    Ok(())
}

fn kept_id(v: &Value) -> Option<String> {
    v.get("@id").and_then(Value::as_str).map(String::from)
}

/// Whether a returned unit changed only what the node declared it may write.
fn unchanged(
    source: &WorkflowUnit,
    returned: &Value,
    writes: &[String],
) -> std::result::Result<(), String> {
    let before: Value = serde_json::from_str(&source.data).unwrap_or(Value::Null);
    let Some(before) = before.as_object() else {
        return Ok(());
    };
    for (field, was) in before {
        if writes.iter().any(|w| w == field) {
            continue;
        }
        match returned.get(field) {
            Some(now) if now == was => {}
            None => {}
            Some(_) => {
                return Err(format!(
                    "`{field}` changed, and the node may write only {}",
                    match writes.is_empty() {
                        true => "nothing".to_string(),
                        false => writes.join(", "),
                    }
                ));
            }
        }
    }
    Ok(())
}

/// The unit as an agent reads it: its declared fields, plus the `@` fields of
/// section 4, so a prompt and a returned unit can name the same identity.
fn exported(unit: &WorkflowUnit, verdicts: &BTreeMap<(String, String), String>) -> Value {
    let mut map = match serde_json::from_str::<Value>(&unit.data) {
        Ok(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    for (k, v) in system_fields(unit, verdicts) {
        map.insert(format!("@{k}"), v);
    }
    Value::Object(map)
}

fn task_of(node: &Node) -> Option<&str> {
    match &node.op {
        Op::Map(m) => m.task.as_deref(),
        Op::Reduce(r) => r.task.as_deref(),
        Op::Edit(e) => Some(&e.task),
        _ => None,
    }
}

fn node_doc(ctx: &Ctx<'_>, node: &Node) -> Option<String> {
    let Op::Map(m) = &node.op else { return None };
    let name = m.doc.as_ref()?;
    let filled = fill(name, &Value::Null, &BTreeMap::new(), &ctx.params, &[]);
    // A document name reaches the filesystem, so it stays a bare file name.
    Some(
        filled
            .rsplit(['/', '\\'])
            .next()
            .filter(|s| !s.is_empty() && *s != "." && *s != "..")
            .unwrap_or("doc.md")
            .to_string(),
    )
}

/// The next numbered directory for this node, so a lap and a retry each keep
/// their own files rather than overwriting the last.
fn artifacts(ctx: &Ctx<'_>, node: &Node) -> Result<std::path::PathBuf> {
    let base = ctx
        .home
        .join("artifacts")
        .join(ctx.instance.to_string())
        .join(node.name.replace('/', "-"));
    let dir = (1..)
        .map(|n| base.join(n.to_string()))
        .find(|p| !p.exists())
        .expect("an unused number exists");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

fn write(file: &Path, text: &str) -> Result<()> {
    std::fs::write(file, text).map_err(|e| format!("{}: {e}", file.display()).into())
}

/// The three placeholder namespaces of W5, plus the file names a run works
/// with. Each is checked against its declaration where the document is read,
/// so anything left unreplaced here is a field the unit does not carry.
fn fill(
    template: &str,
    data: &Value,
    system: &BTreeMap<String, Value>,
    params: &BTreeMap<String, Value>,
    files: &[(&str, String)],
) -> String {
    let text = |v: &Value| -> String {
        v.as_str()
            .map(String::from)
            .unwrap_or_else(|| v.to_string())
    };
    let mut out = template.to_string();
    for (name, value) in files {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    for (name, value) in params {
        out = out.replace(&format!("{{${name}}}"), &text(value));
    }
    for (name, value) in system {
        out = out.replace(&format!("{{@{name}}}"), &text(value));
    }
    if let Value::Object(map) = data {
        for (name, value) in map {
            out = out.replace(&format!("{{{name}}}"), &text(value));
        }
    }
    out
}

/// An `edit` node: the only op that changes a repository. It goes through
/// `dispatch::prepare` and the phase 1 gates like any other dispatch, so a
/// workflow chooses order, prompts and parameters, never authority (W14). The
/// runs it leaves are reviewed and shipped the way every other run is.
///
/// Every unit is prepared before any is run, because `dispatch::execute`
/// admits runs against `batch_budget` across the set it is given and spreads
/// them over `max_parallel`. Handing it one run at a time would make both
/// settings mean nothing here while they mean something to `pma dispatch`.
fn run_edit(
    ctx: &Ctx<'_>,
    node: &Node,
    waiting: &[WorkflowUnit],
    verdicts: &BTreeMap<(String, String), String>,
    left: f64,
) -> Result<Turn> {
    let Op::Edit(e) = &node.op else {
        return Err(format!("node `{}` is not an edit", node.name).into());
    };
    let mut verdicts = verdicts.clone();
    let mut edits = edits_made(ctx)?;
    let mut queued: Vec<crate::store::Run> = Vec::new();
    let mut driving: BTreeMap<i64, WorkflowUnit> = BTreeMap::new();
    let mut refused = 0;
    // `execute` admits against the budget too, but a run it refuses already
    // has a worktree. Preparing only what fits leaves none to clean up.
    let room = match ctx.cfg.agent_budget > 0.0 {
        true => (left / ctx.cfg.agent_budget).floor().max(0.0) as usize,
        false => waiting.len(),
    };
    for unit in waiting {
        if queued.len() >= room {
            break;
        }
        if edits >= ctx.w.caps.max_edits {
            return Err(capped(
                ctx,
                &node.name,
                format!(
                    "instance {} has made {edits} edits, its `caps.max_edits`",
                    ctx.instance
                ),
            ));
        }
        let project = unit
            .project
            .clone()
            .ok_or_else(|| format!("node `{}`: unit `{}` names no project", node.name, unit.id))?;
        let row = ctx
            .store
            .project(&project)?
            .ok_or_else(|| format!("no project `{project}`; `pma scan` first"))?;
        let data: Value = serde_json::from_str(&unit.data).unwrap_or(Value::Null);
        let task = fill(
            &e.task,
            &data,
            &system_fields(unit, &verdicts),
            &ctx.params,
            &[],
        );
        let pick = crate::dispatch::Pick {
            workflow: Some(crate::dispatch::UnitRef {
                instance: ctx.instance,
                node: node.name.clone(),
                unit: unit.id.clone(),
                lap: unit.lap,
            }),
            project: project.clone(),
            repo: row.path.clone(),
            // The exhaustion counter keys on the root, so a retry and a lap
            // draw from one budget (W23).
            key: format!("workflow:{}:{}", ctx.instance, unit.root),
            text: summary(&task),
            gh: None,
            tier: row.tier,
            class: None,
            details: Some(task),
            quadrant: None,
        };
        match crate::dispatch::prepare(ctx.store, ctx.home, ctx.cfg, ctx.over, &pick)? {
            crate::dispatch::Prepared::Refused(why) => {
                ctx.store.add_workflow_verdict(
                    ctx.instance,
                    &unit.id,
                    &node.name,
                    "failed",
                    Some(&why),
                )?;
                route_unit(ctx.store, ctx.instance, ctx.w, &node.name, unit, &verdicts)?;
                mark_handled(ctx.store, ctx.instance, ctx.w, &node.name, &unit.id)?;
                refused += 1;
            }
            crate::dispatch::Prepared::Queued(run) => {
                edits += 1;
                driving.insert(run.id, unit.clone());
                queued.push(*run);
            }
        }
    }
    if queued.is_empty() {
        return Ok(Turn {
            spent: 0.0,
            advanced: refused > 0,
        });
    }
    let committed = queued.len() as f64 * ctx.cfg.agent_budget;
    let done = crate::dispatch::execute(ctx.store, ctx.home, ctx.cfg, ctx.over, queued, |_| {})?;
    for run in done {
        let Some(unit) = driving.get(&run.id) else {
            continue;
        };
        // A run the batch budget refused never reached the agent, so its unit
        // keeps its place in the frontier and the next pass prepares it
        // again. Its worktree goes, or the branch would be in the way.
        if run
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with(crate::dispatch::NOT_STARTED))
        {
            let _ = crate::dispatch::remove_worktree(&run.repo, &run.worktree, &run.branch);
            continue;
        }
        // The node's own check writes its verdict against the unit, under the
        // rule's name, so a guard reads `@verify` rather than the run.
        if let Some(rule) = &e.check {
            let (verdict, detail) = verdict_of(&run, rule);
            ctx.store.add_workflow_verdict(
                ctx.instance,
                &unit.id,
                rule,
                &verdict,
                detail.as_deref(),
            )?;
            verdicts.insert((unit.id.clone(), rule.clone()), verdict);
        }
        route_unit(ctx.store, ctx.instance, ctx.w, &node.name, unit, &verdicts)?;
        mark_handled(ctx.store, ctx.instance, ctx.w, &node.name, &unit.id)?;
    }
    Ok(Turn {
        spent: committed,
        advanced: true,
    })
}

/// Edits this instance has already made, which is what `caps.max_edits`
/// bounds. Counted from the runs recorded, because a pass holds no state.
fn edits_made(ctx: &Ctx<'_>) -> Result<i64> {
    Ok(ctx
        .store
        .runs()?
        .iter()
        .filter(|r| r.workflow_instance == Some(ctx.instance))
        .filter(|r| {
            r.node
                .as_deref()
                .and_then(|n| ctx.w.node(n))
                .is_some_and(|n| matches!(n.op, Op::Edit(_)))
        })
        .count() as i64)
}

/// A one-line title for the branch and the run list. The prompt itself is the
/// run's details.
fn summary(task: &str) -> String {
    let line = task.lines().find(|l| !l.trim().is_empty()).unwrap_or(task);
    line.chars()
        .take(120)
        .collect::<String>()
        .trim()
        .to_string()
}
