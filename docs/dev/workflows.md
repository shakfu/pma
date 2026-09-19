# pma workflows (draft)

Status: nothing here is built. 2026-09-19.

A workflow is a directed graph of operations over typed units of work. Nodes do work; edges carry units and decide, by rule, which node sees a unit next. Parallelism is a property of the graph, not a node kind: everything whose inputs are ready runs, bounded by `max_parallel` and the budget.

Five node primitives, three flow primitives, three forms of iteration, and one hard rule: **an agent produces data, a rule decides routing.**

This specifies the model, the document, the one routing change, the one migration, and the bound computed before a document may be activated. It is phase 6 of [implementation-plan.md](implementation-plan.md). The chat and container half is `~/projects/minos/docs/dev/design.md`; its section 5 calls a node a stage and assumes a sequence, which is one shape of the graph.

## 1. The test for a primitive

A verb earns a primitive when it changes what `pma` must check. Everything else is vocabulary: review, validate, triage, decompose, dedupe, fix and ship are the same five operations with different prompts.

Three dimensions generate the set.

| Dimension | Values | What it decides |
|-|-|-|
| cardinality | `0..n`, `1`, `0..1`, many to one | where parallel work is created and joined |
| effect | data, repository, outside world | which gates apply. Only a repository writer needs a worktree, base and head verify, a scope check, approval and ship |
| decider | agent or rule | an agent costs money, is nondeterministic and untrusted; a rule is free, deterministic and replayable |

## 2. The five node primitives

| Primitive | Shape | Decider | What `pma` checks -- its reason to exist |
|-|-|-|-|
| `map` | one unit in, `out` units out | agent or rule | the declared type; the `out` bound; at `0..1`, that kept ids are a subset of the input and each drop carries a reason; at `1` and `0..1`, that no field outside `writes` changed |
| `reduce` | many units in, one per group, out | agent or rule | provenance: every output unit names the inputs it came from |
| `edit` | one unit in, a patch to a worktree out | agent | all of phase 1: base and head verify, scope, attempts, approval evidence, ship. The only primitive that changes a repository |
| `check` | a unit or a bag in, a verdict out | rule | nothing. It is the checker |
| `emit` | a bag in, a write outside the repository | rule | the sink's rules: `TODO.md` lint and item identity, the `pma sync` conflict |

Remove any one and something becomes inexpressible: without `map`, no units are created or dropped; without `reduce`, no join; without `edit`, no code changes; without `check`, every verdict comes from a model, so guards read untrusted data; without `emit`, nothing leaves the workflow.

Domain verbs collapse onto them:

| Verb | Primitive |
|-|-|
| review a project, list candidates, decompose a task | `map out: 0..n`, agent |
| collect issues, read scan facts, list outdated dependencies, read `TODO.md` | `map out: 0..n`, rule |
| validate, confirm, triage out | `map out: 0..1`, agent, drop reason required |
| annotate, estimate, judge one unit | `map out: 1`, agent; the verdict is a field |
| select by field, drop duplicates, rank, limit | `map out: 0..1` or `reduce`, rule |
| synthesise one report from three reviews, choose the best of three patches | `reduce`, agent |
| fix, implement, upgrade, add a workflow file | `edit` |
| verify, lint, is CI green, did the pull request merge | `check` |
| write items, tick items, prune items, add a note, write a report | `emit` |

## 3. Units and bags

A **unit** is a typed record. A **bag** is a node's output: the units it wrote. An edge names a source node, so a bag needs no separate name.

System fields, on every unit, reserved and refused in a type declaration:

| Field | Meaning |
|-|-|
| `@id` | minted by the node that wrote the unit |
| `@type` | the declared type |
| `@node` | the node that wrote it |
| `@parent` | the unit it was derived from, or null |
| `@root` | the first unit of its lineage. The attempt counter keys on this |
| `@depth` | lineage depth, for recursion |
| `@lap` | how many times it has crossed a lap edge |
| `@children` | how many units this one produced, once its node has run |
| `@project` | the project it belongs to |
| `@<check>` | a verdict written by a check: `passed`, `failed` or `unknown` |

Units are immutable. Every node writes new units with `@parent` set, so a lap, an annotation and a decomposition all leave a readable chain. Nothing in the graph mutates state, which is why the frontier can be re-derived rather than stored.

## 4. Decisions

### The model

**W1. A workflow is a stored revision, proposed then activated.** The two-act shape is `routes`': a revision is a draft until someone activates it, and each run records the revision it ran under. Rejected: a file read at run time, which leaves no record of what ran.

**W2. Parallelism is derived from the graph.** There is no parallel node, fork, join or barrier. Two edges out of one node fan out; a `reduce` with several incoming edges fans in. Rejected: declared concurrency, which would be a second statement of what the edges already say.

**W3. An agent produces data; a rule decides routing.** Guards, checks and sinks are rules. A model's opinion enters as a field, and a rule reads the field. This is route.rs's existing commitment -- "the policy is an artifact, not a judgment per task" -- applied to the graph. Rejected: a guard that calls a model, which would let an untrusted party choose the graph's shape.

**W4. Conditions live on edges, never inside nodes.** A guarded edge is visible in the topology; a condition inside a node is not. A unit that no outgoing edge accepts is settled there, and the guard that refused it is recorded.

**W5. Readiness is derived, never stored.** A node is runnable when its incoming bags exist and its units have arrived. There is no cursor, so a crash resumes by re-deriving. This is plan 1.7's rule applied to sequencing: store the evidence, not the verdict.

**W6. `pma workflow run` is a pass, not a daemon.** It advances every runnable node of every named instance and exits, holding `session.lock` for the pass like `pma dispatch`. A boundary that needs a human ends the pass; the next invocation resumes it. Waiting for an external condition is the same mechanism: a `check` node is simply not ready. Rejected: a resident process, which would hold the lock across a human decision.

**W7. Agent and model live in the routing policy, not in the workflow document.** A node names no agent. `node` and `lap` become match dimensions on a route (section 10). Plan 2.5 refused a second place to name them: "a second place to say the same thing would have to be reconciled with it."

**W8. A route that states no `node` matches only a dispatch with no node.** Node is a partition, not a filter. Rejected: unstated means any, under which an existing policy's trailing catch-all absorbs every node silently at whatever approval it names.

**W9. A workflow is not an escape from the class rules.** Every `edit` run goes through `dispatch::prepare` and the phase 1 gates: class D is refused before a worktree exists, `TODO.md` and the privileged paths are refused to the classes that may not touch them, and `unattended` on A-, C or D is refused where a policy is read. A workflow chooses order and prompts, never authority.

**W10. `pma-agent` may propose a revision and request a trigger; it may not activate one.** minos design section 2 puts "which workflow a situation gets" behind a submission to `pma`, and D2 keeps mint, ship and push out of a model's hands. `pma workflow activate` records who activated it.

### Data and artifacts

**W11. A type is declared in the document, not registered in Rust.** A revision carries `types`; `pma` checks a node's output against the declaration it names. Rejected: a schema per workflow kind compiled in, which costs the same code and makes every new workflow a code change.

The declaration is deliberately weak: presence, type, enumerated values, length, uniqueness. It cannot express that a finding is real. That is what a `map out: 0..1` and the reviewer are for.

**W12. A `map out: 0..1` may not rewrite its input.** It returns ids to keep, plus fields named in `writes`. A validator that could edit a finding's text could launder work past the reviewer who reads it; the restriction is a trust boundary, not tidiness. Under `out: 1` the same rule bounds an annotation.

**W13. Files are how an agent reads and writes units; the store is where they live.** `pma` writes `in.json` before a run and reads `out.json` after, under `<data>/artifacts/<instance>/<node>/`. Prose artifacts sit beside them. Rejected: units as files only, which cannot record why a unit was dropped or which guard stopped it.

**W14. Artifacts live outside the worktree.** An untracked file there enters `dispatch::changed_paths`, counts as a violation for any class whose scope is bounded ([class.rs:101](../../src/class.rs)), and `git add -A` at ship publishes it. A node that wants its prose committed says `"publish": true`.

**W15. No node writes `TODO.md`. A node emits units and `pma` writes the items.** `OWNED` puts `TODO.md` outside every class's scope ([class.rs:34](../../src/class.rs)), the dispatch prompt says so ([dispatch.rs:502](../../src/dispatch.rs)), and plan 4.7 depends on it: ship ticks the item after the rebase, which is admissible only because no agent may touch the file.

**W16. An `edit` node takes its unit from the graph, not from `TODO.md`.** Dispatch requires the item open in the remote default branch (`dispatch::on_origin`), so an item written by an earlier node cannot be dispatched until it is committed and pushed. A unit-keyed task takes the existing no-item path, with key `workflow:<instance>:<root>`. A sink is therefore independent of an `edit`: switching `record` off changes nothing about what `fix` runs.

### Iteration

**W17. Three forms, chosen by what re-applies between tries.** If no gate runs between attempts, the loop belongs inside the agent's own turn, where `timeout` and `agent_budget` bound it and `pma` neither sees nor records it.

| Form | Example | Mechanism | Bound |
|-|-|-|-|
| retry | fix until `verify` passes | a node property; same worktree, appended attempts | `retry.max` |
| lap | fix, audit, fix again | a back edge | `max_laps`, counted on the unit |
| recursion | split until each piece is one change | a self-edge on `map out: 0..n` | `max_depth`, and the unit caps |
| waiting | until CI is green | none: a `check` node is not ready | none |

**W18. Retries and laps draw from one counter per lineage.** `exhaustion` is keyed on project and task revision and refuses a third attempt (plan 1.9). A lap that re-keyed itself would be a hole straight through that limit; a lap that kept the existing key would make `max_laps` above 2 dead on arrival. One `attempts_used` per `@root`, consumed by a retry and by a lap alike, with `retry.max` and `max_laps` as tighter local bounds under the code ceiling. A red check or a rejection consumes one; an infrastructure failure or a timeout consumes none, as today.

**W19. A lap edge requires a terminal path.** A document where a unit can exhaust its laps with nowhere to go is refused at parse. The usual terminal path emits the unit to `TODO.md`, which is the honest outcome of "the agent did not converge in two laps".

**W20. Recursion terminates on the model's own answer, bounded by depth.** A unit that produced no children is a leaf and routes onward; a unit that produced children is settled and its children re-enter at `@depth + 1`. Models judge "small enough" poorly, so the caps are the real bound. Rejected: a declared termination predicate over unit content, which is a second control-flow language.

**W21. No unbounded construct, and the bound is computed before activation.** Every loop bound and every `map out: 0..n` declares a constant, so the worst case is a product of constants. `pma workflow propose` prints the worst-case number of agent runs and the worst-case cost; `pma workflow activate` refuses a document whose worst case exceeds `workflow_budget`. What makes a loop safe to write down is not a promise to converge but a refusal to activate a graph that could cost more than you said.

## 5. The document

JSON, for the reason `route.rs` states: this crate parses JSON already. `types` sit beside `workflow` so two workflows share them. Nodes and edges are separate lists, so the topology is read in one place.

```json
{
  "types": {
    "finding": {
      "fields": {
        "id":       {"type": "id",    "required": true},
        "severity": {"type": "enum",  "required": true, "values": ["critical", "high", "medium", "low"]},
        "title":    {"type": "line",  "required": true, "max": 200, "unique": "normalised"},
        "detail":   {"type": "lines", "max": 40},
        "paths":    {"type": "list"},
        "class":    {"type": "enum",  "values": ["A", "A-", "B", "C", "D"]},
        "reason":   {"type": "line",  "max": 200}
      }
    }
  },
  "workflow": [
    {
      "name": "review-fix-critical",
      "input": {"type": "project"},
      "caps": {"max_units": 40, "max_edits": 6},
      "nodes": [],
      "edges": []
    }
  ]
}
```

### 5.1 Workflow

| Field | Type | Default | Meaning |
|-|-|-|-|
| `name` | string | required | Unique in the revision. Names every instance and run. |
| `input` | object | required | The root bag: `{"type": "project"}`, filled from the command's project or tag. |
| `caps` | object | required | `max_units` and `max_edits` per instance. Section 11. |
| `nodes` | array | required | At least one. |
| `edges` | array | required | Every node but the input's successors must be reachable. |

### 5.2 Node

| Field | Applies to | Default | Meaning |
|-|-|-|-|
| `name` | all | required | Unique. Matched by a route's `node` condition and stored on the run. |
| `op` | all | required | `map`, `reduce`, `edit`, `check`, `emit`. |
| `via` | map, reduce | `agent` | `agent` or `rule`. `edit` is agent only; `check` and `emit` are rule only. |
| `in` | all | required | The type it expects. Every incoming edge must carry it. |
| `out` | map | required | `0..n`, `1` or `0..1`. |
| `emits` | map, reduce | required | The type it writes. |
| `writes` | map at `1` or `0..1` | `[]` | Fields it may set. Any other change is refused (W12). |
| `max_units` | map at `0..n` | required | Units one input unit may yield. |
| `max_depth` | map with a self-edge | required | Lineage depth, counted from the root. |
| `group_by` | reduce | `[]` | Fields forming a group. Empty means one group. |
| `task` | agent nodes | required | The prompt. `{in}`, `{out}`, `{doc}`, `{project}` and `{field}` are replaced. |
| `rule` | rule nodes | required | A named builtin. Sections 7 and 8. |
| `doc` | map, agent | absent | A prose artifact the node also writes. |
| `publish` | map, agent | `false` | Copy `doc` into the worktree, so ship commits it. |
| `check` | edit | absent | A check run after the node. `verify` is the usual one. |
| `retry` | edit, map | absent | `{max, while, escalate}`. Section 6. |
| `sink` | emit | required | `todo`, `note` or `doc`. Section 8. |
| `action` | emit | `add` | `add`, `tick` or `remove`. |
| `map` | emit | required | Sink field to `{unit field}`. |

### 5.3 Edge

| Field | Default | Meaning |
|-|-|-|
| `from` | required | A node name, or `@input`. |
| `to` | required | A node name. |
| `when` | absent | A guard. All keys must hold. |
| `default` | `false` | Takes the units no guarded edge accepted. |
| `max_laps` | absent | Marks a back edge and bounds it. Requires a terminal path (W19). |

Guards, a closed set of forms:

| Form | Example |
|-|-|
| membership | `{"severity": ["critical", "high"]}` |
| number | `{"@children": {"==": 0}}`, `{"@depth": {"<": 2}}`. Operators `==`, `<`, `<=`, `>`, `>=` |
| presence | `{"paths": "present"}`, `{"reason": "absent"}` |
| verdict | `{"@verify": ["passed"]}` |

A bare name is a declared field; an `@` name is a system field or a verdict. No disjunction: two edges express it. A verdict guard matching only `passed` sends `unknown` to the default edge, which is why `unknown` is distinct from `failed` (the precedent is plan 1.6: a base check that could not start is unknown, not failing).

### 5.4 Refused where the document is read

For the reason `route.rs` gives -- a node that silently does nothing is worse than one that is refused:

- a duplicate type, workflow, node or edge

- an edge naming an unknown node, or a cycle that is not a `max_laps` back edge

- an incoming edge whose source `emits` differs from the target's `in`

- a `map out: 0..n` with no `max_units`, or a self-edge with no `max_depth`

- a `max_laps` edge with no terminal path for units that exhaust it

- a guard on an undeclared field, an unknown `@` name, or a value outside an enum's `values`

- a `{field}` placeholder in a `task` or an emit `map` naming no declared field

- a type declaring a reserved `@` name, or with no `id` field, or with two

- `via: rule` with no `rule`, or `via: agent` with no `task`

- `via: agent` on a `check` or an `emit`

- a worst case above `workflow_budget` (at activation, not at parse)

- an unknown field, at any level

## 6. Iteration, concretely

### Retry: the same node, the same gate

```json
{"name": "fix", "op": "edit", "in": "finding", "check": "verify",
 "retry": {"max": 2, "while": "@verify != passed", "escalate": {"model": "opus"}},
 "task": "Fix this in `{project}`:\n\n{title}\n\n{detail}"}
```

One run, up to three attempts, one worktree, cumulative cost, one `attempts` row each. This is the existing path made declarative: `escalate` is `retry.max: 1` with a model change (plan 4.3). `while` takes the same guard forms as an edge, negated with `!=` for a verdict.

### Lap: two nodes alternating

```json
{"from": "fix",   "to": "audit"},
{"from": "audit", "to": "fix",     "when": {"verdict": ["reject"]}, "max_laps": 2},
{"from": "audit", "to": "record",  "when": {"verdict": ["accept"]}},
{"from": "audit", "to": "handoff", "default": true}
```

`audit` is a `map out: 1` writing `verdict`; the guard reads it. The counter is on the unit, not the edge: two units crossing the same edge must not share a budget. Each lap mints a new unit whose `@parent` is the previous one and whose `@lap` is one higher, so the chain is readable and replay is exact.

`handoff` is the terminal path W19 requires. It takes the units that exhausted their laps and the ones whose verdict was `unknown`.

### Recursion: a self-edge with depth

```json
{"name": "split", "op": "map", "out": "0..n", "in": "task", "emits": "task",
 "max_units": 5, "max_depth": 2,
 "task": "If `{title}` needs more than one coherent change, return the independent subtasks. Otherwise return nothing."}
```

```json
{"from": "@input", "to": "split"},
{"from": "split",  "to": "split",     "when": {"@depth": {"<": 2}}},
{"from": "split",  "to": "implement", "when": {"@children": {"==": 0}}}
```

A unit that produced children is settled; its children re-enter `split`. A unit that produced none is a leaf and goes to `implement`. Termination is the model's own answer under a depth cap (W20).

### Waiting: not a loop

```json
{"name": "merged", "op": "check", "rule": "pr-merged", "in": "finding"}
```

The node is not runnable until the check passes. The pass ends, the next `pma workflow run` re-derives the frontier. This is what `ship.rs::settle` and the `pr-open` state already do (plan 4.8).

## 7. Checks

Rule only. Each writes `passed`, `failed` or `unknown`.

| `rule` | Reads |
|-|-|
| `verify` | the project's verify command against the run's tree. Already built (plan 1.6) |
| `scope-clean` | the run's recorded changed paths against its class (plan 1.7) |
| `lint-todo` | `pma lint` on the project's `TODO.md` |
| `ci-green` | required checks for the current head, through `gh` |
| `pr-merged` | the pull request's state, through `gh` |
| `nonempty` | whether the incoming bag holds at least one unit |

A check costs nothing and is replayable, which is why guards read checks rather than models.

## 8. Sinks

`emit` is where a workflow writes outside the repository. The mapping is in the document; the sink list is closed, because each one writes to a real file or table.

| `sink` | `action` | Writes |
|-|-|-|
| `todo` | `add` | items in the project's `TODO.md`, uncommitted, in the user's clone |
| `todo` | `tick` | marks matching items `[x]`, through `todo::mark_done` |
| `todo` | `remove` | removes finished items, through `todo::prune`. Refused for an open item |
| `note` | `add` | one portfolio note per unit, through `Store::add_note` |
| `doc` | `add` | a rendered file under the instance's artifact directory |
| `issue` | -- | refused. `pma sync` owns issue creation for `Critical` items (design.md, Sync); a second creator needs reconciling with it first |

`todo` writes are the `pma sync` precedent, not a ship batch: the edit is uncommitted and `scripts/commit_todo.py` commits it.

A new `todo::insert(text, priority, item, description) -> Option<String>`:

- inserts before the section's first `###` heading, or at the end of the section when it has none. Appending at the end would place the item under a trailing group heading and mislabel it.

- refuses when the `##` section heading is absent, rather than creating it.

- writes nothing when the file has lint errors, for the reason `pma sync` skips such a file: duplicate text breaks item identity.

## 9. Acceptance per op

| `op` | Clean when |
|-|-|
| `map out: 0..n` | `out.json` parses, every unit matches the declared type, count within `max_units`, `@depth` within `max_depth` |
| `map out: 1` | the above, plus the id is preserved and no field outside `writes` changed |
| `map out: 0..1` | the above, plus kept ids are a subset of the input, and every dropped id carries a non-empty reason |
| `reduce` | every output unit names its inputs, and the count is at most one per group |
| `edit` | the existing table of plan 1.8: base and head verify by class, scope respected |
| `check` | the rule ran. `unknown` is a result, not a failure |
| `emit` | the sink's rules held; every refusal is named per unit |
| a `doc` | the file exists, is a regular file, is non-empty, at most 1 MiB |

An acceptance failure adds a reason to `accept::review_reasons`. It approves and rejects nothing (plan 1.8). The unit routes by its guards; a node whose run was not clean sends its units to the default edge, or settles them when there is none.

## 10. Routing: `node` and `lap`

Two match dimensions, no new mechanism. `Policy::route` stays first match wins.

`src/route.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Subject<'a> {
    pub class: Class,
    pub complexity: i64,
    pub tier: Option<u8>,
    /// The workflow node this dispatch serves, or `None` for a task
    /// dispatched on its own.
    pub node: Option<&'a str>,
    /// Laps completed by the unit. 0 outside a workflow.
    pub lap: i64,
}
```

A shared reference is `Copy`, so `Subject` stays `Copy`. `Route` gains:

```rust
    /// Node names this route serves. `None` matches only a dispatch that
    /// names no node, so an existing policy keeps its behaviour exactly.
    pub nodes: Option<Vec<String>>,
    /// Laps this route serves, as an inclusive range.
    pub lap: Option<(i64, i64)>,
```

`Route::matches`, added before the class test:

```rust
        match (&self.nodes, s.node) {
            (None, None) => {}
            (None, Some(_)) | (Some(_), None) => return false,
            (Some(names), Some(node)) if !names.iter().any(|n| n == node) => return false,
            (Some(_), Some(_)) => {}
        }
        if let Some((lo, hi)) = self.lap
            && !(lo..=hi).contains(&s.lap)
        {
            return false;
        }
```

`parse_route` reads `match.node` with the shape `match.class` already has -- a name or a list -- and `match.lap` with the existing range parser, so `"1-2"` and `2` both work. An empty node list is refused. `Policy::to_json` writes both, so two revisions still diff by what they mean.

The `unattended` guard needs no change. It reads `classes`, so a node route with `unattended` and no class list is refused exactly as a task route is.

`route::replay` fills `node` from `run.node` and `lap` from `run.lap`. Runs recorded before migration 20 have `node` null and `lap` 0, hence the same routes they matched before: replaying an old revision over old runs reports no new difference. That is the migration-safety property to test.

A node no route matches refuses the dispatch by name, as a task with no matching route does today. There is no fallback to `pma config agent`.

The `lap` dimension is what makes per-lap specialisation a policy statement rather than a document one: `{"match": {"node": "fix", "lap": "1-2"}, "model": "opus", "approval": "each"}` puts a second lap on a stronger model under a human, without the workflow naming a model.

## 11. Bounds and cost

Worst case per node, walked from the input:

```
bound(@input)        = the instance's project count
bound(map 0..n)      = bound(in) x max_units, summed over depth <= max_depth
bound(map 1|0..1)    = bound(in)
bound(reduce)        = number of groups <= bound(in)
runs(node)           = bound(in) x (1 + sum of max_laps on incoming lap edges)
                                 x (1 + retry.max)
cost                 = sum over agent nodes of runs(node) x agent_budget
```

Truncated by `caps.max_units` and `caps.max_edits` per instance, which is what actually holds a recursive graph: `max_units` per node compounds with depth, the instance cap does not.

`pma workflow propose` prints the table. `pma workflow activate` refuses a document whose cost exceeds `workflow_budget`, a new setting under the config rules of the plan's "Configuration compatibility" section. A node that hits a cap leaves the remainder undispatched and names it, rather than truncating silently.

Rule nodes cost nothing and are excluded from the sum. A document whose graph is all rules has a worst case of zero, and section 14.10 is one.

## 12. Scheduling

One pass: collect every runnable node, run them, record, route the units, exit. Within a pass, runs are bounded by `max_parallel` and admitted by `batch_budget`, exactly as `pma dispatch` admits them today. `edit` runs get one worktree each, as they do now.

Two `edit` nodes over the same project in one pass produce two worktrees and two branches. That is today's behaviour under `--auto`, and the conflict at ship is plan 5.3's barrier problem, which this design does not solve.

## 13. Schema 20

`VERSION` is 19 ([store.rs:26](../../src/store.rs)); the migration list is indexed by version, so `WORKFLOWS` is appended to `steps` and `VERSION` becomes 20.

```rust
/// Version 20. A workflow is a graph of operations over typed units. Units
/// are immutable and carry their lineage, so a lap, an annotation and a
/// decomposition each leave a readable chain; a node's bag is derived from
/// the units it wrote, and the frontier from the moves recorded, so nothing
/// holds a cursor a crash could lose.
const WORKFLOWS: &str = "
CREATE TABLE workflows (
    revision INTEGER PRIMARY KEY,
    document TEXT NOT NULL,
    proposed_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    activated_at INTEGER,
    activated_by TEXT,
    worst_case_runs INTEGER,
    worst_case_cost REAL
);
CREATE TABLE workflow_instances (
    id INTEGER PRIMARY KEY,
    workflow TEXT NOT NULL,
    revision INTEGER NOT NULL REFERENCES workflows(revision),
    input TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    outcome TEXT
);
CREATE TABLE workflow_units (
    instance INTEGER NOT NULL REFERENCES workflow_instances(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    type TEXT NOT NULL,
    node TEXT NOT NULL,
    parent TEXT,
    root TEXT NOT NULL,
    depth INTEGER NOT NULL DEFAULT 0,
    lap INTEGER NOT NULL DEFAULT 0,
    project TEXT,
    data TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (instance, id)
);
CREATE INDEX idx_workflow_units_root ON workflow_units(instance, root);
CREATE TABLE workflow_moves (
    instance INTEGER NOT NULL REFERENCES workflow_instances(id) ON DELETE CASCADE,
    unit TEXT NOT NULL,
    edge INTEGER NOT NULL,
    taken INTEGER NOT NULL,
    reason TEXT,
    at INTEGER NOT NULL,
    PRIMARY KEY (instance, unit, edge)
);
CREATE TABLE workflow_verdicts (
    instance INTEGER NOT NULL REFERENCES workflow_instances(id) ON DELETE CASCADE,
    unit TEXT NOT NULL,
    check_name TEXT NOT NULL,
    verdict TEXT NOT NULL,
    detail TEXT,
    at INTEGER NOT NULL,
    PRIMARY KEY (instance, unit, check_name)
);
ALTER TABLE runs ADD COLUMN workflow_instance INTEGER;
ALTER TABLE runs ADD COLUMN node TEXT;
ALTER TABLE runs ADD COLUMN unit TEXT;
ALTER TABLE runs ADD COLUMN lap INTEGER NOT NULL DEFAULT 0;
";
```

| Column | On | Null or zero means |
|-|-|-|
| `workflow_instance` | `runs` | dispatched on its own, not by a workflow |
| `node` | `runs` | the same, and the routing partition of W8 reads it |
| `unit` | `runs` | not driven by a unit |
| `lap` | `runs` | never crossed a lap edge |

Notes on shape:

- A bag needs no table: it is the units whose `node` is that node. A unit's presence in a downstream node's input is `workflow_moves` with `taken = 1`.

- `workflow_moves.reason` holds the guard that refused a unit, so "why did F3 stop" is answerable without re-deriving anything.

- No `shadow` column on `workflows`. `routes` has one because a route can be computed and not applied; a workflow has no counterfactual to compute. `pma route replay` covers the routing half of a node's decision.

- No foreign key from `runs` to `workflow_instances`. `runs` is the calibration corpus and rows are never deleted, so a run must survive a workflow being removed. `runs.route_revision` is unconstrained for the same reason.

- The exhaustion counter needs no schema change: its `revision` column is text, and a unit-driven task supplies `workflow:<instance>:<root>`, which is what makes retries and laps draw from one budget (W18).

- `dispatch::without_item` gains the `workflow:` prefix beside `campaign:`.

## 14. Examples

Ten graphs. Each states the primitives it exercises, its worst case, and where the human is. `->` is an edge, `[...]` a guard, `=>` a sink.

### 14.1 Review, validate, fix

The original request. One fork: the confirmed findings are recorded for the human and, separately, the critical ones are fixed.

```
@input -> review -> confirm -> dedupe -+-> record => TODO.md
                                        \-> fix [severity=critical] -> review, ship
```

```json
{
  "name": "review-fix-critical",
  "input": {"type": "project"},
  "caps": {"max_units": 40, "max_edits": 6},
  "nodes": [
    {"name": "review", "op": "map", "out": "0..n", "in": "project", "emits": "finding",
     "max_units": 20, "doc": "REVIEW.md",
     "task": "Review `{project}`. Write your prose to {doc} and your findings to {out} as `finding` units."},

    {"name": "confirm", "op": "map", "out": "0..1", "in": "finding", "emits": "finding",
     "writes": ["reason"],
     "task": "Each unit in {in} is a claim. Confirm it against the code. Keep the ones you can prove; drop the rest with a `reason`. Change nothing else."},

    {"name": "dedupe", "op": "reduce", "via": "rule", "rule": "dedupe",
     "group_by": ["title"], "in": "finding", "emits": "finding"},

    {"name": "record", "op": "emit", "in": "finding", "sink": "todo", "action": "add",
     "map": {"priority": "{severity}", "text": "{title}", "description": "{detail}"}},

    {"name": "fix", "op": "edit", "in": "finding", "check": "verify",
     "retry": {"max": 1, "while": "@verify != passed", "escalate": {"model": "opus"}},
     "task": "Fix this in `{project}`:\n\n{title}\n\n{detail}"}
  ],
  "edges": [
    {"from": "@input", "to": "review"},
    {"from": "review", "to": "confirm"},
    {"from": "confirm", "to": "dedupe"},
    {"from": "dedupe", "to": "record"},
    {"from": "dedupe", "to": "fix", "when": {"severity": ["critical"]}}
  ]
}
```

Worst case: 1 review, 20 confirms, 0 reduce runs (a rule), 6 edits at 2 attempts each. 33 agent runs at `agent_budget` 1.0 is $33, which `workflow_budget` had better allow. Routes: `review` cheap under `propose`, `confirm` strong under `propose`, `fix` under `batch`.

The human reads `pma review`, approves the fixes in one batch, ships. The uncommitted `TODO.md` edit is theirs to commit.

### 14.2 Specify, then implement

Two nodes, no fan-out, no rules. The shape for a one-liner that an agent should not start coding against.

```
@input -> specify -> implement -> review, ship
```

```json
{
  "name": "specify-then-implement",
  "input": {"type": "task"},
  "caps": {"max_units": 4, "max_edits": 2},
  "nodes": [
    {"name": "specify", "op": "map", "out": "1", "in": "task", "emits": "task",
     "writes": ["spec"], "doc": "SPEC.md",
     "task": "The task is `{title}`. Write to {doc} what changes, which files, how it is checked, and what is out of scope. Put a one-line summary in `spec`."},
    {"name": "implement", "op": "edit", "in": "task", "check": "verify",
     "task": "Implement the specification in {doc}. Do not widen it.\n\nTask: {title}"}
  ],
  "edges": [
    {"from": "@input", "to": "specify"},
    {"from": "specify", "to": "implement"}
  ]
}
```

`out: 1` with `writes: ["spec"]` is the enforceable part: the specifier may add its summary and may not rewrite the task it was given.

### 14.3 Triage issues, no model on the read

The first node is a rule, so reading the issues costs nothing. Only the classification spends a model, and nothing is dispatched.

```
@input -> issues(rule) -> classify -> file [actionable=true] => TODO.md
```

```json
{
  "name": "triage-issues",
  "input": {"type": "project"},
  "caps": {"max_units": 60, "max_edits": 0},
  "nodes": [
    {"name": "issues", "op": "map", "out": "0..n", "via": "rule", "rule": "open-issues",
     "in": "project", "emits": "issue", "max_units": 50},
    {"name": "classify", "op": "map", "out": "1", "in": "issue", "emits": "issue",
     "writes": ["priority", "why", "actionable"],
     "task": "For each issue in {in}, set `priority`, a one-line `why`, and `actionable`."},
    {"name": "file", "op": "emit", "in": "issue", "sink": "todo", "action": "add",
     "map": {"priority": "{priority}", "text": "{title}", "description": "{why}"}}
  ],
  "edges": [
    {"from": "@input", "to": "issues"},
    {"from": "issues", "to": "classify"},
    {"from": "classify", "to": "file", "when": {"actionable": ["true"]}}
  ]
}
```

A non-actionable issue is settled at `classify` with the guard recorded, so the reason it was not filed is readable later.

### 14.4 Ensemble review, then reduce

Three reviewers at three models, joined. This is the shape a strictly sequential design cannot express.

```
@input -+-> review-a -+
        +-> review-b -+-> merge(rule) -> confirm -> record => TODO.md
        \-> review-c -+
```

```json
{
  "name": "ensemble-review",
  "input": {"type": "project"},
  "caps": {"max_units": 80, "max_edits": 0},
  "nodes": [
    {"name": "review-a", "op": "map", "out": "0..n", "in": "project", "emits": "finding", "max_units": 20,
     "task": "Review `{project}` for correctness bugs. Write findings to {out}."},
    {"name": "review-b", "op": "map", "out": "0..n", "in": "project", "emits": "finding", "max_units": 20,
     "task": "Review `{project}` for missing tests. Write findings to {out}."},
    {"name": "review-c", "op": "map", "out": "0..n", "in": "project", "emits": "finding", "max_units": 20,
     "task": "Review `{project}` for interfaces that are hard to use correctly. Write findings to {out}."},
    {"name": "merge", "op": "reduce", "via": "rule", "rule": "dedupe", "group_by": ["title"],
     "in": "finding", "emits": "finding"},
    {"name": "confirm", "op": "map", "out": "0..1", "in": "finding", "emits": "finding",
     "writes": ["reason"],
     "task": "Confirm each unit in {in} against the code. Drop what you cannot prove, with a `reason`."},
    {"name": "record", "op": "emit", "in": "finding", "sink": "todo", "action": "add",
     "map": {"priority": "{severity}", "text": "{title}", "description": "{detail}"}}
  ],
  "edges": [
    {"from": "@input", "to": "review-a"},
    {"from": "@input", "to": "review-b"},
    {"from": "@input", "to": "review-c"},
    {"from": "review-a", "to": "merge"},
    {"from": "review-b", "to": "merge"},
    {"from": "review-c", "to": "merge"},
    {"from": "merge", "to": "confirm"},
    {"from": "confirm", "to": "record"}
  ]
}
```

Three reviewers run in one pass, bounded by `max_parallel`. `merge` waits for all three because it has three incoming edges and `reduce` is the barrier. The reviewers are three prompts rather than three temperatures, because three identical prompts at one model mostly agree and the cost triples either way.

Routes may give `review-a` haiku, `review-b` sonnet and `review-c` opus, which is the cost experiment the whole design is for. Worst case: 3 reviews, up to 60 findings into the reducer, at most 60 confirms.

### 14.5 Decompose recursively, then implement

```
@input -> split -+-> split [depth<2]
                 \-> implement [children=0] -> review, ship
```

```json
{
  "name": "decompose-then-implement",
  "input": {"type": "task"},
  "caps": {"max_units": 20, "max_edits": 8},
  "nodes": [
    {"name": "split", "op": "map", "out": "0..n", "in": "task", "emits": "task",
     "max_units": 5, "max_depth": 2,
     "task": "`{title}` may be too large for one coherent change. If it is, return independent subtasks, each one change. If it is not, return nothing."},
    {"name": "implement", "op": "edit", "in": "task", "check": "verify",
     "retry": {"max": 1, "while": "@verify != passed"},
     "task": "Implement `{title}`.\n\n{detail}"}
  ],
  "edges": [
    {"from": "@input", "to": "split"},
    {"from": "split", "to": "split", "when": {"@depth": {"<": 2}}},
    {"from": "split", "to": "implement", "when": {"@children": {"==": 0}}}
  ]
}
```

Per-node worst case is 1 + 5 + 25 units, so `caps.max_units: 20` is what actually holds it, and the truncation is named in the report rather than hidden. Expect the depth cap to be reached: a model asked whether a task is small enough usually says no.

The leaf test is the decomposer's own empty answer, so no termination predicate is needed (W20).

### 14.6 Fix with an audit lap

Two nodes alternating, with a bound and an honest exit.

```
@input -> fix -> audit -+-> record  [verdict=accept] => TODO.md
                        +-> fix     [verdict=reject] (max_laps 2)
                        \-> handoff (default) => TODO.md
```

```json
{
  "name": "fix-with-audit",
  "input": {"type": "finding"},
  "caps": {"max_units": 12, "max_edits": 9},
  "nodes": [
    {"name": "fix", "op": "edit", "in": "finding", "check": "verify",
     "task": "Fix this in `{project}`:\n\n{title}\n\n{detail}"},
    {"name": "audit", "op": "map", "out": "1", "in": "finding", "emits": "finding",
     "writes": ["verdict", "reason"],
     "task": "Read the diff for `{title}`. Set `verdict` to accept or reject, and `reason` to one line. You are checking the change, not making one."},
    {"name": "record", "op": "emit", "in": "finding", "sink": "todo", "action": "add",
     "map": {"priority": "{severity}", "text": "{title}", "description": "{reason}"}},
    {"name": "handoff", "op": "emit", "in": "finding", "sink": "todo", "action": "add",
     "map": {"priority": "{severity}", "text": "{title}", "description": "{reason}"}}
  ],
  "edges": [
    {"from": "@input", "to": "fix"},
    {"from": "fix", "to": "audit"},
    {"from": "audit", "to": "record", "when": {"verdict": ["accept"]}},
    {"from": "audit", "to": "fix", "when": {"verdict": ["reject"]}, "max_laps": 2},
    {"from": "audit", "to": "handoff", "default": true}
  ]
}
```

Three findings, at most three `fix` runs each: 9 edits, 9 audits. The route for `fix` may state `lap: "1-2"` at a stronger model, so a rejected first attempt is retried by a better one without the document naming it.

`handoff` catches two cases: laps exhausted, and a verdict the auditor left unreadable. Both end with the human holding the task, which is the truthful outcome.

### 14.7 Upgrade, and triage only on breakage

The conditional the earlier draft could not express, and it needs no `when` on a node: the guard reads a check's verdict, and a node with no units does nothing.

```
@input -> upgrade -+-> diagnose [verify=failed] -> fix -> review, ship
                    \-> (settled) [verify=passed]
```

```json
{
  "name": "upgrade-then-triage",
  "input": {"type": "project"},
  "caps": {"max_units": 20, "max_edits": 6},
  "nodes": [
    {"name": "upgrade", "op": "edit", "in": "project", "check": "verify",
     "task": "Update this project's dependencies within its version constraints. Change a constraint only where the tests still pass."},
    {"name": "diagnose", "op": "map", "out": "0..n", "in": "project", "emits": "finding",
     "max_units": 10,
     "task": "The upgrade in this worktree broke the build. Write one `finding` per distinct breakage to {out}: what broke, where, and the smallest fix."},
    {"name": "fix", "op": "edit", "in": "finding", "check": "verify",
     "task": "Fix this breakage from the dependency upgrade:\n\n{title}\n\n{detail}"}
  ],
  "edges": [
    {"from": "@input", "to": "upgrade"},
    {"from": "upgrade", "to": "diagnose", "when": {"@verify": ["failed"]}},
    {"from": "diagnose", "to": "fix"}
  ]
}
```

A green upgrade settles at `upgrade` and the run goes to review as a clean patch. A red one routes into `diagnose`, whose bag is empty when nothing broke, so `fix` is never even considered. `@verify: unknown` matches no edge and settles, which is right: a check that could not run is not evidence of breakage.

### 14.8 A campaign, as a graph

One definition over a set of repositories: probe by rule, then edit what needs it. This is the campaign mechanism (plan 5.1, 5.2) expressed in the same document, with the fan-out axis being a bag of projects rather than a bag of findings.

```
@input(tag) -> needs-ci(rule) -> add-ci -> review, ship
```

```json
{
  "name": "add-missing-ci",
  "input": {"type": "project"},
  "caps": {"max_units": 100, "max_edits": 10},
  "nodes": [
    {"name": "needs-ci", "op": "map", "out": "0..1", "via": "rule",
     "rule": "path-absent:.github/workflows", "in": "project", "emits": "project"},
    {"name": "add-ci", "op": "edit", "in": "project", "check": "verify",
     "task": "Add a minimal CI workflow that runs this project's own check command on push and on pull request. Change nothing else."}
  ],
  "edges": [
    {"from": "@input", "to": "needs-ci"},
    {"from": "needs-ci", "to": "add-ci"}
  ]
}
```

`pma workflow run add-missing-ci --tag rust` freezes the project set as the input bag, which is `add_campaign`'s frozen membership by another route: the bag is written once and never recomputed, so a rescan cannot move work under a running instance.

Class A- applies because the change touches `.github/**`, `unattended` is refused for A- where the policy is read, and `max_edits: 10` caps the blast radius per instance. Plan 4.9's per-repository daily cap still applies on top.

### 14.9 Ship, then wait for the merge

No loop, no model. The graph expresses waiting by having a node that is not ready.

```
@input -> merged(check) -> close => note
```

```json
{
  "name": "settle-pull-requests",
  "input": {"type": "finding"},
  "caps": {"max_units": 40, "max_edits": 0},
  "nodes": [
    {"name": "merged", "op": "check", "rule": "pr-merged", "in": "finding"},
    {"name": "close", "op": "emit", "in": "finding", "sink": "note", "action": "add",
     "map": {"text": "shipped: {title}"}}
  ],
  "edges": [
    {"from": "@input", "to": "merged"},
    {"from": "merged", "to": "close", "when": {"@merged": ["passed"]}}
  ]
}
```

Each pass costs nothing and either advances or does not. Run it from the same place a scheduled `pma dispatch` pass would run (plan 4.11).

### 14.10 Prune finished items, with no model at all

Every node is a rule. Worst-case cost is zero, which `pma workflow propose` prints as `$0.00`, and the document is a scheduled chore rather than an agent workflow.

```
@input -> items(rule) -> prune [done=true] => TODO.md (remove)
```

```json
{
  "name": "prune-finished",
  "input": {"type": "project"},
  "caps": {"max_units": 500, "max_edits": 0},
  "nodes": [
    {"name": "items", "op": "map", "out": "0..n", "via": "rule", "rule": "todo-items",
     "in": "project", "emits": "item", "max_units": 400},
    {"name": "prune", "op": "emit", "in": "item", "sink": "todo", "action": "remove",
     "map": {"text": "{text}"}}
  ],
  "edges": [
    {"from": "@input", "to": "items"},
    {"from": "items", "to": "prune", "when": {"done": ["true"]}}
  ]
}
```

`action: remove` refuses an open item, so the guard and the sink agree. This is `pma prune` with a record of what it removed and why.

## 15. Acceptance

Routing:

- a route with `node` does not match a node-less dispatch, and a route without `node` does not match a node

- `lap` matches a range, and a run outside a workflow has lap 0

- `to_json` round trip keeps `node` and `lap`; an empty node list is refused

- `unattended` with a node and no class list is refused

- replay over runs recorded before migration 20 reports no difference

The document, one test per refusal in 5.4, each naming its position as `route.rs`'s parse tests do. In particular: a cycle without `max_laps`; a `max_laps` edge with no terminal path; a `map out: 0..n` with no `max_units`; a self-edge with no `max_depth`; a type agreement failure across an edge; a guard on an undeclared field.

Units and ops:

- `0..n` past `max_units` is refused, and the instance cap truncates with a named remainder

- `0..1` returning an id that was not in the input is refused; a drop with no reason is refused

- `1` changing a field outside `writes` is refused

- a `reduce` output with no provenance is refused

- a duplicate `unique: "normalised"` value against an open item is dropped and named, not failed

Iteration:

- `retry` appends attempts to one run in one worktree, and stops at `max`

- a lap mints a new unit with `@parent`, `@lap + 1`, and the same `@root`

- retries and laps draw from one counter: a unit that used two attempts on retries cannot take a third on a lap

- a unit that exhausts its laps reaches the terminal path, and the move is recorded

- recursion stops at `max_depth`, and a unit with no children routes to the leaf edge

- a `check` node that is not satisfied ends the pass without advancing, and the next pass re-derives the same frontier

Bounds:

- `propose` prints per-node worst case and total cost; a rules-only document prints zero

- `activate` refuses a document above `workflow_budget`

Sinks:

- `todo add` into a section with a `###` group inserts above the group

- a missing section refuses; a file with lint errors is skipped

- `todo remove` refuses an open item

- `note add` writes one note per unit

End to end, on a fixture repository: 14.1 with two findings, one fixed and shipped; 14.7 with a red upgrade producing one breakage; 14.10 with three finished items.

## 16. Limits

**No sub-workflows.** Reusing "review and confirm" inside three documents means copying two nodes. A `call` node needs nested instances, nested caps and a cost bound that composes. Deferred until a second document wants the same pair.

**Two `edit` nodes can conflict at ship.** Separate worktrees make them safe to run; merging them is plan 5.3's barrier problem, and nothing here solves it. `caps.max_edits` bounds how bad it gets.

**A node's input is unbounded in size.** A 200-file repository does not fit a prompt, and no node declares which files it reads. The agent walks the worktree within its timeout.

**Recursion is the least bounded construct.** The model decides termination and is bad at it. Only `max_depth` and the instance cap hold, and they truncate rather than converge. Do not build it before 14.1 and 14.5's cheaper forms have run.

**Artifacts are never collected.** `<data>/artifacts/<instance>/` grows with every pass. A finished instance is an unambiguous trigger, as minos section 11 says of a completed workflow, and nothing acts on it.

**Per-node specialisation is unmeasured.** The premise -- a cheap model reviews, a strong one fixes -- has no data behind it. `pma report --by node` is what would show it, and it needs runs first.

**A confirming node reads its producer's prose.** It is meant to check the producer, so what it inherits matters. Here it inherits the units and the named document, which is minos D19's default setting expressed as a data edge. When minos lands, a node's grant carries `since` and the same choice becomes three-valued.

## 17. Gates and sequencing

| Part | Contents | Gate |
|-|-|-|
| 6a | types, units, `map` at three bounds, `check`, `emit`, guards, default edges, the pass, caps and the cost bound, migration 20, `node` and `lap` on a route, `pma workflow` commands | none. No `edit`, so nothing changes a repository: 14.2, 14.3, 14.9 and 14.10 run under it |
| 6b | `edit` with `retry` | phase 0 measured, and phase 4b's gate met: 10 runs shipped through batch approval. 14.1 and 14.8 run here |
| 6c | `reduce`, lap edges | 6a in use. 14.4 and 14.6 run here |
| 6d | recursion | evidence that one level of decomposition helps. 14.5 runs here |

No node is `unattended` before phase 4c's gate. The parse guard already refuses it for A-, C, D and for a route with no class list.

Size: 4 sessions for 6a, 2 for 6b, 2 for 6c, 1 for 6d. All exclude agent cost.
