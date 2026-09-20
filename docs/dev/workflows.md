# pma workflows (draft)

Status: 2026-09-20. Built: the document in both its forms with its refusals and cost bound, migration 20, `node` and `lap` on a route, `pma workflow check|propose|activate|show|run`, call flattening at propose time, all five primitives including `edit`, the rules of section 8, typed targets and `--set`, and the caps enforced where units and runs are written. Runs go `max_parallel` at a time within a node's bag, and `batch_budget` bounds the pass.

Not built: section 7's iteration. `retry`, a lap edge and a self-edge are parsed, bounded and costed, and none of the three is taken -- nothing increments `@lap`, and `handled` cannot tell a unit a node routed onward from one an edge routed back in. A document using them runs its forward path once. Also not built: a reason per drop at `map out: 0..1` (section 10), which the runtime records against the move rather than taking from the model; and `publish` on a node, which is parsed and ignored -- an agent node's worktree is discarded after the run, so there is nothing for its document to be committed from (W19).

Two decisions below were corrected by the implementation rather than by review: an `edit` preserves its unit's type (section 3), and a lap mints a unit while a retry does not (section 11).

A workflow is a typed, parameterised function over bags of units:

```
find-issues        (in: [Project], review_doc = "REVIEW.md") -> [Finding]  !pure
apply-fixes        (in: [Finding], laps = 2)                 -> [Finding]  !repo !writes
review-fix-critical(in: [Project], severity = "critical")    -> [Finding]  !repo !writes
```

A node applies a function: one of five primitives, or another workflow. Edges carry units between nodes and decide by rule where each goes next. Parallelism is a property of the graph, not a node kind: everything whose inputs are ready runs, bounded by `max_parallel` and the budget.

Three things keep a composable document from becoming an unbounded engine: calls are flattened before anything runs, every bound is a constant or a parameter with a declared maximum, and guards are rules rather than models.

It is phase 6 of [implementation-plan.md](implementation-plan.md). The chat and container half is `~/projects/minos/docs/dev/design.md`; its section 5 calls a node a stage and assumes a sequence, which is one shape of a graph.

## 1. Signatures

A signature has three parts, all checked.

| Part | Form | Checked |
|-|-|-|
| types | `in: [T] -> [U]` | every edge's source type matches its target's `in`; every call site matches its callee |
| parameters | `name = default` | type, allowed values, and a `max` when the parameter feeds a bound |
| effects | `!pure`, `!repo`, `!writes` | the declared set equals the set the flattened graph actually has |

Effects are a set, not a level. `!repo` means some node changes a repository through the `edit` primitive; `!writes` means some node writes outside it through `emit`; `!pure` is the empty set. A workflow declaring `!pure` that contains an `edit` is refused where the document is read. That check is cheap and it is what lets a caller know whether calling something can change a repository.

Composition unions effects and multiplies bounds. `[Project] -> [Finding]` and `[Finding] -> [Finding]` compose into `[Project] -> [Finding]`, and `!pure` with `!repo` is `!repo`.

## 2. The test for a primitive

A verb earns a primitive when it changes what `pma` must check. Everything else is vocabulary: review, validate, triage, decompose, dedupe, fix and ship are the same five operations with different prompts.

Three dimensions generate the set.

| Dimension | Values | What it decides |
|-|-|-|
| cardinality | grows, preserved, shrinks, one per group | where parallel work is created and joined |
| effect | data, repository, outside world | which gates apply. Only a repository writer needs a worktree, base and head verify, a scope check, approval and ship |
| decider | agent or rule | an agent costs money, is nondeterministic and untrusted; a rule is free, deterministic and replayable |

## 3. The five primitives

Each reads as a bag function, which is what makes composition check.

| Primitive | Signature | Decider | What `pma` checks -- its reason to exist |
|-|-|-|-|
| `map out: 0..n` | `[T] -> [U]` | agent or rule | the declared type; `max_units`; `max_depth` on a self-edge |
| `map out: 1` | `[T] -> [T]` | agent or rule | the type, and that no field outside `writes` changed |
| `map out: 0..1` | `[T] -> [T]` | agent or rule | the above, plus kept ids being a subset of the input and a reason on every drop |
| `reduce` | `[T] -> [U]` | agent or rule | provenance: every output unit names the inputs it came from |
| `edit` | `[T] -> [T]` `!repo` | agent | all of phase 1: base and head verify, scope, attempts, approval evidence, ship. The only primitive that changes a repository |
| `check` | `[T] -> [T]` | rule | nothing. It is the checker |
| `emit` | `[T] -> [T]` `!writes` | rule | the sink's rules: `TODO.md` lint and item identity, the `pma sync` conflict |
| `call` | the callee's signature | -- | type and parameter agreement; resolved by flattening at propose time |

`emit` returns its input, so a graph can record something and carry on. `check` returns its input with a verdict annotated. `edit` does the same: it records a run against its unit and passes the unit on, reachable afterwards as `@run` and through the verdicts its check wrote. A graph that fixes and then audits must carry the thing being fixed, not a description of the diff -- which is why `edit` is `[T] -> [T]` and there is no patch type.

Remove any one and something becomes inexpressible: without `map`, no units are created or dropped; without `reduce`, no join; without `edit`, no code changes; without `check`, every verdict comes from a model, so guards read untrusted data; without `emit`, nothing reaches the human; without `call`, a signature buys nothing.

Domain verbs collapse onto them:

| Verb | Primitive |
|-|-|
| review a project, list candidates, decompose a task | `map out: 0..n`, agent |
| read `TODO.md`, collect issues, list outdated dependencies, list open runs | `map out: 0..n`, rule |
| validate, confirm, triage out | `map out: 0..1`, agent, drop reason required |
| annotate, estimate, judge one unit | `map out: 1`, agent; the verdict is a field |
| select by field, drop duplicates, rank, limit | `map out: 0..1` or `reduce`, rule |
| synthesise one report from three reviews, choose the best of three patches | `reduce`, agent |
| fix, implement, upgrade, add a workflow file | `edit` |
| verify, lint, is CI green, did the pull request merge | `check` |
| write items, tick items, prune items, add a note, write a report | `emit` |

## 4. Units, bags and types

A **unit** is a typed record. A **bag** is a node's output: the units it wrote. An edge names a source node, so a bag needs no name of its own.

System fields, on every unit, reserved and refused in a type declaration:

| Field | Meaning |
|-|-|
| `@id` | minted by `pma`, never by an agent |
| `@type` | the declared type |
| `@node` | the qualified node that wrote it |
| `@parent` | the unit it was derived from, or null |
| `@root` | the first unit of its lineage. The attempt counter keys on this |
| `@depth` | lineage depth, for recursion |
| `@lap` | how many times it has crossed a lap edge |
| `@children` | how many units this one produced, once its node has run |
| `@project` | the project it belongs to |
| `@run` | the run an `edit` recorded against it, once it has run |
| `@<check>` | a verdict written by a check: `passed`, `failed` or `unknown` |

Units are immutable. Every node writes new units with `@parent` set, so a lap, an annotation and a decomposition each leave a readable chain. Nothing in the graph mutates state, which is why the frontier can be re-derived rather than stored.

A node's `in.json` carries `@id` on every unit, and a `map out: 0..1` returns the ids it keeps, which is what lets a drop be attributed. A type therefore declares no identity field of its own.

### 4.1 Built-in types

Four types `pma` writes itself, from data it already holds. A document may not declare a type with these names and may not add fields to them; a workflow that needs its own fields projects with the `as:` rule of section 8.1.

| Type | Source | Fields |
|-|-|-|
| `project` | the projects table and the last scan | `name`, `tier`, `repo`, `owner`, `default_branch`, `ci`, `deps`, `tags` |
| `item` | the last scan's `TODO.md` items | `key`, `text`, `priority`, `tags`, `due`, `gh`, `group`, `description`, `done` |
| `signal` | the last scan | `kind` (`ci` or `deps`), `detail` |
| `run` | the `runs` table | `id`, `task`, `state`, `branch`, `pr`, `verify`, `cost` |

An `issue` type is not built in: reading GitHub needs `gh`, which the `open-issues` rule runs, and the document declares the shape it wants back.

### 4.2 Field types

A closed list, each with a check `pma` runs without a model.

| `type` | Checked |
|-|-|
| `line` | a one-line string, 1 to `max` characters. `unique: "normalised"` compares by `todo::normal_text`, within the bag and against the project's open items |
| `lines` | an array of strings, at most `max` entries |
| `enum` | a string in `values` |
| `list` | an array of strings, at most 20 entries, each at most 200 characters |
| `int` | a whole number, within `min` and `max` when given |
| `bool` | `true` or `false` |

Every declaration also takes `required`, default false.

The declaration is deliberately weak. It cannot express that a finding is real or that a specification is complete; a `map out: 0..1` and the reviewer do that.

### 4.3 `@input` and `@output`

`@input` is the function's parameter: the only bag no node wrote. `pma` writes it once when the instance starts, from the target on the command line, and never recomputes it. That is `add_campaign`'s frozen membership arriving by another route -- a rescan cannot move work under a running instance, and a restart reads the same units.

The target syntax is `dispatch::target`'s, so the entry point needs no second selector language:

| `pma workflow run <name> ...` | Root units | Type |
|-|-|-|
| `cynn` | one | `project` |
| `--tag rust` | one per tagged project | `project` |
| `cynn:31` | one, that `TODO.md` line | `item` |
| `cynn:critical` | one per open item under the heading | `item` |
| `cynn:ci`, `cynn:deps` | one | `signal` |

A target whose element type differs from the workflow's declared input is a type error at the call site, refused by name. So is one yielding two types at once: `cynn:q1` holds items and signals together, so it is not an argument.

`@output` is the return value: a target-only pseudo-node. Several guarded edges may enter it, and their union is the result bag; every one must carry the declared output type. A workflow with no edge into `@output` returns an empty bag, which is legal for one whose purpose is its effects.

Both are bags, not nodes: they run nothing and cost nothing. `@input` appears in `edges` only as a `from`, `@output` only as a `to`.

## 5. Decisions

### Composition

**W1. A workflow is a function, and a node applies one.** Primitives and workflows are the same kind of thing, distinguished by whether `pma` implements them. Rejected: a workflow that is only a top-level graph, which makes reuse copy-and-paste and gives a signature nothing to check.

**W2. Calls are flattened at propose time.** A callee's nodes are inlined under the call site's name -- `issues/confirm`, `audit/confirm` -- so the runtime has one flat graph, one instance, one set of caps, and one frontier. Rejected: nested instances, which need nested caps, nested budgets and a second resume story. The cost is that workflow-level recursion cannot terminate and is refused; node-level recursion keeps `max_depth`, which unrolls finitely.

Prefixing by call site rather than by callee is what lets one workflow be called twice in one graph.

**W3. Effects are declared and checked.** The set the flattened graph has must equal the set the signature states. A caller can then tell, without reading the callee, whether calling it changes a repository.

**W4. A parameter may set a bound if it declares a maximum.** `laps = 2` with `max: 3` is admitted, and the worst case is computed at 3. Rejected: forbidding parameters on bounds, which would make `max_laps` and `max_units` unconfigurable and push authors into copying documents; also rejected: unbounded numeric parameters, which would make the cost bound depend on the invocation.

**W5. Three placeholder namespaces, no overlap.** `{field}` reads the unit, `{@field}` a system field or verdict, `{$param}` a parameter. Each is checked against its declaration where the document is read.

### The graph

**W6. Parallelism is derived from the graph.** There is no parallel node, fork, join or barrier. Two edges out of one node fan out; a `reduce` with several incoming edges fans in. Rejected: declared concurrency, which restates what the edges already say.

**W7. An agent produces data; a rule decides routing.** Guards, checks and sinks are rules. A model's opinion enters as a field, and a rule reads the field. This is route.rs's existing commitment -- "the policy is an artifact, not a judgment per task" -- applied to the graph. Rejected: a guard that calls a model, which would let an untrusted party choose the graph's shape.

**W8. Conditions live on edges, never inside nodes.** A guarded edge is visible in the topology; a condition inside a node is not. A unit that no outgoing edge accepts is settled there, and the guard that refused it is recorded.

**W9. Readiness is derived, never stored.** A node is runnable when its incoming units have arrived. There is no cursor, so a crash resumes by re-deriving. This is plan 1.7's rule applied to sequencing: store the evidence, not the verdict.

**W10. `pma workflow run` is a pass, not a daemon.** It advances every runnable node of every named instance and exits, holding `session.lock` for the pass like `pma dispatch`. A boundary that needs a human ends the pass; the next invocation resumes it. Waiting for an external condition is the same mechanism: a `check` node is simply not ready. Rejected: a resident process, which would hold the lock across a human decision. A pass runs every node a rule decides as soon as its units arrive, because a rule is free and deterministic. It stops at the first node an agent decides, prices it, and spends nothing until `--yes`: what a pass would spend is a decision, and it is the developer's. `--dry-run` plans and prices without running even the free nodes, and `-m` runs a graph at a cheap model, which is how a workflow is tried out.

### Policy and authority

**W11. A workflow is a stored revision, proposed then activated.** The two-act shape is `routes`': a revision is a draft until someone activates it, and each run records the revision and the parameters it ran under. Rejected: a file read at run time, which leaves no record of what ran.

**W12. Agent and model live in the routing policy, not in the workflow document.** A node names no agent. `node` and `lap` become match dimensions on a route (section 10). Plan 2.5 refused a second place to name them: "a second place to say the same thing would have to be reconciled with it."

**W13. A route that states no `node` matches only a dispatch with no node.** Node is a partition, not a filter. Rejected: unstated means any, under which an existing policy's trailing catch-all absorbs every node silently at whatever approval it names.

**W14. A workflow is not an escape from the class rules.** Every `edit` run goes through `dispatch::prepare` and the phase 1 gates: class D is refused before a worktree exists, `TODO.md` and the privileged paths are refused to the classes that may not touch them, and `unattended` on A-, C or D is refused where a policy is read. A workflow chooses order, prompts and parameters, never authority.

**W15. `pma-agent` may propose a revision and request a trigger; it may not activate one.** minos design section 2 puts "which workflow a situation gets" behind a submission to `pma`, and D2 keeps mint, ship and push out of a model's hands. `pma workflow activate` records who activated it.

### Data

**W16. A type is declared in the document, not registered in Rust.** Rejected: a schema per workflow kind compiled in, which costs the same code and makes every new workflow a code change.

**W17. A `map out: 0..1` may not rewrite its input.** It returns ids to keep, plus fields named in `writes`. A validator that could edit a finding's text could launder work past the reviewer who reads it; the restriction is a trust boundary, not tidiness. Under `out: 1` the same rule bounds an annotation.

**W18. Files are how an agent reads and writes units; the store is where they live.** `pma` writes `in.json` before a run and reads `out.json` after, under `<data>/artifacts/<instance>/<node>/<n>/`, numbered per run so nothing overwrites an earlier one. Prose documents sit beside them. Rejected: units as files only, which cannot record why a unit was dropped or which guard stopped it.

**W19. Prose documents live outside the worktree.** An untracked file there enters `dispatch::changed_paths`, counts as a violation for any class whose scope is bounded ([class.rs:101](../../src/class.rs)), and `git add -A` at ship publishes it. A node that wants its prose committed says `"publish": true`.

**W20. No node writes `TODO.md`. A node emits units and `pma` writes the items.** `OWNED` puts `TODO.md` outside every class's scope ([class.rs:34](../../src/class.rs)), the dispatch prompt says so ([dispatch.rs:502](../../src/dispatch.rs)), and plan 4.7 depends on it: ship ticks the item after the rebase, which is admissible only because no agent may touch the file.

**W21. An `edit` node takes its unit from the graph, not from `TODO.md`.** Dispatch requires the item open in the remote default branch (`dispatch::on_origin`), so an item written by an earlier node cannot be dispatched until it is committed and pushed. A unit-keyed task takes the existing no-item path, with key `workflow:<instance>:<root>`. An `emit` is therefore independent of an `edit`: switching the record off changes nothing about what the fix runs.

### Iteration

**W22. Three forms, chosen by what re-applies between tries.** If no gate runs between attempts, the loop belongs inside the agent's own turn, where `timeout` and `agent_budget` bound it and `pma` neither sees nor records it.

| Form | Example | Mechanism | Bound |
|-|-|-|-|
| retry | fix until `verify` passes | a node property; same worktree, appended attempts | `retry.max` |
| lap | fix, audit, fix again | a back edge | `max_laps`, counted on the unit |
| recursion | split until each piece is one change | a self-edge on `map out: 0..n` | `max_depth`, and the unit caps |
| waiting | until CI is green | none: a `check` node is not ready | none |

**W23. Retries and laps draw from one counter per lineage.** `exhaustion` is keyed on project and task revision and refuses a third attempt (plan 1.9). A lap that re-keyed itself would be a hole straight through that limit; a lap that kept the existing key would make `max_laps` above 2 dead on arrival. One `attempts_used` per `@root`, consumed by a retry and by a lap alike, with `retry.max` and `max_laps` as tighter local bounds under the code ceiling. A red check or a rejection consumes one; an infrastructure failure or a timeout consumes none, as today.

**W24. A lap edge requires a terminal path.** A document where a unit can exhaust its laps with nowhere to go is refused at parse. The usual terminal path emits the unit to `TODO.md`, which is the honest outcome of "the agent did not converge in two laps".

**W25. Recursion terminates on the model's own answer, bounded by depth.** A unit that produced no children is a leaf and routes onward; a unit that produced children is settled and its children re-enter at `@depth + 1`. Models judge "small enough" poorly, so the caps are the real bound. Rejected: a declared termination predicate over unit content, which is a second control-flow language.

**W26. No unbounded construct, and the bound is computed before activation.** Every bound is a constant or a parameter with a declared maximum, so the worst case is a product of constants over the flattened graph. `pma workflow propose` prints the worst-case number of agent runs and the worst-case cost; `pma workflow activate` refuses a document whose worst case exceeds `workflow_budget`. What makes a loop safe to write down is not a promise to converge but a refusal to activate a graph that could cost more than you said.

### Authoring

**W27. A document may be JSON or a script; the document is the artifact either way.** A Rhai script applies combinators to a graph value and returns the same structure, which the same reader validates, the same walk costs, and a revision stores as the generated JSON. `--emit-json` prints it. Rejected: Rhai as the runtime language for guards, checks or rules. A guard decided by a script could not be checked against the fields it reads, could not be named in `workflow_moves.reason` when it stops a unit, and would put the graph's shape in the hands of whatever wrote the script -- which W15 lets be a model. Also rejected: a script as the only form, which would make the stored revision code rather than data.

The engine is deterministic by construction: no clock, no modules, no `eval`, and every limit set (6.6). A script that will not terminate is stopped where the document is read, which is before anything has been dispatched.

## 6. The document

JSON, for the reason `route.rs` states: this crate parses JSON already. `types` sit beside `workflow` so several workflows share them. Nodes and edges are separate lists, so the topology is read in one place.

A document may also be built by a Rhai script, which is a second way to write the same thing rather than a second thing (6.6). The two forms of one workflow sit side by side in 15.2.

```json
{
  "types": {
    "finding": {
      "fields": {
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
      "name": "find-issues",
      "in": "project",
      "out": "finding",
      "effects": [],
      "params": {
        "review_doc": {"type": "name", "default": "REVIEW.md"},
        "breadth":    {"type": "int",  "default": 20, "max": 40}
      },
      "caps": {"max_units": 60, "max_edits": 0},
      "nodes": [],
      "edges": []
    }
  ]
}
```

### 6.1 Workflow

| Field | Default | Meaning |
|-|-|-|
| `name` | required | Unique in the revision. Names every instance, run and call site. |
| `in` | required | The input element type: a built-in for a workflow called from the command line, any declared type for one that is only called. |
| `out` | `null` | The output element type. Null means it returns nothing. |
| `effects` | `[]` | `repo`, `writes`, or both. Checked against the flattened graph. |
| `params` | `{}` | Section 6.2. |
| `caps` | required | `max_units` and `max_edits` per instance. Section 11. |
| `nodes` | required | At least one. |
| `edges` | required | Every node must be reachable from `@input`. |

### 6.2 Parameters

| Field | Meaning |
|-|-|
| `type` | `name`, `line`, `int`, `enum`, `bool` or `list`. `name` is a filename-safe string, for a document such as `REVIEW.md` |
| `default` | required. A workflow is runnable with no arguments |
| `values` | for `enum` |
| `max` | required when the parameter is used as a bound: `max_units`, `max_depth`, `max_laps` or `retry.max` |

A parameter is referenced as `{$name}`: in a prompt, a document name, a guard's value, or a bound. Arguments are given per run (`--set severity=high`), recorded on the instance, and read back by a replay.

### 6.3 Node

| Field | Applies to | Default | Meaning |
|-|-|-|-|
| `name` | all | required | Unique in the workflow. Qualified with its call path after flattening. |
| `op` | all | required | `map`, `reduce`, `edit`, `check`, `emit`, `call`. |
| `via` | map, reduce | `agent` | `agent` or `rule`. `edit` is agent only; `check` and `emit` are rule only. |
| `in` | all | required | The type it expects. Every incoming edge must carry it. |
| `out` | map | required | `0..n`, `1` or `0..1`. |
| `emits` | map, reduce | required | The type it writes. |
| `writes` | map at `1` or `0..1` | `[]` | Fields it may set. Any other change is refused (W17). |
| `max_units` | map at `0..n` | required | Units one input unit may yield. A constant or `{$param}`. |
| `max_depth` | map with a self-edge | required | Lineage depth from the root. |
| `group_by` | reduce | `[]` | Fields forming a group. Empty means one group. |
| `task` | agent nodes | required | The prompt. `{field}`, `{@field}`, `{$param}`, `{in}`, `{out}` and `{doc}` are replaced. |
| `rule` | rule nodes | required | A named builtin. Section 8. |
| `doc` | map, agent | absent | A prose document the node also writes. Usually `{$param}`. |
| `publish` | map, agent | `false` | Copy `doc` into the worktree, so ship commits it. |
| `check` | edit | absent | A check run after the node. `verify` is the usual one. |
| `retry` | edit, map | absent | `{max, while, escalate}`. Section 7. |
| `sink` | emit | required | `todo`, `note` or `doc`. Section 9. |
| `action` | emit | `add` | `add`, `tick` or `remove`. |
| `map` | emit | required | Sink field to a placeholder. |
| `workflow` | call | required | The callee's name. |
| `with` | call | `{}` | Arguments for the callee's parameters. |

### 6.4 Edge

| Field | Default | Meaning |
|-|-|-|
| `from` | required | A node name, or `@input`. |
| `to` | required | A node name, or `@output`. |
| `when` | absent | A guard. All keys must hold. |
| `default` | `false` | Takes the units no guarded edge accepted. |
| `max_laps` | absent | Marks a back edge and bounds it. Requires a terminal path (W24). |

Guards, a closed set of forms:

| Form | Example |
|-|-|
| membership | `{"severity": ["critical", "high"]}`, or `{"severity": ["{$severity}"]}` |
| number | `{"@children": {"==": 0}}`, `{"@depth": {"<": 2}}`. Operators `==`, `<`, `<=`, `>`, `>=` |
| presence | `{"paths": "present"}`, `{"reason": "absent"}` |
| verdict | `{"@verify": ["passed"]}` |

No disjunction: two edges express it. A verdict guard matching only `passed` sends `unknown` to the default edge, which is why `unknown` is distinct from `failed`; the precedent is plan 1.6, where a base check that could not start is unknown rather than failing.

### 6.5 Refused where the document is read

For the reason `route.rs` gives -- a node that silently does nothing is worse than one that is refused:

- a duplicate type, workflow, node or edge

- an edge naming an unknown node, or a cycle that is not a `max_laps` back edge

- an incoming edge whose source type differs from the target's `in`, or an edge into `@output` whose type differs from `out`

- a call whose callee is unknown, whose input type disagrees, or whose `with` names an undeclared parameter or a value outside its `values`

- a workflow that calls itself, directly or through another

- declared `effects` that differ from the flattened graph's

- a `map out: 0..n` with no `max_units`, or a self-edge with no `max_depth`

- a parameter used as a bound with no `max`, or a parameter with no `default`

- a `max_laps` edge with no terminal path for units that exhaust it

- a guard on an undeclared field, an unknown `@` name, or a value outside an enum's `values`

- a `{field}`, `{@field}` or `{$param}` naming nothing declared

- an `input` type that is not a built-in, for a workflow the command line may name

- an `as:` projection whose target type has a required field with no same-named source

- a type declaring a reserved `@` name, or a built-in type's name

- `via: agent` on a `check` or an `emit`

- a node unreachable from `@input`

- an unknown field, at any level

A worst case above `workflow_budget` is refused at activation rather than at parse, because the ceiling is a setting and the document is not wrong.

### 6.6 The same document as a script

A document may be written as JSON, or built by a [Rhai](https://rhai.rs) script. `pma workflow check lib.rhai` runs the script, converts its value to JSON, and hands it to the same reader: one validator, one cost bound, one stored artifact. `--emit-json` prints what a script built, so a generated document can be committed and read without the script.

What a script adds is variables, functions, loops and comments. What it does not do is run during a pass: it never sees a unit, decides no guard and implements no rule. A script that ran at dispatch time would make the worst case uncomputable before activation, and a revision proposed by `pma-agent` would be code rather than data (W15).

The engine is deterministic by construction rather than by discipline: `rhai` is compiled with `no_time` and `no_module`, so there is no clock and no module system to reach a file with; `eval` is disabled; and the limits are 2,000,000 operations, call depth 32, expression depth 96, 64 KiB strings, and 10,000 entries per array or map. A runaway script is terminated where a document is read, which is before anything has been dispatched.

A script does not write nodes and edges. It applies combinators to a graph value, which carries its own output port, so a stage is wired by application and no node's name is repeated in an edge:

```rhai
let graph = source("project")
    .expand("review", "finding", "{$breadth}", "Review `{name}`. Write findings to {out}.")
    .filter("confirm", ["reason"], "Confirm each unit in {in}; drop what you cannot prove.")
    .join("dedupe", ["title"])
    .output();
```

One combinator per primitive, plus the plumbing a graph needs:

| Combinator | Builds |
|-|-|
| `source(type)`, `.output()` | the parameter and the return |
| `.expand(name, emits, max_units, task)` | `map out: 0..n` |
| `.transform(name, writes, task)` | `map out: 1` |
| `.filter(name, writes, task)` | `map out: 0..1` |
| `.rule_expand(name, emits, rule, max_units)`, `.rule_filter(name, rule)` | the same two, decided by a rule |
| `.join(name, group_by)`, `.join_by(name, group_by, rule)` | `reduce` by rule; the barrier after a `fan` |
| `.reduce_with(name, group_by, task)` | `reduce` by agent |
| `.edit(name, check, task)` | `edit` |
| `.check(name, rule)` | `check` |
| `.emit_todo(name, action, fields)`, `.emit_note(name, fields)` | `emit` |
| `.invoke(name, workflow, args, emits)` | `call`, named `invoke` because Rhai's own `call` applies a function pointer |
| `.retrying(max, predicate)`, `.retrying(max, predicate, model)` | a `retry` on the stage before it |
| `.when(guard)` | a guard on the next edges |
| `.lap(back_to, guard, max_laps)` | a bounded back edge |
| `.otherwise(fn)` | the default path, as a branch off this stage; the main path continues |
| `.fan([fn, ...])` | one branch per function, all reading this stage |
| `workflow(name, graph, opts)` | the workflow; `opts` takes `params`, `caps` and `effects` |
| `document(types, workflows)` | the document, which is a script's last expression |

`fan` and `otherwise` take functions from a graph to a graph, which is what a reusable piece of a graph is. Naming a node stays an argument, because a route matches on node names and a run records one; only the wiring stops being strings.

Declarations, for a type's fields and a workflow's parameters:

| Helper | Returns |
|-|-|
| `line(max)`, `lines(max)`, `choice(values)`, `list()`, `number()`, `flag()` | a field declaration |
| `req(field)`, `unique(field)` | that declaration, marked |
| `param(type, default)`, `bounded(type, default, max)` | a parameter; the second form is what a bound needs |
| `eq(n)`, `lt(n)`, `le(n)`, `gt(n)`, `ge(n)` | a numeric guard test |

`effects` may be left out of `opts`: the builder infers it from the graph, where an `edit` gives `repo` and an `emit` gives `writes`. Stating it asserts it instead, with the same error a JSON document gets. A graph that invokes another workflow must state it, because a callee's effects are not visible from the caller.

The raw constructors -- `node`, `edge`, `edge_when`, `edge_default`, `edge_lap`, `call_to`, `retry` -- stay registered as an escape hatch for a shape the combinators do not cover. `in`, `while` and `with` are Rhai keywords, which is why those take their arguments positionally.

## 7. Iteration, concretely

Specified, not built. See the status note at the top: the three constructs below parse, bound and cost correctly, and the runtime takes none of them.

### Retry: the same node, the same gate

```json
{"name": "fix", "op": "edit", "in": "finding", "check": "verify",
 "retry": {"max": "{$retries}", "while": "@verify != passed", "escalate": {"model": "opus"}},
 "task": "Fix this in `{@project}`:\n\n{title}\n\n{detail}"}
```

One run, up to `max + 1` attempts, one worktree, cumulative cost, one `attempts` row each. This is the existing path made declarative: `escalate` is `retry.max: 1` with a model change (plan 4.3). `while` takes the guard forms of an edge, negated with `!=` for a verdict. With `retries` declared `{"type": "int", "default": 1, "max": 2}`, the bound is computed at 2.

### Lap: two nodes alternating

```json
{"from": "fix",   "to": "audit"},
{"from": "audit", "to": "@output", "when": {"verdict": ["accept"]}},
{"from": "audit", "to": "fix",     "when": {"verdict": ["reject"]}, "max_laps": "{$laps}"},
{"from": "audit", "to": "handoff", "default": true}
```

`audit` is a `map out: 1` writing `verdict`; the guard reads it. The counter is on the unit, not the edge: two units crossing the same edge must not share a budget. Each lap mints a new unit whose `@parent` is the previous one and whose `@lap` is one higher, so the chain is readable and replay is exact.

`handoff` is the terminal path W24 requires. It takes the units that exhausted their laps and the ones whose verdict was unreadable.

### Recursion: a self-edge with depth

```json
{"name": "split", "op": "map", "out": "0..n", "in": "task", "emits": "task",
 "max_units": 5, "max_depth": "{$depth}",
 "task": "If `{text}` needs more than one coherent change, return the independent subtasks. Otherwise return nothing."}
```

```json
{"from": "split", "to": "split",     "when": {"@depth": {"<": "{$depth}"}}},
{"from": "split", "to": "@output",   "when": {"@children": {"==": 0}}}
```

A unit that produced children is settled; its children re-enter `split`. A unit that produced none is a leaf and leaves the workflow. Termination is the model's own answer under a depth cap (W25).

### Waiting: not a loop

```json
{"name": "merged", "op": "check", "rule": "pr-merged", "in": "run"}
```

The node is not runnable until the check passes. The pass ends, the next `pma workflow run` re-derives the frontier. This is what `ship.rs::settle` and the `pr-open` state already do (plan 4.8).

## 8. Rules

A rule node costs nothing, is deterministic and replays exactly.

### 8.1 For `map` and `reduce`

| `rule` | Does |
|-|-|
| `todo-items` | the project's `TODO.md` items from the last scan, as `item` units |
| `open-issues` | open issues through `gh issue list --json`, as units of the declared type |
| `open-runs` | the project's runs that are not shipped or rejected, as `run` units |
| `outdated-deps` | the last `--deps` measurement, as `signal` units |
| `as:<type>` | projects a unit onto another type by field name, dropping the rest. Refused when a required field of the target has no same-named source |
| `where:<field>=<v>` | keeps a unit whose field matches. `out: 0..1`; the drop reason is the rule |
| `path-present:<glob>`, `path-absent:<glob>` | keeps a unit whose project does or does not hold a matching path |
| `dedupe` | one unit per group, keeping the first. `reduce` only |
| `rank`, `limit:<n>` | orders or truncates a group. `reduce` only |

### 8.2 For `check`

Each writes `passed`, `failed` or `unknown`.

| `rule` | Reads |
|-|-|
| `verify` | the project's verify command against the run's tree. Already built (plan 1.6) |
| `scope-clean` | the run's recorded changed paths against its class (plan 1.7) |
| `lint-todo` | `pma lint` on the project's `TODO.md` |
| `ci-green` | required checks for the current head, through `gh` |
| `pr-merged` | the pull request's state, through `gh` |
| `nonempty` | whether the incoming bag holds at least one unit |

## 9. Sinks

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

## 10. Acceptance per op

| `op` | Clean when |
|-|-|
| `map out: 0..n` | `out.json` parses, every unit matches the declared type, the count is within `max_units`, `@depth` within `max_depth` |
| `map out: 1` | the above, plus the id is preserved and no field outside `writes` changed |
| `map out: 0..1` | the above, plus kept ids are a subset of the input and every dropped id carries a non-empty reason |
| `reduce` | every output unit names its inputs, and there is at most one per group |
| `edit` | the existing table of plan 1.8: base and head verify by class, scope respected |
| `check` | the rule ran. `unknown` is a result, not a failure |
| `emit` | the sink's rules held; every refusal is named per unit |
| a `doc` | the file exists, is a regular file, is non-empty, at most 1 MiB |

An acceptance failure adds a reason to `accept::review_reasons`. It approves and rejects nothing (plan 1.8). The unit routes by its guards; a node whose run was not clean sends its units to the default edge, or settles them when there is none.

## 11. Bounds and cost

Computed on the flattened graph, at every parameter's declared maximum:

A lap mints a unit and a retry does not: a retry is another attempt on the same run against the same unit, so it multiplies runs alone, while a lap hands a new unit to the node's successors.

```
bound(@input)        = the argument's unit count
units_seen(node)     = bound(in) x (1 + sum of max_laps on incoming lap edges)
bound(map 0..n)      = bound(in) x max_units, summed over depth <= max_depth
bound(map 1|0..1)    = bound(in)
bound(reduce)        = groups <= bound(in)
bound(call)          = the callee's own bound, with the caller's bag as its input
runs(node)           = units_seen(node) x (1 + retry.max)
cost                 = sum over agent nodes of runs(node) x agent_budget
```

Truncated by `caps.max_units` and `caps.max_edits` per instance, which is what actually holds a recursive graph: `max_units` per node compounds with depth, the instance cap does not. A node that hits a cap leaves the remainder undispatched and names it, rather than truncating silently.

`pma workflow propose` prints the table per node and stores the worst case over one unit of input, because the argument bag is not known until a pass names its target. `pma workflow activate` refuses a revision whose per-unit cost exceeds `workflow_budget`, a new setting under the config rules of the plan's "Configuration compatibility" section. The total for a pass is that figure times the argument bag, which the pass checks when it is built.

Rule nodes cost nothing and are excluded from the sum. A document whose graph is all rules has a worst case of zero, and section 15.9 is one.

## 12. Scheduling

One pass: collect every runnable node, run them, record, route the units, exit. Within a pass, runs are bounded by `max_parallel` and admitted by `batch_budget`, exactly as `pma dispatch` admits them today. `edit` runs get one worktree each, as they do now.

Two `edit` nodes over the same project in one pass produce two worktrees and two branches. That is today's behaviour under `--auto`, and the conflict at ship is plan 5.3's barrier problem, which this design does not solve.

## 13. Routing: `node` and `lap`

Two match dimensions, no new mechanism. `Policy::route` stays first match wins.

`src/route.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Subject<'a> {
    pub class: Class,
    pub complexity: i64,
    pub tier: Option<u8>,
    /// The qualified workflow node this dispatch serves, or `None` for a
    /// task dispatched on its own.
    pub node: Option<&'a str>,
    /// Laps completed by the unit. 0 outside a workflow.
    pub lap: i64,
}
```

A shared reference is `Copy`, so `Subject` stays `Copy`. `Route` gains:

```rust
    /// Node names this route serves: a qualified name, or `*/leaf` to match
    /// one node wherever it was called from. `None` matches only a dispatch
    /// that names no node, so an existing policy keeps its behaviour.
    pub nodes: Option<Vec<String>>,
    /// Laps this route serves, as an inclusive range.
    pub lap: Option<(i64, i64)>,
```

`Route::matches`, added before the class test:

```rust
        match (&self.nodes, s.node) {
            (None, None) => {}
            (None, Some(_)) | (Some(_), None) => return false,
            (Some(names), Some(node)) if !names.iter().any(|n| node_matches(n, node)) => {
                return false;
            }
            (Some(_), Some(_)) => {}
        }
        if let Some((lo, hi)) = self.lap
            && !(lo..=hi).contains(&s.lap)
        {
            return false;
        }
```

`node_matches` accepts an exact qualified name, or `*/leaf` against the name's last segment. Two forms, both checkable; no glob language.

`parse_route` reads `match.node` with the shape `match.class` already has -- a name or a list -- and `match.lap` with the existing range parser, so `"1-2"` and `2` both work. An empty node list is refused. `Policy::to_json` writes both, so two revisions still diff by what they mean.

The `unattended` guard needs no change. It reads `classes`, so a node route with `unattended` and no class list is refused exactly as a task route is.

`route::replay` fills `node` from `run.node` and `lap` from `run.lap`. Runs recorded before migration 20 have `node` null and `lap` 0, hence the same routes they matched before: replaying an old revision over old runs reports no new difference. That is the migration-safety property to test.

A node no route matches refuses the dispatch by name, as a task with no matching route does today. There is no fallback to `pma config agent`.

The `lap` dimension is what makes per-lap specialisation a policy statement rather than a document one: `{"match": {"node": "*/fix", "lap": "1-2"}, "model": "opus", "approval": "each"}` puts a second attempt on a stronger model under a human, without any workflow naming a model.

## 14. Schema 20

`VERSION` is 19 ([store.rs:26](../../src/store.rs)); the migration list is indexed by version, so `WORKFLOWS` is appended to `steps` and `VERSION` becomes 20.

```rust
/// Version 20. A workflow is a typed function over bags of units, stored as a
/// revision and activated like a routing policy. Calls are flattened before a
/// run, so the runtime holds one flat graph. Units are immutable and carry
/// their lineage, so a lap, an annotation and a decomposition each leave a
/// readable chain; a bag is derived from the units a node wrote and the
/// frontier from the moves recorded, so nothing holds a cursor a crash could
/// lose.
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
    args TEXT NOT NULL,
    target TEXT NOT NULL,
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
| `node` | `runs` | the same. Holds the qualified name, and the routing partition of W13 reads it |
| `unit` | `runs` | not driven by a unit |
| `lap` | `runs` | never crossed a lap edge |

Notes on shape:

- `workflow_instances.args` holds the parameters the run was given, so a replay reads the same instantiation rather than the document's defaults.

- A bag needs no table: it is the units whose `node` is that node. A unit's presence in a downstream node's input is `workflow_moves` with `taken = 1`.

- `workflow_moves.reason` holds the guard that refused a unit, so "why did this one stop" is answerable without re-deriving anything.

- The flattened graph is not stored. It is a function of the document and the arguments, both recorded, so it is recomputed rather than cached.

- No `shadow` column on `workflows`. `routes` has one because a route can be computed and not applied; a workflow has no counterfactual to compute. `pma route replay` covers the routing half of a node's decision.

- No foreign key from `runs` to `workflow_instances`. `runs` is the calibration corpus and rows are never deleted, so a run must survive a workflow being removed. `runs.route_revision` is unconstrained for the same reason.

- The exhaustion counter needs no schema change: its `revision` column is text, and a unit-driven task supplies `workflow:<instance>:<root>`, which is what makes retries and laps draw from one budget (W23).

- `dispatch::without_item` gains the `workflow:` prefix beside `campaign:`.

## 15. A library, and three compositions

Nine small workflows and three that call them. `->` is an edge, `[...]` a guard, `=>` a sink.

### 15.1 `find-issues(in: [Project], review_doc = "REVIEW.md", breadth = 20) -> [Finding] !pure`

Review, then confirm what the review claimed, then drop duplicates. Called by 15.10 and 15.12.

```
@input -> review -> confirm -> dedupe -> @output
```

```json
{
  "name": "find-issues",
  "in": "project", "out": "finding", "effects": [],
  "params": {
    "review_doc": {"type": "name", "default": "REVIEW.md"},
    "breadth":    {"type": "int",  "default": 20, "max": 40}
  },
  "caps": {"max_units": 60, "max_edits": 0},
  "nodes": [
    {"name": "review", "op": "map", "out": "0..n", "in": "project", "emits": "finding",
     "max_units": "{$breadth}", "doc": "{$review_doc}",
     "task": "Review `{name}`. Write your prose to {doc} and at most {$breadth} findings to {out}."},
    {"name": "confirm", "op": "map", "out": "0..1", "in": "finding", "emits": "finding",
     "writes": ["reason"],
     "task": "Each unit in {in} is a claim. Confirm it against the code. Keep what you can prove; drop the rest with a `reason`. Change nothing else."},
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
```

`review_doc` is the parameter that made this worth doing: the same workflow writes `REVIEW.md` for one caller and `AUDIT.md` for another, with no second document. `breadth` bounds the reviewer and, because it declares `max: 40`, the worst case is computed at 40 whatever a run passes.

Worst case: 1 review, 40 confirms, 0 for the rule. `!pure`, so it can run before phase 4b's gate.

### 15.2 `ensemble-review(in: [Project]) -> [Finding] !pure`

Three reviewers at three models, joined. The shape a sequence cannot express. Drop-in replacement for 15.1 at any call site, because the signature matches.

```
@input -+-> bugs  -+
        +-> tests -+-> merge(rule) -> confirm -> @output
        \-> api   -+
```

```json
{
  "name": "ensemble-review",
  "in": "project", "out": "finding", "effects": [],
  "params": {"breadth": {"type": "int", "default": 10, "max": 20}},
  "caps": {"max_units": 80, "max_edits": 0},
  "nodes": [
    {"name": "bugs", "op": "map", "out": "0..n", "in": "project", "emits": "finding",
     "max_units": "{$breadth}",
     "task": "Review `{name}` for correctness bugs. Write findings to {out}."},
    {"name": "tests", "op": "map", "out": "0..n", "in": "project", "emits": "finding",
     "max_units": "{$breadth}",
     "task": "Review `{name}` for missing tests. Write findings to {out}."},
    {"name": "api", "op": "map", "out": "0..n", "in": "project", "emits": "finding",
     "max_units": "{$breadth}",
     "task": "Review `{name}` for interfaces that are easy to use incorrectly. Write findings to {out}."},
    {"name": "merge", "op": "reduce", "via": "rule", "rule": "dedupe",
     "group_by": ["title"], "in": "finding", "emits": "finding"},
    {"name": "confirm", "op": "map", "out": "0..1", "in": "finding", "emits": "finding",
     "writes": ["reason"],
     "task": "Confirm each unit in {in} against the code. Drop what you cannot prove, with a `reason`."}
  ],
  "edges": [
    {"from": "@input", "to": "bugs"},
    {"from": "@input", "to": "tests"},
    {"from": "@input", "to": "api"},
    {"from": "bugs", "to": "merge"},
    {"from": "tests", "to": "merge"},
    {"from": "api", "to": "merge"},
    {"from": "merge", "to": "confirm"},
    {"from": "confirm", "to": "@output"}
  ]
}
```

The same workflow as a script. The three reviewers differ only in a phrase, so they are one function applied three times, and the fan-out and the barrier wire themselves:

```rhai
// A reviewer is a function from a graph to a graph, so three reviewers are
// three values in a list.
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
    .filter("confirm", ["reason"],
            "Confirm each unit in {in} against the code. Drop what you cannot prove, with a `reason`.")
    .output();

document(#{
    finding: #{ fields: #{
        severity: req(choice(["critical", "high", "medium", "low"])),
        title:    req(unique(line(200))),
        detail:   lines(40),
        reason:   line(200),
    }},
}, [
    workflow("ensemble-review", graph, #{
        params: #{ breadth: bounded("int", 10, 20) },
        caps:   #{ max_units: 80, max_edits: 0 },
    }),
])
```

Both forms produce the same document: the same five nodes in the same order, and the same eight edges in a different order. Edge order carries no meaning -- no two edges share a source and a target, guards are independent of each other, and the default edge is marked rather than positional. `pma workflow check` prints the same estimate for each, 86 agent runs over two projects at `breadth`'s maximum of 20. 31 lines against 102, and no edge written down.

The JSON above lists the workflow alone, against the `finding` type declared in section 6; the script carries that declaration, because a script's value is a whole document.

The three reviewers run in one pass, bounded by `max_parallel`. `merge` waits for all three because it has three incoming edges and a `reduce` is the barrier. Three prompts rather than three temperatures: identical prompts at one model mostly agree, and the cost triples either way.

Routes may give `ensemble-review/bugs` haiku and `ensemble-review/api` opus, which is the cost experiment this design exists for.

### 15.3 `apply-fixes(in: [Finding], laps = 2, retries = 1) -> [Finding] !repo !writes`

Fix, audit the fix, and hand back what would not converge.

```
@input -> fix -> audit -+-> @output [verdict=accept]
                        +-> fix     [verdict=reject] (max_laps {$laps})
                        \-> handoff (default) => TODO.md
```

```json
{
  "name": "apply-fixes",
  "in": "finding", "out": "finding", "effects": ["repo", "writes"],
  "params": {
    "laps":    {"type": "int", "default": 2, "max": 3},
    "retries": {"type": "int", "default": 1, "max": 2}
  },
  "caps": {"max_units": 40, "max_edits": 12},
  "nodes": [
    {"name": "fix", "op": "edit", "in": "finding", "check": "verify",
     "retry": {"max": "{$retries}", "while": "@verify != passed"},
     "task": "Fix this in `{@project}`:\n\n{title}\n\n{detail}"},
    {"name": "audit", "op": "map", "out": "1", "in": "finding", "emits": "finding",
     "writes": ["verdict", "reason"],
     "task": "Read the diff for `{title}`. Set `verdict` to accept or reject and `reason` to one line. You are checking the change, not making one."},
    {"name": "handoff", "op": "emit", "in": "finding", "sink": "todo", "action": "add",
     "map": {"priority": "{severity}", "text": "{title}", "description": "{reason}"}}
  ],
  "edges": [
    {"from": "@input", "to": "fix"},
    {"from": "fix", "to": "audit"},
    {"from": "audit", "to": "@output", "when": {"verdict": ["accept"]}},
    {"from": "audit", "to": "fix", "when": {"verdict": ["reject"]}, "max_laps": "{$laps}"},
    {"from": "audit", "to": "handoff", "default": true}
  ]
}
```

`audit` writes only `verdict` and `reason`, so it cannot restate the finding it is judging. `handoff` catches two cases -- laps exhausted, and a verdict left unreadable -- and both end with the human holding the task, which is the truthful outcome. `effects` names `writes` because of that sink, and a caller sees it in the signature.

### 15.4 `triage-issues(in: [Project]) -> [Finding] !pure`

The read costs nothing: `open-issues` is a rule. Only the classification spends a model.

```
@input -> issues(rule) -> classify -> @output [actionable=true]
```

```json
{
  "name": "triage-issues",
  "in": "project", "out": "finding", "effects": [],
  "params": {},
  "caps": {"max_units": 120, "max_edits": 0},
  "nodes": [
    {"name": "issues", "op": "map", "out": "0..n", "via": "rule", "rule": "open-issues",
     "in": "project", "emits": "finding", "max_units": 50},
    {"name": "classify", "op": "map", "out": "1", "in": "finding", "emits": "finding",
     "writes": ["severity", "class", "reason"],
     "task": "For each unit in {in}, set `severity`, a class, and a one-line `reason`. Set `severity` to low for anything that is not actionable."}
  ],
  "edges": [
    {"from": "@input", "to": "issues"},
    {"from": "issues", "to": "classify"},
    {"from": "classify", "to": "@output", "when": {"severity": ["critical", "high", "medium"]}}
  ]
}
```

A `low` unit is settled at `classify` with the guard recorded, so why it was not passed on is readable later. Because the output type is `finding`, this composes with `apply-fixes` exactly as `find-issues` does.

### 15.5 `specify(in: [Item], spec_doc = "SPEC.md") -> [Task] !pure`

For a one-liner an agent should not start coding against.

```
@input(item) -> seed(rule) -> write -> @output
```

```json
{
  "name": "specify",
  "in": "item", "out": "task", "effects": [],
  "params": {"spec_doc": {"type": "name", "default": "SPEC.md"}},
  "caps": {"max_units": 8, "max_edits": 0},
  "nodes": [
    {"name": "seed", "op": "map", "out": "1", "via": "rule", "rule": "as:task",
     "in": "item", "emits": "task"},
    {"name": "write", "op": "map", "out": "1", "in": "task", "emits": "task",
     "writes": ["spec"], "doc": "{$spec_doc}",
     "task": "The task is `{text}`. Write to {doc} what changes, which files, how it is checked, and what is out of scope. Put a one-line summary in `spec`."}
  ],
  "edges": [
    {"from": "@input", "to": "seed"},
    {"from": "seed", "to": "write"},
    {"from": "write", "to": "@output"}
  ]
}
```

`seed` exists because `item` is a built-in type with no `spec` field. The projection costs nothing and is what makes the type discipline real rather than assumed.

### 15.6 `decompose(in: [Task], depth = 2, width = 5) -> [Task] !pure`

```
@input -> split -+-> split   [depth < {$depth}]
                 \-> @output [children = 0]
```

```json
{
  "name": "decompose",
  "in": "task", "out": "task", "effects": [],
  "params": {
    "depth": {"type": "int", "default": 2, "max": 3},
    "width": {"type": "int", "default": 5, "max": 8}
  },
  "caps": {"max_units": 20, "max_edits": 0},
  "nodes": [
    {"name": "split", "op": "map", "out": "0..n", "in": "task", "emits": "task",
     "max_units": "{$width}", "max_depth": "{$depth}",
     "task": "`{text}` may be too large for one coherent change. If it is, return the independent subtasks, each one change. If it is not, return nothing."}
  ],
  "edges": [
    {"from": "@input", "to": "split"},
    {"from": "split", "to": "split", "when": {"@depth": {"<": "{$depth}"}}},
    {"from": "split", "to": "@output", "when": {"@children": {"==": 0}}}
  ]
}
```

Per-node worst case is 1 + 8 + 64 units at the declared maxima, so `caps.max_units: 20` is what holds it, and the truncation is named rather than hidden. Expect the depth cap to be reached: a model asked whether a task is small enough usually says no.

### 15.7 `implement(in: [Task], retries = 1) -> [Task] !repo`

```
@input -> build -> @output
```

```json
{
  "name": "implement",
  "in": "task", "out": "task", "effects": ["repo"],
  "params": {"retries": {"type": "int", "default": 1, "max": 2}},
  "caps": {"max_units": 20, "max_edits": 10},
  "nodes": [
    {"name": "build", "op": "edit", "in": "task", "check": "verify",
     "retry": {"max": "{$retries}", "while": "@verify != passed"},
     "task": "Implement `{text}`.\n\n{description}\n\n{spec}"}
  ],
  "edges": [
    {"from": "@input", "to": "build"},
    {"from": "build", "to": "@output"}
  ]
}
```

### 15.8 `record(in: [Finding], section = "critical") -> [Finding] !writes`

An effectful identity: it writes the items and returns its input, so a caller can record and continue.

```json
{
  "name": "record",
  "in": "finding", "out": "finding", "effects": ["writes"],
  "params": {"section": {"type": "enum", "default": "critical",
                         "values": ["critical", "high", "medium", "low"]}},
  "caps": {"max_units": 100, "max_edits": 0},
  "nodes": [
    {"name": "file", "op": "emit", "in": "finding", "sink": "todo", "action": "add",
     "map": {"priority": "{severity}", "text": "{title}", "description": "{detail}"}}
  ],
  "edges": [
    {"from": "@input", "to": "file"},
    {"from": "file", "to": "@output"}
  ]
}
```

### 15.9 `settle(in: [Project]) -> [Run] !writes`

Waiting with no loop and no model. Every node is a rule or a check, so the worst-case cost is `$0.00` and `pma workflow propose` prints it as such.

```
@input -> open-runs(rule) -> merged(check) -> close => note -> @output
```

```json
{
  "name": "settle",
  "in": "project", "out": "run", "effects": ["writes"],
  "params": {},
  "caps": {"max_units": 40, "max_edits": 0},
  "nodes": [
    {"name": "open-runs", "op": "map", "out": "0..n", "via": "rule", "rule": "open-runs",
     "in": "project", "emits": "run", "max_units": 20},
    {"name": "merged", "op": "check", "rule": "pr-merged", "in": "run"},
    {"name": "close", "op": "emit", "in": "run", "sink": "note", "action": "add",
     "map": {"text": "shipped: {task}"}}
  ],
  "edges": [
    {"from": "@input", "to": "open-runs"},
    {"from": "open-runs", "to": "merged"},
    {"from": "merged", "to": "close", "when": {"@merged": ["passed"]}},
    {"from": "close", "to": "@output"}
  ]
}
```

A run whose pull request is still open settles at `merged`, because `@merged` is then `failed` or `unknown` and no edge accepts it. The next pass picks it up.

### 15.10 `review-fix-critical(in: [Project], severity = "critical") -> [Finding] !repo !writes`

The original request, as a composition. One fork: everything confirmed is recorded for the human, and the severe ones are fixed.

```
@input -> call find-issues -+-> call record
                             \-> call apply-fixes [severity={$severity}] -> @output
```

```json
{
  "name": "review-fix-critical",
  "in": "project", "out": "finding", "effects": ["repo", "writes"],
  "params": {"severity": {"type": "enum", "default": "critical",
                          "values": ["critical", "high"]}},
  "caps": {"max_units": 120, "max_edits": 8},
  "nodes": [
    {"name": "issues", "op": "call", "workflow": "find-issues", "in": "project",
     "with": {"review_doc": "REVIEW.md", "breadth": 20}},
    {"name": "keep", "op": "call", "workflow": "record", "in": "finding",
     "with": {"section": "{$severity}"}},
    {"name": "repair", "op": "call", "workflow": "apply-fixes", "in": "finding",
     "with": {"laps": 2, "retries": 1}}
  ],
  "edges": [
    {"from": "@input", "to": "issues"},
    {"from": "issues", "to": "keep"},
    {"from": "issues", "to": "repair", "when": {"severity": ["{$severity}"]}},
    {"from": "repair", "to": "@output"}
  ]
}
```

Flattened, this is `issues/review`, `issues/confirm`, `issues/dedupe`, `keep/file`, `repair/fix`, `repair/audit`, `repair/handoff`: seven nodes, one instance, one set of caps. A route matching `*/fix` reaches the fix wherever it was called from; one matching `repair/fix` reaches only this call site.

Swapping `find-issues` for `ensemble-review` is a one-word change, because the signatures agree.

### 15.11 `plan-and-build(in: [Item], depth = 2) -> [Task] !repo`

```
@input -> call specify -> call decompose -> call implement -> @output
```

```json
{
  "name": "plan-and-build",
  "in": "item", "out": "task", "effects": ["repo"],
  "params": {"depth": {"type": "int", "default": 2, "max": 3}},
  "caps": {"max_units": 40, "max_edits": 10},
  "nodes": [
    {"name": "spec", "op": "call", "workflow": "specify", "in": "item",
     "with": {"spec_doc": "SPEC.md"}},
    {"name": "parts", "op": "call", "workflow": "decompose", "in": "task",
     "with": {"depth": "{$depth}", "width": 5}},
    {"name": "build", "op": "call", "workflow": "implement", "in": "task",
     "with": {"retries": 1}}
  ],
  "edges": [
    {"from": "@input", "to": "spec"},
    {"from": "spec", "to": "parts"},
    {"from": "parts", "to": "build"},
    {"from": "build", "to": "@output"}
  ]
}
```

Three workflows, none of which knows about the others. `depth` passes through from the caller's parameter to the callee's bound, and the worst case is computed at the caller's declared `max: 3`.

### 15.12 `portfolio-sweep(in: [Project]) -> [Finding] !pure !writes`

Across a tag rather than one project: `pma workflow run portfolio-sweep --tag rust`. The argument bag holds one unit per tagged project, frozen at the start, which is what a campaign does (plan 5.1) with no second mechanism.

```
@input(tag) -> call ensemble-review -> call record -> @output
```

```json
{
  "name": "portfolio-sweep",
  "in": "project", "out": "finding", "effects": ["writes"],
  "params": {},
  "caps": {"max_units": 400, "max_edits": 0},
  "nodes": [
    {"name": "look", "op": "call", "workflow": "ensemble-review", "in": "project",
     "with": {"breadth": 8}},
    {"name": "keep", "op": "call", "workflow": "record", "in": "finding",
     "with": {"section": "medium"}}
  ],
  "edges": [
    {"from": "@input", "to": "look"},
    {"from": "look", "to": "keep"},
    {"from": "keep", "to": "@output"}
  ]
}
```

No `edit`, so `effects` is `writes` alone and this runs before phase 4b's gate. With 12 tagged projects the worst case is 36 review runs and up to 192 confirms, which is the number `propose` prints and the reason `workflow_budget` exists.

### 15.13 Where the human is

| Workflow | Human | Gate it needs |
|-|-|-|
| 15.1, 15.2, 15.4, 15.5, 15.6 | reads the documents if they want | none. `!pure` |
| 15.8, 15.9, 15.12 | reads uncommitted `TODO.md` edits and notes; commits them | none |
| 15.3, 15.7, 15.10, 15.11 | `pma review`, then `pma review --approve <ids>`, then `pma ship` | phase 4b |

## 16. Acceptance

Signatures and composition:

- a call whose callee's input type disagrees is refused, naming both types

- a workflow declaring `!pure` that reaches an `edit` is refused

- a workflow that calls itself, directly or through a chain, is refused

- flattening qualifies node names by call site, so the same workflow called twice yields distinct nodes and distinct runs

- swapping `find-issues` for `ensemble-review` at a call site parses unchanged

Parameters:

- a parameter with no default is refused; one used as a bound with no `max` is refused

- an argument outside an enum's `values` is refused at run, not at parse

- the worst case is computed at every parameter's `max`, not its default

- `workflow_instances.args` is read back by a replay rather than the defaults

Routing:

- a route with `node` does not match a node-less dispatch, and one without `node` does not match a node

- `*/fix` matches `repair/fix`; `repair/fix` does not match `other/fix`

- `lap` matches a range, and a run outside a workflow has lap 0

- `to_json` round trip keeps `node` and `lap`; an empty node list is refused

- `unattended` with a node and no class list is refused

- replay over runs recorded before migration 20 reports no difference

The document: one test per refusal in 6.5, each naming its position as `route.rs`'s parse tests do. In particular a cycle without `max_laps`, a `max_laps` edge with no terminal path, a `map out: 0..n` with no `max_units`, a self-edge with no `max_depth`, a type disagreement across an edge, a guard on an undeclared field, and a node unreachable from `@input`.

Units and ops:

- `0..n` past `max_units` is refused, and the instance cap truncates with a named remainder

- `0..1` returning an id that was not in the input is refused; a drop with no reason is refused

- `1` changing a field outside `writes` is refused

- a `reduce` output with no provenance is refused

- a duplicate `unique: "normalised"` value against an open item is dropped and named, not failed

Iteration:

- `retry` appends attempts to one run in one worktree and stops at `max`

- a lap mints a new unit with `@parent`, `@lap + 1` and the same `@root`

- retries and laps draw from one counter: a unit that spent two attempts on retries cannot take a third on a lap

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

End to end, on fixture repositories: 15.10 with two findings, one fixed and shipped; 15.12 over three projects with no `edit` at all; 15.9 with one merged and one open pull request.

## 17. Commands

```sh
pma workflow                          # revisions, and instances with their frontier
pma workflow propose <file>           # store a draft; print the worst case; activate nothing
pma workflow activate <rev>           # put it into effect, recording who
pma workflow show <rev|instance>      # the flattened graph, or an instance's units and moves
pma workflow run <name> <target>      # one pass, then exit; --set k=v per parameter
pma workflow run <name> --dry-run     # the flattened graph, the frontier, and the route per node
pma workflow stop <instance>          # no further node; runs already open stand
```

`run` holds `session.lock` for the pass, as `dispatch` does. `pma report --by node` and `--by workflow` follow, for the reason plan 1.10 refuses `--by model` until routing produces one: the dimension arrives with the data.

## 18. Limits

**A workflow cannot be a value.** No workflow takes another as a parameter, so "review with whichever reviewer this caller prefers" means two call sites rather than one higher-order call. Flattening is why: a call must be resolvable at propose time for the bound to exist. If this bites, the smallest fix is a parameter of type `workflow` constrained to a declared signature, resolved at propose time from a closed list.

**A script cannot reach into a pass.** It builds a document and stops. A custom deterministic rule for `map` or `reduce` -- a projection or a ranking the closed rule list cannot express -- is the one seam where running a script later would add something, and it would have to stay pure, effect-free and unable to influence the graph's shape. The trigger is a third library wanting the same projection; until then `as:`, `where:`, `dedupe`, `rank` and `limit:` are the list.

**Two `edit` nodes can conflict at ship.** Separate worktrees make them safe to run; merging them is plan 5.3's barrier problem, and nothing here solves it. `caps.max_edits` bounds how bad it gets.

**A node's input is unbounded in size.** A 200-file repository does not fit a prompt, and no node declares which files it reads. The agent walks the worktree within its timeout.

**Recursion is the least bounded construct.** The model decides termination and is bad at it. Only `max_depth` and the instance cap hold, and they truncate rather than converge.

**Artifacts are never collected.** `<data>/artifacts/<instance>/` grows with every pass. A finished instance is an unambiguous trigger, as minos section 11 says of a completed workflow, and nothing acts on it.

**Per-node specialisation is unmeasured.** The premise -- a cheap model reviews, a strong one fixes -- has no data behind it. `pma report --by node` is what would show it, and it needs runs first.

**A confirming node reads its producer's prose.** It is meant to check the producer, so what it inherits matters. Here it inherits the units and the named document, which is minos D19's default setting expressed as a data edge. When minos lands, a node's grant carries `since` and the same choice becomes three-valued.

## 19. Gates and sequencing

| Part | Contents | Gate |
|-|-|-|
| 6a | types, parameters, units, `map` at three bounds, `check`, `emit`, guards, default edges, `@input` and `@output`, the pass, caps and the cost bound, migration 20, `node` and `lap` on a route, the commands | none. No `edit`, so nothing changes a repository: 15.1, 15.4, 15.5, 15.8, 15.9 run under it |
| 6b | `call` and propose-time flattening | 6a in use. 15.12 runs here |
| 6c | `edit` with `retry` | phase 0 measured and phase 4b's gate met: 10 runs shipped through batch approval. 15.7, 15.10, 15.11 run here |
| 6d | `reduce` and lap edges | 6a in use. 15.2 and 15.3 run here |
| 6e | recursion | evidence that one level of decomposition helps. 15.6 runs here |

No node is `unattended` before phase 4c's gate. The parse guard already refuses it for A-, C, D and for a route with no class list.

Size: 4 sessions for 6a, 1 for 6b, 2 for 6c, 2 for 6d, 1 for 6e. All exclude agent cost.
