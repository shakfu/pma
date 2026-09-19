# pma workflows (draft)

Status: nothing here is built. 2026-09-19.

A workflow is an ordered set of stages over one project. A stage is one agent invocation, or a fan-out of one invocation per row of a structured artifact. Each stage may take a different agent and model, so cost and strength are chosen per stage rather than per task.

Nothing in the mechanism is specific to reviewing. A stage reads artifacts and writes one; what the artifact holds is declared in the document. Section 9 works four shapes through it: review then fix, one-liner then specification then implementation, triage of GitHub issues, and probe then apply across repositories. The fourth does not fit, and section 11 says why.

This specifies the document, the artifact contract, the one routing change and the one migration. It is phase 6 of [implementation-plan.md](implementation-plan.md). The chat and container half is `~/projects/minos/docs/dev/design.md`, which states the same definition from the other side: "It is `pma` policy: minos does not know what a workflow is and carries only its id."

## 1. What already exists

The document is thin because most parts are built.

| Part | Built as | Where |
|-|-|-|
| per-stage agent, model, approval, escalation | `Route` | [route.rs:71](../../src/route.rs) |
| a versioned policy, proposed then activated, replayable | `routes`, `pma route propose\|activate\|replay` | plan 4.1-4.4 |
| a definition over a frozen project set | `campaigns`, `campaign_members` | [store.rs:1053](../../src/store.rs) |
| a task with no `TODO.md` line | `dispatch::without_item` | [dispatch.rs:112](../../src/dispatch.rs) |
| worktree, base and head verify, scope check, attempt counter | phase 1 | `dispatch.rs`, `class.rs`, `accept.rs` |
| why a human must read a run | `accept::review_reasons` | [accept.rs:34](../../src/accept.rs) |

Missing: a sequencer, an artifact contract, and an acceptance rule for a stage that changes no code.

## 2. Decisions

### The mechanism

**W1. A workflow is a stored revision, proposed then activated.** The two-act shape is `routes`': a revision is a draft until someone activates it, and each run records the revision it ran under. Rejected: a file read at run time, which leaves no record of what ran.

**W2. Agent and model live in the routing policy, not in the workflow document.** A stage names no agent. `stage` becomes a match dimension on a route (section 6). Rejected: inline `agent` and `model` per stage. Plan 2.5 already refused a second place to name them: "a second place to say the same thing would have to be reconciled with it."

**W3. A route that states no `stage` matches only a dispatch with no stage.** Stage is a partition, not a filter. Rejected: unstated means any, under which the trailing catch-all route of an existing policy absorbs every stage silently at whatever approval it names.

**W4. Artifacts live outside the worktree**, at `<data>/artifacts/<instance>/<stage>/`. The path is passed in the prompt. Rejected: the worktree. An untracked file there enters `dispatch::changed_paths`, counts as a violation for any class whose scope is bounded ([class.rs:101](../../src/class.rs)), and `git add -A` at ship publishes it. A stage that wants its artifact committed says `"publish": true`.

**W5. A stage that changes no code is accepted by its artifact.** Verify at base against verify at head discriminates nothing for a document, so the acceptance table of plan 1.8 returns nothing. Three acceptance forms: `verify`, `artifact`, `rows:<schema>`.

**W6. Readiness is derived, never stored.** A stage is runnable when every predecessor's runs are final and every artifact it consumes exists. There is no stored cursor, so a crash resumes by re-deriving. This is plan 1.7's rule applied to sequencing: store the evidence, not the verdict.

**W7. `pma workflow run` is a pass, not a daemon.** It advances every runnable stage of every named instance and exits, holding `session.lock` for its whole pass like `pma dispatch`. A stage boundary that needs approval ends the pass; the next invocation resumes it. Rejected: a resident process, which would hold the lock across a human decision and need its own restart story. This is also the shape plan 4.11 needs for a scheduled pass.

**W8. One instance per project; a project set is a set of instances.** Membership is frozen when the set is taken, as `add_campaign` freezes it, so a rescan cannot move work under an instance in flight.

**W9. Approval stays a route property.** A workflow adds no approval mode. The intended first use of a fan-out stage is `batch`: `pma review --approve <ids...>`, which checks every named run before approving any (plan 4.5).

**W10. Stages are a sequence. No branches, no conditionals, no parallel stages.** `on_refusal` takes `stop` or `continue`. Retry is `escalate` on the route (plan 4.3) and rework is `pma review --rework`. What this excludes is real and named in section 11.

**W11. A workflow is not an escape from the class rules.** Every stage run goes through `dispatch::prepare` and the phase 1 gates: class D is refused before a worktree exists, `TODO.md` and the privileged paths are refused to the classes that may not touch them, and `unattended` on A-, C or D is refused where a policy is read. A workflow chooses order and prompts, never authority.

**W12. `pma-agent` may propose a revision and request a trigger; it may not activate one.** minos design section 2 puts "which workflow a situation gets" behind a submission to `pma`, and D2 keeps mint, ship and push out of a model's hands. `pma workflow activate` records who activated it, as `pma route activate` does.

### The artifact contract

**W13. An artifact's schema is declared in the document, not registered in Rust.** A revision carries a `schemas` block; `pma` checks a produced artifact against the declaration it names. Rejected: a schema per workflow kind compiled in, which was the first draft of this document. It cost about the same code and made every new workflow kind a code change, which is how a mechanism becomes a review mechanism.

The declaration is deliberately weak: field presence, type, enumerated values, length, uniqueness. It cannot express that a finding is real or that a specification is complete. Those are what the next stage and the reviewer are for.

**W14. A structured artifact is a set of rows, each with an `id`.** One shape covers findings, specifications, target lists and issue triage, and it is what a fan-out and a sink both need: something to iterate and something to key by. Rejected: free-form JSON per kind, which no general fan-out can read.

**W15. No stage writes `TODO.md`. A stage emits rows and `pma` writes the items.** `OWNED` puts `TODO.md` outside every class's scope ([class.rs:34](../../src/class.rs)), the dispatch prompt says so ([dispatch.rs:502](../../src/dispatch.rs)), and plan 4.7 depends on it: ship ticks the item after the rebase, which is admissible only because no agent may touch the file. An agent rewriting items also breaks item identity, which is file plus normalised text, and with it the attempt counter and `pma sync`'s `gh:N` links.

**W16. A fan-out reads rows, not `TODO.md`.** Dispatch requires the item open in the remote default branch (`dispatch::on_origin`), so an item written by an earlier stage cannot be dispatched until it is committed and pushed. A row-keyed task takes the existing no-item path instead, with key `workflow:<instance>:<row>`. A sink is therefore independent of a fan-out: switching `emits` off changes nothing about what the next stage runs.

**W17. A fan-out task's attempt counter keys on the instance and the row id**, not on normalised text. `dispatch::revision` keys on text so that a reworded specification starts fresh ([dispatch.rs:124](../../src/dispatch.rs)); rows are re-minted on every pass, so text keying would reset the limit whenever the producing agent rephrased.

**W18. `where` filters on enumerated fields only, by equality against a closed set.** No expression language. A filter on a free-text field is refused where the document is read, because it cannot be checked and it hides which rows a stage will take.

## 3. The document

JSON, for the reason `route.rs` states: this crate parses JSON already. `schemas` sit beside `workflow` so two workflows can share one.

```json
{
  "schemas": {
    "findings": {
      "rows": "findings",
      "max_rows": 20,
      "fields": {
        "id":       {"type": "id",    "required": true},
        "severity": {"type": "enum",  "required": true, "values": ["critical", "high", "medium", "low"]},
        "title":    {"type": "line",  "required": true, "max": 200, "unique": "normalised"},
        "detail":   {"type": "lines", "max": 40},
        "paths":    {"type": "list"},
        "class":    {"type": "enum",  "values": ["A", "A-", "B", "C", "D"]},
        "tags":     {"type": "list"}
      }
    }
  },

  "workflow": [
    {
      "name": "review-fix-critical",
      "stages": [
        {
          "name": "review",
          "task": "Review this project. Write your findings to {artifact}.",
          "produces": "REVIEW.md",
          "accept": "artifact"
        },
        {
          "name": "validate",
          "task": "Validate the review in {consumes}. Keep only findings you can confirm against the code. Write them to {artifact} as JSON matching the `findings` schema.",
          "consumes": ["REVIEW.md"],
          "produces": "findings.json",
          "accept": "rows:findings",
          "emits": {
            "sink": "todo",
            "priority": "{severity}",
            "text": "{title}",
            "description": "{detail}",
            "tags": "{tags}"
          }
        },
        {
          "name": "fix",
          "expands": {
            "artifact": "findings.json",
            "where": {"severity": ["critical"]},
            "task": "Fix this, in project `{project}`:\n\n{title}\n\n{detail}",
            "class": "{class}"
          },
          "accept": "verify",
          "on_refusal": "continue"
        }
      ]
    }
  ]
}
```

### Workflow

| Field | Type | Default | Meaning |
|-|-|-|-|
| `name` | string | required | Unique in the revision. Names the instance and appears on every run. |
| `stages` | array | required | Ordered, at least one. |

### Stage

| Field | Type | Default | Meaning |
|-|-|-|-|
| `name` | string | required | Unique in the workflow. Matched by a route's `stage` condition, and stored on the run. |
| `task` | string | required unless `expands` | The prompt body. `{artifact}`, `{consumes}` and `{project}` are replaced. |
| `consumes` | array of names | `[]` | Artifacts produced by earlier stages of this workflow. |
| `produces` | name | none | A file name, written under the stage's artifact directory. |
| `accept` | `verify` \| `artifact` \| `rows:<schema>` | `verify` | Section 4. |
| `publish` | bool | `false` | Copy the artifact into the worktree, so ship commits it. |
| `emits` | object | absent | Write the rows somewhere `pma` owns. Section 5. |
| `expands` | object | absent | One run per row. Section 4.3. |
| `on_refusal` | `stop` \| `continue` | `stop` | What a non-clean run of this stage does to the instance. |
| `source` | name | absent | An input `pma` produces rather than an agent. Not built; see section 11. |

### Schema

| Field | Type | Meaning |
|-|-|-|
| `rows` | name | The document's array field holding the rows. |
| `max_rows` | int | 1 to this many rows. A producer past it is refused, not truncated. |
| `fields` | object | Field name to declaration. An undeclared field in a row is refused. |

Field types, a closed list:

| `type` | Checked |
|-|-|
| `id` | `[A-Za-z0-9_-]{1,16}`, unique in the document. Exactly one field per schema may be `id`, and it is required. |
| `enum` | A string in `values`. The only type `where` may filter on. |
| `line` | A one-line string, 1 to `max` characters. `unique: "normalised"` compares by `todo::normal_text`. |
| `lines` | An array of strings, at most `max` entries. |
| `list` | An array of strings, at most 20 entries, each at most 200 characters. |
| `int` | A whole number, within `min` and `max` when given. |
| `bool` | `true` or `false`. |

Every declaration takes `required`, default false.

### Refused where the document is read

For the reason `route.rs` gives -- a stage that silently does nothing is worse than one that is refused:

- a duplicate stage, workflow or schema name

- a `consumes` naming an artifact no earlier stage produces

- a stage with neither `task` nor `expands`

- `accept` naming `artifact` or `rows:` with no `produces`

- `accept: rows:<name>` where no schema has that name

- an `expands` or `emits` naming an artifact whose stage does not accept rows

- a `where` on a field that is not `enum`, or a value outside its `values`

- a `{field}` placeholder in a `task`, `emits` or `expands` naming no declared field

- a schema with no `id` field, or with two

- an unknown field, at any level

## 4. Acceptance

### 4.1 The three forms

| `accept` | Clean when | Used by |
|-|-|-|
| `verify` | the existing table of plan 1.8: base and head verify by class, scope respected | a stage that changes code |
| `artifact` | `produces` exists, is a regular file, is non-empty, at most 1 MiB | a prose stage: a review, a specification, a draft |
| `rows:<schema>` | `artifact`, plus the declaration in section 4.2 | a stage whose output another stage or a sink reads |

An acceptance failure adds a reason to `accept::review_reasons`. It approves and rejects nothing, per plan 1.8. The instance stops or continues by `on_refusal`.

The 1 MiB ceiling is stated so a stage cannot fill the artifact directory, and because the next stage reads the file into a prompt.

### 4.2 Checking rows

In order: the file parses as JSON; the top level is an object; `schema` equals the declared name; `rows` is an array of 1 to `max_rows` objects; each row's fields are all declared; every `required` field is present; each value matches its type.

Two rules are applied after that, and they drop a row rather than failing the stage, because a duplicate is the normal result of running a workflow twice:

- a `unique: "normalised"` value matching an open item of that project at the last scan

- a value matching a row this instance already dispatched or emitted

Each drop is named in the stage's record.

### 4.3 Fan-out

```json
"expands": {
  "artifact": "findings.json",
  "where": {"severity": ["critical"]},
  "task": "Fix this, in project `{project}`:\n\n{title}\n\n{detail}",
  "class": "{class}"
}
```

| Field | Default | Meaning |
|-|-|-|
| `artifact` | required | An earlier stage's rows. |
| `where` | `{}` | Enum field to allowed values. Every condition must hold. |
| `task` | required | The prompt, with `{field}` from the row and `{project}`. |
| `class` | `B` | A class name, or `{field}` reading one from the row. `B` is `Class::of`'s default. |
| `max_runs` | 10 | Rows past it are left undispatched and named. A cap `pma` applies whatever the schema allows. |

One run per matching row: its own worktree, base and head verify, scope check and attempt counter, keyed `workflow:<instance>:<row>` (W16, W17). The row's other fields are recorded on the run.

A row-declared `class` widens nothing. `A-` is refused for `unattended` where a policy is read, and the privileged paths are judged against the paths a run actually changed, whatever class was predicted (plan 1.4, 1.7). A row whose class is `D` is not dispatched, which is how a producing stage hands work to the human.

Scope is not read from a row. It is policy: a route carries one ([route.rs:71](../../src/route.rs)) and a class resolves one ([class.rs:89](../../src/class.rs)). A `paths` field is recorded and shown.

## 5. Sinks

`emits` writes rows to a place `pma` owns. The mapping is in the document; the sinks are a closed list, because each one writes to a real file or table.

| `sink` | Writes | Mapping fields |
|-|-|-|
| `todo` | items in the project's `TODO.md`, uncommitted, in the user's clone | `priority` (one of the four section names), `text`, `description`, `tags` |
| `note` | a portfolio note per row, through `Store::add_note` | `text` |
| `issue` | refused | `pma sync` owns issue creation for `Critical` items (design.md, Sync). A second creator needs reconciling with it first. |

`todo` is the `pma sync` precedent, not a ship batch: the edit is uncommitted and `scripts/commit_todo.py` commits it.

A new `todo::insert(text, priority, item, description) -> Option<String>`:

- inserts before the section's first `###` heading, or at the end of the section when it has none. Appending at the end would place the item under a trailing group heading and mislabel it.

- refuses when the `##` section heading is absent, rather than creating it.

- writes nothing when the file has lint errors, for the reason `pma sync` skips such a file: duplicate text breaks item identity.

## 6. Routing: `Subject.stage`

One field, one match dimension, no new mechanism. `Policy::route` stays first match wins.

`src/route.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Subject<'a> {
    pub class: Class,
    pub complexity: i64,
    pub tier: Option<u8>,
    /// The workflow stage this dispatch serves, or `None` for a task
    /// dispatched on its own.
    pub stage: Option<&'a str>,
}
```

A shared reference is `Copy`, so `Subject` stays `Copy`. `Route` gains:

```rust
    /// Stage names this route serves. `None` matches only a dispatch that
    /// names no stage, so an existing policy keeps its behaviour exactly.
    pub stages: Option<Vec<String>>,
```

`Route::matches`, added before the class test:

```rust
        match (&self.stages, s.stage) {
            (None, None) => {}
            (None, Some(_)) | (Some(_), None) => return false,
            (Some(names), Some(stage)) if !names.iter().any(|n| n == stage) => return false,
            (Some(_), Some(_)) => {}
        }
```

`parse_route` reads `match.stage` with the shape `match.class` already has: a name, or a list of names. An empty list is refused, since it would match nothing. `Policy::to_json` writes `stage` into the condition map, so two revisions still diff by what they mean.

The `unattended` guard needs no change. It reads `classes`, so a stage route with `unattended` and no class list is refused exactly as a task route is.

`route::replay` fills `stage` from `run.stage`. Runs recorded before migration 20 have `stage` null, hence `None`, hence the same routes they matched before: replaying an old revision over old runs reports no new difference. That is the migration-safety property to test.

A stage whose name no route matches refuses the dispatch by name, as a task with no matching route does today. There is no fallback to `pma config agent`.

## 7. Schema 20

`VERSION` is 19 ([store.rs:26](../../src/store.rs)); the migration list is indexed by version, so `WORKFLOWS` is appended to `steps` and `VERSION` becomes 20.

```rust
/// Version 20. A workflow is an ordered set of stages over one project,
/// stored as a revision and activated like a routing policy. An instance is
/// per project, and its progress is derived from its runs rather than stored,
/// so a crash resumes by re-deriving rather than by trusting a cursor.
const WORKFLOWS: &str = "
CREATE TABLE workflows (
    revision INTEGER PRIMARY KEY,
    document TEXT NOT NULL,
    proposed_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    activated_at INTEGER,
    activated_by TEXT
);
CREATE TABLE workflow_runs (
    id INTEGER PRIMARY KEY,
    workflow TEXT NOT NULL,
    revision INTEGER NOT NULL REFERENCES workflows(revision),
    project TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    outcome TEXT
);
CREATE INDEX idx_workflow_runs_project ON workflow_runs(project, workflow);
ALTER TABLE runs ADD COLUMN workflow_run INTEGER;
ALTER TABLE runs ADD COLUMN stage TEXT;
ALTER TABLE runs ADD COLUMN row TEXT;
";
```

| Column | On | Null means |
|-|-|-|
| `workflow_run` | `runs` | dispatched on its own, not by a workflow |
| `stage` | `runs` | the same, and the routing partition of W3 reads it |
| `row` | `runs` | not a fan-out run |

Notes on shape:

- No `shadow` column on `workflows`. `routes` has one because a route can be computed and not applied; a workflow has no counterfactual to compute. `pma route replay` already covers the routing half of a stage decision.

- No foreign key from `runs` to `workflow_runs`. `runs` is the calibration corpus and rows are never deleted, so a run must survive a workflow definition being removed. `runs.route_revision` is unconstrained for the same reason.

- `workflow_runs.outcome` is `finished`, `stopped` or `abandoned`. Which stage it reached is derived from its runs.

- The exhaustion counter needs no schema change: its `revision` column is text, and a fan-out task supplies `workflow:<instance>:<row>` in place of the normalised item text (W17).

- `dispatch::without_item` gains the `workflow:` prefix beside `campaign:`.

- `row` holds the row's `id` value, not its text, so a reworded row is the same task (W17).

## 8. Commands

```sh
pma workflow                         # revisions, and instances with their stage
pma workflow propose <file>          # store a draft; activate nothing
pma workflow activate <rev>          # put it into effect, recording who
pma workflow show <rev|instance>
pma workflow run <name> <project>    # or --tag <t>; one pass, then exit
pma workflow run <name> --dry-run    # what the pass would dispatch, and under which route
pma workflow stop <instance>         # no further stage; runs already open stand
```

`run` holds `session.lock` for the pass, as `dispatch` does. `pma report --by stage` follows, for the reason plan 1.10 refuses `--by model` until routing produces one: the dimension arrives with the data.

## 9. Four shapes

The first three are the same mechanism with different documents. The fourth is what does not fit.

### 9.1 Review, validate, fix

The document of section 3. `pma workflow run review-fix-critical cynn`, with `max_parallel = 2`:

| Pass | What runs | Where the human is |
|-|-|-|
| 1 | `review`: one run, route `review-cheap`, approval `propose`, writes `REVIEW.md`, accepted as `artifact` | reads it if they want; nothing is published |
| 1 | `validate` is runnable at once: route `validate-strong`, writes 6 rows, one dropped as a duplicate of an open item | `TODO.md` in the clone gains 5 items, uncommitted |
| 1 | `fix` expands the 2 rows with `severity: critical`: 2 runs, keys `workflow:17:F1` and `workflow:17:F4`, one worktree each, route `fix-strong`, approval `batch` | `pma review`, then `pma review --approve 41 42` |
| 2 | nothing runnable; the instance is `finished` | `pma ship` |

Four runs, three routes, three models if the policy names three. The review file never enters a worktree diff, and no agent touched `TODO.md`.

### 9.2 Specify, then implement

One `TODO.md` one-liner into a specification, then the change. No fan-out, two stages, one artifact:

```json
{
  "name": "specify-then-implement",
  "stages": [
    {"name": "specify",
     "task": "The task is: {project}'s item `{consumes}`. Write a specification to {artifact}: what changes, which files, how it is checked, and what is out of scope.",
     "produces": "SPEC.md", "accept": "artifact"},
    {"name": "implement",
     "consumes": ["SPEC.md"],
     "task": "Implement the specification in {consumes}. Do not widen it.",
     "accept": "verify"}
  ]
}
```

Routes: `specify` at a strong model under `propose`, `implement` at a cheaper one under `each`. This is the shape the plan's phase 3.7 was reaching for from the other direction -- it improves what an agent is told, rather than guessing whether the telling was good enough.

### 9.3 Triage GitHub issues

Rows come from an artifact the *first* stage writes by reading the repository's issues, which today means an agent with `gh` in its allowlist. With `source` built (section 11) the first stage disappears and `pma` produces the rows.

```json
{
  "name": "triage-issues",
  "stages": [
    {"name": "classify",
     "task": "Read the open issues in {project} with `gh issue list --json number,title,body,labels`. For each one, decide a priority and whether it is actionable. Write rows to {artifact} matching the `triage` schema.",
     "produces": "triage.json", "accept": "rows:triage",
     "emits": {"sink": "todo", "priority": "{priority}", "text": "{title}", "description": "{why}"}}
  ]
}
```

One stage, no fan-out, one sink. The `triage` schema declares `id, number, priority, title, why, actionable`. Nothing is dispatched: the output is items in `TODO.md` for the human to rank.

### 9.4 Probe, then apply across repositories

Add a minimal CI workflow to every repository without one. The fan-out axis is projects, not rows, and this design has no way to say that. It is the campaign mechanism (plan 5.1, 5.2), and section 11 states the unresolved merge.

## 10. Acceptance

Routing:

- a route with `stage` does not match a stage-less dispatch, and a route without `stage` does not match a stage

- `to_json` round trip keeps `stage`; a revision with an empty stage list is refused

- `unattended` with a stage and no class list is refused

- replay over runs recorded before migration 20 reports no difference

The document:

- each refusal in section 3 has a test naming its position, as `route.rs`'s parse tests do

- a `{field}` naming no declared field is refused

- a `where` on a `line` field is refused

Rows:

- duplicate `id`; a duplicate `line` by `normal_text`; a `line` ending in `due:2026-10-01`, which `todo::is_token` accepts and an item line may not carry as text; an unknown `enum` value; `max_rows` plus one; an undeclared field present; a row duplicating an open item is dropped and named, not failed

- a schema with two `id` fields is refused

Artifacts:

- a prose stage's `changed_paths` is empty, and its artifact is outside the worktree

- with `publish: true` the artifact is in the diff; without it, ship does not carry it

Sinks:

- `todo` into a section with a `###` group inserts above the group

- a missing section refuses

- a file with lint errors is skipped

- `note` writes one note per row

Fan-out:

- `where` selects a subset, and `max_runs` leaves the remainder undispatched and named

- a row whose `class` is `D` is not dispatched

- `on_refusal: continue` finishes the rest; `stop` does not start the remainder

- two passes over the same rows do not dispatch the same row twice

Sequencer:

- a second pass with no intervening approval advances nothing

- an instance whose process was killed mid-pass re-derives the same next stage

CLI, end to end: a fixture repository, the section 3 document, 2 rows, both fan-out runs verified, one approved and shipped.

## 11. Limits

Each of these is a shape the mechanism refuses. They are stated rather than solved, and each names what it would cost.

**No stage input that `pma` produces.** A triage workflow starts by having an agent shell out to `gh`, which needs an allowlist entry and spends a model on work `pma` already does. The fix is a `source` field naming a closed list of producers -- `todo`, `issues`, `runs`, `scan` -- run by `pma`, costing nothing, accepted automatically. The field is in the stage table now so that adding it later is additive rather than a reshape. Build it when a second workflow wants it.

**One fan-out axis.** Rows of an artifact, within one project. Fan-out over projects is a campaign, and the two objects are not joined: a campaign is one task over many projects, a workflow many stages over one. `--tag` gives a set of instances, which is a fan-out of workflows rather than a workflow over a set, and it cannot express probe-then-apply, where one stage decides the membership of the next. Merging them means one object -- stages by projects -- with campaigns as the single-stage case. That is a larger change than this document, and it should not be attempted before a real campaign runs.

**No parallel stages and no join.** Three reviewers at three models, merged and deduplicated, is a common and cheap ensemble, and this refuses it. Sequencing two prose stages costs a second full pass for no reason. What it needs: a stage group that runs together, and a merge rule for rows with the same normalised text. What it risks: several agents in one project at once, which is where minos D10 draws its line, although read-only prose stages in separate worktrees are the easy case.

**No conditional edge.** Upgrade dependencies, and only on breakage produce a findings artifact and fix them, is a natural three-stage workflow with one condition. `on_refusal` is not that condition: it decides whether the instance continues, not which stage comes next. A `when` on a stage, reading an earlier stage's outcome, is the smallest form. Refused for now because W10 keeps the document a sequence, and because a second control-flow field invites a third.

**A prose stage's input is unbounded.** A 200-file repository does not fit a prompt, and no stage declares which files it reads. Today's answer is that the agent walks the worktree within its timeout.

**Artifacts are never collected.** `<data>/artifacts/<instance>/` grows with every pass. A finished instance is an unambiguous trigger, as minos section 11 says of a completed workflow, and nothing acts on it.

**Per-stage specialisation is unmeasured.** The premise -- a cheap model reviews, a strong one fixes -- has no data behind it. `pma report --by stage` is what would show it, and it needs runs first.

**A validating stage reads the producer's prose.** It is meant to check the producer, so what it inherits matters. Here it inherits the named artifacts and nothing else, which is minos D19's default setting expressed as an artifact edge. When minos lands, a stage's grant carries `since` and the same choice becomes three-valued.

## 12. Gates and sequencing

| Part | Contents | Gate |
|-|-|-|
| 6a | the document and its schemas, migration 20, `Subject.stage`, `accept: artifact` and `rows:`, the `todo` and `note` sinks, `pma workflow` commands. Prose and row stages only | none. It publishes nothing: stages produce artifacts, and a sink writes an uncommitted `TODO.md` edit |
| 6b | `expands` | phase 0 measured, and phase 4b's gate met: 10 runs shipped through batch approval |

6a is worth having on its own: 9.2 and 9.3 both run under it, and it publishes nothing.

No stage is `unattended` before phase 4c's gate. The parse guard already refuses it for A-, C, D and for a route with no class list.

Size: 3 sessions for 6a, 2 for 6b. Both exclude agent cost.
