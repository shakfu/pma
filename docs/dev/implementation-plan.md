# pma implementation plan

Derived from `design-review.md` (2026-09-17) and revised against `plan-review.md` (2026-09-18). Sequenced by dependency and by evidence: records before gates, gates before autonomy, data before policy, cuts throughout.

Each phase states what it changes, how it is accepted, and the gate that must hold before the next phase starts. Sizes are rough estimates, in sessions of work, not commitments.

Revision note. The first version of this plan placed phase 1's gate ahead of the records that prove it, numbered one migration for a column that already exists, and treated unattended shipping as a single step. This version moves the attempt and decision records into phase 1, splits phase 4 into three gated parts, and adds the config schema work that the worker adapter needs. Superseded decisions in `design.md` are listed under "Amendments to design.md".

## Phase 0: measured pilot

Goal: produce the evidence the rest of the plan depends on. No new features. Manual review throughout.

| Step | Detail |
|-|-|
| 0.1 | Tier the repositories you know well, plus `pma`. Commit and push every pilot item before dispatch: the dispatcher requires the item to be open in the remote default branch (`dispatch.rs`). |
| 0.2 | Preselect the cohort and record it before dispatching: 20 tasks, each labelled with specification detail (one-liner or described), class, repository, and an estimate of difficulty. Detail and difficulty confound each other, so record both. |
| 0.3 | Run each project's `verify` once at HEAD by hand. Record the command, the exact SHA, the result and the duration. Where the dispatch base differs from local HEAD, record that the two differ; a manual run at a different SHA is not that run's baseline. |
| 0.4 | Dispatch the cohort. Approve, reject or rework each one as you would in normal use. |
| 0.5 | Report review time with `pma review <id> --minutes N`, counting only time spent reading the diff and deciding. |

The sidecar ledger this step originally specified is gone: 1.1 and 1.2 were built first, so `attempts` records every attempt and the run carries a timestamp per transition. Nothing is appended by hand.

Acceptance: 20 runs in `runs`, one `attempts` row per agent invocation carrying verify result, cost, duration and outcome, and review time on each run.

Report three outcomes separately, not one: passed verify on the first attempt, accepted after rework, and merged. A pull request still open counts in no numerator and is named in the report.

Gate: measurements 1 and 2 from the review. Accepted share below roughly 40%, or review above 10 minutes per run, changes the plan: fix specification quality (phase 3) before automation (phases 4 and 5). Both thresholds are provisional decision rules over 20 tasks, not calibrated limits.

Size: 1 session plus agent cost. No code; 1.1 and 1.2 supply the recording.

## Phase 1: records, then the gate

Goal: make each attempt reconstructable, and make a clean run provable without reading the diff.

Records come first. A gate whose evidence is overwritten on the next rework cannot be audited, and phase 4's replay has nothing to replay.

### 1a. Records

| Step | Change | Files |
|-|-|-|
| 1.1 | Done. `attempts` table, append-only, one row per agent invocation: run id, attempt number, agent, prompt, feedback, started, seconds, cost, verify result, error, outcome. `runs` keeps the lifecycle summary. `model` arrives with the adapter in 2.2 (schema 11), because until then every attempt runs the same worker at its default model and the column would hold one value. | `store.rs` (schema 6), `dispatch.rs` |
| 1.2 | Done. Transition timestamps on `runs`: `dispatched_at`, `ready_at`, `decided_at`, `published_at`, and `review_seconds`, set through `Run::enter`. `started_at` moved to `attempts`. Named `published_at` rather than `shipped_at`: a pull request opened on day 1 and merged on day 5 must count against day 1 for the per-repository cap. `pma review --minutes N` reports review time, which nothing else measures. | `store.rs` (schema 6), `dispatch.rs`, `ship.rs`, `main.rs` |
| 1.3 | Done, with 1.4 and 1.5 in one migration: their fields are one snapshot, and splitting them would have added a `class` column with nothing to write into it. Written at dispatch and never updated: class, scope globs, tier, description, `agent_budget`, `timeout`. The task revision hash is derived from `text` through `todo::normal_text`, so it needs no column. Raw features, estimator version and policy revision arrive with 3.5, 3.6 and 4.1, which produce them. | `store.rs` (schema 7), `dispatch.rs`, `main.rs` |

`runs` is the calibration corpus. Never delete rows; add columns. Features stored only on scanned tasks cannot reconstruct a past decision after a rescan or a rewording, which is why the snapshot lives on the run.

### 1b. Minimal class and scope

Classes arrive here, not in phase 3. Phase 1's scope check and acceptance table both need them, and the full routing matrix does not.

| Step | Change | Files |
|-|-|-|
| 1.4 | Done. `Class::of` in `src/class.rs`: `deps` is A, `ci` is B, an unclassified item is B, and `#manual` is D, refused before the worktree is created. `#agent` marks eligibility, not class. Two deviations, both to avoid a label with no effect: `#manual` is D rather than C, because C's behaviour is the `propose` approval mode that arrives in 4.5, and until then a C task would be dispatched exactly like a B one; and A- is not predicted from the task text, because 1.7 applies the privileged-path rule to the paths a run actually changed, whatever class was predicted, which is the stronger check. | new `src/class.rs`, `dispatch.rs` |
| 1.5 | Done. `Class::scope` resolves the allowed globs at dispatch and the run stores them. Kept as code constants rather than settings: one manifest list covers every ecosystem, and 4.1 gives each route its own `scope`, which is where per-policy globs belong. The privileged-path list lands with its enforcement in 1.7. | `class.rs`, `dispatch.rs` |

### 1c. The gate

| Step | Change | Files |
|-|-|-|
| 1.6 | Done. `verify` runs in the new worktree before the agent, which is a clean checkout of the base. `verify_base_ok` and `verify_base_seconds` go on the run; the `verify_base` table caches by project, base SHA, command and timeout, so every task of a project in one batch pays for one run and a settings change misses rather than reuses. A base that could not be started is unknown, not failing. The command is frozen at dispatch and dropped from `update_run`: it was re-detected on every rework, so a rework could check the head with a command the base was never checked with. | `dispatch.rs`, `store.rs` (schema 8) |
| 1.7 | Done. `dispatch::changed_paths` stages intent-to-add, then reads `git diff --name-status -z --find-renames` against the base: NUL-delimited because a path may contain a newline, `--name-status` because a rename names two paths and only the status says so. A failure to enumerate stores null with the reason in `scope_error`, never an empty list. `Class::violations` applies the privileged paths to what the run actually changed, whatever class was predicted. The run stores the paths rather than the verdict, so a later change to the class rules re-reads the evidence instead of trusting a verdict recorded under rules nobody can name. | `dispatch.rs`, `class.rs`, `report.rs`, `store.rs` (schema 9) |
| 1.8 | Done. `accept::review_reasons` reads the recorded gates and returns why a human is needed; empty means every gate is clean. It approves nothing: `pma review` shows the reasons, and the approval modes of 4b and 4c are what consume an empty list. The verify table is by class: A needs green at the base and the head, B needs a check that fails at the base and passes at the head. Missing, unrun and unknown base results are each distinct from red. A run that edited the files implementing its own verify command is read whatever else passed; the driver files are derived from the frozen command, and test files are not among them, since adding a test is the point of a B task. | new `src/accept.rs`, `report.rs` |
| 1.9 | Done. The counter is keyed by project and normalised task text, so it survives the run that raised it, a reworded specification starts fresh, and a `ci` incident is identified by the workflows it names rather than by the word `ci`, which recurs. Red verification and rejection each consume one; a budget refusal, a spawn failure and a timeout consume none. At two, `--auto` passes the task over and a named dispatch refuses with `--retry` as the reset. The exhausted state needs no worktree mechanism: a failed run's worktree is still released by `pma review <id> --reject`, which the refusal names. Escalation resuming a failed tree is deferred to 4.3, which is where a second attempt at a different model exists. | `store.rs` (schema 10), `dispatch.rs`, `main.rs` |
| 1.10 | `pma report`: accepted share, cost, duration, verify outcomes, grouped by project, class, agent and model. Reads `runs` and `attempts`. | new `src/report_runs.rs` |

What this gate establishes: no observed regression, a change inside its declared scope, and for bug fixes a test that discriminates base from candidate. It does not establish arbitrary task correctness, and the worker can edit the tests that implement it. Treat it as bounded evidence.

Acceptance: unit tests for scope matching, including untracked forbidden files, a rename across a scope boundary, a deletion, and a modified verification script; unit tests for every base-and-head verify combination including missing and timed-out; a CLI test where an agent edits a file outside scope and the run is flagged; a CLI test proving that rejecting and redispatching does not reset the attempt counter, and that an infrastructure failure does not mark a task unsuitable; `make check` green.

Gate: none. Phase 1 stands alone and is worth having even if the rest is dropped.

Size: 4 sessions.

## Phase 2: worker adapter

Goal: agent-agnostic workers, per-run model selection, and an honest cost contract.

| Step | Change | Files |
|-|-|-|
| 2.1 | Config schema for structured values (schema 11). `config` is a flat `(key, value)` scalar table with a closed key list (`config.rs::keys`) and a closed `slot` match; `with_overrides` rejects an unknown stored key with a hard error on every command. An agent record is an open-ended name holding a list, so it needs its own table rather than a dotted key. | `store.rs` (schema 11), `config.rs` |
| 2.2 | `agents.<name>`: command, args template with `{prompt} {dir} {model} {budget}`, allowlist form, output parser, capability flags for sandbox, budget enforcement, cost reporting and session resume. `claude` becomes one seeded entry, not the code path. | `config.rs`, `agent.rs` |
| 2.3 | Normalized `Report {ok, summary, cost, error}` with parsers `claude-json` and `text-tail`. A worker that reports no cost yields `None` and the run records unknown, never zero. `codex-json` when a second worker is actually used. | `agent.rs` |
| 2.4 | Budget contract, stated rather than promised. `batch_budget` admits runs; it does not cap spend. `claude` checks its cap between turns and can overshoot, and a worker with no cap overshoots without limit. Reserve retry capacity when escalation is enabled. `agent_budget` applies per attempt; the per-task total is the sum over attempts and is recorded, not bounded. A route may require a worker with an enforceable bound; a timeout is not one. | `config.rs`, `dispatch.rs` |
| 2.5 | Settings: default agent, default model, per class overrides read later by the matrix. | `config.rs` |

Acceptance: two fake workers in `tests/cli.rs`, one JSON and one plain text, both dispatched and reviewed through the same path; a worker that reports no cost leaves `cost_usd` null through review and report; a worker that exceeds its reservation is recorded and does not corrupt the batch accounting; concurrent attempts plus an escalation at the batch limit behave as specified; the existing `claude` tests unchanged.

Gate: a dispatch on a non-`claude` worker completes end to end.

Size: 3 sessions.

## Phase 3: eligibility, specification and complexity

Goal: decide what an agent may take, improve what it is told, and produce the complexity value phase 4 matches on.

| Step | Change | Files |
|-|-|-|
| 3.1 | Eligibility replaces quadrant gating: the `ci` and `deps` signals at any tier, plus items tagged `#agent`. The `activity` and `hygiene` signals stay undispatchable; "signals at any tier" must not admit them. Retire `dispatch_quadrants` and `overflow_quadrants`. | `config.rs`, `main.rs` |
| 3.2 | Retired-key policy. A retired key left in a store today aborts every command. Retirement drops the row in the migration and keeps the name in a rejected-key list with the reason, so `pma config` explains it instead of erroring. | `config.rs`, `store.rs` |
| 3.3 | `default_tier`, so untiered projects appear at all. | `config.rs`, `main.rs` |
| 3.4 | Urgency becomes sequencing: `due:`, blocking signals, `#urgent`. Drop `stale_after` and its 5 settings. Age moves to tiebreak and to a rotting-backlog report. | `rank.rs`, `config.rs`, `report.rs` |
| 3.5 | Deterministic complexity features stored per task and snapshotted per run (schema 12): names a file or symbol, text and description length, repository size, verify duration, prior success in that repository. | `scan.rs`, `store.rs` (schema 12) |
| 3.6 | Versioned complexity rule: an explicit, deterministic function from features to 1-5, with its version recorded on every run. This is the producer phase 4 matches on, and it exists whether or not 3.7 is built. | `rank.rs` or new `src/complexity.rs` |
| 3.7 | Optional: one LLM step, classify-and-specify, schema-checked and cached per item revision, producing `{class, complexity, acceptance, expected_paths}`. It proposes; 3.6 remains the applied rule unless a route says otherwise. | new `src/judge.rs` |

Acceptance: deterministic tests for 3.6 over a fixture set, independent of 3.7; a golden set of 50 hand-labelled items with measured agreement for 3.7; ranking tests updated for the urgency change; a store carrying a retired key opens and reports it.

Gate: measurement 3. If detail predicts success, 3.7 is worth its cost; if not, stop at 3.6 and keep routing on deterministic features.

Size: 3 sessions, of which 3.7 is 1.

## Phase 4a: policy and replay, no autonomy

Goal: policy as an artifact, applied deterministically, with nothing new published.

| Step | Change | Files |
|-|-|-|
| 4.1 | `routing.toml` in the store: match on class, complexity, tier; agent, model, escalation, scope, approval. Versioned, with provenance for who proposed and approved it. Define unmatched and overlapping routes: first match wins, no match refuses. | new `src/route.rs`, `store.rs` (schema 10) |
| 4.2 | Activation is a separate action from editing the file. An edit is a draft; `pma route activate` records the approving user and the revision. In-flight runs keep the revision they were dispatched under. | `route.rs`, `main.rs` |
| 4.3 | Matcher and escalation: retry once at the next model on a failed verify, within budgets, each attempt appended to `attempts`. | `route.rs`, `dispatch.rs` |
| 4.4 | `pma route replay`: apply a candidate matrix to recorded runs and snapshots, report routing differences. Cost for an alternative model is an estimate with its assumptions printed, never a measured number. Shadow mode logs the computed route without using it. | `route.rs`, `main.rs` |

Acceptance: replay reproduces the routes of recorded runs from their snapshots, and still does so after the task was edited, its tier changed, the project rescanned, or the run reworked.

Gate: replay reproduces every recorded run's route exactly.

Size: 2 sessions.

## Phase 4b: batch approval

Goal: reduce review cost with a human still approving every publication.

| Step | Change | Files |
|-|-|-|
| 4.5 | Approval modes per route: `propose`, `each`, `batch`. `unattended` is parsed and refused until phase 4c. `pma review --approve` over many ids for `batch`, filtered to runs whose gates are clean. | `main.rs`, `dispatch.rs` |
| 4.6 | Approval evidence, recorded at approval and checked at ship: verified tree hash, base SHA, verify command, scope policy revision, approver. | `store.rs`, `dispatch.rs`, `ship.rs` |
| 4.7 | Invalidate approval when agent-owned content changes after approval, and recheck the integrated tree after the rebase in `ship_one`. Two runs that each pass against the same base can fail together. `ship.rs` writes `TODO.md` itself after the rebase; admit that one edit by path and content, without granting the agent write access to the file. | `ship.rs` |

Today `ship_one` rebases, edits `TODO.md`, amends and pushes with no verification after approval, so an edited worktree or a clean but incompatible rebase publishes unverified content.

Acceptance: CLI tests for a worktree edited after approval, two changes that rebase cleanly but fail together, and a `TODO.md` edit made by the agent rather than by ship. None publishes on stale evidence.

Gate: a batch of at least 10 runs shipped through `batch` approval with no stale-evidence refusal that turned out to be spurious.

Size: 2 sessions.

## Phase 4c: unattended publication

Goal: publish without a human in the loop, for the narrowest case that the evidence supports.

Unattended means opening a pull request and completing its merge. Restricted to `publish = "pr"`. Direct push to a default branch has no review step and no pre-merge CI; it stays out until open question 6 in `design-review.md` is answered.

| Step | Change | Files |
|-|-|-|
| 4.8 | CI gate: require positive success for the exact head revision, over a named set of required checks. Pending, missing, cancelled, skipped, unreadable and API failure all leave the pull request pending, not merged. A head change re-arms the gate. A pull request closed unmerged ends the run. Restart resumes from recorded state. `ship.rs::settle` currently only observes what a person did; this adds the merge lifecycle. | `ship.rs` |
| 4.9 | Code-level limits no matrix may raise: never unattended for class C, D or A-; merge only on positive green CI for the current head; at most N unattended changes per repository per day, counted from `published_at`; stop after K consecutive failures. | `dispatch.rs`, `ship.rs` |
| 4.10 | Digest after an unattended pass: what shipped, what was refused and why, and what it cost. Bounded by the pass window from the transition timestamps. | `report.rs` |
| 4.11 | The trigger for an unattended pass. `pma dispatch` blocks until every run finishes and holds an exclusive `flock` for its whole run, so a scheduled pass and an interactive session cannot overlap. Specify the command, its schedule, and what it does when the lock is held. | `main.rs` |

Acceptance: CLI tests proving each hard limit refuses; a pending check, a missing check, an unreadable API and a changed head each leave the pull request unmerged; a restart mid-pass neither double-merges nor loses a run; an unattended route ships only when every gate is clean.

Gate: replay clean (4a), plus shadow mode until it has observed at least 20 routed runs, not for a fixed week. A week during which no task matched proves nothing. The first enabled unattended routes are the canary: tier 4 and 5 repositories, class A only, with a stated minimum of completed runs and tolerated failures before autonomy widens. Widening is an explicit approval, recorded like a policy activation.

Size: 3 sessions.

## Phase 5: campaigns

Goal: one task definition across many repositories.

| Step | Change | Files |
|-|-|-|
| 5.1 | Campaign definition and persistent membership: task text or spec, repository selector, class, scope, approval mode. Restart semantics, so a retry does not duplicate a pull request. | new `src/campaign.rs` |
| 5.2 | Run a campaign over independent repositories: one worktree per repository, verified individually, batched review, N pull requests. | `campaign.rs`, `dispatch.rs`, `ship.rs` |
| 5.3 | Deferred until a concrete campaign needs it: manifest dependency graph, manifest identity resolution, cycle handling, readiness barriers, and how a dependent resolves the intended upstream revision without changing the user's checkout. | `scan.rs`, `campaign.rs` |

Creating pull requests in topological order does not make a library land before its dependents. A merged library may still not be consumable until a release or a pinned-revision update, and a dependent can verify against its old lockfile while never exercising the upstream change. Ordering alone is insufficient, which is why 5.3 is a barrier problem and not a sort.

Acceptance for 5.2: a CLI test running a campaign over 3 independent fake repositories, checking per-repository verification, batched review, and that a restart after a partial failure creates no duplicate pull request.

Acceptance for 5.3, when built: a fixture keeping the upstream pull request open, proving that dependent execution waits, then that the dependent verifies against the intended upstream version; plus an upstream failure and a restart.

Gate: phase 4b in use, and at least one campaign you would actually run. The first candidate from the review: add a minimal CI workflow to the 13 repositories without one, under `batch` approval, never unattended. Those repositories are independent, so 5.2 alone covers it.

Size: 2 sessions for 5.1 and 5.2. 5.3 is unsized until a campaign needs it.

## Schema and migrations

Current `user_version` is 5. `runs` already carries `agent`; only `model` is new.

| Version | Contents | Phase |
|-|-|-|
| 6 | `attempts` table; transition timestamps and `review_seconds` on `runs`; `started_at` moves to `attempts`. Applied | 1 |
| 7 | Decision snapshot on `runs`: class, scope globs, tier, description, `agent_budget`, `timeout`. Applied | 1 |
| 8 | `verify_base` cache table; `verify_base_ok` and `verify_base_seconds` on `runs`. Applied | 1 |
| 9 | `changed_paths` and `scope_error` on `runs`. Applied | 1 |
| 10 | `exhaustion` counter per project and task revision. Applied | 1 |
| 11 | `agents` table and `model` on `attempts`; retired-key drops | 2 |
| 12 | Complexity features on `tasks` | 3 |
| 13 | Routing revisions, activations and approval evidence | 4a |

Each migration is additive to existing tables, applied on open in one transaction, and raises `user_version` so an older binary refuses the file rather than misreading it.

## Configuration compatibility

Database migrations do not define configuration compatibility. `with_overrides` turns an unknown stored key into a hard error on every command, so retiring a key breaks any store that set it.

Rules for every retirement:

- The migration deletes the stored row.
- The name stays in a rejected-key list with a reason and a replacement, so `pma config <old>` explains rather than errors.
- A key whose meaning changes gets a new name. Do not reinterpret an existing one.

Retired in this plan: `dispatch_quadrants`, `overflow_quadrants`, `stale_after.1` through `stale_after.5`, and the `weights.*` group when the health score is cut.

## Amendments to design.md

Update alongside the relevant phase, marking each superseded decision rather than rewriting it silently.

| Section | Superseded by |
|-|-|
| Quadrant actions, `dispatch_quadrants`, `overflow_quadrants` | 3.1 eligibility |
| Urgency, `stale_after` | 3.4 sequencing |
| Ship, step 4 publish | 4.7 reverification and 4.8 the merge lifecycle |
| Project health, `status --explain` | 1.10 for runs; portfolio views are not replaced, see Cuts |

Two decisions stand and need no amendment. `design.md` already states that `batch_budget` bounds how many runs start rather than what they spend; step 2.4 restates it against the old step 4.5, which promised a spend cap. "No per-item ids" also stands: step 1.3's task revision identity is a hash of the item's text, derivable at scan time, so it needs no written-back id. Ids in the file remain under "Not yet".

## Cuts

Do these as each area is touched, not as a separate project.

| Cut | When |
|-|-|
| Eisenhower as dispatch policy; keep the 2x2 as a view | with 3.1 |
| `stale_after` | with 3.4 |
| Health score and `status --explain` | separately from 1.10. Run outcomes describe attempted work; the portfolio view also describes repositories where nothing was ever dispatched. Decide the replacement for those repositories before removing the view |
| Deps counter, in favour of Renovate or Dependabot | when the deps class proves it adds nothing over bot pull requests |
| Issues sync | if no one else files issues on these repositories |
| Notes, TUI | freeze now, remove if unused after phase 4b |

## Not yet

| Deferred | Trigger |
|-|-|
| MCP server, so an outside agent drives `pma` | phase 4b done and the CLI shape stable |
| Second agent reviewing a matrix revision | replay proves insufficient on its own |
| Per-item ids for dependency edges (`id:a7`, `needs:`) | real tasks block each other often enough to notice |
| Unattended direct push | open question 6 answered, and a stated blast-radius rule |
| Cross-repository dependency barriers | a campaign whose repositories actually depend on each other |
| Security advisories, bot pull requests, release lag as signals | after phase 1, and only if `ci` and `deps` earn their place |
| Agents beyond `claude` and one other | phase 2 proves the adapter on a second worker |

## Risks to the plan

Phase 0 may show that agents cannot close these tasks, or that review is too slow. Then phases 4 and 5 are premature and the work moves to specification quality and the queue.

Phases 1 and 2 are worth building in either case: the first records what happened and proves a run is clean, the second removes a vendor from the core.

Phase 1 grew from 2 sessions to 4 because the records moved into it. That cost is what makes replay, the per-day cap, the digest and the report possible at all.

Every phase adds code to a tool that is itself one of the repositories being maintained. The cut list is not optional.
