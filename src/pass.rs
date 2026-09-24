//! One pass over a workflow instance: write the argument bag, derive what is
//! ready, run what a rule decides, and run an agent only under a plan the
//! developer approved by its id.
//!
//! A pass holds no state of its own. What has run is the units a node wrote and
//! the moves recorded against them, so the frontier is re-derived on every
//! invocation and a killed pass resumes by recomputing rather than by trusting
//! a cursor.
//!
//! A node is ready once every node before it has finished, so a node always
//! sees its whole bag: a `reduce` joins every branch, and a plan names every
//! unit it will spend on. Design: `docs/dev/workflows.md`.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use crate::config::Config;
use crate::dispatch::{Chosen, Overrides};
use crate::route::{Approval, Policy};
use crate::store::{Result, Store, WorkflowUnit};
use crate::todo;
use crate::workflow::{
    Action, Document, FieldType, From, Node, Op, Out, Sink, To, Type, Via, Workflow,
};

/// What a pass reads while it runs one node: the store it records into, the
/// settings, the graph it is walking, and what it has learnt this pass.
struct Ctx<'a> {
    store: &'a Store,
    cfg: &'a Config,
    doc: &'a Document,
    w: &'a Workflow,
    instance: i64,
    /// Where artifacts, logs and the scratch trees a node works in live.
    home: &'a Path,
    /// The flags this pass was given, which override any route.
    over: &'a Overrides,
    /// Every parameter's value for this instance: the declared default,
    /// replaced by the argument the run was given. What `{$name}` reads.
    params: BTreeMap<String, Value>,
    /// The routing policy in effect, if any.
    policy: Option<Active>,
    /// Units a node refused this pass, each with its reason, for the report.
    refused: RefCell<Vec<String>>,
    /// Units a check found not ready this pass. They wait for the next one.
    deferred: RefCell<BTreeSet<(String, String)>>,
    /// The fetched head of each project's default branch, which the read
    /// nodes of a workflow that edits work from.
    bases: RefCell<BTreeMap<String, std::result::Result<String, String>>>,
}

/// A policy revision in effect: its number, whether it is in shadow, and the
/// policy itself.
pub struct Active {
    revision: i64,
    shadow: bool,
    policy: Policy,
}

/// The routing policy in effect, if any.
pub fn active_policy(store: &Store) -> Result<Option<Active>> {
    Ok(match store.active_route()? {
        None => None,
        Some(rev) => Some(Active {
            revision: rev.revision,
            shadow: rev.shadow,
            policy: rev.policy()?,
        }),
    })
}

/// A warning per agent node that the policy in effect would refuse, because
/// no route names it. Under a policy, a node is never sent to the settings.
pub fn unrouted(store: &Store, w: &Workflow) -> Result<Vec<String>> {
    let Some(active) = active_policy(store)? else {
        return Ok(Vec::new());
    };
    Ok(w.nodes
        .iter()
        .filter(|n| !free(n))
        .filter(|n| {
            !active
                .policy
                .names_node(&n.name, matches!(n.op, Op::Edit(_)))
        })
        .map(|n| {
            format!(
                "node `{}`: no route in policy revision {} names it, so it would be refused; \
                 add {{\"match\": {{\"node\": \"*/{}\"}}, ...}}",
                n.name,
                active.revision,
                n.name.rsplit('/').next().unwrap_or(&n.name)
            )
        })
        .collect())
}

/// A cap an instance hit. `advance` records it on the instance outside any
/// transaction a node was writing in, so the record survives the rollback.
#[derive(Debug)]
pub struct Capped(pub String);

impl std::fmt::Display for Capped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Capped {}

/// What a guard and a prompt may read about a unit beyond its fields: the
/// verdicts written against it, and the run an `edit` recorded for it.
#[derive(Debug, Clone, Default)]
struct Known {
    verdicts: BTreeMap<(String, String), String>,
    runs: BTreeMap<String, i64>,
}

impl Known {
    fn load(store: &Store, instance: i64) -> Result<Known> {
        let verdicts = store
            .workflow_verdicts(instance)?
            .into_iter()
            .map(|(u, c, v)| ((u, c), v))
            .collect();
        // Oldest first, so a later run for the same unit replaces an earlier.
        let runs = store
            .runs()?
            .into_iter()
            .filter(|r| r.workflow_instance == Some(instance))
            .filter_map(|r| Some((r.unit?, r.id)))
            .collect();
        Ok(Known { verdicts, runs })
    }
}

/// What one invocation of a pass was told: where to work, the arguments the
/// instance was given, and the plan the developer approved, if any.
pub struct Invocation<'a> {
    pub home: &'a Path,
    pub args: &'a Value,
    pub approve: Option<&'a str>,
}

/// What one node of a plan would spend on.
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub node: String,
    pub op: String,
    /// The units it would run over, by id.
    pub units: Vec<String>,
    /// `None` for a node a rule decides, which costs nothing.
    pub agent: Option<String>,
    pub model: Option<String>,
    /// Upper bound on what running it would spend.
    pub cost: f64,
}

impl Planned {
    pub fn free(&self) -> bool {
        self.agent.is_none()
    }
}

/// The agent runs a pass would make, which is what an approval names.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    pub steps: Vec<Planned>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn cost(&self) -> f64 {
        self.steps.iter().map(|p| p.cost).sum()
    }

    /// A short hash of what the plan runs: each node, its units, its worker
    /// and its model. Approving by id means a pass runs what was printed and
    /// nothing else; a frontier that changed has another id.
    pub fn id(&self) -> String {
        let mut text = String::new();
        for p in &self.steps {
            text.push_str(&format!(
                "{}\u{0}{}\u{0}{}\u{0}{}\n",
                p.node,
                p.units.join(","),
                p.agent.as_deref().unwrap_or(""),
                p.model.as_deref().unwrap_or("")
            ));
        }
        // FNV-1a: stable across builds and machines, unlike `DefaultHasher`.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in text.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{h:016x}")[..12].to_string()
    }
}

/// What a pass did and where it stopped.
#[derive(Debug, Default)]
pub struct Outcome {
    /// The agent runs the next pass would make, priced. Empty when no agent
    /// node is ready.
    pub plan: Plan,
    /// Nodes holding units that cannot move yet, with how many: a check
    /// waiting on the world, and every node after it.
    pub waiting: Vec<(String, usize)>,
    /// Units a node refused this pass, each with its reason.
    pub refused: Vec<String>,
    /// Whether an approved plan ran.
    pub ran: bool,
}

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
    let system = system_fields(unit, &Known::default());
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

/// Nodes that may run now, in dependency order, each with the units waiting
/// at it. A node is ready once every node before it has finished: it holds
/// nothing and everything before it has finished. A node therefore sees its
/// whole bag, which a `reduce` joining several branches depends on.
fn frontier(
    w: &Workflow,
    units: &[WorkflowUnit],
    moves: &[(String, usize, bool)],
    deferred: &BTreeSet<(String, String)>,
) -> Vec<(String, Vec<WorkflowUnit>)> {
    let waiting = waiting(w, units, moves);
    let mut finished: BTreeSet<String> = BTreeSet::new();
    let mut ready = Vec::new();
    for name in crate::workflow::topological(w) {
        if !w.predecessors(&name).iter().all(|p| finished.contains(*p)) {
            continue;
        }
        let held = waiting.get(&name).cloned().unwrap_or_default();
        if held.is_empty() {
            finished.insert(name);
            continue;
        }
        let here: Vec<WorkflowUnit> = held
            .into_iter()
            .filter(|u| !deferred.contains(&(name.clone(), u.id.clone())))
            .collect();
        if !here.is_empty() {
            ready.push((name, here));
        }
    }
    ready
}

/// Nodes holding units, with how many. What a pass reports when it stops with
/// work that cannot move yet.
fn holding(
    w: &Workflow,
    units: &[WorkflowUnit],
    moves: &[(String, usize, bool)],
) -> Vec<(String, usize)> {
    let waiting = waiting(w, units, moves);
    crate::workflow::topological(w)
        .into_iter()
        .filter_map(|n| waiting.get(&n).map(|u| (n.clone(), u.len())))
        .filter(|(_, n)| *n > 0)
        .collect()
}

/// The units one agent run takes: one per unit, or one per group for a
/// `reduce`, which is what the worst case prices.
fn batches(node: &Node, units: &[WorkflowUnit]) -> Vec<Vec<WorkflowUnit>> {
    match &node.op {
        Op::Reduce(r) => {
            let mut groups: Vec<(String, Vec<WorkflowUnit>)> = Vec::new();
            for unit in units {
                let key = group_key(r, unit);
                match groups.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, bag)) => bag.push(unit.clone()),
                    None => groups.push((key, vec![unit.clone()])),
                }
            }
            groups.into_iter().map(|(_, bag)| bag).collect()
        }
        _ => units.iter().map(|u| vec![u.clone()]).collect(),
    }
}

/// The worker a plan shows for a node. An `edit` is routed at dispatch, where
/// its class and complexity are known, so under a policy it shows the route.
fn shown_worker(
    store: &Store,
    cfg: &Config,
    over: &Overrides,
    policy: &Option<Active>,
    node: &Node,
    unit: &WorkflowUnit,
) -> Result<(String, Option<String>)> {
    if matches!(node.op, Op::Edit(_)) && policy.as_ref().is_some_and(|p| !p.shadow) {
        let chosen = crate::dispatch::choose_worker(store, cfg, over, None)?;
        let flagged = over.agent.is_some() || over.model.is_some() || over.preset.is_some();
        return Ok(match flagged {
            true => (chosen.agent, chosen.model),
            false => ("by route".into(), None),
        });
    }
    let tier = tier_of(store, unit)?;
    Ok(match resolve(store, cfg, over, policy, node, unit, tier)? {
        Ok(r) => (r.chosen.agent, r.chosen.model),
        Err(_) => ("no route".into(), None),
    })
}

fn tier_of(store: &Store, unit: &WorkflowUnit) -> Result<Option<u8>> {
    Ok(match &unit.project {
        Some(p) => store.project(p)?.and_then(|r| r.tier),
        None => None,
    })
}

/// A worker and a model for one run of a node that edits nothing, and the
/// route that chose them. The flags win, then the route, then the settings,
/// exactly as a dispatch resolves. Under a policy, a node no route serves is
/// refused rather than sent to the settings.
struct Resolved {
    chosen: Chosen,
    route: Option<(i64, String, Approval)>,
}

fn resolve(
    store: &Store,
    cfg: &Config,
    over: &Overrides,
    policy: &Option<Active>,
    node: &Node,
    unit: &WorkflowUnit,
    tier: Option<u8>,
) -> Result<std::result::Result<Resolved, String>> {
    let mut applied = None;
    let mut route = None;
    if let Some(active) = policy {
        let Some(r) = active.policy.route_read(&node.name, unit.lap, tier) else {
            return Ok(Err(format!(
                "node `{}` matches no route in policy revision {}; add a route naming it",
                node.name, active.revision
            )));
        };
        route = Some((active.revision, r.name.clone(), r.approval));
        if !active.shadow {
            applied = Some((r.agent.clone(), r.model.clone()));
        }
    }
    let chosen = crate::dispatch::choose_worker(store, cfg, over, applied)?;
    Ok(Ok(Resolved { chosen, route }))
}

/// The agent runs the ready nodes would make, in dependency order, as many as
/// `batch_budget` admits at `agent_budget` each. What `--approve` names.
fn plan_of(
    store: &Store,
    cfg: &Config,
    over: &Overrides,
    policy: &Option<Active>,
    w: &Workflow,
    ready: &[(String, Vec<WorkflowUnit>)],
) -> Result<Plan> {
    let mut room = match cfg.agent_budget > 0.0 {
        true => (cfg.batch_budget / cfg.agent_budget).floor().max(0.0) as usize,
        false => usize::MAX,
    };
    let mut steps = Vec::new();
    for (name, units) in ready {
        let Some(node) = w.node(name) else { continue };
        if free(node) {
            continue;
        }
        if room == 0 {
            if steps.is_empty() {
                return Err(format!(
                    "batch_budget of ${:.2} admits no run at agent_budget of ${:.2}",
                    cfg.batch_budget, cfg.agent_budget
                )
                .into());
            }
            break;
        }
        let all = batches(node, units);
        let take = all.len().min(room);
        room -= take;
        let taken: Vec<WorkflowUnit> = all[..take].concat();
        let (agent, model) = shown_worker(store, cfg, over, policy, node, &taken[0])?;
        steps.push(Planned {
            node: name.clone(),
            op: node.op.name().to_string(),
            units: taken.iter().map(|u| u.id.clone()).collect(),
            agent: Some(agent),
            model,
            cost: take as f64 * cfg.agent_budget,
        });
    }
    Ok(Plan { steps })
}

/// The ready nodes of a bag, priced, whether or not the store holds it. What
/// a dry run reads. A node a rule decides is listed as free, and the agent
/// nodes form the plan a first pass would print.
pub fn plan_over(
    store: &Store,
    cfg: &Config,
    over: &Overrides,
    w: &Workflow,
    units: &[WorkflowUnit],
    moves: &[(String, usize, bool)],
) -> Result<(Vec<Planned>, Plan)> {
    let policy = active_policy(store)?;
    let ready = frontier(w, units, moves, &BTreeSet::new());
    let rules = ready
        .iter()
        .filter_map(|(name, units)| {
            let node = w.node(name)?;
            free(node).then(|| Planned {
                node: name.clone(),
                op: node.op.name().to_string(),
                units: units.iter().map(|u| u.id.clone()).collect(),
                agent: None,
                model: None,
                cost: 0.0,
            })
        })
        .collect();
    let plan = plan_of(store, cfg, over, &policy, w, &ready)?;
    Ok((rules, plan))
}

/// The frontier of a stored instance, priced, without running anything.
pub fn plan(
    store: &Store,
    cfg: &Config,
    over: &Overrides,
    w: &Workflow,
    instance: i64,
) -> Result<(Vec<Planned>, Plan)> {
    let units = store.workflow_units(instance)?;
    let moves = store.workflow_moves(instance)?;
    plan_over(store, cfg, over, w, &units, &moves)
}

/// One line per node, priced. What the developer approves.
pub fn describe(steps: &[Planned], lead: &str) -> String {
    if steps.is_empty() {
        return "nothing is runnable\n".to_string();
    }
    let rows: Vec<Vec<String>> = steps
        .iter()
        .map(|p| {
            vec![
                format!("{lead}{}", p.node),
                p.op.clone(),
                format!("{} unit(s)", p.units.len()),
                match (&p.agent, &p.model) {
                    (None, _) => "a rule, free".to_string(),
                    (Some(a), None) => a.clone(),
                    (Some(a), Some(m)) => format!("{a}/{m}"),
                },
                match p.free() {
                    true => "$0.00".to_string(),
                    false => format!("<= ${:.2}", p.cost),
                },
            ]
        })
        .collect();
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
/// has not handled. A node whose units have all been handled is done, which
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
    known: &Known,
) -> Result<()> {
    let data: Value = serde_json::from_str(&unit.data).unwrap_or(Value::Null);
    let system = system_fields(unit, known);
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

/// Where a unit goes when its node did not run cleanly for it: a default edge
/// alone, and only one that carries the unit's own type, else nowhere. An
/// unguarded edge is for units the node produced; taking it here would pass
/// on an unconfirmed claim, or a unit of the wrong type (section 10).
fn route_failed(ctx: &Ctx<'_>, node: &Node, unit: &WorkflowUnit, why: &str) -> Result<()> {
    let Ctx {
        store, w, instance, ..
    } = *ctx;
    store.add_workflow_verdict(instance, &unit.id, &node.name, "failed", Some(why))?;
    ctx.refused
        .borrow_mut()
        .push(format!("{}: {}: {why}", node.name, unit.id));
    let carries = w.output_of(node) == unit.ty;
    let mut taken = false;
    for (i, e) in w.edges.iter().enumerate() {
        if e.from != From::Node(node.name.clone()) {
            continue;
        }
        if e.default && carries {
            store.add_workflow_move(instance, &unit.id, i, true, Some("the node failed"))?;
            taken = true;
        } else {
            let reason = format!("the node failed: {why}");
            store.add_workflow_move(instance, &unit.id, i, false, Some(&reason))?;
        }
    }
    if !taken {
        let reason = format!("failed: {why}");
        store.add_workflow_move(
            instance,
            &unit.id,
            settled_at(w, &node.name),
            false,
            Some(&reason),
        )?;
    }
    Ok(())
}

/// The `@` fields a guard may read: the unit's own, every verdict written
/// against it, and `@run` once an `edit` has recorded one.
fn system_fields(unit: &WorkflowUnit, known: &Known) -> BTreeMap<String, Value> {
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
    if let Some(run) = known.runs.get(&unit.id) {
        out.insert("run".into(), Value::from(run.to_string()));
    }
    for ((u, check), verdict) in &known.verdicts {
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
/// to hold them or the approved figure bounds nothing.
fn mint(
    ctx: &Ctx<'_>,
    node: &Node,
    unit: WorkflowUnit,
    held: &mut i64,
    known: &Known,
) -> Result<()> {
    if *held >= ctx.w.caps.max_units {
        return Err(capped(
            &node.name,
            format!(
                "instance {} holds {held} units, its `caps.max_units`",
                ctx.instance
            ),
        ));
    }
    *held += 1;
    ctx.store.add_workflow_unit(ctx.instance, &unit)?;
    route_unit(ctx.store, ctx.instance, ctx.w, &node.name, &unit, known)
}

/// Stops the pass at a cap, naming the node and what it exceeded.
fn capped(node: &str, why: String) -> Box<dyn std::error::Error> {
    Box::new(Capped(format!(
        "node `{node}`: {why}. Nothing further was written; resume with a higher \
         cap, `--cap max_units=<n>` or `--cap max_edits=<n>`"
    )))
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

/// Runs every ready node a rule decides, then either reports the plan for the
/// agent nodes that are ready or, when `approve` names that plan, runs it.
/// An approval runs one plan: nodes that become ready after it are priced for
/// the next invocation, never run under an approval that did not name them.
pub fn advance(
    store: &Store,
    cfg: &Config,
    over: &Overrides,
    doc: &Document,
    w: &Workflow,
    instance: i64,
    given: &Invocation<'_>,
) -> Result<Outcome> {
    let result = advance_inner(store, cfg, over, doc, w, instance, given);
    if let Err(e) = &result
        && e.downcast_ref::<Capped>().is_some()
    {
        store.note_instance_outcome(instance, "capped")?;
    }
    result
}

fn advance_inner(
    store: &Store,
    cfg: &Config,
    over: &Overrides,
    doc: &Document,
    w: &Workflow,
    instance: i64,
    given: &Invocation<'_>,
) -> Result<Outcome> {
    let ctx = Ctx {
        store,
        cfg,
        doc,
        w,
        instance,
        home: given.home,
        over,
        params: w.arguments(given.args),
        policy: active_policy(store)?,
        refused: RefCell::new(Vec::new()),
        deferred: RefCell::new(BTreeSet::new()),
        bases: RefCell::new(BTreeMap::new()),
    };
    let mut approve = given.approve.map(String::from);
    let mut ran = false;
    // One iteration per node run. A graph's worst case bounds the units, so a
    // pass that exceeds this is a bug rather than a long job.
    let ceiling = (w.caps.max_units.max(1) as usize + 1) * (w.nodes.len() + 1);
    for _ in 0..ceiling {
        let units = store.workflow_units(instance)?;
        let moves = store.workflow_moves(instance)?;
        let known = Known::load(store, instance)?;
        let ready = frontier(w, &units, &moves, &ctx.deferred.borrow());

        // Free nodes first, every time: a rule costs nothing and needs no
        // approval.
        if let Some((name, waiting)) = ready.iter().find(|(n, _)| w.node(n).is_some_and(free)) {
            let node = w.node(name).expect("found above");
            run_free(&ctx, node, waiting, &known)?;
            continue;
        }
        let plan = plan_of(store, cfg, over, &ctx.policy, w, &ready)?;
        let id = plan.id();
        match approve.take() {
            Some(given) if !plan.is_empty() && given == id => {
                for step in &plan.steps {
                    let node = w.node(&step.node).expect("planned from the graph");
                    let waiting = &ready
                        .iter()
                        .find(|(n, _)| *n == step.node)
                        .expect("planned from the frontier")
                        .1;
                    let units: Vec<WorkflowUnit> = waiting
                        .iter()
                        .filter(|u| step.units.contains(&u.id))
                        .cloned()
                        .collect();
                    match node.op {
                        Op::Edit(_) => run_edit(&ctx, node, &units, &known)?,
                        _ => run_agent(&ctx, node, &units, &known)?,
                    }
                }
                ran = true;
                continue;
            }
            Some(given) if plan.is_empty() => {
                return Err(format!("plan {given} is not ready: no agent node is").into());
            }
            Some(given) => {
                return Err(format!(
                    "plan {given} is not the plan for this pass, which is {id}:\n{}\
                     Nothing was spent.",
                    describe(&plan.steps, "")
                )
                .into());
            }
            None => {
                return Ok(Outcome {
                    plan,
                    waiting: holding(w, &units, &moves),
                    refused: ctx.refused.take(),
                    ran,
                });
            }
        }
    }
    Err(format!(
        "workflow `{}` ran {ceiling} nodes without settling, which is a bug in the pass",
        w.name
    )
    .into())
}

/// A node a rule decides. It reads what is already recorded, writes units or a
/// verdict or a file `pma` owns, and costs nothing. A unit the rule cannot
/// take is refused on its own; the pass goes on.
fn run_free(ctx: &Ctx<'_>, node: &Node, waiting: &[WorkflowUnit], known: &Known) -> Result<()> {
    let Ctx {
        store,
        doc,
        w,
        instance,
        ..
    } = *ctx;
    let mut known = known.clone();
    match &node.op {
        Op::Map(m) => {
            let rule = m.rule.as_deref().unwrap_or_default();
            let all = store.workflow_units(instance)?;
            let mut held = all.len() as i64;
            let mut minted = 0;
            let mut seen = unique_seen(ctx, node, &m.emits)?;
            for unit in waiting {
                let produced = match rule_map(store, doc, rule, &m.emits, m.out, unit) {
                    Ok(p) => p,
                    Err(why) => {
                        store.atomically(|| route_failed(ctx, node, unit, &why.to_string()))?;
                        continue;
                    }
                };
                if let Some(width) = width(w, node)
                    && produced.len() as i64 > width
                {
                    return Err(capped(
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
                        if let Some(why) = duplicate(ctx, &m.emits, &data, unit, &mut seen)? {
                            ctx.refused
                                .borrow_mut()
                                .push(format!("{}: {}: dropped: {why}", node.name, unit.id));
                            continue;
                        }
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
                            &known,
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
            for bag in batches(node, waiting) {
                let bag: Vec<&WorkflowUnit> = bag.iter().collect();
                let (kept, dropped) = rule_reduce(rule, &bag)?;
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
                            &known,
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
                let (verdict, detail) = match run_check(ctx, rule, unit)? {
                    Checked::NotReady(_) => {
                        ctx.deferred
                            .borrow_mut()
                            .insert((node.name.clone(), unit.id.clone()));
                        continue;
                    }
                    Checked::Verdict(v, d) => (v, d),
                };
                known
                    .verdicts
                    .insert((unit.id.clone(), rule.clone()), verdict.clone());
                store.atomically(|| {
                    store.add_workflow_verdict(
                        instance,
                        &unit.id,
                        rule,
                        &verdict,
                        detail.as_deref(),
                    )?;
                    route_unit(store, instance, w, &node.name, unit, &known)?;
                    mark_handled(store, instance, w, &node.name, &unit.id)
                })?;
            }
        }
        Op::Emit(e) => {
            for unit in waiting {
                let data: Value = serde_json::from_str(&unit.data).unwrap_or(Value::Null);
                // The file and the moves are written together: a refusal
                // writes nothing to the file, and a unit already written is
                // found there on a resumed pass rather than written twice.
                store.atomically(|| match emit(ctx, e, unit, &data, &known)? {
                    Ok(()) => {
                        route_unit(store, instance, w, &node.name, unit, &known)?;
                        mark_handled(store, instance, w, &node.name, &unit.id)
                    }
                    Err(why) => route_failed(ctx, node, unit, &why),
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

/// The rules a `map` may name. Each reads what `pma` already knows. An error
/// is the reason the unit is refused.
fn rule_map(
    store: &Store,
    doc: &Document,
    rule: &str,
    emits: &str,
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
        // A run that is final no longer holds its task or its
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
        // `gh`'s fields under the names the built-in `item` uses, projected
        // onto the type the node emits and checked against it.
        "open-issues" => {
            let row = row()?;
            let mut cmd = std::process::Command::new("gh");
            cmd.args(["issue", "list", "--state", "open", "--limit", "100"])
                .args(["--json", "number,title,body,labels"])
                .current_dir(&row.path);
            let listed = crate::scan::call(cmd, crate::scan::CALL_TIMEOUT)
                .map_err(|e| format!("gh issue list: {e}"))?;
            let listed: Value =
                serde_json::from_str(&listed).map_err(|e| format!("gh issue list: {e}"))?;
            let ty = doc
                .resolve(emits)
                .ok_or_else(|| format!("unknown type `{emits}`"))?;
            listed
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|v| {
                    let issue = serde_json::json!({
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
                    });
                    // An empty value, such as an issue with no labels, is an
                    // absent field rather than a `line` of no characters.
                    let projected: serde_json::Map<String, Value> = ty
                        .fields
                        .keys()
                        .filter_map(|f| Some((f.clone(), issue.get(f)?.clone())))
                        .filter(|(_, v)| v.as_str() != Some(""))
                        .collect();
                    let projected = Value::Object(projected);
                    if !crate::workflow::BUILTIN_TYPES.contains(&emits) {
                        ty.check(&projected)
                            .map_err(|e| format!("issue {}: {e}", issue["gh"]))?;
                    }
                    Ok(projected)
                })
                .collect()
        }
        other => Err(format!("rule `{other}` is not built yet").into()),
    }
}

/// The values of every `unique: "normalised"` field already in the node's
/// bag, keyed by field, so a repeat is dropped rather than minted.
fn unique_seen(ctx: &Ctx<'_>, node: &Node, emits: &str) -> Result<BTreeSet<String>> {
    let Some(ty) = ctx.doc.resolve(emits) else {
        return Ok(BTreeSet::new());
    };
    let mut seen = BTreeSet::new();
    for u in ctx.store.workflow_units(ctx.instance)? {
        if u.node != node.name {
            continue;
        }
        let data: Value = serde_json::from_str(&u.data).unwrap_or(Value::Null);
        for key in unique_keys(&ty, &data) {
            seen.insert(key);
        }
    }
    Ok(seen)
}

fn unique_keys(ty: &Type, data: &Value) -> Vec<String> {
    ty.fields
        .iter()
        .filter(|(_, f)| matches!(f.ty, FieldType::Line { unique: true, .. }))
        .filter_map(|(name, _)| {
            let v = data.get(name)?.as_str()?;
            Some(format!("{name}\u{0}{}", todo::normal_text(v)))
        })
        .collect()
}

/// Why a produced unit repeats one already in the bag or an open item of its
/// project, by a `unique: "normalised"` field. `None` when it does not.
fn duplicate(
    ctx: &Ctx<'_>,
    emits: &str,
    data: &Value,
    source: &WorkflowUnit,
    seen: &mut BTreeSet<String>,
) -> Result<Option<String>> {
    let Some(ty) = ctx.doc.resolve(emits) else {
        return Ok(None);
    };
    let keys = unique_keys(&ty, data);
    if keys.is_empty() {
        return Ok(None);
    }
    let open: Vec<String> = match &source.project {
        Some(p) => ctx
            .store
            .tasks()?
            .into_iter()
            .filter(|t| &t.project == p)
            .map(|t| todo::normal_text(&t.text))
            .collect(),
        None => Vec::new(),
    };
    for key in &keys {
        let (field, value) = key.split_once('\u{0}').expect("built above");
        if open.iter().any(|t| t == value) {
            return Ok(Some(format!("`{field}` repeats an open item: {value}")));
        }
        if seen.contains(key) {
            return Ok(Some(format!(
                "`{field}` repeats a unit already in the bag: {value}"
            )));
        }
    }
    seen.extend(keys);
    Ok(None)
}

/// A check's result for one unit: a verdict, or not ready yet, which leaves
/// the unit waiting at the check for a later pass.
enum Checked {
    Verdict(String, Option<String>),
    NotReady(String),
}

fn verdict(v: &str, detail: Option<String>) -> Checked {
    Checked::Verdict(v.to_string(), detail)
}

/// A check writes `passed`, `failed` or `unknown`. An unknown is a result: a
/// check that could not run is not evidence either way.
fn run_check(ctx: &Ctx<'_>, rule: &str, unit: &WorkflowUnit) -> Result<Checked> {
    let store = ctx.store;
    match rule {
        "lint-todo" => {
            let Some(project) = &unit.project else {
                return Ok(verdict("unknown", Some("the unit names no project".into())));
            };
            let Some(row) = store.project(project)? else {
                return Ok(verdict("unknown", Some(format!("no project `{project}`"))));
            };
            let Ok(text) = std::fs::read_to_string(row.path.join("TODO.md")) else {
                return Ok(verdict("unknown", Some("no TODO.md".into())));
            };
            Ok(match todo::parse(&text).has_errors() {
                true => verdict("failed", Some("TODO.md has lint errors".into())),
                false => verdict("passed", None),
            })
        }
        "nonempty" => Ok(verdict("unknown", Some("`nonempty` is not built".into()))),
        // The rest read a run. A unit that has none has no evidence either
        // way, which is what `unknown` says.
        other => match run_of(ctx, unit)? {
            None => Ok(verdict(
                "unknown",
                Some(format!("`{other}` reads a run, and this unit has none")),
            )),
            Some(run) => Ok(verdict_of(&run, other)),
        },
    }
}

/// The run behind a unit: the latest an `edit` recorded against it, so a lap
/// reads its own attempt, or the run a `run` unit names by its `id`. This is
/// what `@run` names.
fn run_of(ctx: &Ctx<'_>, unit: &WorkflowUnit) -> Result<Option<crate::store::Run>> {
    let runs = ctx.store.runs()?;
    if let Some(run) = runs
        .iter()
        .rev()
        .find(|r| r.workflow_instance == Some(ctx.instance) && r.unit.as_deref() == Some(&unit.id))
    {
        return Ok(Some(run.clone()));
    }
    if unit.ty != "run" {
        return Ok(None);
    }
    let data: Value = serde_json::from_str(&unit.data).unwrap_or(Value::Null);
    let id = data
        .get("id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<i64>().ok());
    Ok(id.and_then(|id| runs.into_iter().find(|r| r.id == id)))
}

/// A check about a run. `ci-green` and `pr-merged` wait while the run is on
/// its way: not yet published, or a pull request still open or still
/// checking. They read the pull request by the URL `pma pr` recorded, since the
/// worktree is gone by then.
fn verdict_of(run: &crate::store::Run, rule: &str) -> Checked {
    use crate::store::RunState;
    let ok = |b: bool| verdict(if b { "passed" } else { "failed" }, None);
    let pr = run
        .outcome
        .as_deref()
        .filter(|o| o.contains("/pull/"))
        .map(str::to_string);
    let unpublished = || match run.state {
        RunState::Rejected => verdict("failed", Some("the run was rejected".into())),
        RunState::Failed => verdict("failed", Some("the run failed".into())),
        _ => Checked::NotReady("the run is not published yet".into()),
    };
    match rule {
        "verify" => match (&run.verify, run.verify_ok) {
            (None, _) => verdict(
                "unknown",
                Some(format!("`{}` states no verify command", run.project)),
            ),
            (Some(_), None) => verdict(
                "unknown",
                Some(
                    run.error
                        .clone()
                        .unwrap_or_else(|| "verify did not run".into()),
                ),
            ),
            (Some(_), Some(true)) => ok(true),
            (Some(_), Some(false)) => verdict("failed", Some("verify failed".into())),
        },
        "scope-clean" => {
            let reasons = crate::accept::review_reasons(run);
            let scope: Vec<&String> = reasons
                .iter()
                .filter(|r| r.contains("outside") || r.contains("scope"))
                .collect();
            match (&run.changed_paths, scope.as_slice()) {
                (None, _) => verdict(
                    "unknown",
                    Some(
                        run.scope_error
                            .clone()
                            .unwrap_or_else(|| "the changed paths are not known".into()),
                    ),
                ),
                (Some(_), []) => ok(true),
                (Some(_), why) => verdict(
                    "failed",
                    Some(
                        why.iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join("; "),
                    ),
                ),
            }
        }
        "pr-merged" => match (&pr, run.state) {
            // Settled already: no need to ask GitHub again.
            (_, RunState::Merged) => ok(true),
            (_, RunState::Closed) => verdict(
                "failed",
                Some("the pull request was closed without merging".into()),
            ),
            // Pushed to the default branch: the change landed with no pull
            // request to merge.
            (None, RunState::Pushed) => ok(true),
            (None, _) => unpublished(),
            (Some(url), _) => match gh_json(&["pr", "view", url, "--json", "state"]) {
                Err(e) => verdict("unknown", Some(e)),
                Ok(v) => match v.get("state").and_then(Value::as_str) {
                    Some("MERGED") => ok(true),
                    Some("CLOSED") => verdict(
                        "failed",
                        Some("the pull request was closed without merging".into()),
                    ),
                    Some(other) => Checked::NotReady(format!("the pull request is {other}")),
                    None => verdict("unknown", Some("gh reported no state".into())),
                },
            },
        },
        "ci-green" => match (&pr, run.state) {
            (None, RunState::Pushed) => verdict(
                "unknown",
                Some("pushed without a pull request, so no checks are read".into()),
            ),
            (None, _) => unpublished(),
            (Some(url), _) => match gh_json(&["pr", "view", url, "--json", "statusCheckRollup"]) {
                Err(e) => verdict("unknown", Some(e)),
                Ok(v) => {
                    let checks = v
                        .get("statusCheckRollup")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let decisive = |c: &Value| -> Option<bool> {
                        let state = c
                            .get("conclusion")
                            .filter(|s| s.as_str().is_some_and(|s| !s.is_empty()))
                            .or_else(|| c.get("state"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_ascii_uppercase();
                        match state.as_str() {
                            "SUCCESS" | "NEUTRAL" | "SKIPPED" => Some(true),
                            "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE" | "ERROR" | "CANCELLED"
                            | "ACTION_REQUIRED" => Some(false),
                            _ => None,
                        }
                    };
                    let verdicts: Vec<Option<bool>> = checks.iter().map(decisive).collect();
                    if verdicts.contains(&Some(false)) {
                        verdict("failed", Some("a check failed".into()))
                    } else if verdicts.iter().any(Option::is_none) {
                        Checked::NotReady("a check has not finished".into())
                    } else if verdicts.is_empty() {
                        verdict("unknown", Some("no checks are reported".into()))
                    } else {
                        ok(true)
                    }
                }
            },
        },
        other => verdict("unknown", Some(format!("check `{other}` is not built yet"))),
    }
}

/// `gh`, bounded like a scan's calls.
fn gh_json(args: &[&str]) -> std::result::Result<Value, String> {
    let mut cmd = std::process::Command::new("gh");
    cmd.args(args);
    let out = crate::scan::call(cmd, crate::scan::CALL_TIMEOUT).map_err(|e| e.to_string())?;
    serde_json::from_str(&out).map_err(|e| format!("gh: {e}"))
}

/// Writes a unit where `pma` owns the file. A `todo` edit is uncommitted in the
/// user's clone, as `pma sync` writes one: `scripts/commit_todo.py` commits it.
/// The inner error refuses this unit alone.
fn emit(
    ctx: &Ctx<'_>,
    e: &crate::workflow::EmitNode,
    unit: &WorkflowUnit,
    data: &Value,
    known: &Known,
) -> Result<std::result::Result<(), String>> {
    let store = ctx.store;
    let system = system_fields(unit, known);
    let fill = |template: &str| -> String { fill(template, data, &system, &ctx.params, &[]) };
    let mapped = |field: &str| e.map.get(field).map(|t| fill(t));
    match e.sink {
        Sink::Note => {
            let text = mapped("text").unwrap_or_default();
            if text.trim().is_empty() {
                return Ok(Err("the note's text is empty".into()));
            }
            store.add_note(&text, crate::dates::now())?;
            Ok(Ok(()))
        }
        Sink::Todo => {
            let Some(project) = &unit.project else {
                return Ok(Err("a todo sink needs a unit that names a project".into()));
            };
            let Some(row) = store.project(project)? else {
                return Ok(Err(format!("no project `{project}`")));
            };
            let file = row.path.join("TODO.md");
            let text = match std::fs::read_to_string(&file) {
                Ok(t) => t,
                Err(e) => return Ok(Err(format!("{}: {e}", file.display()))),
            };
            let parsed = todo::parse(&text);
            if parsed.has_errors() {
                return Ok(Err(format!(
                    "{}: lint errors, so an item cannot be identified; `pma lint` first",
                    file.display()
                )));
            }
            let item = mapped("text").unwrap_or_default();
            if item.trim().is_empty() || item.contains('\n') {
                return Ok(Err(format!("an item is one non-empty line, not `{item}`")));
            }
            let key = todo::normal_text(&item);
            let updated = match e.action {
                Action::Add => {
                    let Some(priority) =
                        mapped("priority").and_then(|p| todo::Priority::parse(&p.to_lowercase()))
                    else {
                        return Ok(Err(
                            "the priority is not critical, high, medium or low".into()
                        ));
                    };
                    // Already open: a resumed pass finds its own write, and a
                    // second copy would be a duplicate the linter refuses.
                    if parsed
                        .items
                        .iter()
                        .any(|i| !i.done && todo::normal_text(&i.text) == key)
                    {
                        return Ok(Ok(()));
                    }
                    let description = mapped("description");
                    // `pma` writes the whole line, so it gives the item an id
                    // at once: rewording it later keeps its age.
                    let line = format!("{item} ^{}", todo::new_id(&text));
                    match todo::insert(&text, priority, &line, description.as_deref()) {
                        Some(t) => t,
                        None => {
                            return Ok(Err(format!(
                                "{} has no `## {}` section",
                                file.display(),
                                capitalised(priority.name())
                            )));
                        }
                    }
                }
                Action::Tick => match todo::mark_done(&text, &key, &item) {
                    Some(t) => t,
                    None => return Ok(Err(format!("no open item `{item}`"))),
                },
                Action::Remove => match todo::remove_done(&text, &key, &item) {
                    Some(t) => t,
                    None => return Ok(Err(format!("no finished item `{item}`"))),
                },
            };
            todo::save(&file, &updated).map_err(|e| format!("{}: {e}", file.display()))?;
            Ok(Ok(()))
        }
        Sink::Doc => Ok(Err("the `doc` sink is not built".into())),
    }
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
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

/// A node a model decides, over the units a plan named. One run per unit for a
/// `map`, one per group for a `reduce`.
///
/// The model reads `in.json` and writes `out.json` under the instance's
/// artifact directory, and it works in a detached worktree rather than in the
/// user's clone.
///
/// Runs go `max_parallel` at a time. The store is a single connection, so it
/// stays on this thread: a worker is handed everything it needs as files and
/// paths, and what comes back is recorded here, in the order it arrives.
fn run_agent(ctx: &Ctx<'_>, node: &Node, units: &[WorkflowUnit], known: &Known) -> Result<()> {
    let mut staged = Vec::new();
    for batch in batches(node, units) {
        match stage(ctx, node, &batch, known)? {
            Ok(job) => staged.push(job),
            Err(why) => {
                for unit in &batch {
                    ctx.store
                        .atomically(|| route_failed(ctx, node, unit, &why))?;
                }
            }
        }
    }
    if staged.is_empty() {
        return Ok(());
    }
    let queue = std::sync::Mutex::new(std::collections::VecDeque::from(staged));
    let (tx, rx) = std::sync::mpsc::channel::<(Staged, Ran)>();
    let mut held = ctx.store.workflow_units(ctx.instance)?.len() as i64;
    let mut failure = None;
    let cfg = ctx.cfg;
    std::thread::scope(|scope| {
        for _ in 0..(cfg.max_parallel as usize).max(1) {
            let tx = tx.clone();
            let queue = &queue;
            scope.spawn(move || {
                loop {
                    let Some(job) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    let ran = work(cfg, &job);
                    let _ = tx.send((job, ran));
                }
            });
        }
        drop(tx);
        for (job, ran) in rx {
            if let Err(e) = record(ctx, node, job, ran, &mut held, known)
                && failure.is_none()
            {
                failure = Some(e);
            }
        }
    });
    match failure {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// One agent run, staged: everything the store and the repository had to say
/// about it, so a worker thread needs nothing but the filesystem.
struct Staged {
    run: crate::store::Run,
    worker: crate::worker::Worker,
    chosen: Chosen,
    batch: Vec<WorkflowUnit>,
    repo: std::path::PathBuf,
    tree: std::path::PathBuf,
    out: std::path::PathBuf,
    doc: Option<std::path::PathBuf>,
    /// Where the run's files are kept once its tree is removed.
    artifacts: std::path::PathBuf,
    log: std::path::PathBuf,
    /// A directory outside the worktree for `agent::restrict`. Inside the
    /// tree the agent could write credentials or hooks into it.
    agent_env: std::path::PathBuf,
}

/// What a worker thread produced: no store, no repository, just the run's own
/// numbers and the units it wrote.
struct Ran {
    seconds: i64,
    cost_usd: Option<f64>,
    summary: String,
    produced: std::result::Result<Vec<Value>, String>,
}

/// The commit a read node works from. In a workflow that edits, the head of
/// the fetched default branch, which is where its `edit` nodes start, so a
/// finding names code the fix will see. Otherwise the clone's `HEAD`, which
/// lets a review read local commits. Fetched once per project per pass.
fn read_base(ctx: &Ctx<'_>, repo: &Path, project: &str) -> std::result::Result<String, String> {
    if !ctx.w.effects.repo {
        return Ok("HEAD".into());
    }
    if let Some(found) = ctx.bases.borrow().get(project) {
        return found.clone();
    }
    let found = (|| {
        crate::dispatch::git(repo, &["fetch", "--quiet", "origin"]).map_err(|e| e.to_string())?;
        let branch = crate::scan::default_branch(repo)
            .ok_or_else(|| crate::scan::DEFAULT_BRANCH_UNKNOWN.to_string())?;
        crate::dispatch::git(
            repo,
            &["rev-parse", &format!("refs/remotes/origin/{branch}")],
        )
        .map(|s| s.trim().to_string())
        .map_err(|e| e.to_string())
    })();
    ctx.bases
        .borrow_mut()
        .insert(project.to_string(), found.clone());
    found
}

/// The output a step accepts, stated from the type it declares and the op
/// it applies, and appended to its prompt. `accept` checks exactly this, so
/// an agent is told the limits it will be held to rather than a prompt
/// author restating them by hand.
fn contract(doc: &Document, w: &Workflow, node: &Node, input: &Path, out: &Path) -> Option<String> {
    let (emits, shape, writes) = match &node.op {
        Op::Map(m) => (m.emits.as_str(), Some(m.out), m.writes.as_slice()),
        Op::Reduce(r) => (r.emits.as_str(), None, &[][..]),
        _ => return None,
    };
    let ty = doc.resolve(emits)?;
    let (input, out) = (input.display(), out.display());
    let mut text = String::from("Output, which pma checks before it accepts anything:\n");
    text.push_str(&match shape {
        Some(Out::Grows) => format!(
            "- Put in {out} a JSON array of new objects, at most {}. Put [] there if there are none.\n",
            width(w, node).unwrap_or(0)
        ),
        Some(Out::Same) => format!(
            "- Put in {out} a JSON array holding every object of {input}, each with its `@id`.\n"
        ),
        Some(Out::Shrinks) => format!(
            "- Put in {out} a JSON array holding the objects of {input} you keep, each with its \
             `@id`. Leave out the ones you drop; put [] there to drop all.\n"
        ),
        None => format!(
            "- Put in {out} a JSON array of at most one object, with `@from` listing the `@id` of \
             every object of {input} it comes from.\n"
        ),
    });
    let fields: Vec<(&String, &crate::workflow::Field)> = match shape {
        // A kept or annotated object comes back as it was given, but for the
        // fields the step may set.
        Some(Out::Same) | Some(Out::Shrinks) => {
            if writes.is_empty() {
                text.push_str("- Change no field.\n");
                return Some(text);
            }
            text.push_str("- Set only these fields; any other change is refused:\n");
            ty.fields
                .iter()
                .filter(|(n, _)| writes.contains(n))
                .collect()
        }
        _ => {
            text.push_str("- Each object has these fields and no others:\n");
            ty.fields.iter().collect()
        }
    };
    for (name, field) in fields {
        let required = match field.required {
            true => ", required",
            false => "",
        };
        text.push_str(&format!(
            "  - `{name}`{required}: {}\n",
            field.ty.describe()
        ));
    }
    Some(text)
}

/// Everything a run needs before a thread can take it: the worker the route
/// chose, the files it reads and writes, the worktree it works in, and the
/// `runs` row that records it. The inner error refuses the batch.
fn stage(
    ctx: &Ctx<'_>,
    node: &Node,
    batch: &[WorkflowUnit],
    known: &Known,
) -> Result<std::result::Result<Staged, String>> {
    let lead = batch.first().expect("a batch holds at least one unit");
    let Some(project) = lead.project.clone() else {
        return Ok(Err(format!("unit `{}` names no project", lead.id)));
    };
    let Some(row) = ctx.store.project(&project)? else {
        return Ok(Err(format!("no project `{project}`; `pma scan` first")));
    };
    let resolved = match resolve(
        ctx.store,
        ctx.cfg,
        ctx.over,
        &ctx.policy,
        node,
        lead,
        row.tier,
    )? {
        Ok(r) => r,
        Err(why) => return Ok(Err(why)),
    };
    let Some(worker) = ctx
        .store
        .agents()?
        .into_iter()
        .find(|w| w.name == resolved.chosen.agent)
    else {
        return Ok(Err(format!(
            "unknown agent `{}`; see `pma agent`",
            resolved.chosen.agent
        )));
    };
    let base = match read_base(ctx, &row.path, &project) {
        Ok(b) => b,
        Err(why) => return Ok(Err(format!("{project}: {why}"))),
    };

    let dir = artifacts(ctx, node)?;
    // A read-only node still gets a tree of its own. An agent that writes to
    // the clone would put a change past every review this tool has. The trees
    // are added from this thread: two `git worktree add` in one repository
    // contend for its index lock.
    let tree = ctx
        .home
        .join("workflow-trees")
        .join(ctx.instance.to_string())
        .join(node.name.replace('/', "-"))
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
            &base,
        ],
    )?;

    // The files a run reads and writes sit inside its tree, where any worker
    // may write: an agent allowed to edit its working directory is not always
    // allowed to write outside it. `record` copies them to `dir` before the
    // tree goes, so each run's files outlive it.
    let io = tree.join(".pma");
    std::fs::create_dir_all(&io).map_err(|e| format!("{}: {e}", io.display()))?;
    let input: Vec<Value> = batch.iter().map(|u| exported(u, known)).collect();
    let in_file = io.join("in.json");
    let out_file = io.join("out.json");
    let doc_file = node_doc(ctx, node).map(|name| io.join(name));
    write(&in_file, &serde_json::to_string_pretty(&input)?)?;

    let data: Value = serde_json::from_str(&lead.data).unwrap_or(Value::Null);
    let task = task_of(node).ok_or_else(|| format!("node `{}` states no task", node.name))?;
    let prompt = fill(
        task,
        &data,
        &system_fields(lead, known),
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
    let prompt = match contract(ctx.doc, ctx.w, node, &in_file, &out_file) {
        Some(c) => format!("{prompt}\n\n{c}"),
        None => prompt,
    };

    let chosen = resolved.chosen;
    let (route_revision, route, approval) = match resolved.route {
        Some((rev, name, approval)) => (Some(rev), Some(name), Some(approval)),
        None => (None, None, None),
    };
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
        route_revision,
        route,
        approval,
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
    let agent_env = ctx.home.join("agent-env");
    Ok(Ok(Staged {
        run,
        worker,
        chosen,
        batch: batch.to_vec(),
        repo: row.path,
        tree,
        out: out_file,
        doc: doc_file,
        artifacts: dir,
        log,
        agent_env,
    }))
}

/// The agent itself. No store and no `&Ctx`: this is the half that runs on a
/// worker thread.
fn work(cfg: &Config, job: &Staged) -> Ran {
    let timeout = std::time::Duration::from_secs(cfg.timeout as u64 * 60);
    let mut cmd = job.worker.build(
        &job.run.prompt,
        &job.tree,
        job.chosen.model.as_deref(),
        cfg.agent_budget,
        &job.chosen.args,
        timeout.saturating_sub(crate::dispatch::INNER_GRACE),
    );
    cmd.current_dir(&job.tree);
    let finished = match crate::agent::restrict(&mut cmd, &job.agent_env)
        .and_then(|()| crate::agent::run_limited(cmd, &job.log, timeout))
    {
        Ok(f) => f,
        Err(e) => {
            return Ran {
                seconds: 0,
                cost_usd: None,
                summary: String::new(),
                produced: Err(format!("{}: {e}", job.worker.command)),
            };
        }
    };
    let report = job.worker.parse.report(
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
/// worktree it no longer needs.
///
/// The row is written first, so its cost is recorded whatever follows. The
/// units and the moves that settle the batch are one transaction: a pass
/// killed part way leaves the batch waiting, not half minted, and a cap hit
/// part way rolls back what the batch had minted.
fn record(
    ctx: &Ctx<'_>,
    node: &Node,
    job: Staged,
    ran: Ran,
    held: &mut i64,
    known: &Known,
) -> Result<()> {
    let Staged {
        mut run,
        batch,
        repo,
        tree,
        artifacts,
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
    keep_files(&tree.join(".pma"), &artifacts);
    let _ = crate::dispatch::remove_worktree(&repo, &tree, "");
    let before = *held;
    let recorded = ctx.store.atomically(|| match ran.produced {
        Ok(units) => accept(ctx, node, &batch, units, held, known),
        Err(why) => {
            for unit in &batch {
                route_failed(ctx, node, unit, &why)?;
            }
            Ok(())
        }
    });
    if recorded.is_err() {
        *held = before;
    }
    recorded
}

fn group_key(r: &crate::workflow::ReduceNode, unit: &WorkflowUnit) -> String {
    let data: Value = serde_json::from_str(&unit.data).unwrap_or(Value::Null);
    r.group_by
        .iter()
        .map(|f| todo::normal_text(data.get(f).and_then(Value::as_str).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("\u{0}")
}

/// Copies what a run read and wrote out of its tree before the tree goes.
/// A file that will not copy is left: the run's row already says what it did.
fn keep_files(io: &Path, artifacts: &Path) {
    let Ok(entries) = std::fs::read_dir(io) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let _ = std::fs::copy(&path, artifacts.join(entry.file_name()));
        }
    }
}

/// `out.json` as a list of units, or why it could not be read. A run that
/// wrote no file failed: an agent that found nothing writes `[]`, and one that
/// could not write at all must not read as one that found nothing.
fn read_out(file: &Path) -> std::result::Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| format!("the agent wrote no {}: {e}", file.display()))?;
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
///
/// An annotation or a filter returns the units it was given. What is minted is
/// the unit it was given with only the `writes` fields taken from the model,
/// so a model can neither rewrite nor drop a field it was not asked for (W17).
/// A `reduce` names the units each output came from in `@from`, and writes at
/// most one unit per group.
fn accept(
    ctx: &Ctx<'_>,
    node: &Node,
    batch: &[WorkflowUnit],
    produced: Vec<Value>,
    held: &mut i64,
    known: &Known,
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
    if out.is_none() && produced.len() > 1 {
        reasons.push(format!(
            "a reduce writes at most one unit per group, and returned {}",
            produced.len()
        ));
    }

    // Each output with the unit it descends from and the data to mint.
    let mut minted: Vec<(&WorkflowUnit, Value)> = Vec::new();
    let mut returned_ids = BTreeSet::new();
    for (i, v) in produced.iter().enumerate() {
        let n = i + 1;
        let (source, data) = match out {
            Some(Out::Same) | Some(Out::Shrinks) => {
                let Some(source) = kept_id(v).and_then(|id| batch.iter().find(|u| u.id == id))
                else {
                    reasons.push(format!("unit {n} names no `@id` it was given"));
                    continue;
                };
                if !returned_ids.insert(source.id.clone()) {
                    reasons.push(format!("unit {n} repeats `@id` {}", source.id));
                    continue;
                }
                if let Err(e) = unchanged(source, v, &writes) {
                    reasons.push(format!("unit {n}: {e}"));
                    continue;
                }
                (source, merged(source, v, &writes))
            }
            Some(Out::Grows) => (&batch[0], stripped(v)),
            None => {
                let from: Vec<&str> = v
                    .get("@from")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let sources: Vec<&WorkflowUnit> = from
                    .iter()
                    .filter_map(|id| batch.iter().find(|u| u.id == *id))
                    .collect();
                if from.is_empty() || sources.len() != from.len() {
                    reasons.push(format!(
                        "unit {n}: `@from` must list the ids of the units it came from"
                    ));
                    continue;
                }
                (sources[0], stripped(v))
            }
        };
        if let Err(e) = ty.check(&data) {
            reasons.push(format!("unit {n}: {e}"));
            continue;
        }
        minted.push((source, data));
    }
    if !reasons.is_empty() {
        let why = reasons.join("; ");
        for unit in batch {
            route_failed(ctx, node, unit, &why)?;
        }
        return Ok(());
    }

    let all = ctx.store.workflow_units(ctx.instance)?;
    let mut seen = unique_seen(ctx, node, &emits)?;
    let mut n = 0;
    for (source, data) in minted {
        if let Some(why) = duplicate(ctx, &emits, &data, source, &mut seen)? {
            ctx.refused
                .borrow_mut()
                .push(format!("{}: {}: dropped: {why}", node.name, source.id));
            continue;
        }
        mint(
            ctx,
            node,
            WorkflowUnit {
                id: next_id(&all, &node.name, n),
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
            known,
        )?;
        n += 1;
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

/// A returned unit without the `@` fields, which are `pma`'s.
fn stripped(v: &Value) -> Value {
    let mut data = v.clone();
    if let Value::Object(map) = &mut data {
        map.retain(|k, _| !k.starts_with('@'));
    }
    data
}

/// The unit a node was given, with the fields in `writes` taken from what the
/// model returned.
fn merged(source: &WorkflowUnit, returned: &Value, writes: &[String]) -> Value {
    let mut data = match serde_json::from_str::<Value>(&source.data) {
        Ok(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    for field in writes {
        match returned.get(field) {
            Some(v) if !v.is_null() => {
                data.insert(field.clone(), v.clone());
            }
            _ => {
                data.remove(field);
            }
        }
    }
    Value::Object(data)
}

/// Whether a returned unit changed only what the node declared it may write.
/// A field left out is not a change: the unit is rebuilt from what it was
/// given.
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
fn exported(unit: &WorkflowUnit, known: &Known) -> Value {
    let mut map = match serde_json::from_str::<Value>(&unit.data) {
        Ok(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    for (k, v) in system_fields(unit, known) {
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

/// The next numbered directory for this node, so each run keeps its own files
/// rather than overwriting the last.
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
/// so anything left unreplaced here is a field the unit does not carry. A list
/// is written one entry per line.
fn fill(
    template: &str,
    data: &Value,
    system: &BTreeMap<String, Value>,
    params: &BTreeMap<String, Value>,
    files: &[(&str, String)],
) -> String {
    let text = |v: &Value| -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Array(items) if items.iter().all(Value::is_string) => items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
            other => other.to_string(),
        }
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
/// `dispatch::prepare` and the phase 1 gates like any other dispatch, and the
/// routing policy sees its node and lap, so a workflow chooses order, prompts
/// and parameters, never authority (W14). The runs it leaves are reviewed and
/// published the way every other run is.
///
/// Every unit is prepared before any is run, because `dispatch::execute`
/// spreads runs over `max_parallel`.
fn run_edit(ctx: &Ctx<'_>, node: &Node, units: &[WorkflowUnit], known: &Known) -> Result<()> {
    let Op::Edit(e) = &node.op else {
        return Err(format!("node `{}` is not an edit", node.name).into());
    };
    let mut known = known.clone();
    let mut edits = edits_made(ctx)?;
    let mut queued: Vec<crate::store::Run> = Vec::new();
    let mut driving: BTreeMap<i64, WorkflowUnit> = BTreeMap::new();
    for unit in units {
        if edits >= ctx.w.caps.max_edits {
            return Err(capped(
                &node.name,
                format!(
                    "instance {} has made {edits} edits, its `caps.max_edits`",
                    ctx.instance
                ),
            ));
        }
        let Some(project) = unit.project.clone() else {
            ctx.store
                .atomically(|| route_failed(ctx, node, unit, "the unit names no project"))?;
            continue;
        };
        let Some(row) = ctx.store.project(&project)? else {
            let why = format!("no project `{project}`; `pma scan` first");
            ctx.store
                .atomically(|| route_failed(ctx, node, unit, &why))?;
            continue;
        };
        // One counter per lineage, whatever node or lap spends it (W23).
        let key = format!("workflow:{}:{}", ctx.instance, unit.root);
        let spent = ctx.store.consumed_attempts(&project, &key)?;
        if spent >= crate::dispatch::ATTEMPT_LIMIT {
            let why = format!("{spent} attempts on this unit's lineage were used");
            ctx.store
                .atomically(|| route_failed(ctx, node, unit, &why))?;
            continue;
        }
        let data: Value = serde_json::from_str(&unit.data).unwrap_or(Value::Null);
        let task = fill(
            &e.task,
            &data,
            &system_fields(unit, &known),
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
            key,
            text: summary(&task),
            gh: None,
            tier: row.tier,
            class: None,
            details: Some(task),
            quadrant: None,
        };
        match crate::dispatch::prepare(ctx.store, ctx.home, ctx.cfg, ctx.over, &pick)? {
            crate::dispatch::Prepared::Refused(why) => {
                ctx.store
                    .atomically(|| route_failed(ctx, node, unit, &why))?;
            }
            crate::dispatch::Prepared::Queued(run) => {
                edits += 1;
                driving.insert(run.id, unit.clone());
                queued.push(*run);
            }
        }
    }
    if queued.is_empty() {
        return Ok(());
    }
    let done = crate::dispatch::execute(ctx.store, ctx.home, ctx.cfg, ctx.over, queued, |_| {})?;
    for run in done {
        let Some(unit) = driving.get(&run.id) else {
            continue;
        };
        // A run the batch budget refused never reached the agent, so its unit
        // keeps its place in the frontier and the next plan names it again.
        // Its worktree goes, or the branch would be in the way.
        if run
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with(crate::dispatch::NOT_STARTED))
        {
            let _ = crate::dispatch::remove_worktree(&run.repo, &run.worktree, &run.branch);
            continue;
        }
        known.runs.insert(unit.id.clone(), run.id);
        ctx.store.atomically(|| {
            // The node's own check writes its verdict against the unit, under
            // the rule's name, so a guard reads `@verify` rather than the run.
            if let Some(rule) = &e.check {
                let (verdict, detail) = match verdict_of(&run, rule) {
                    Checked::Verdict(v, d) => (v, d),
                    Checked::NotReady(why) => ("unknown".to_string(), Some(why)),
                };
                ctx.store.add_workflow_verdict(
                    ctx.instance,
                    &unit.id,
                    rule,
                    &verdict,
                    detail.as_deref(),
                )?;
                known
                    .verdicts
                    .insert((unit.id.clone(), rule.clone()), verdict);
            }
            route_unit(ctx.store, ctx.instance, ctx.w, &node.name, unit, &known)?;
            mark_handled(ctx.store, ctx.instance, ctx.w, &node.name, &unit.id)
        })?;
    }
    Ok(())
}

/// Edits this instance has made, which is what `caps.max_edits` bounds.
/// Counted from the runs recorded, because a pass holds no state. A run the
/// batch budget never started made no edit.
fn edits_made(ctx: &Ctx<'_>) -> Result<i64> {
    Ok(ctx
        .store
        .runs()?
        .iter()
        .filter(|r| r.workflow_instance == Some(ctx.instance))
        .filter(|r| {
            !r.error
                .as_deref()
                .is_some_and(|e| e.starts_with(crate::dispatch::NOT_STARTED))
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
      "types": {"finding": {"fields": {
        "severity": {"type": "enum", "required": true, "values": ["high", "low"]},
        "title": {"type": "line", "required": true, "max": 120},
        "reason": {"type": "lines", "max": 30}
      }}},
      "workflow": [{"name": "w", "in": "project", "out": "finding", "effects": [],
        "caps": {"max_units": 20, "max_edits": 0},
        "nodes": [
          {"name": "review", "op": "map", "out": "0..n", "in": "project", "emits": "finding",
           "max_units": 7, "task": "Review {name} into {out}."},
          {"name": "confirm", "op": "map", "out": "0..1", "in": "finding", "emits": "finding",
           "writes": ["reason"], "task": "Confirm {in} into {out}."}],
        "edges": [{"from": "@input", "to": "review"}, {"from": "review", "to": "confirm"},
                  {"from": "confirm", "to": "@output"}]}]}"#;

    /// The contract states what `accept` checks: the shape by op, and each
    /// field's limits, from the declared type.
    #[test]
    fn a_step_is_told_the_output_it_is_held_to() {
        let doc = Document::parse(DOC).unwrap();
        let w = doc.flatten("w").unwrap();
        let (input, out) = (Path::new("/t/in.json"), Path::new("/t/out.json"));
        let review = contract(&doc, &w, w.node("review").unwrap(), input, out).unwrap();
        assert!(
            review.contains("Put in /t/out.json a JSON array of new objects, at most 7."),
            "{review}"
        );
        assert!(
            review.contains("- `severity`, required: one of high, low\n"),
            "{review}"
        );
        assert!(
            review.contains("- `title`, required: one line of text, at most 120 characters\n"),
            "{review}"
        );
        assert!(
            review.contains("- `reason`: a JSON array of at most 30 strings, one line each\n"),
            "{review}"
        );

        let confirm = contract(&doc, &w, w.node("confirm").unwrap(), input, out).unwrap();
        assert!(
            confirm.contains("the objects of /t/in.json you keep, each with its `@id`"),
            "{confirm}"
        );
        assert!(confirm.contains("Set only these fields"), "{confirm}");
        assert!(
            confirm.contains("`reason`") && !confirm.contains("`title`"),
            "only what it may set: {confirm}"
        );
    }
}
