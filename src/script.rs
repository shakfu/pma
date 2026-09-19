//! Workflow documents written as [Rhai](https://rhai.rs) scripts.
//!
//! A script builds the same document JSON states, and `Document::parse` then
//! reads it: the graph a script produces is type-checked, cost-bounded and
//! stored exactly as a hand-written one is. What a script adds is variables,
//! functions, loops and comments -- a library of twelve near-identical review
//! workflows is a loop rather than twelve copies.
//!
//! The script runs once, when a document is read. It never runs during a pass,
//! never sees a unit, and decides no guard: were it to, the worst case could
//! not be computed before activation, and a proposed revision would be code
//! rather than data. A revision therefore stores the generated document, and
//! `--emit-json` prints it, so what was activated is readable without the
//! script.
//!
//! The engine is built without a clock and without modules (`no_time` and
//! `no_module` in `Cargo.toml`), with `eval` disabled and every limit set, so
//! the same script yields the same document on every machine.

use rhai::{Array, Dynamic, Engine, FnPtr, ImmutableString, Map, NativeCallContext};
use serde_json::Value;

use crate::workflow::Document;

/// Operations one document may cost. A builder walks its own lists; a runaway
/// loop is stopped rather than waited on.
const MAX_OPERATIONS: u64 = 2_000_000;
const MAX_CALL_DEPTH: usize = 32;
/// Expression nesting, at the top level and inside a function. A builder nests
/// a map inside a call inside a loop inside a function, which Rhai's default
/// for a function body does not allow.
const MAX_EXPR_DEPTH: usize = 96;
const MAX_STRING: usize = 64 * 1024;
const MAX_ARRAY: usize = 10_000;
const MAX_MAP: usize = 10_000;

impl Document {
    /// Reads a document a script builds. `name` appears in an error.
    pub fn from_script(text: &str, name: &str) -> Result<Document, String> {
        let json = script_json(text, name)?;
        let text = serde_json::to_string(&json).map_err(|e| format!("{name}: {e}"))?;
        Document::parse(&text).map_err(|e| format!("{name}: {e}"))
    }
}

/// The document a script returns, as JSON. Kept separate from `from_script` so
/// `--emit-json` can print it without validating twice.
pub fn script_json(text: &str, name: &str) -> Result<Value, String> {
    let engine = engine();
    let value: Dynamic = engine
        .eval(text)
        .map_err(|e| format!("{name}: {}", e.to_string().trim_end_matches('.')))?;
    let json = to_json(&value).map_err(|e| format!("{name}: {e}"))?;
    if !json.is_object() {
        return Err(format!(
            "{name}: a script's last expression is the document, an object with \
             `types` and `workflow`; this one is {}",
            kind(&json)
        ));
    }
    Ok(json)
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "nothing",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// An engine that can build a document and do nothing else. The clock and the
/// module system are compiled out; `eval` is disabled here, because a script
/// that writes its own script is not reviewable.
fn engine() -> Engine {
    let mut engine = Engine::new();
    engine.disable_symbol("eval");
    engine.set_max_operations(MAX_OPERATIONS);
    engine.set_max_call_levels(MAX_CALL_DEPTH);
    engine.set_max_expr_depths(MAX_EXPR_DEPTH, MAX_EXPR_DEPTH);
    engine.set_max_string_size(MAX_STRING);
    engine.set_max_array_size(MAX_ARRAY);
    engine.set_max_map_size(MAX_MAP);
    register(&mut engine);
    register_graph(&mut engine);
    register_composition(&mut engine);
    engine
}

fn map(pairs: &[(&str, Dynamic)]) -> Map {
    pairs
        .iter()
        .map(|(k, v)| ((*k).into(), v.clone()))
        .collect()
}

/// Constructors for the shapes a document repeats: field declarations,
/// parameters, edges and comparisons. Everything else is an object map, which
/// Rhai writes as `#{...}`.
fn register(engine: &mut Engine) {
    // Field declarations.
    engine.register_fn("line", |max: i64| {
        map(&[("type", "line".into()), ("max", max.into())])
    });
    engine.register_fn("lines", |max: i64| {
        map(&[("type", "lines".into()), ("max", max.into())])
    });
    engine.register_fn("choice", |values: Array| {
        map(&[("type", "enum".into()), ("values", values.into())])
    });
    engine.register_fn("list", || map(&[("type", "list".into())]));
    engine.register_fn("number", || map(&[("type", "int".into())]));
    engine.register_fn("flag", || map(&[("type", "bool".into())]));

    // Modifiers, so `req(unique(line(200)))` reads as one declaration.
    engine.register_fn("req", |mut field: Map| {
        field.insert("required".into(), true.into());
        field
    });
    engine.register_fn("unique", |mut field: Map| {
        field.insert("unique".into(), "normalised".into());
        field
    });

    // Parameters. A bound takes the second form, because a parameter used as
    // one must declare its maximum.
    engine.register_fn("param", |ty: ImmutableString, default: Dynamic| {
        map(&[("type", ty.into()), ("default", default)])
    });
    engine.register_fn(
        "bounded",
        |ty: ImmutableString, default: Dynamic, max: i64| {
            map(&[
                ("type", ty.into()),
                ("default", default),
                ("max", max.into()),
            ])
        },
    );

    // Nodes: a name and an op, merged with whatever else the node states.
    engine.register_fn(
        "node",
        |name: ImmutableString, op: ImmutableString, mut rest: Map| {
            rest.insert("name".into(), name.into());
            rest.insert("op".into(), op.into());
            rest
        },
    );

    // `in`, `while` and `with` are Rhai keywords, so a document key that
    // collides with one gets a constructor rather than asking every author to
    // quote it.
    engine.register_fn(
        "node",
        |name: ImmutableString, op: ImmutableString, input: ImmutableString, mut rest: Map| {
            rest.insert("name".into(), name.into());
            rest.insert("op".into(), op.into());
            rest.insert("in".into(), input.into());
            rest
        },
    );
    engine.register_fn(
        "call_to",
        |name: ImmutableString, input: ImmutableString, workflow: ImmutableString, with: Map| {
            map(&[
                ("name", name.into()),
                ("op", "call".into()),
                ("in", input.into()),
                ("workflow", workflow.into()),
                ("with", with.into()),
            ])
        },
    );
    engine.register_fn("retry", |max: Dynamic, predicate: ImmutableString| {
        map(&[("max", max), ("while", predicate.into())])
    });
    engine.register_fn(
        "retry",
        |max: Dynamic, predicate: ImmutableString, escalate_to: ImmutableString| {
            map(&[
                ("max", max),
                ("while", predicate.into()),
                (
                    "escalate",
                    Map::from_iter([("model".into(), escalate_to.into())]).into(),
                ),
            ])
        },
    );

    // Edges.
    engine.register_fn("edge", |from: ImmutableString, to: ImmutableString| {
        map(&[("from", from.into()), ("to", to.into())])
    });
    engine.register_fn(
        "edge_when",
        |from: ImmutableString, to: ImmutableString, when: Map| {
            map(&[
                ("from", from.into()),
                ("to", to.into()),
                ("when", when.into()),
            ])
        },
    );
    engine.register_fn(
        "edge_default",
        |from: ImmutableString, to: ImmutableString| {
            map(&[
                ("from", from.into()),
                ("to", to.into()),
                ("default", true.into()),
            ])
        },
    );
    engine.register_fn(
        "edge_lap",
        |from: ImmutableString, to: ImmutableString, when: Map, laps: Dynamic| {
            map(&[
                ("from", from.into()),
                ("to", to.into()),
                ("when", when.into()),
                ("max_laps", laps),
            ])
        },
    );

    // Comparisons, for a guard on a number.
    for (name, op) in [
        ("eq", "=="),
        ("lt", "<"),
        ("le", "<="),
        ("gt", ">"),
        ("ge", ">="),
    ] {
        let op = op.to_string();
        engine.register_fn(name, move |n: Dynamic| {
            let mut m = Map::new();
            m.insert(op.as_str().into(), n);
            m
        });
    }

    // The document itself, so a script ends with one call rather than a map
    // whose two keys are easy to misspell.
    engine.register_fn("document", |types: Map, workflows: Array| {
        map(&[("types", types.into()), ("workflow", workflows.into())])
    });
    engine.register_fn("document", |workflows: Array| {
        map(&[("workflow", workflows.into())])
    });
}

// ------------------------------------------------------------------- a graph

/// A graph under construction, and the value a script passes from one
/// combinator to the next. It carries its own output port, so a stage is wired
/// by application rather than by repeating a node's name in an edge list, and a
/// reusable piece of a graph is an ordinary function from `Graph` to `Graph`.
#[derive(Clone, Default)]
pub struct Graph {
    nodes: Array,
    edges: Array,
    /// The nodes a new stage reads from. Several after a `fan`, one otherwise.
    tail: Vec<ImmutableString>,
    /// The type at the tail, which fills a node's `in` and a preserving node's
    /// `emits`.
    ty: ImmutableString,
    input_ty: ImmutableString,
    out_ty: Option<ImmutableString>,
    /// A guard for the next edges, from `when`.
    pending: Option<Map>,
    /// Whether the next edges are the default, from `otherwise`.
    pending_default: bool,
    repo: bool,
    writes: bool,
    /// Set by `invoke`: a callee's effects are not visible from here, so the
    /// workflow must state its own rather than have them inferred wrongly.
    opaque: bool,
}

type Built<T> = Result<T, Box<rhai::EvalAltResult>>;

fn fail<T>(message: String) -> Built<T> {
    Err(message.into())
}

impl Graph {
    fn source(ty: ImmutableString) -> Graph {
        Graph {
            tail: vec!["@input".into()],
            ty: ty.clone(),
            input_ty: ty,
            ..Graph::default()
        }
    }

    /// Adds a node, an edge from every tail into it, and makes it the tail.
    fn stage(mut self, name: &str, mut node: Map) -> Built<Graph> {
        if self.tail.is_empty() {
            return fail(format!(
                "`{name}` has nothing to read: the graph has already reached `@output`"
            ));
        }
        if self.nodes.iter().any(|n| {
            n.clone()
                .try_cast::<Map>()
                .and_then(|m| m.get("name").cloned())
                .and_then(|v| v.try_cast::<ImmutableString>())
                .is_some_and(|n| n == name)
        }) {
            return fail(format!("node `{name}` is added twice"));
        }
        node.insert("name".into(), name.into());
        self.nodes.push(node.into());
        let guard = self.pending.take();
        let default = std::mem::take(&mut self.pending_default);
        for from in std::mem::take(&mut self.tail) {
            self.edges
                .push(edge_map(&from, name, guard.clone(), default, None));
        }
        self.tail = vec![name.into()];
        Ok(self)
    }
}

fn edge_map(
    from: &str,
    to: &str,
    when: Option<Map>,
    default: bool,
    max_laps: Option<Dynamic>,
) -> Dynamic {
    let mut e = Map::new();
    e.insert("from".into(), from.into());
    e.insert("to".into(), to.into());
    if let Some(g) = when {
        e.insert("when".into(), g.into());
    }
    if default {
        e.insert("default".into(), true.into());
    }
    if let Some(l) = max_laps {
        e.insert("max_laps".into(), l);
    }
    e.into()
}

fn node_map(op: &str, input: &str, pairs: &[(&str, Dynamic)]) -> Map {
    let mut m = map(pairs);
    m.insert("op".into(), op.into());
    m.insert("in".into(), input.into());
    m
}

/// The combinators. One per primitive, plus the plumbing a graph needs: a
/// guard, a default path, a fan-out and its barrier.
fn register_graph(engine: &mut Engine) {
    engine.register_type_with_name::<Graph>("Graph");

    engine.register_fn("source", Graph::source);

    // `map`, at its three bounds.
    engine.register_fn(
        "expand",
        |g: Graph,
         name: ImmutableString,
         emits: ImmutableString,
         max_units: Dynamic,
         task: ImmutableString|
         -> Built<Graph> {
            let node = node_map(
                "map",
                &g.ty,
                &[
                    ("out", "0..n".into()),
                    ("emits", emits.clone().into()),
                    ("max_units", max_units),
                    ("task", task.into()),
                ],
            );
            let mut g = g.stage(&name, node)?;
            g.ty = emits;
            Ok(g)
        },
    );
    engine.register_fn(
        "transform",
        |g: Graph, name: ImmutableString, writes: Array, task: ImmutableString| -> Built<Graph> {
            let node = node_map(
                "map",
                &g.ty,
                &[
                    ("out", "1".into()),
                    ("emits", g.ty.clone().into()),
                    ("writes", writes.into()),
                    ("task", task.into()),
                ],
            );
            g.stage(&name, node)
        },
    );
    engine.register_fn(
        "filter",
        |g: Graph, name: ImmutableString, writes: Array, task: ImmutableString| -> Built<Graph> {
            let node = node_map(
                "map",
                &g.ty,
                &[
                    ("out", "0..1".into()),
                    ("emits", g.ty.clone().into()),
                    ("writes", writes.into()),
                    ("task", task.into()),
                ],
            );
            g.stage(&name, node)
        },
    );

    // The same three, decided by a rule rather than a model.
    engine.register_fn(
        "rule_expand",
        |g: Graph,
         name: ImmutableString,
         emits: ImmutableString,
         rule: ImmutableString,
         max_units: Dynamic|
         -> Built<Graph> {
            let node = node_map(
                "map",
                &g.ty,
                &[
                    ("out", "0..n".into()),
                    ("via", "rule".into()),
                    ("rule", rule.into()),
                    ("emits", emits.clone().into()),
                    ("max_units", max_units),
                ],
            );
            let mut g = g.stage(&name, node)?;
            g.ty = emits;
            Ok(g)
        },
    );
    engine.register_fn(
        "rule_filter",
        |g: Graph, name: ImmutableString, rule: ImmutableString| -> Built<Graph> {
            let node = node_map(
                "map",
                &g.ty,
                &[
                    ("out", "0..1".into()),
                    ("via", "rule".into()),
                    ("rule", rule.into()),
                    ("emits", g.ty.clone().into()),
                ],
            );
            g.stage(&name, node)
        },
    );

    // `reduce`: the barrier after a fan, or a rule over one bag.
    engine.register_fn(
        "join",
        |g: Graph, name: ImmutableString, group_by: Array| -> Built<Graph> {
            let node = node_map(
                "reduce",
                &g.ty,
                &[
                    ("via", "rule".into()),
                    ("rule", "dedupe".into()),
                    ("emits", g.ty.clone().into()),
                    ("group_by", group_by.into()),
                ],
            );
            g.stage(&name, node)
        },
    );
    engine.register_fn(
        "join_by",
        |g: Graph, name: ImmutableString, group_by: Array, rule: ImmutableString| -> Built<Graph> {
            let node = node_map(
                "reduce",
                &g.ty,
                &[
                    ("via", "rule".into()),
                    ("rule", rule.into()),
                    ("emits", g.ty.clone().into()),
                    ("group_by", group_by.into()),
                ],
            );
            g.stage(&name, node)
        },
    );
    engine.register_fn(
        "reduce_with",
        |g: Graph, name: ImmutableString, group_by: Array, task: ImmutableString| -> Built<Graph> {
            let node = node_map(
                "reduce",
                &g.ty,
                &[
                    ("emits", g.ty.clone().into()),
                    ("group_by", group_by.into()),
                    ("task", task.into()),
                ],
            );
            g.stage(&name, node)
        },
    );

    // `edit`, the only primitive that changes a repository.
    engine.register_fn(
        "edit",
        |g: Graph,
         name: ImmutableString,
         check: ImmutableString,
         task: ImmutableString|
         -> Built<Graph> {
            let node = node_map(
                "edit",
                &g.ty,
                &[("check", check.into()), ("task", task.into())],
            );
            let mut g = g.stage(&name, node)?;
            g.repo = true;
            Ok(g)
        },
    );

    // `check`, which annotates a verdict a guard may read.
    engine.register_fn(
        "check",
        |g: Graph, name: ImmutableString, rule: ImmutableString| -> Built<Graph> {
            let node = node_map("check", &g.ty, &[("rule", rule.into())]);
            g.stage(&name, node)
        },
    );

    // `emit`, which writes outside the repository and returns its input.
    engine.register_fn(
        "emit_todo",
        |g: Graph, name: ImmutableString, action: ImmutableString, fields: Map| -> Built<Graph> {
            let node = node_map(
                "emit",
                &g.ty,
                &[
                    ("sink", "todo".into()),
                    ("action", action.into()),
                    ("map", fields.into()),
                ],
            );
            let mut g = g.stage(&name, node)?;
            g.writes = true;
            Ok(g)
        },
    );
    engine.register_fn(
        "emit_note",
        |g: Graph, name: ImmutableString, fields: Map| -> Built<Graph> {
            let node = node_map(
                "emit",
                &g.ty,
                &[
                    ("sink", "note".into()),
                    ("action", "add".into()),
                    ("map", fields.into()),
                ],
            );
            let mut g = g.stage(&name, node)?;
            g.writes = true;
            Ok(g)
        },
    );

    // A call to another workflow. Named `invoke` because Rhai's own `call`
    // applies a function pointer, and a method that shadowed it would report
    // the wrong error. The callee's return type is stated here and checked
    // against the callee where the document is read.
    engine.register_fn(
        "invoke",
        |g: Graph,
         name: ImmutableString,
         workflow: ImmutableString,
         with: Map,
         emits: ImmutableString|
         -> Built<Graph> {
            let node = node_map(
                "call",
                &g.ty,
                &[("workflow", workflow.into()), ("with", with.into())],
            );
            let mut g = g.stage(&name, node)?;
            g.ty = emits;
            g.opaque = true;
            Ok(g)
        },
    );

    // A retry belongs to the node just added, so it reads as a property of the
    // stage rather than a fifth argument to it.
    engine.register_fn(
        "retrying",
        |g: Graph, max: Dynamic, predicate: ImmutableString| -> Built<Graph> {
            retrying(g, max, predicate, None)
        },
    );
    engine.register_fn(
        "retrying",
        |g: Graph,
         max: Dynamic,
         predicate: ImmutableString,
         model: ImmutableString|
         -> Built<Graph> { retrying(g, max, predicate, Some(model)) },
    );

    // Guards, the default path, and a bounded back edge.
    engine.register_fn("when", |mut g: Graph, guard: Map| {
        g.pending = Some(guard);
        g
    });
    engine.register_fn(
        "lap",
        |mut g: Graph, back_to: ImmutableString, guard: Map, max_laps: Dynamic| -> Built<Graph> {
            let [from] = &g.tail[..] else {
                return fail("a lap edge needs one node to start from".into());
            };
            g.edges
                .push(edge_map(from, &back_to, Some(guard), false, Some(max_laps)));
            Ok(g)
        },
    );

    engine.register_fn("output", |mut g: Graph| -> Built<Graph> {
        if g.tail.is_empty() {
            return fail("the graph has already reached `@output`".into());
        }
        let guard = g.pending.take();
        let default = std::mem::take(&mut g.pending_default);
        for from in std::mem::take(&mut g.tail) {
            g.edges
                .push(edge_map(&from, "@output", guard.clone(), default, None));
        }
        g.out_ty = Some(g.ty.clone());
        Ok(g)
    });
}

fn retrying(
    mut g: Graph,
    max: Dynamic,
    predicate: ImmutableString,
    model: Option<ImmutableString>,
) -> Built<Graph> {
    let Some(last) = g.nodes.last_mut() else {
        return fail("`retrying` describes the stage before it, and there is none".into());
    };
    let Some(mut node) = last.clone().try_cast::<Map>() else {
        return fail("`retrying` describes a stage".into());
    };
    let mut retry = map(&[("max", max), ("while", predicate.into())]);
    if let Some(m) = model {
        retry.insert(
            "escalate".into(),
            Map::from_iter([("model".into(), m.into())]).into(),
        );
    }
    node.insert("retry".into(), retry.into());
    *last = node.into();
    Ok(g)
}

/// A fan-out and the workflow a graph becomes. Both take a script function, so
/// a branch of the graph is a value like any other.
fn register_composition(engine: &mut Engine) {
    engine.register_fn(
        "fan",
        |ctx: NativeCallContext, g: Graph, branches: Array| -> Built<Graph> {
            if branches.is_empty() {
                return fail("`fan` needs at least one branch".into());
            }
            let mut out = Graph {
                tail: Vec::new(),
                pending: None,
                pending_default: false,
                ..g.clone()
            };
            for branch in branches {
                let Some(f) = branch.try_cast::<FnPtr>() else {
                    return fail("`fan` takes functions from a graph to a graph".into());
                };
                let start = Graph {
                    nodes: Array::new(),
                    edges: Array::new(),
                    tail: g.tail.clone(),
                    ty: g.ty.clone(),
                    pending: g.pending.clone(),
                    ..Graph::default()
                };
                let done: Graph = f.call_within_context(&ctx, (start,))?;
                out.nodes.extend(done.nodes);
                out.edges.extend(done.edges);
                out.tail.extend(done.tail);
                out.repo |= done.repo;
                out.writes |= done.writes;
                out.opaque |= done.opaque;
                out.ty = done.ty;
            }
            Ok(out)
        },
    );

    // A branch off the current stage, taking the units its guards refused. The
    // main path is unchanged, which is what a lap edge's terminal path needs.
    engine.register_fn(
        "otherwise",
        |ctx: NativeCallContext, g: Graph, branch: FnPtr| -> Built<Graph> {
            let start = Graph {
                tail: g.tail.clone(),
                ty: g.ty.clone(),
                pending_default: true,
                ..Graph::default()
            };
            let done: Graph = branch.call_within_context(&ctx, (start,))?;
            let mut out = g;
            out.nodes.extend(done.nodes);
            out.edges.extend(done.edges);
            out.repo |= done.repo;
            out.writes |= done.writes;
            out.opaque |= done.opaque;
            Ok(out)
        },
    );

    engine.register_fn(
        "workflow",
        |name: ImmutableString, g: Graph, opts: Map| -> Built<Map> {
            if let Some(key) = opts
                .keys()
                .find(|k| !["params", "caps", "effects"].contains(&k.as_str()))
            {
                return fail(format!(
                    "workflow `{name}`: unknown option `{key}`; expected params, caps or effects"
                ));
            }
            let mut w = Map::new();
            w.insert("name".into(), name.clone().into());
            w.insert("in".into(), g.input_ty.clone().into());
            if let Some(out) = &g.out_ty {
                w.insert("out".into(), out.clone().into());
            }
            // Effects are asserted when stated and inferred otherwise. Either
            // way the stored document declares them, so a caller reads the
            // signature rather than the graph.
            let effects = match opts.get("effects") {
                Some(stated) => stated.clone(),
                None if g.opaque => {
                    return fail(format!(
                        "workflow `{name}`: it invokes another workflow, whose effects are not \
                         visible here, so state its own: effects: [\"repo\"], [\"writes\"], \
                         both, or []"
                    ));
                }
                None => {
                    let mut list = Array::new();
                    if g.repo {
                        list.push("repo".into());
                    }
                    if g.writes {
                        list.push("writes".into());
                    }
                    list.into()
                }
            };
            w.insert("effects".into(), effects);
            w.insert(
                "params".into(),
                opts.get("params")
                    .cloned()
                    .unwrap_or_else(|| Map::new().into()),
            );
            let Some(caps) = opts.get("caps") else {
                return fail(format!(
                    "workflow `{name}`: no `caps`; state max_units and max_edits per instance"
                ));
            };
            w.insert("caps".into(), caps.clone());
            w.insert("nodes".into(), g.nodes.into());
            w.insert("edges".into(), g.edges.into());
            Ok(w)
        },
    );
}

/// A script's value as JSON. Only the six shapes JSON has are accepted, so a
/// document cannot carry a Rhai value nothing downstream could read.
fn to_json(d: &Dynamic) -> Result<Value, String> {
    if d.is_unit() {
        return Ok(Value::Null);
    }
    if let Some(b) = d.clone().try_cast::<bool>() {
        return Ok(Value::Bool(b));
    }
    if let Some(n) = d.clone().try_cast::<i64>() {
        return Ok(Value::from(n));
    }
    if let Some(f) = d.clone().try_cast::<f64>() {
        return serde_json::Number::from_f64(f)
            .map(Value::Number)
            .ok_or_else(|| format!("{f} is not a number JSON can hold"));
    }
    if let Some(s) = d.clone().try_cast::<ImmutableString>() {
        return Ok(Value::String(s.into_owned()));
    }
    if let Some(a) = d.clone().try_cast::<Array>() {
        return a
            .iter()
            .map(to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array);
    }
    if let Some(m) = d.clone().try_cast::<Map>() {
        let mut out = serde_json::Map::new();
        for (k, v) in &m {
            out.insert(k.to_string(), to_json(v)?);
        }
        return Ok(Value::Object(out));
    }
    Err(format!(
        "a document cannot hold a {}; use a string, number, boolean, list or map",
        d.type_name()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same workflow the JSON tests use, written as a script.
    const FIND_ISSUES: &str = r#"
        // Two reviewers would differ only in their prompt, so the nodes are
        // built rather than copied.
        let review = node("review", "map", #{
            "in": "project", out: "0..n", emits: "finding",
            max_units: "{$breadth}", doc: "{$review_doc}",
            task: "Review `{name}`. Write prose to {doc} and findings to {out}.",
        });

        let confirm = node("confirm", "map", #{
            "in": "finding", out: "0..1", emits: "finding", writes: ["reason"],
            task: "Confirm each unit in {in}. Drop what you cannot prove, with a `reason`.",
        });

        let dedupe = node("dedupe", "reduce", #{
            "in": "finding", emits: "finding", via: "rule", rule: "dedupe",
            group_by: ["title"],
        });

        document(#{
            finding: #{ fields: #{
                severity: req(choice(["critical", "high"])),
                title: req(unique(line(200))),
                detail: lines(40),
                reason: line(200),
            }},
        }, [#{
            name: "find-issues",
            "in": "project", out: "finding", effects: [],
            params: #{
                review_doc: param("name", "REVIEW.md"),
                breadth: bounded("int", 20, 40),
            },
            caps: #{ max_units: 60, max_edits: 0 },
            nodes: [review, confirm, dedupe],
            edges: [
                edge("@input", "review"),
                edge("review", "confirm"),
                edge("confirm", "dedupe"),
                edge("dedupe", "@output"),
            ],
        }])
    "#;

    #[test]
    fn a_script_builds_the_same_document_json_states() {
        let from_script = Document::from_script(FIND_ISSUES, "find.rhai").unwrap();
        let w = from_script.workflow("find-issues").unwrap();
        assert_eq!(w.input, "project");
        assert_eq!(w.output.as_deref(), Some("finding"));
        assert_eq!(w.nodes.len(), 3);
        assert_eq!(w.params["breadth"].max, Some(40));
        // And it costs what the graph costs, not what the script did.
        let e = from_script.estimate("find-issues", 1, 1.0).unwrap();
        assert_eq!(e.agent_runs, 41);
    }

    /// The reason to have a script at all: one loop where a document repeats.
    #[test]
    fn a_loop_builds_a_node_per_dimension() {
        let script = r#"
            let dimensions = ["bugs", "tests", "api"];
            let nodes = [];
            let edges = [];
            for d in dimensions {
                nodes.push(node(d, "map", #{
                    "in": "project", out: "0..n", emits: "finding", max_units: 10,
                    task: "Review {name} for " + d + ". Write findings to {out}.",
                }));
                edges.push(edge("@input", d));
                edges.push(edge(d, "merge"));
            }
            nodes.push(node("merge", "reduce", #{
                "in": "finding", emits: "finding", via: "rule", rule: "dedupe",
                group_by: ["title"],
            }));
            edges.push(edge("merge", "@output"));

            document(#{ finding: #{ fields: #{
                title: req(line(200)),
            }}}, [#{
                name: "ensemble", "in": "project", out: "finding", effects: [],
                caps: #{ max_units: 80, max_edits: 0 },
                nodes: nodes, edges: edges,
            }])
        "#;
        let doc = Document::from_script(script, "ensemble.rhai").unwrap();
        let w = doc.workflow("ensemble").unwrap();
        assert_eq!(w.nodes.len(), 4, "three reviewers and a reducer");
        let e = doc.estimate("ensemble", 2, 1.0).unwrap();
        // Three reviewers over two projects, then a rule that costs nothing.
        assert_eq!(e.agent_runs, 6);
        assert!(w.node("api").is_some());
    }

    /// One checker, whichever form the document arrived in.
    #[test]
    fn a_script_is_held_to_the_same_refusals() {
        let broken = FIND_ISSUES.replace(r#"max_units: "{$breadth}","#, "");
        let e = Document::from_script(&broken, "find.rhai").unwrap_err();
        assert!(e.contains("`0..n` map must state `max_units`"), "{e}");
        assert!(e.starts_with("find.rhai:"), "{e}");
    }

    #[test]
    fn a_script_must_end_with_a_document() {
        let e = Document::from_script("40 + 2", "n.rhai").unwrap_err();
        assert!(e.contains("last expression is the document"), "{e}");
        assert!(e.contains("a number"), "{e}");
    }

    /// The module system is compiled out, so a script cannot reach the
    /// filesystem or another script whatever it writes.
    #[test]
    fn a_script_cannot_import_anything() {
        let e = Document::from_script(r#"import "fs" as fs; fs::read("/etc/passwd")"#, "i.rhai")
            .unwrap_err();
        assert!(e.to_lowercase().contains("import"), "{e}");
    }

    /// `eval` would let a document carry a script that builds another one,
    /// which is not reviewable.
    #[test]
    fn a_script_cannot_eval() {
        let e = Document::from_script(r#"eval("40 + 2")"#, "e.rhai").unwrap_err();
        assert!(!e.is_empty(), "eval must not succeed");
        assert!(
            e.contains("eval") || e.contains("last expression is the document"),
            "{e}"
        );
    }

    /// A document is read before anything runs, so a script that will not stop
    /// must be stopped rather than waited on.
    #[test]
    fn a_runaway_script_is_terminated() {
        let e = Document::from_script("let n = 0; loop { n += 1; } n", "loop.rhai").unwrap_err();
        assert!(e.to_lowercase().contains("operation"), "{e}");
    }

    /// The clock is compiled out: two readings of one script must not differ.
    #[test]
    fn a_script_cannot_read_the_clock() {
        let e = Document::from_script("timestamp()", "t.rhai").unwrap_err();
        assert!(!e.is_empty(), "timestamp must not resolve");
    }

    /// The combinator form of the same workflow: a stage is wired by
    /// application, so no edge is written down and no node name is repeated.
    const ENSEMBLE: &str = r#"
        fn reviewer(node, what) {
            |g| g.expand(node, "finding", "{$breadth}",
                         "Review `{name}` for " + what + ". Write findings to {out}.")
        }

        let graph = source("project")
            .fan([
                reviewer("bugs",  "correctness bugs"),
                reviewer("tests", "missing tests"),
                reviewer("api",   "interfaces that are easy to use incorrectly"),
            ])
            .join("merge", ["title"])
            .filter("confirm", ["reason"], "Confirm each unit in {in}.")
            .output();

        document(#{ finding: #{ fields: #{
            title: req(unique(line(200))),
            reason: line(200),
        }}}, [
            workflow("ensemble-review", graph, #{
                params: #{ breadth: bounded("int", 10, 20) },
                caps:   #{ max_units: 80, max_edits: 0 },
            }),
        ])
    "#;

    #[test]
    fn a_fan_and_a_join_wire_themselves() {
        let doc = Document::from_script(ENSEMBLE, "e.rhai").unwrap();
        let w = doc.workflow("ensemble-review").unwrap();
        assert_eq!(
            w.nodes.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
            ["bugs", "tests", "api", "merge", "confirm"]
        );
        // Three branches from the input, three into the barrier, then one each.
        let from = |name: &str| {
            w.edges
                .iter()
                .filter(|e| match &e.from {
                    crate::workflow::From::Input => name == "@input",
                    crate::workflow::From::Node(n) => n == name,
                })
                .count()
        };
        assert_eq!(from("@input"), 3);
        assert_eq!((from("bugs"), from("tests"), from("api")), (1, 1, 1));
        assert_eq!(w.edges.len(), 8);
        assert_eq!(w.input, "project");
        assert_eq!(w.output.as_deref(), Some("finding"));
    }

    /// A reusable piece of a graph is a function, so two of them compose by
    /// application and nothing in either names the other.
    #[test]
    fn pipelines_compose_as_functions() {
        let script = r#"
            fn find(g) { g.expand("review", "finding", 5, "Review {name}.") }
            fn fix(g)  { g.edit("patch", "verify", "Fix {title}.") }

            document(#{ finding: #{ fields: #{ title: req(line(200)) }}}, [
                workflow("both", fix(find(source("project"))).output(), #{
                    caps: #{ max_units: 20, max_edits: 5 },
                }),
            ])
        "#;
        let doc = Document::from_script(script, "c.rhai").unwrap();
        let w = doc.workflow("both").unwrap();
        assert_eq!(
            w.nodes.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
            ["review", "patch"]
        );
        // Effects follow from the graph rather than from a declaration.
        assert!(w.effects.repo && !w.effects.writes);
    }

    /// A lap edge and the terminal path it requires both leave one stage, and
    /// `retrying` describes the stage before it.
    #[test]
    fn a_lap_its_default_path_and_a_retry_come_from_the_stage() {
        let script = r#"
            let graph = source("finding")
                .edit("fix", "verify", "Fix {title}.")
                .retrying("{$laps}", "@verify != passed")
                .transform("audit", ["verdict"], "Judge {title}.")
                .lap("fix", #{ verdict: ["reject"] }, "{$laps}")
                .otherwise(|g| g.emit_todo("handoff", "add", #{ text: "{title}" }))
                .when(#{ verdict: ["accept"] })
                .output();

            document(#{ finding: #{ fields: #{
                title: req(line(200)),
                verdict: choice(["accept", "reject"]),
            }}}, [
                workflow("audited", graph, #{
                    params: #{ laps: bounded("int", 2, 3) },
                    caps: #{ max_units: 20, max_edits: 6 },
                }),
            ])
        "#;
        let doc = Document::from_script(script, "l.rhai").unwrap();
        let w = doc.workflow("audited").unwrap();
        let audit = w.node("audit").unwrap();
        let crate::workflow::Op::Map(_) = &audit.op else {
            panic!("audit is a map")
        };
        let leaving: Vec<_> = w
            .edges
            .iter()
            .filter(|e| e.from == crate::workflow::From::Node("audit".into()))
            .collect();
        assert_eq!(leaving.len(), 3, "output, lap and default");
        assert_eq!(leaving.iter().filter(|e| e.max_laps.is_some()).count(), 1);
        assert_eq!(leaving.iter().filter(|e| e.default).count(), 1);
        // The retry belongs to `fix`, not to the graph.
        let crate::workflow::Op::Edit(fix) = &w.node("fix").unwrap().op else {
            panic!("fix is an edit")
        };
        assert!(fix.retry.is_some());
        assert!(w.effects.repo && w.effects.writes, "the handoff writes");
    }

    /// A callee's effects are not visible from a caller's graph, so the
    /// caller states them rather than having an empty set inferred.
    #[test]
    fn a_workflow_that_invokes_another_states_its_own_effects() {
        let script = r#"
            document(#{ finding: #{ fields: #{ title: req(line(200)) }}}, [
                workflow("inner", source("project")
                    .edit("touch", "verify", "Change something in {name}.")
                    .output(), #{ caps: #{ max_units: 4, max_edits: 2 }}),
                workflow("outer", source("project")
                    .invoke("inner_call", "inner", #{}, "project")
                    .output(), #{ caps: #{ max_units: 4, max_edits: 2 }}),
            ])
        "#;
        let e = Document::from_script(script, "i.rhai").unwrap_err();
        assert!(e.contains("invokes another workflow"), "{e}");
        let fixed = script.replace(
            r#"#{ caps: #{ max_units: 4, max_edits: 2 }}),
            ])"#,
            r#"#{ effects: ["repo"], caps: #{ max_units: 4, max_edits: 2 }}),
            ])"#,
        );
        let doc = Document::from_script(&fixed, "i.rhai").unwrap();
        assert!(doc.workflow("outer").unwrap().effects.repo);
    }

    #[test]
    fn a_builder_refuses_a_stage_it_cannot_wire() {
        let twice = r#"
            document(#{}, [workflow("w", source("project")
                .expand("a", "project", 2, "One {name}.")
                .expand("a", "project", 2, "Two {name}.")
                .output(), #{ caps: #{ max_units: 4, max_edits: 0 }})])
        "#;
        assert!(
            Document::from_script(twice, "t.rhai")
                .unwrap_err()
                .contains("added twice")
        );
        let after = r#"
            document(#{}, [workflow("w", source("project")
                .expand("a", "project", 2, "One {name}.")
                .output()
                .expand("b", "project", 2, "Two {name}.")
                , #{ caps: #{ max_units: 4, max_edits: 0 }})])
        "#;
        assert!(
            Document::from_script(after, "a.rhai")
                .unwrap_err()
                .contains("already reached `@output`")
        );
    }

    /// A graph is not a document: it must go through `workflow`, which is
    /// where caps and the signature are stated.
    #[test]
    fn a_workflow_states_its_caps() {
        let e = Document::from_script(
            r#"document(#{}, [workflow("w", source("project")
                 .expand("a", "project", 2, "{name}").output(), #{})])"#,
            "w.rhai",
        )
        .unwrap_err();
        assert!(e.contains("no `caps`"), "{e}");
    }

    #[test]
    fn only_json_shapes_reach_the_document() {
        let e = Document::from_script(r#"document(#{}, [|| 1])"#, "f.rhai").unwrap_err();
        assert!(e.contains("cannot hold"), "{e}");
    }
}
