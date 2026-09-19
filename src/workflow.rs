//! A workflow is a typed, parameterised function over bags of units: a graph
//! of nodes, each applying one of five primitives or another workflow, joined
//! by edges that decide where a unit goes next.
//!
//! This module reads a document, refuses everything it cannot apply exactly,
//! and computes the worst case a graph can cost. Nothing here runs a node or
//! touches the store: a document is read long before anything is dispatched,
//! and the bound is what `pma workflow activate` weighs against the budget.
//!
//! The document is JSON for the reason `route.rs` gives: this crate parses
//! JSON already. Design: `docs/dev/workflows.md`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// Types `pma` writes itself, from data it already holds. A document may not
/// declare one of these names, nor add fields to them; a workflow that needs
/// its own fields projects with the `as:` rule.
pub const BUILTIN_TYPES: [&str; 4] = ["project", "item", "signal", "run"];

/// Fields on every unit, whatever its type. Reserved, so a declaration may
/// not shadow one, and addressed in a guard or a prompt with a leading `@`.
pub const SYSTEM_FIELDS: [&str; 10] = [
    "id", "type", "node", "parent", "root", "depth", "lap", "children", "project", "run",
];

/// Rules a `check` node, or an `edit` node's `check`, may name. Each writes a
/// verdict under its own name: `verify` writes `@verify`.
pub const CHECK_RULES: [&str; 6] = [
    "verify",
    "scope-clean",
    "lint-todo",
    "ci-green",
    "pr-merged",
    "nonempty",
];

/// Rules a `map` may name. `as:`, `where:`, `path-present:` and `path-absent:`
/// take an argument after the colon.
const MAP_RULES: [&str; 4] = ["todo-items", "open-issues", "open-runs", "outdated-deps"];
const MAP_RULE_PREFIXES: [&str; 4] = ["as:", "where:", "path-present:", "path-absent:"];

/// Rules a `reduce` may name. `limit:` takes a count.
const REDUCE_RULES: [&str; 2] = ["dedupe", "rank"];
const REDUCE_RULE_PREFIXES: [&str; 1] = ["limit:"];

/// What a `map` may do to the size of its input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Out {
    /// `0..n`: creates units, so it is where parallel work comes from.
    Grows,
    /// `1`: an annotation. The id is preserved and only `writes` may change.
    Same,
    /// `0..1`: a filter. Kept ids are a subset and each drop carries a reason.
    Shrinks,
}

impl Out {
    fn parse(s: &str) -> Option<Out> {
        match s {
            "0..n" => Some(Out::Grows),
            "1" => Some(Out::Same),
            "0..1" => Some(Out::Shrinks),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Out::Grows => "0..n",
            Out::Same => "1",
            Out::Shrinks => "0..1",
        }
    }
}

/// Who decides: a model, which costs money and is untrusted, or a rule, which
/// is free and replays exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Agent,
    Rule,
}

/// A count a document states: a constant, or a parameter that declares its own
/// maximum. The worst case is computed at the maximum, so a bound never
/// depends on the arguments an invocation happened to pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Count {
    Fixed(i64),
    Param(String),
}

impl Count {
    /// The largest value this count can take, which is what a bound reads.
    fn ceiling(&self, params: &BTreeMap<String, Param>) -> i64 {
        match self {
            Count::Fixed(n) => *n,
            Count::Param(p) => params.get(p).and_then(|p| p.max).unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldType {
    Line { max: i64, unique: bool },
    Lines { max: i64 },
    Enum { values: Vec<String> },
    List,
    Int { min: Option<i64>, max: Option<i64> },
    Bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub ty: FieldType,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Type {
    pub fields: BTreeMap<String, Field>,
}

impl Type {
    fn of(names: &[&str]) -> Type {
        Type {
            fields: names
                .iter()
                .map(|n| {
                    (
                        (*n).to_string(),
                        Field {
                            ty: FieldType::Line {
                                max: 4096,
                                unique: false,
                            },
                            required: false,
                        },
                    )
                })
                .collect(),
        }
    }
}

/// A built-in type's fields. They are all read as text: nothing declares them,
/// so nothing checks them beyond their names being addressable.
pub fn builtin_type(name: &str) -> Option<Type> {
    let fields: &[&str] = match name {
        "project" => &[
            "name",
            "tier",
            "repo",
            "owner",
            "default_branch",
            "ci",
            "deps",
            "tags",
        ],
        "item" => &[
            "key",
            "text",
            "priority",
            "tags",
            "due",
            "gh",
            "group",
            "description",
            "done",
        ],
        "signal" => &["kind", "detail"],
        "run" => &["id", "task", "state", "branch", "pr", "verify", "cost"],
        _ => return None,
    };
    Some(Type::of(fields))
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamType {
    /// A filename-safe string, for a document such as `REVIEW.md`.
    Name,
    Line,
    Int,
    Enum(Vec<String>),
    Bool,
    List,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub ty: ParamType,
    pub default: Value,
    /// Required when the parameter is used as a bound. The worst case reads
    /// this rather than the default.
    pub max: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Effects {
    /// Some node changes a repository, through `edit`.
    pub repo: bool,
    /// Some node writes outside it, through `emit`.
    pub writes: bool,
}

impl Effects {
    fn union(self, other: Effects) -> Effects {
        Effects {
            repo: self.repo || other.repo,
            writes: self.writes || other.writes,
        }
    }

    pub fn names(self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.repo {
            v.push("repo");
        }
        if self.writes {
            v.push("writes");
        }
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub max_units: i64,
    pub max_edits: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Retry {
    pub max: Count,
    pub predicate: String,
    pub escalate: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sink {
    Todo,
    Note,
    Doc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Add,
    Tick,
    Remove,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapNode {
    pub via: Via,
    pub out: Out,
    pub emits: String,
    pub writes: Vec<String>,
    pub max_units: Option<Count>,
    pub max_depth: Option<Count>,
    pub task: Option<String>,
    pub rule: Option<String>,
    pub doc: Option<String>,
    pub publish: bool,
    pub retry: Option<Retry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReduceNode {
    pub via: Via,
    pub emits: String,
    pub group_by: Vec<String>,
    pub task: Option<String>,
    pub rule: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditNode {
    pub task: String,
    pub check: Option<String>,
    pub retry: Option<Retry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmitNode {
    pub sink: Sink,
    pub action: Action,
    pub map: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallNode {
    pub workflow: String,
    pub with: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Map(MapNode),
    Reduce(ReduceNode),
    Edit(EditNode),
    Check { rule: String },
    Emit(EmitNode),
    Call(CallNode),
}

impl Op {
    pub fn name(&self) -> &'static str {
        match self {
            Op::Map(_) => "map",
            Op::Reduce(_) => "reduce",
            Op::Edit(_) => "edit",
            Op::Check { .. } => "check",
            Op::Emit(_) => "emit",
            Op::Call(_) => "call",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub name: String,
    /// The type it expects. Every incoming edge must carry it.
    pub input: String,
    pub op: Op,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum From {
    Input,
    Node(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum To {
    Output,
    Node(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Cmp {
    fn parse(s: &str) -> Option<Cmp> {
        match s {
            "==" => Some(Cmp::Eq),
            "<" => Some(Cmp::Lt),
            "<=" => Some(Cmp::Le),
            ">" => Some(Cmp::Gt),
            ">=" => Some(Cmp::Ge),
            _ => None,
        }
    }
}

/// What a guard reads: a declared field of the units on the edge, or a system
/// field or verdict, written with a leading `@`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Field(String),
    System(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Test {
    /// Membership. A value may be `{$param}`.
    In(Vec<String>),
    Compare(Cmp, Count),
    Present,
    Absent,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Guard(pub Vec<(Key, Test)>);

#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub from: From,
    pub to: To,
    pub when: Option<Guard>,
    /// Takes the units no guarded edge accepted.
    pub default: bool,
    /// A back edge, and its bound. Requires a terminal path from its source.
    pub max_laps: Option<Count>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Workflow {
    pub name: String,
    pub input: String,
    pub output: Option<String>,
    pub effects: Effects,
    pub params: BTreeMap<String, Param>,
    pub caps: Caps,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Workflow {
    pub fn node(&self, name: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Document {
    pub types: BTreeMap<String, Type>,
    pub workflows: Vec<Workflow>,
}

impl Document {
    pub fn workflow(&self, name: &str) -> Option<&Workflow> {
        self.workflows.iter().find(|w| w.name == name)
    }

    /// A type by name, whether declared or built in.
    pub fn resolve(&self, name: &str) -> Option<Type> {
        builtin_type(name).or_else(|| self.types.get(name).cloned())
    }

    /// Reads a document, refusing anything it cannot apply exactly. A node
    /// that silently does nothing is worse than one that is refused, so every
    /// check names the workflow and node it failed at.
    pub fn parse(text: &str) -> Result<Document, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
        known(&v, &["types", "workflow"], "document")?;
        let mut types = BTreeMap::new();
        if let Some(map) = v.get("types") {
            let map = map
                .as_object()
                .ok_or_else(|| "`types` must be an object".to_string())?;
            for (name, decl) in map {
                types.insert(name.clone(), parse_type(name, decl)?);
            }
        }
        let list = v
            .get("workflow")
            .and_then(Value::as_array)
            .ok_or("expected a `workflow` array")?;
        if list.is_empty() {
            return Err("a document with no workflow defines nothing".into());
        }
        let workflows = list
            .iter()
            .map(parse_workflow)
            .collect::<Result<Vec<_>, String>>()?;
        let doc = Document { types, workflows };
        doc.validate()?;
        Ok(doc)
    }
}

fn known(v: &Value, fields: &[&str], what: &str) -> Result<(), String> {
    let Some(map) = v.as_object() else {
        return Err(format!("{what} must be an object"));
    };
    match map.keys().find(|k| !fields.contains(&k.as_str())) {
        Some(k) => Err(format!(
            "{what}: unknown field `{k}`; expected one of {}",
            fields.join(", ")
        )),
        None => Ok(()),
    }
}

fn parse_type(name: &str, v: &Value) -> Result<Type, String> {
    let at = |e: String| format!("type `{name}`: {e}");
    if BUILTIN_TYPES.contains(&name) {
        return Err(at("that is a built-in type; choose another name".into()));
    }
    known(v, &["fields"], &format!("type `{name}`"))?;
    let fields = v
        .get("fields")
        .and_then(Value::as_object)
        .ok_or_else(|| at("no `fields`".into()))?;
    let mut out = BTreeMap::new();
    for (field, decl) in fields {
        if field.starts_with('@') {
            return Err(at(format!("field `{field}` is reserved")));
        }
        known(
            decl,
            &["type", "required", "max", "min", "values", "unique"],
            &format!("type `{name}` field `{field}`"),
        )?;
        let kind = decl["type"]
            .as_str()
            .ok_or_else(|| at(format!("field `{field}` has no `type`")))?;
        let max = decl["max"].as_i64();
        let ty = match kind {
            "line" => FieldType::Line {
                max: max.unwrap_or(200),
                unique: decl["unique"].as_str() == Some("normalised"),
            },
            "lines" => FieldType::Lines {
                max: max.unwrap_or(40),
            },
            "enum" => {
                let values = decl["values"]
                    .as_array()
                    .ok_or_else(|| at(format!("field `{field}` is an enum with no `values`")))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(String::from)
                            .ok_or_else(|| at(format!("field `{field}`: values must be strings")))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                if values.is_empty() {
                    return Err(at(format!("field `{field}`: an enum with no values")));
                }
                FieldType::Enum { values }
            }
            "list" => FieldType::List,
            "int" => FieldType::Int {
                min: decl["min"].as_i64(),
                max,
            },
            "bool" => FieldType::Bool,
            other => return Err(at(format!("field `{field}`: unknown type `{other}`"))),
        };
        out.insert(
            field.clone(),
            Field {
                ty,
                required: decl["required"].as_bool().unwrap_or(false),
            },
        );
    }
    Ok(Type { fields: out })
}

fn count(v: &Value, what: &str) -> Result<Count, String> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .map(Count::Fixed)
            .ok_or_else(|| format!("{what} must be a whole number")),
        Value::String(s) => match param_ref(s) {
            Some(p) => Ok(Count::Param(p)),
            None => Err(format!(
                "{what} must be a number or `{{$param}}`, not `{s}`"
            )),
        },
        _ => Err(format!("{what} must be a number or `{{$param}}`")),
    }
}

/// `{$name}` as a whole string, which is the only place a parameter may stand
/// in for a value rather than be substituted into text.
fn param_ref(s: &str) -> Option<String> {
    let inner = s.strip_prefix("{$")?.strip_suffix('}')?;
    (!inner.is_empty() && !inner.contains(['{', '}'])).then(|| inner.to_string())
}

fn parse_params(
    v: &Value,
    at: &dyn Fn(String) -> String,
) -> Result<BTreeMap<String, Param>, String> {
    let mut out = BTreeMap::new();
    let Some(map) = v.as_object() else {
        return Err(at("`params` must be an object".into()));
    };
    for (name, decl) in map {
        known(
            decl,
            &["type", "default", "values", "max", "min"],
            &format!("parameter `{name}`"),
        )
        .map_err(at)?;
        let kind = decl["type"]
            .as_str()
            .ok_or_else(|| at(format!("parameter `{name}` has no `type`")))?;
        let ty = match kind {
            "name" => ParamType::Name,
            "line" => ParamType::Line,
            "int" => ParamType::Int,
            "bool" => ParamType::Bool,
            "list" => ParamType::List,
            "enum" => ParamType::Enum(
                decl["values"]
                    .as_array()
                    .ok_or_else(|| at(format!("parameter `{name}` is an enum with no `values`")))?
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
            ),
            other => return Err(at(format!("parameter `{name}`: unknown type `{other}`"))),
        };
        let default = decl.get("default").cloned().ok_or_else(|| {
            at(format!(
                "parameter `{name}` has no `default`; a workflow must be runnable with no arguments"
            ))
        })?;
        if let ParamType::Enum(values) = &ty
            && !default
                .as_str()
                .is_some_and(|d| values.iter().any(|v| v == d))
        {
            return Err(at(format!(
                "parameter `{name}`: default `{default}` is not one of its values"
            )));
        }
        out.insert(
            name.clone(),
            Param {
                ty,
                default,
                max: decl["max"].as_i64(),
            },
        );
    }
    Ok(out)
}

fn parse_retry(v: &Value, at: &dyn Fn(String) -> String) -> Result<Retry, String> {
    known(v, &["max", "while", "escalate"], "retry").map_err(at)?;
    let max = count(&v["max"], "retry max").map_err(at)?;
    let predicate = v["while"]
        .as_str()
        .ok_or_else(|| at("retry needs a `while`".into()))?
        .to_string();
    let escalate = match &v["escalate"] {
        Value::Null => None,
        e => Some(
            e["model"]
                .as_str()
                .ok_or_else(|| at("escalate needs a model".into()))?
                .to_string(),
        ),
    };
    Ok(Retry {
        max,
        predicate,
        escalate,
    })
}

fn parse_node(wf: &str, v: &Value) -> Result<Node, String> {
    let name = v["name"]
        .as_str()
        .ok_or_else(|| format!("workflow `{wf}`: a node with no `name`"))?
        .to_string();
    let at = |e: String| format!("workflow `{wf}`: node `{name}`: {e}");
    known(
        v,
        &[
            "name",
            "op",
            "via",
            "in",
            "out",
            "emits",
            "writes",
            "max_units",
            "max_depth",
            "group_by",
            "task",
            "rule",
            "doc",
            "publish",
            "check",
            "retry",
            "sink",
            "action",
            "map",
            "workflow",
            "with",
        ],
        &format!("workflow `{wf}` node `{name}`"),
    )?;
    let input = v["in"]
        .as_str()
        .ok_or_else(|| at("no `in` type".into()))?
        .to_string();
    let via = match v["via"].as_str() {
        None => None,
        Some("agent") => Some(Via::Agent),
        Some("rule") => Some(Via::Rule),
        Some(other) => return Err(at(format!("`via` is agent or rule, not `{other}`"))),
    };
    let task = v["task"].as_str().map(String::from);
    let rule = v["rule"].as_str().map(String::from);
    let retry = match &v["retry"] {
        Value::Null => None,
        r => Some(parse_retry(r, &at)?),
    };
    let strings = |key: &str| -> Result<Vec<String>, String> {
        match &v[key] {
            Value::Null => Ok(Vec::new()),
            Value::Array(a) => a
                .iter()
                .map(|s| {
                    s.as_str()
                        .map(String::from)
                        .ok_or_else(|| at(format!("`{key}` takes field names")))
                })
                .collect(),
            _ => Err(at(format!("`{key}` must be a list"))),
        }
    };
    let op = match v["op"].as_str() {
        None => return Err(at("no `op`".into())),
        Some("map") => {
            let out = v["out"]
                .as_str()
                .and_then(Out::parse)
                .ok_or_else(|| at("`out` is 0..n, 1 or 0..1".into()))?;
            Op::Map(MapNode {
                via: via.unwrap_or(Via::Agent),
                out,
                emits: v["emits"]
                    .as_str()
                    .ok_or_else(|| at("a map states what it `emits`".into()))?
                    .to_string(),
                writes: strings("writes")?,
                max_units: match &v["max_units"] {
                    Value::Null => None,
                    c => Some(count(c, "max_units").map_err(&at)?),
                },
                max_depth: match &v["max_depth"] {
                    Value::Null => None,
                    c => Some(count(c, "max_depth").map_err(&at)?),
                },
                task,
                rule,
                doc: v["doc"].as_str().map(String::from),
                publish: v["publish"].as_bool().unwrap_or(false),
                retry,
            })
        }
        Some("reduce") => Op::Reduce(ReduceNode {
            via: via.unwrap_or(Via::Agent),
            emits: v["emits"]
                .as_str()
                .ok_or_else(|| at("a reduce states what it `emits`".into()))?
                .to_string(),
            group_by: strings("group_by")?,
            task,
            rule,
        }),
        Some("edit") => Op::Edit(EditNode {
            task: task.ok_or_else(|| at("an edit needs a `task`".into()))?,
            check: v["check"].as_str().map(String::from),
            retry,
        }),
        Some("check") => Op::Check {
            rule: rule.ok_or_else(|| at("a check needs a `rule`".into()))?,
        },
        Some("emit") => {
            let sink = match v["sink"].as_str() {
                Some("todo") => Sink::Todo,
                Some("note") => Sink::Note,
                Some("doc") => Sink::Doc,
                Some("issue") => {
                    return Err(at(
                        "the issue sink is refused: `pma sync` owns issue creation".into(),
                    ));
                }
                Some(other) => return Err(at(format!("unknown sink `{other}`"))),
                None => return Err(at("an emit names a `sink`".into())),
            };
            let action = match v["action"].as_str() {
                None | Some("add") => Action::Add,
                Some("tick") => Action::Tick,
                Some("remove") => Action::Remove,
                Some(other) => return Err(at(format!("unknown action `{other}`"))),
            };
            let map = v["map"]
                .as_object()
                .ok_or_else(|| at("an emit needs a `map` from sink field to placeholder".into()))?
                .iter()
                .map(|(k, val)| {
                    val.as_str()
                        .map(|s| (k.clone(), s.to_string()))
                        .ok_or_else(|| at(format!("`map.{k}` must be a placeholder string")))
                })
                .collect::<Result<BTreeMap<_, _>, String>>()?;
            Op::Emit(EmitNode { sink, action, map })
        }
        Some("call") => Op::Call(CallNode {
            workflow: v["workflow"]
                .as_str()
                .ok_or_else(|| at("a call names a `workflow`".into()))?
                .to_string(),
            with: v["with"]
                .as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
        }),
        Some(other) => return Err(at(format!("unknown op `{other}`"))),
    };
    Ok(Node { name, input, op })
}

fn parse_guard(v: &Value, at: &dyn Fn(String) -> String) -> Result<Guard, String> {
    let map = v
        .as_object()
        .ok_or_else(|| at("`when` must be an object".into()))?;
    let mut tests = Vec::new();
    for (key, test) in map {
        let key = match key.strip_prefix('@') {
            Some(rest) => Key::System(rest.to_string()),
            None => Key::Field(key.clone()),
        };
        let test = match test {
            Value::Array(a) => Test::In(
                a.iter()
                    .map(|v| {
                        v.as_str()
                            .map(String::from)
                            .ok_or_else(|| at("a membership test takes strings".into()))
                    })
                    .collect::<Result<Vec<_>, String>>()?,
            ),
            Value::String(s) if s == "present" => Test::Present,
            Value::String(s) if s == "absent" => Test::Absent,
            Value::String(s) => {
                return Err(at(format!("`{s}` is not a test; use present or absent")));
            }
            Value::Object(o) if o.len() == 1 => {
                let (op, n) = o.iter().next().expect("one entry");
                let cmp =
                    Cmp::parse(op).ok_or_else(|| at(format!("`{op}` is not a comparison")))?;
                Test::Compare(cmp, count(n, "a comparison").map_err(at)?)
            }
            _ => {
                return Err(at(
                    "a test is a list, present, absent, or one comparison".into()
                ));
            }
        };
        tests.push((key, test));
    }
    Ok(Guard(tests))
}

fn parse_edge(wf: &str, i: usize, v: &Value) -> Result<Edge, String> {
    let at = |e: String| format!("workflow `{wf}`: edge {}: {e}", i + 1);
    known(
        v,
        &["from", "to", "when", "default", "max_laps"],
        &format!("workflow `{wf}` edge {}", i + 1),
    )?;
    let from = match v["from"].as_str() {
        Some("@input") => From::Input,
        Some("@output") => return Err(at("`@output` is a destination, not a source".into())),
        Some(n) => From::Node(n.to_string()),
        None => return Err(at("no `from`".into())),
    };
    let to = match v["to"].as_str() {
        Some("@output") => To::Output,
        Some("@input") => return Err(at("`@input` is a source, not a destination".into())),
        Some(n) => To::Node(n.to_string()),
        None => return Err(at("no `to`".into())),
    };
    let when = match &v["when"] {
        Value::Null => None,
        g => Some(parse_guard(g, &at)?),
    };
    let default = v["default"].as_bool().unwrap_or(false);
    if default && when.is_some() {
        return Err(at("a default edge takes no `when`".into()));
    }
    Ok(Edge {
        from,
        to,
        when,
        default,
        max_laps: match &v["max_laps"] {
            Value::Null => None,
            c => Some(count(c, "max_laps").map_err(&at)?),
        },
    })
}

fn parse_workflow(v: &Value) -> Result<Workflow, String> {
    let name = v["name"]
        .as_str()
        .ok_or("a workflow with no `name`")?
        .to_string();
    let at = |e: String| format!("workflow `{name}`: {e}");
    known(
        v,
        &[
            "name", "in", "out", "effects", "params", "caps", "nodes", "edges",
        ],
        &format!("workflow `{name}`"),
    )?;
    let input = v["in"]
        .as_str()
        .ok_or_else(|| at("no `in` type".into()))?
        .to_string();
    let output = v["out"].as_str().map(String::from);
    let mut effects = Effects::default();
    if let Some(list) = v["effects"].as_array() {
        for e in list {
            match e.as_str() {
                Some("repo") => effects.repo = true,
                Some("writes") => effects.writes = true,
                other => {
                    return Err(at(format!(
                        "unknown effect `{}`; expected repo or writes",
                        other.unwrap_or("")
                    )));
                }
            }
        }
    }
    let params = match &v["params"] {
        Value::Null => BTreeMap::new(),
        p => parse_params(p, &at)?,
    };
    let caps = {
        let c = &v["caps"];
        known(
            c,
            &["max_units", "max_edits"],
            &format!("workflow `{name}` caps"),
        )?;
        Caps {
            max_units: c["max_units"]
                .as_i64()
                .ok_or_else(|| at("caps need `max_units`".into()))?,
            max_edits: c["max_edits"]
                .as_i64()
                .ok_or_else(|| at("caps need `max_edits`".into()))?,
        }
    };
    let nodes = v["nodes"]
        .as_array()
        .ok_or_else(|| at("no `nodes` array".into()))?
        .iter()
        .map(|n| parse_node(&name, n))
        .collect::<Result<Vec<_>, String>>()?;
    if nodes.is_empty() {
        return Err(at("a workflow with no nodes does nothing".into()));
    }
    let edges = v["edges"]
        .as_array()
        .ok_or_else(|| at("no `edges` array".into()))?
        .iter()
        .enumerate()
        .map(|(i, e)| parse_edge(&name, i, e))
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Workflow {
        name,
        input,
        output,
        effects,
        params,
        caps,
        nodes,
        edges,
    })
}

// ---------------------------------------------------------------- validation

impl Document {
    fn validate(&self) -> Result<(), String> {
        let mut seen = BTreeSet::new();
        for w in &self.workflows {
            if !seen.insert(&w.name) {
                return Err(format!("workflow `{}` is declared twice", w.name));
            }
        }
        for w in &self.workflows {
            self.validate_workflow(w)?;
        }
        self.validate_calls()?;
        Ok(())
    }

    /// The type a node's units carry when they leave it.
    fn output_type(&self, w: &Workflow, node: &Node) -> Result<String, String> {
        Ok(match &node.op {
            Op::Map(m) => m.emits.clone(),
            Op::Reduce(r) => r.emits.clone(),
            // An edit records a run against its unit and passes the unit
            // on: the patch is the run, reachable as `@run` and through the
            // verdicts its check wrote. A graph that fixes and then audits
            // must carry the thing being fixed, not a description of a diff.
            Op::Edit(_) | Op::Check { .. } | Op::Emit(_) => node.input.clone(),
            Op::Call(c) => {
                let callee = self.workflow(&c.workflow).ok_or_else(|| {
                    format!(
                        "workflow `{}`: node `{}`: unknown workflow `{}`",
                        w.name, node.name, c.workflow
                    )
                })?;
                callee.output.clone().ok_or_else(|| {
                    format!(
                        "workflow `{}`: node `{}`: `{}` returns nothing, so nothing may read it",
                        w.name, node.name, c.workflow
                    )
                })?
            }
        })
    }

    fn validate_workflow(&self, w: &Workflow) -> Result<(), String> {
        let at = |e: String| format!("workflow `{}`: {e}", w.name);
        let node_at = |n: &str, e: String| format!("workflow `{}`: node `{n}`: {e}", w.name);

        if self.resolve(&w.input).is_none() {
            return Err(at(format!("unknown input type `{}`", w.input)));
        }
        if let Some(out) = &w.output
            && self.resolve(out).is_none()
        {
            return Err(at(format!("unknown output type `{out}`")));
        }
        for (name, p) in &w.params {
            if let ParamType::Enum(values) = &p.ty
                && values.is_empty()
            {
                return Err(at(format!("parameter `{name}`: an enum with no values")));
            }
        }

        let mut names = BTreeSet::new();
        for n in &w.nodes {
            if !names.insert(n.name.clone()) {
                return Err(at(format!("node `{}` is declared twice", n.name)));
            }
            if n.name.contains('/') {
                return Err(node_at(&n.name, "a node name may not contain `/`".into()));
            }
            if self.resolve(&n.input).is_none() {
                return Err(node_at(&n.name, format!("unknown type `{}`", n.input)));
            }
            self.validate_node(w, n)?;
        }

        // Edges: endpoints, duplicates, and type agreement.
        let mut pairs = BTreeSet::new();
        for (i, e) in w.edges.iter().enumerate() {
            let at = |msg: String| format!("workflow `{}`: edge {}: {msg}", w.name, i + 1);
            let from_type = match &e.from {
                From::Input => w.input.clone(),
                From::Node(n) => {
                    let node = w.node(n).ok_or_else(|| at(format!("unknown node `{n}`")))?;
                    self.output_type(w, node)?
                }
            };
            match &e.to {
                To::Output => {
                    let out = w
                        .output
                        .as_deref()
                        .ok_or_else(|| at("this workflow returns nothing".into()))?;
                    if out != from_type {
                        return Err(at(format!(
                            "carries `{from_type}` into `@output`, which returns `{out}`"
                        )));
                    }
                }
                To::Node(n) => {
                    let node = w.node(n).ok_or_else(|| at(format!("unknown node `{n}`")))?;
                    if node.input != from_type {
                        return Err(at(format!(
                            "carries `{from_type}` into node `{n}`, which reads `{}`",
                            node.input
                        )));
                    }
                }
            }
            let pair = (e.from.clone(), e.to.clone());
            if !pairs.insert(pair) {
                return Err(at(
                    "that pair already has an edge; use one guard or two targets".into(),
                ));
            }
            if let Some(g) = &e.when {
                self.validate_guard(w, &from_type, g, &at)?;
            }
            if let Some(c) = &e.max_laps {
                self.bounded(w, c, &at, "max_laps")?;
                let From::Node(src) = &e.from else {
                    return Err(at("a lap edge cannot start at `@input`".into()));
                };
                if !w.edges.iter().any(|o| o.from == e.from && o.default) {
                    return Err(at(format!(
                        "node `{src}` has a lap edge and no default edge, so a unit that \
                         exhausts its laps has nowhere to go"
                    )));
                }
            }
        }

        self.validate_reachable(w)?;
        self.validate_acyclic(w)?;
        let effects = self.effects_of(w, &mut Vec::new())?;
        if effects != w.effects {
            let show = |e: Effects| match e.names().as_slice() {
                [] => "pure".to_string(),
                names => names.join(", "),
            };
            return Err(at(format!(
                "declares effects {}, but its graph has {}",
                show(w.effects),
                show(effects)
            )));
        }
        Ok(())
    }

    /// A count that feeds a bound must be knowable before a run: a constant,
    /// or a parameter that states its own maximum.
    fn bounded(
        &self,
        w: &Workflow,
        c: &Count,
        at: &dyn Fn(String) -> String,
        what: &str,
    ) -> Result<(), String> {
        match c {
            Count::Fixed(n) if *n < 0 => Err(at(format!("{what} cannot be negative"))),
            Count::Fixed(_) => Ok(()),
            Count::Param(p) => match w.params.get(p) {
                None => Err(at(format!("{what} names undeclared parameter `{p}`"))),
                Some(param) if param.max.is_none() => Err(at(format!(
                    "{what} uses parameter `{p}`, which declares no `max`; a bound must be \
                     knowable before a run"
                ))),
                Some(_) => Ok(()),
            },
        }
    }

    fn validate_node(&self, w: &Workflow, n: &Node) -> Result<(), String> {
        let at = |e: String| format!("workflow `{}`: node `{}`: {e}", w.name, n.name);
        let self_edge = w
            .edges
            .iter()
            .any(|e| e.from == From::Node(n.name.clone()) && e.to == To::Node(n.name.clone()));
        let input = self.resolve(&n.input).expect("checked by the caller");
        match &n.op {
            Op::Map(m) => {
                if self.resolve(&m.emits).is_none() {
                    return Err(at(format!("unknown type `{}`", m.emits)));
                }
                match m.via {
                    Via::Agent if m.task.is_none() => {
                        return Err(at("an agent node needs a `task`".into()));
                    }
                    Via::Rule if m.rule.is_none() => {
                        return Err(at("a rule node needs a `rule`".into()));
                    }
                    _ => {}
                }
                if let Some(rule) = &m.rule {
                    self.validate_map_rule(w, n, rule)?;
                }
                if m.out == Out::Grows {
                    let units = m
                        .max_units
                        .as_ref()
                        .ok_or_else(|| at("a `0..n` map must state `max_units`".into()))?;
                    self.bounded(w, units, &at, "max_units")?;
                } else if !m.writes.is_empty() {
                    let emits = self.resolve(&m.emits).expect("checked above");
                    for f in &m.writes {
                        if !emits.fields.contains_key(f) {
                            return Err(at(format!(
                                "`writes` names no field `{f}` of `{}`",
                                m.emits
                            )));
                        }
                    }
                }
                if self_edge {
                    let depth = m
                        .max_depth
                        .as_ref()
                        .ok_or_else(|| at("a self-edge must state `max_depth`".into()))?;
                    self.bounded(w, depth, &at, "max_depth")?;
                    if m.out != Out::Grows {
                        return Err(at("only a `0..n` map may have a self-edge".into()));
                    }
                }
                if let Some(t) = &m.task {
                    self.placeholders(w, &input, t, &at)?;
                }
                if let Some(d) = &m.doc {
                    self.placeholders(w, &input, d, &at)?;
                }
                if let Some(r) = &m.retry {
                    self.bounded(w, &r.max, &at, "retry max")?;
                }
            }
            Op::Reduce(r) => {
                if self.resolve(&r.emits).is_none() {
                    return Err(at(format!("unknown type `{}`", r.emits)));
                }
                match r.via {
                    Via::Agent if r.task.is_none() => {
                        return Err(at("an agent node needs a `task`".into()));
                    }
                    Via::Rule => {
                        let rule = r
                            .rule
                            .as_deref()
                            .ok_or_else(|| at("a rule node needs a `rule`".into()))?;
                        if !REDUCE_RULES.contains(&rule)
                            && !REDUCE_RULE_PREFIXES.iter().any(|p| rule.starts_with(p))
                        {
                            return Err(at(format!("unknown reduce rule `{rule}`")));
                        }
                    }
                    _ => {}
                }
                for f in &r.group_by {
                    if !input.fields.contains_key(f) {
                        return Err(at(format!(
                            "`group_by` names no field `{f}` of `{}`",
                            n.input
                        )));
                    }
                }
                if let Some(t) = &r.task {
                    self.placeholders(w, &input, t, &at)?;
                }
            }
            Op::Edit(e) => {
                self.placeholders(w, &input, &e.task, &at)?;
                if let Some(c) = &e.check
                    && !CHECK_RULES.contains(&c.as_str())
                {
                    return Err(at(format!("unknown check `{c}`")));
                }
                if let Some(r) = &e.retry {
                    self.bounded(w, &r.max, &at, "retry max")?;
                }
            }
            Op::Check { rule } => {
                if !CHECK_RULES.contains(&rule.as_str()) {
                    return Err(at(format!("unknown check rule `{rule}`")));
                }
            }
            Op::Emit(e) => {
                for (field, placeholder) in &e.map {
                    self.placeholders(w, &input, placeholder, &at)?;
                    if e.sink == Sink::Todo && e.action == Action::Add && field == "priority" {
                        continue;
                    }
                }
                if e.sink == Sink::Todo && e.action == Action::Add && !e.map.contains_key("text") {
                    return Err(at("adding an item needs a `text` mapping".into()));
                }
            }
            Op::Call(c) => {
                let callee = self
                    .workflow(&c.workflow)
                    .ok_or_else(|| at(format!("unknown workflow `{}`", c.workflow)))?;
                if callee.input != n.input {
                    return Err(at(format!(
                        "`{}` reads `{}`, but this node is given `{}`",
                        c.workflow, callee.input, n.input
                    )));
                }
                for (key, value) in &c.with {
                    let param = callee.params.get(key).ok_or_else(|| {
                        at(format!("`{}` declares no parameter `{key}`", c.workflow))
                    })?;
                    if let Some(p) = value.as_str().and_then(param_ref) {
                        if !w.params.contains_key(&p) {
                            return Err(at(format!("`{key}` names undeclared parameter `{p}`")));
                        }
                    } else if let ParamType::Enum(values) = &param.ty
                        && !value
                            .as_str()
                            .is_some_and(|v| values.iter().any(|x| x == v))
                    {
                        return Err(at(format!(
                            "`{key}` is `{value}`, which is not one of its values"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_map_rule(&self, w: &Workflow, n: &Node, rule: &str) -> Result<(), String> {
        let at = |e: String| format!("workflow `{}`: node `{}`: {e}", w.name, n.name);
        if MAP_RULES.contains(&rule) {
            return Ok(());
        }
        let Some(prefix) = MAP_RULE_PREFIXES.iter().find(|p| rule.starts_with(**p)) else {
            return Err(at(format!("unknown map rule `{rule}`")));
        };
        let arg = &rule[prefix.len()..];
        if arg.is_empty() {
            return Err(at(format!("rule `{rule}` names nothing after the colon")));
        }
        if *prefix == "as:" {
            let target = self
                .resolve(arg)
                .ok_or_else(|| at(format!("rule `{rule}`: unknown type `{arg}`")))?;
            let source = self.resolve(&n.input).expect("checked by the caller");
            for (field, decl) in &target.fields {
                if decl.required && !source.fields.contains_key(field) {
                    return Err(at(format!(
                        "rule `{rule}`: `{arg}` requires `{field}`, which `{}` does not have",
                        n.input
                    )));
                }
            }
        }
        Ok(())
    }

    /// `{field}` reads the unit, `{@field}` a system field or verdict, and
    /// `{$param}` a parameter. Each is checked where the document is read, so
    /// a prompt cannot name something that will never exist.
    fn placeholders(
        &self,
        w: &Workflow,
        unit: &Type,
        text: &str,
        at: &dyn Fn(String) -> String,
    ) -> Result<(), String> {
        const LITERALS: [&str; 3] = ["in", "out", "doc"];
        let mut rest = text;
        while let Some(start) = rest.find('{') {
            let Some(end) = rest[start..].find('}') else {
                break;
            };
            let name = &rest[start + 1..start + end];
            rest = &rest[start + end + 1..];
            if name.is_empty() {
                continue;
            }
            if let Some(param) = name.strip_prefix('$') {
                if !w.params.contains_key(param) {
                    return Err(at(format!("`{{${param}}}` names no parameter")));
                }
            } else if let Some(system) = name.strip_prefix('@') {
                if !SYSTEM_FIELDS.contains(&system) && !self.verdicts(w).contains(system) {
                    return Err(at(format!(
                        "`{{@{system}}}` names no system field or verdict"
                    )));
                }
            } else if !LITERALS.contains(&name) && !unit.fields.contains_key(name) {
                return Err(at(format!(
                    "`{{{name}}}` names no field of this node's input"
                )));
            }
        }
        Ok(())
    }

    /// Verdict names a graph can produce: the rules its checks run.
    fn verdicts(&self, w: &Workflow) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for n in &w.nodes {
            match &n.op {
                Op::Check { rule } => {
                    out.insert(rule.clone());
                }
                Op::Edit(e) => {
                    if let Some(c) = &e.check {
                        out.insert(c.clone());
                    }
                }
                Op::Call(c) => {
                    if let Some(callee) = self.workflow(&c.workflow) {
                        out.extend(self.verdicts(callee));
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn validate_guard(
        &self,
        w: &Workflow,
        unit_type: &str,
        g: &Guard,
        at: &dyn Fn(String) -> String,
    ) -> Result<(), String> {
        let unit = self
            .resolve(unit_type)
            .ok_or_else(|| at(format!("unknown type `{unit_type}`")))?;
        for (key, test) in &g.0 {
            let field = match key {
                Key::System(s) => {
                    if !SYSTEM_FIELDS.contains(&s.as_str()) && !self.verdicts(w).contains(s) {
                        return Err(at(format!("`@{s}` names no system field or verdict")));
                    }
                    None
                }
                Key::Field(f) => Some(
                    unit.fields
                        .get(f)
                        .ok_or_else(|| at(format!("`{f}` is no field of `{unit_type}`")))?,
                ),
            };
            match test {
                Test::In(values) => {
                    for v in values {
                        if let Some(p) = param_ref(v) {
                            if !w.params.contains_key(&p) {
                                return Err(at(format!("`{{${p}}}` names no parameter")));
                            }
                            continue;
                        }
                        if let Some(Field {
                            ty: FieldType::Enum { values: allowed },
                            ..
                        }) = field
                            && !allowed.iter().any(|a| a == v)
                        {
                            return Err(at(format!("`{v}` is not one of that enum's values")));
                        }
                    }
                }
                Test::Compare(_, c) => self.bounded(w, c, at, "a comparison")?,
                Test::Present | Test::Absent => {}
            }
        }
        Ok(())
    }

    fn validate_reachable(&self, w: &Workflow) -> Result<(), String> {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut queue: Vec<&str> = w
            .edges
            .iter()
            .filter(|e| e.from == From::Input)
            .filter_map(|e| match &e.to {
                To::Node(n) => Some(n.as_str()),
                To::Output => None,
            })
            .collect();
        if queue.is_empty() {
            return Err(format!(
                "workflow `{}`: no edge leaves `@input`, so nothing can run",
                w.name
            ));
        }
        while let Some(n) = queue.pop() {
            if !seen.insert(n) {
                continue;
            }
            for e in &w.edges {
                if e.from == From::Node(n.to_string())
                    && let To::Node(next) = &e.to
                {
                    queue.push(next.as_str());
                }
            }
        }
        match w.nodes.iter().find(|n| !seen.contains(n.name.as_str())) {
            Some(n) => Err(format!(
                "workflow `{}`: node `{}` is not reachable from `@input`",
                w.name, n.name
            )),
            None => Ok(()),
        }
    }

    /// Every cycle must be a declared lap edge. Kahn's algorithm over the
    /// graph with lap edges removed: what remains must be a DAG.
    fn validate_acyclic(&self, w: &Workflow) -> Result<(), String> {
        let forward: Vec<&Edge> = w.edges.iter().filter(|e| e.max_laps.is_none()).collect();
        let mut indegree: BTreeMap<&str, usize> =
            w.nodes.iter().map(|n| (n.name.as_str(), 0)).collect();
        for e in &forward {
            if let (From::Node(_), To::Node(to)) = (&e.from, &e.to)
                && let Some(d) = indegree.get_mut(to.as_str())
            {
                *d += 1;
            }
        }
        let mut ready: Vec<&str> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(n, _)| *n)
            .collect();
        let mut settled = 0;
        while let Some(n) = ready.pop() {
            settled += 1;
            for e in &forward {
                if e.from == From::Node(n.to_string())
                    && let To::Node(to) = &e.to
                    && let Some(d) = indegree.get_mut(to.as_str())
                {
                    *d -= 1;
                    if *d == 0 {
                        ready.push(to.as_str());
                    }
                }
            }
        }
        if settled < w.nodes.len() {
            return Err(format!(
                "workflow `{}`: a cycle that is not a `max_laps` back edge",
                w.name
            ));
        }
        Ok(())
    }

    /// Effects of a graph, following calls. The declared set must equal this.
    fn effects_of(&self, w: &Workflow, stack: &mut Vec<String>) -> Result<Effects, String> {
        if stack.contains(&w.name) {
            return Err(format!(
                "workflow `{}` calls itself, directly or through another; \
                 a call is resolved before a run, so it could not terminate",
                w.name
            ));
        }
        stack.push(w.name.clone());
        let mut effects = Effects::default();
        for n in &w.nodes {
            effects = effects.union(match &n.op {
                Op::Edit(_) => Effects {
                    repo: true,
                    writes: false,
                },
                Op::Emit(_) => Effects {
                    repo: false,
                    writes: true,
                },
                Op::Call(c) => {
                    let callee = self.workflow(&c.workflow).ok_or_else(|| {
                        format!("workflow `{}`: unknown `{}`", w.name, c.workflow)
                    })?;
                    self.effects_of(callee, stack)?
                }
                _ => Effects::default(),
            });
        }
        stack.pop();
        Ok(effects)
    }

    fn validate_calls(&self) -> Result<(), String> {
        for w in &self.workflows {
            self.effects_of(w, &mut Vec::new())?;
        }
        Ok(())
    }
}

// --------------------------------------------------------------------- bound

/// What one node can cost at worst.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeBound {
    pub node: String,
    /// Units arriving at it.
    pub units_in: i64,
    /// Units it can produce.
    pub units_out: i64,
    /// Runs it can start, laps and retries included.
    pub runs: i64,
    /// Of those, the ones that spend a model.
    pub agent_runs: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    pub workflow: String,
    pub per_node: Vec<NodeBound>,
    pub runs: i64,
    pub agent_runs: i64,
    pub edits: i64,
    pub cost: f64,
}

/// Multiplication that saturates rather than wrapping. A graph's worst case is
/// a product of declared constants, and a document that would overflow is
/// refused by the caps rather than by arithmetic.
fn times(a: i64, b: i64) -> i64 {
    a.saturating_mul(b)
}

impl Document {
    /// The worst case, walked over the graph at every parameter's declared
    /// maximum, so a bound never depends on what an invocation passed.
    /// `input_units` is the size of the argument bag.
    pub fn estimate(
        &self,
        workflow: &str,
        input_units: i64,
        agent_budget: f64,
    ) -> Result<Estimate, String> {
        let w = self
            .workflow(workflow)
            .ok_or_else(|| format!("unknown workflow `{workflow}`"))?;
        self.estimate_inner(w, input_units, agent_budget, &mut Vec::new())
    }

    fn estimate_inner(
        &self,
        w: &Workflow,
        input_units: i64,
        agent_budget: f64,
        stack: &mut Vec<String>,
    ) -> Result<Estimate, String> {
        if stack.contains(&w.name) {
            return Err(format!("workflow `{}` calls itself", w.name));
        }
        stack.push(w.name.clone());
        let order = self.topological(w);
        let mut out_units: BTreeMap<&str, i64> = BTreeMap::new();
        let mut per_node = Vec::new();
        let (mut runs, mut agent_runs, mut edits, mut cost) = (0i64, 0i64, 0i64, 0.0);

        for name in &order {
            let node = w.node(name).expect("from the node list");
            let mut units_in = 0i64;
            for e in &w.edges {
                if let To::Node(to) = &e.to
                    && to == name
                {
                    units_in = units_in.saturating_add(match &e.from {
                        From::Input => input_units,
                        From::Node(src) => *out_units.get(src.as_str()).unwrap_or(&0),
                    });
                }
            }
            units_in = units_in.min(w.caps.max_units);

            let laps: i64 = w
                .edges
                .iter()
                .filter(|e| matches!(&e.to, To::Node(to) if to == name))
                .filter_map(|e| e.max_laps.as_ref())
                .map(|c| c.ceiling(&w.params))
                .sum();
            let retry = match &node.op {
                Op::Map(m) => m.retry.as_ref(),
                Op::Edit(e) => e.retry.as_ref(),
                _ => None,
            }
            .map_or(0, |r| r.max.ceiling(&w.params));

            // A lap mints a new unit, so a lapped node's successor sees
            // one per lap. A retry does not: it is another attempt on the
            // same run, against the same unit.
            let units_seen = times(units_in, 1 + laps);
            let node_runs = times(units_seen, 1 + retry);
            let (units_out, node_agent_runs, node_edits, node_cost) = match &node.op {
                Op::Map(m) => {
                    let out = match m.out {
                        Out::Grows => {
                            let width = m.max_units.as_ref().map_or(0, |c| c.ceiling(&w.params));
                            let depth = m.max_depth.as_ref().map_or(0, |c| c.ceiling(&w.params));
                            // Each level multiplies by the width; the caps
                            // truncate what compounding would otherwise reach.
                            let mut total = 0i64;
                            let mut level = units_seen;
                            for _ in 0..=depth {
                                level = times(level, width);
                                total = total.saturating_add(level);
                            }
                            total
                        }
                        Out::Same | Out::Shrinks => units_seen,
                    };
                    let agent = matches!(m.via, Via::Agent);
                    (
                        out.min(w.caps.max_units),
                        if agent { node_runs } else { 0 },
                        0,
                        if agent {
                            node_runs as f64 * agent_budget
                        } else {
                            0.0
                        },
                    )
                }
                Op::Reduce(r) => {
                    let agent = matches!(r.via, Via::Agent);
                    (
                        units_seen,
                        if agent { node_runs } else { 0 },
                        0,
                        if agent {
                            node_runs as f64 * agent_budget
                        } else {
                            0.0
                        },
                    )
                }
                Op::Edit(_) => {
                    let capped = node_runs.min(w.caps.max_edits);
                    (
                        units_seen.min(w.caps.max_edits),
                        capped,
                        capped,
                        capped as f64 * agent_budget,
                    )
                }
                Op::Check { .. } | Op::Emit(_) => (units_seen, 0, 0, 0.0),
                Op::Call(c) => {
                    let callee = self.workflow(&c.workflow).expect("checked at validation");
                    let inner = self.estimate_inner(callee, units_in, agent_budget, stack)?;
                    let returned = inner
                        .per_node
                        .iter()
                        .filter(|b| {
                            callee.edges.iter().any(
                                |e| matches!((&e.from, &e.to), (From::Node(n), To::Output) if *n == b.node),
                            )
                        })
                        .map(|b| b.units_out)
                        .sum::<i64>();
                    (
                        returned.min(w.caps.max_units),
                        inner.agent_runs,
                        inner.edits,
                        inner.cost,
                    )
                }
            };

            // A call runs nothing itself; what it costs is its callee's.
            let node_runs = match &node.op {
                Op::Call(c) => self
                    .workflow(&c.workflow)
                    .and_then(|callee| {
                        self.estimate_inner(callee, units_in, agent_budget, stack)
                            .ok()
                    })
                    .map_or(node_runs, |e| e.runs),
                _ => node_runs,
            };
            out_units.insert(node.name.as_str(), units_out);
            runs = runs.saturating_add(node_runs);
            agent_runs = agent_runs.saturating_add(node_agent_runs);
            edits = edits.saturating_add(node_edits);
            cost += node_cost;
            per_node.push(NodeBound {
                node: node.name.clone(),
                units_in,
                units_out,
                runs: node_runs,
                agent_runs: node_agent_runs,
            });
        }
        stack.pop();
        Ok(Estimate {
            workflow: w.name.clone(),
            per_node,
            runs,
            agent_runs,
            edits: edits.min(w.caps.max_edits),
            cost,
        })
    }

    /// Node names in dependency order, ignoring lap edges. The graph is a DAG
    /// without them, which validation already established.
    fn topological(&self, w: &Workflow) -> Vec<String> {
        let forward: Vec<&Edge> = w.edges.iter().filter(|e| e.max_laps.is_none()).collect();
        let mut indegree: BTreeMap<&str, usize> =
            w.nodes.iter().map(|n| (n.name.as_str(), 0)).collect();
        for e in &forward {
            if let (From::Node(_), To::Node(to)) = (&e.from, &e.to)
                && let Some(d) = indegree.get_mut(to.as_str())
            {
                *d += 1;
            }
        }
        let mut ready: Vec<&str> = w
            .nodes
            .iter()
            .map(|n| n.name.as_str())
            .filter(|n| indegree[n] == 0)
            .collect();
        ready.reverse();
        let mut order = Vec::new();
        while let Some(n) = ready.pop() {
            order.push(n.to_string());
            for e in &forward {
                if e.from == From::Node(n.to_string())
                    && let To::Node(to) = &e.to
                    && let Some(d) = indegree.get_mut(to.as_str())
                {
                    *d -= 1;
                    if *d == 0 {
                        ready.insert(0, to.as_str());
                    }
                }
            }
        }
        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The library's first workflow, which every other test varies.
    const FIND_ISSUES: &str = r#"{
      "types": {
        "finding": {"fields": {
          "severity": {"type": "enum", "required": true, "values": ["critical", "high"]},
          "title":    {"type": "line", "required": true, "max": 200, "unique": "normalised"},
          "detail":   {"type": "lines", "max": 40},
          "reason":   {"type": "line", "max": 200}
        }}
      },
      "workflow": [
        {
          "name": "find-issues",
          "in": "project", "out": "finding", "effects": [],
          "params": {
            "review_doc": {"type": "name", "default": "REVIEW.md"},
            "breadth":    {"type": "int", "default": 20, "max": 40}
          },
          "caps": {"max_units": 60, "max_edits": 0},
          "nodes": [
            {"name": "review", "op": "map", "out": "0..n", "in": "project", "emits": "finding",
             "max_units": "{$breadth}", "doc": "{$review_doc}",
             "task": "Review `{name}`. Write prose to {doc} and findings to {out}."},
            {"name": "confirm", "op": "map", "out": "0..1", "in": "finding", "emits": "finding",
             "writes": ["reason"],
             "task": "Confirm each unit in {in}. Drop what you cannot prove, with a `reason`."},
            {"name": "dedupe", "op": "reduce", "via": "rule", "rule": "dedupe",
             "group_by": ["title"], "in": "finding", "emits": "finding"}
          ],
          "edges": [
            {"from": "@input", "to": "review"},
            {"from": "review", "to": "confirm"},
            {"from": "confirm", "to": "dedupe"},
            {"from": "dedupe", "to": "@output"}
          ]
        }
      ]
    }"#;

    fn doc() -> Document {
        Document::parse(FIND_ISSUES).expect("the library document parses")
    }

    #[test]
    fn a_library_workflow_parses_with_its_types_and_parameters() {
        let d = doc();
        let w = d.workflow("find-issues").unwrap();
        assert_eq!(w.input, "project");
        assert_eq!(w.output.as_deref(), Some("finding"));
        assert_eq!(w.effects, Effects::default());
        assert_eq!(w.nodes.len(), 3);
        assert_eq!(
            w.params["breadth"],
            Param {
                ty: ParamType::Int,
                default: Value::from(20),
                max: Some(40)
            }
        );
        let Op::Map(review) = &w.nodes[0].op else {
            panic!("the first node is a map")
        };
        assert_eq!(review.out, Out::Grows);
        assert_eq!(review.max_units, Some(Count::Param("breadth".into())));
        assert_eq!(review.doc.as_deref(), Some("{$review_doc}"));
    }

    /// Replacing a workflow with another of the same signature is the point of
    /// having one, so the types a document declares are what edges are checked
    /// against, not the node names.
    #[test]
    fn edges_must_agree_on_the_type_they_carry() {
        let broken = FIND_ISSUES.replace(
            r#"{"from": "review", "to": "confirm"}"#,
            r#"{"from": "@input", "to": "confirm"}"#,
        );
        let e = Document::parse(&broken).unwrap_err();
        assert!(
            e.contains("carries `project` into node `confirm`, which reads `finding`"),
            "{e}"
        );
    }

    #[test]
    fn an_output_edge_must_carry_the_declared_return_type() {
        let broken = FIND_ISSUES.replace(
            r#""out": "finding", "effects""#,
            r#""out": "project", "effects""#,
        );
        let e = Document::parse(&broken).unwrap_err();
        assert!(e.contains("into `@output`, which returns `project`"), "{e}");
    }

    #[test]
    fn a_growing_map_must_bound_what_it_produces() {
        let broken = FIND_ISSUES.replace(r#""max_units": "{$breadth}", "#, "");
        let e = Document::parse(&broken).unwrap_err();
        assert!(e.contains("`0..n` map must state `max_units`"), "{e}");
    }

    /// A bound that depended on the arguments would make the worst case
    /// unknowable before a run, which is the whole point of computing it.
    #[test]
    fn a_parameter_used_as_a_bound_must_declare_its_maximum() {
        let broken = FIND_ISSUES.replace(r#""default": 20, "max": 40"#, r#""default": 20"#);
        let e = Document::parse(&broken).unwrap_err();
        assert!(e.contains("declares no `max`"), "{e}");
        assert!(e.contains("max_units"), "{e}");
    }

    #[test]
    fn every_parameter_needs_a_default() {
        let broken = FIND_ISSUES.replace(
            r#""type": "name", "default": "REVIEW.md""#,
            r#""type": "name""#,
        );
        let e = Document::parse(&broken).unwrap_err();
        assert!(e.contains("has no `default`"), "{e}");
    }

    #[test]
    fn placeholders_are_checked_against_what_they_could_read() {
        for (from, to, want) in [
            ("{$review_doc}", "{$missing}", "names no parameter"),
            ("Review `{name}`", "Review `{title}`", "names no field"),
        ] {
            let broken = FIND_ISSUES.replace(from, to);
            let e = Document::parse(&broken).unwrap_err();
            assert!(e.contains(want), "{from} -> {to}: {e}");
        }
    }

    #[test]
    fn a_type_may_not_shadow_a_built_in_or_a_system_field() {
        let shadow = FIND_ISSUES.replace(r#""finding": {"fields""#, r#""project": {"fields""#);
        assert!(
            Document::parse(&shadow)
                .unwrap_err()
                .contains("built-in type")
        );
        let reserved = FIND_ISSUES.replace(
            r#""reason":   {"type": "line""#,
            r#""@id":   {"type": "line""#,
        );
        assert!(Document::parse(&reserved).unwrap_err().contains("reserved"));
    }

    #[test]
    fn an_unreachable_node_is_refused() {
        let broken = FIND_ISSUES.replace(r#"{"from": "confirm", "to": "dedupe"},"#, "");
        let e = Document::parse(&broken).unwrap_err();
        assert!(e.contains("node `dedupe` is not reachable"), "{e}");
    }

    #[test]
    fn an_unknown_field_is_refused_rather_than_ignored() {
        let broken = FIND_ISSUES.replace(
            r#""name": "find-issues","#,
            r#""name": "find-issues", "retries": 2,"#,
        );
        let e = Document::parse(&broken).unwrap_err();
        assert!(e.contains("unknown field `retries`"), "{e}");
    }

    /// The worst case reads every parameter's maximum, not its default: 40
    /// findings, not 20.
    #[test]
    fn the_bound_is_computed_at_the_declared_maximum() {
        let d = doc();
        let e = d.estimate("find-issues", 1, 1.0).unwrap();
        assert_eq!(e.per_node[0].units_out, 40, "review at breadth's max");
        assert_eq!(e.per_node[1].units_in, 40, "confirm sees them all");
        assert_eq!(e.agent_runs, 41, "one review, forty confirms");
        assert_eq!(e.edits, 0);
        assert!((e.cost - 41.0).abs() < f64::EPSILON, "{}", e.cost);
    }

    /// A rule costs nothing, so a graph of rules is free and says so.
    #[test]
    fn rule_nodes_are_free() {
        let d = doc();
        let e = d.estimate("find-issues", 1, 1.0).unwrap();
        let dedupe = e.per_node.iter().find(|b| b.node == "dedupe").unwrap();
        assert_eq!(dedupe.agent_runs, 0);
        assert_eq!(dedupe.runs, 40, "it still handles every unit");
    }

    const WITH_EDIT: &str = r#"{
      "types": {"finding": {"fields": {
        "title": {"type": "line", "required": true, "max": 200},
        "verdict": {"type": "enum", "values": ["accept", "reject"]},
        "reason": {"type": "line", "max": 200}
      }}},
      "workflow": [{
        "name": "apply-fixes",
        "in": "finding", "out": "finding", "effects": ["repo", "writes"],
        "params": {"laps": {"type": "int", "default": 2, "max": 3},
                   "retries": {"type": "int", "default": 1, "max": 2}},
        "caps": {"max_units": 40, "max_edits": 12},
        "nodes": [
          {"name": "fix", "op": "edit", "in": "finding", "check": "verify",
           "retry": {"max": "{$retries}", "while": "@verify != passed"},
           "task": "Fix `{title}` in `{@project}`."},
          {"name": "audit", "op": "map", "out": "1", "in": "finding", "emits": "finding",
           "writes": ["verdict", "reason"],
           "task": "Judge the diff for `{title}`. Set `verdict` and `reason`."},
          {"name": "handoff", "op": "emit", "in": "finding", "sink": "todo", "action": "add",
           "map": {"text": "{title}", "description": "{reason}"}}
        ],
        "edges": [
          {"from": "@input", "to": "fix"},
          {"from": "fix", "to": "audit"},
          {"from": "audit", "to": "@output", "when": {"verdict": ["accept"]}},
          {"from": "audit", "to": "fix", "when": {"verdict": ["reject"]}, "max_laps": "{$laps}"},
          {"from": "audit", "to": "handoff", "default": true}
        ]
      }]
    }"#;

    #[test]
    fn an_edit_and_a_sink_are_read_back_as_effects() {
        let d = Document::parse(WITH_EDIT).unwrap();
        let w = d.workflow("apply-fixes").unwrap();
        assert_eq!(
            w.effects,
            Effects {
                repo: true,
                writes: true
            }
        );
    }

    /// A caller must be able to tell from the signature whether calling this
    /// can change a repository, so the declaration is checked against the graph.
    #[test]
    fn declared_effects_must_match_the_graph() {
        let lying = WITH_EDIT.replace(r#""effects": ["repo", "writes"]"#, r#""effects": []"#);
        let e = Document::parse(&lying).unwrap_err();
        assert!(e.contains("declares effects pure"), "{e}");
        assert!(e.contains("repo, writes"), "{e}");
    }

    /// A unit that runs out of laps must have somewhere to go, or the work
    /// disappears with no record of why.
    #[test]
    fn a_lap_edge_needs_a_terminal_path() {
        let broken = WITH_EDIT.replace(
            r#",
          {"from": "audit", "to": "handoff", "default": true}"#,
            "",
        );
        let e = Document::parse(&broken).unwrap_err();
        assert!(e.contains("lap edge and no default edge"), "{e}");
    }

    #[test]
    fn a_cycle_that_is_not_a_lap_edge_is_refused() {
        let broken = WITH_EDIT.replace(r#", "max_laps": "{$laps}""#, "");
        let e = Document::parse(&broken).unwrap_err();
        assert!(
            e.contains("cycle that is not a `max_laps` back edge"),
            "{e}"
        );
    }

    /// Laps and retries both multiply the runs a node can start, and the
    /// instance cap is what finally holds an `edit`.
    #[test]
    fn laps_and_retries_multiply_and_the_cap_truncates() {
        let d = Document::parse(WITH_EDIT).unwrap();
        let e = d.estimate("apply-fixes", 2, 1.0).unwrap();
        let fix = e.per_node.iter().find(|b| b.node == "fix").unwrap();
        // 2 units x (1 + 3 laps) x (1 + 2 retries) = 24, capped at 12 edits.
        assert_eq!(fix.runs, 24);
        assert_eq!(fix.agent_runs, 12, "max_edits truncates");
        assert_eq!(e.edits, 12);
    }

    /// A lap mints a unit and a retry does not, so what a lapped node hands
    /// on grows with its laps while its attempts stay inside one run.
    #[test]
    fn a_lap_mints_a_unit_but_a_retry_does_not() {
        let d = Document::parse(WITH_EDIT).unwrap();
        let e = d.estimate("apply-fixes", 1, 1.0).unwrap();
        let at = |name: &str| e.per_node.iter().find(|b| b.node == name).unwrap();
        // One finding, three laps: four units through fix, twelve attempts.
        assert_eq!(at("fix").units_out, 4);
        assert_eq!(at("fix").runs, 12);
        // The auditor sees one unit per lap, and runs once for each.
        assert_eq!(at("audit").units_in, 4);
        assert_eq!(at("audit").runs, 4);
    }

    #[test]
    fn a_guard_may_only_read_what_its_units_carry() {
        let broken = WITH_EDIT.replace(r#"{"verdict": ["accept"]}"#, r#"{"outcome": ["accept"]}"#);
        let e = Document::parse(&broken).unwrap_err();
        assert!(e.contains("`outcome` is no field of `finding`"), "{e}");
        let bad_value =
            WITH_EDIT.replace(r#"{"verdict": ["accept"]}"#, r#"{"verdict": ["maybe"]}"#);
        let e = Document::parse(&bad_value).unwrap_err();
        assert!(e.contains("not one of that enum's values"), "{e}");
    }

    #[test]
    fn a_verdict_guard_names_a_check_the_graph_runs() {
        // `@verify` is written by the edit node's own check.
        assert!(Document::parse(WITH_EDIT).is_ok());
        let broken = WITH_EDIT.replace(
            r#"{"from": "fix", "to": "audit"}"#,
            r#"{"from": "fix", "to": "audit", "when": {"@merged": ["passed"]}}"#,
        );
        let e = Document::parse(&broken).unwrap_err();
        assert!(
            e.contains("`@merged` names no system field or verdict"),
            "{e}"
        );
    }

    const COMPOSED: &str = r#"{
      "types": {"finding": {"fields": {
        "severity": {"type": "enum", "required": true, "values": ["critical", "high"]},
        "title": {"type": "line", "required": true, "max": 200}
      }}},
      "workflow": [
        {"name": "look", "in": "project", "out": "finding", "effects": [],
         "params": {"breadth": {"type": "int", "default": 5, "max": 10}},
         "caps": {"max_units": 40, "max_edits": 0},
         "nodes": [{"name": "review", "op": "map", "out": "0..n", "in": "project",
                    "emits": "finding", "max_units": "{$breadth}",
                    "task": "Review `{name}`."}],
         "edges": [{"from": "@input", "to": "review"},
                   {"from": "review", "to": "@output"}]},
        {"name": "sweep", "in": "project", "out": "finding", "effects": ["writes"],
         "params": {},
         "caps": {"max_units": 80, "max_edits": 0},
         "nodes": [
           {"name": "issues", "op": "call", "workflow": "look", "in": "project",
            "with": {"breadth": 4}},
           {"name": "keep", "op": "emit", "in": "finding", "sink": "todo", "action": "add",
            "map": {"text": "{title}", "priority": "{severity}"}}
         ],
         "edges": [{"from": "@input", "to": "issues"},
                   {"from": "issues", "to": "keep"},
                   {"from": "keep", "to": "@output"}]}
      ]
    }"#;

    #[test]
    fn a_call_takes_its_callees_signature() {
        let d = Document::parse(COMPOSED).unwrap();
        let sweep = d.workflow("sweep").unwrap();
        assert_eq!(
            sweep.effects,
            Effects {
                repo: false,
                writes: true
            }
        );
        // The callee's bound composes into the caller's: 3 projects, 10 each
        // at the maximum, then one emit per finding and no model.
        let e = d.estimate("sweep", 3, 1.0).unwrap();
        let issues = e.per_node.iter().find(|b| b.node == "issues").unwrap();
        assert_eq!(issues.units_out, 30);
        assert_eq!(e.agent_runs, 3, "one review run per project");
    }

    #[test]
    fn a_call_must_agree_with_what_its_callee_reads() {
        let broken = COMPOSED.replace(
            r#""workflow": "look", "in": "project""#,
            r#""workflow": "look", "in": "finding""#,
        );
        let e = Document::parse(&broken).unwrap_err();
        assert!(e.contains("reads `project`"), "{e}");
    }

    #[test]
    fn a_call_may_not_pass_an_undeclared_parameter() {
        let broken = COMPOSED.replace(r#""with": {"breadth": 4}"#, r#""with": {"depth": 4}"#);
        let e = Document::parse(&broken).unwrap_err();
        assert!(e.contains("declares no parameter `depth`"), "{e}");
    }

    /// Flattening resolves a call before anything runs, so a workflow that
    /// reached itself could not terminate.
    #[test]
    fn a_workflow_may_not_call_itself() {
        let broken = COMPOSED.replace(
            r#""workflow": "look", "in": "project",
            "with": {"breadth": 4}"#,
            r#""workflow": "sweep", "in": "project""#,
        );
        let e = Document::parse(&broken).unwrap_err();
        assert!(e.contains("calls itself"), "{e}");
    }
}
