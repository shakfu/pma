# pma implementation plan

Derived from `design-review.md` (2026-09-17) and revised against `plan-review.md` (2026-09-18). Sequenced by dependency and by evidence: records before gates, gates before autonomy, data before policy, cuts throughout.

Each phase states what it changes, how it is accepted, and the gate that must hold before the next phase starts. Sizes are rough estimates, in sessions of work, not commitments.

Revision note, 2026-09-19. Phase 2b, containment and the channel, is added from `using-containers.md`, with the `design.md` decisions it supersedes listed under "Amendments to design.md". It is numbered out of order because phases 3, 4a and 4b were built before it.

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
| 1.1 | Done. `attempts` table, append-only, one row per agent invocation: run id, attempt number, agent, prompt, feedback, started, seconds, cost, verify result, error, outcome. `runs` keeps the lifecycle summary. `model` arrived with the adapter in 2.2 (schema 11), because until then every attempt runs the same worker at its default model and the column would hold one value. | `store.rs` (schema 6), `dispatch.rs` |
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
| 1.10 | Done. `pma report [--by project\|class\|agent]` over `runs` and `attempts`: runs, first-attempt passes, accepted, merged, open, attempts, cost, agent time and review time. Passing on the first attempt, being accepted after a rework, and being merged are three columns, not one share. A queued or running run is excluded rather than counted as a failure; an open pull request is in no share and is named; a worker that reported no cost shows as `$0.40+1?` rather than being summed as free. `--by model` is refused; 2.2 records the model, and the dimension follows when routing chooses one. | new `src/report_runs.rs` |

What this gate establishes: no observed regression, a change inside its declared scope, and for bug fixes a test that discriminates base from candidate. It does not establish arbitrary task correctness, and the worker can edit the tests that implement it. Treat it as bounded evidence.

Phase 1 is complete. Acceptance: unit tests for scope matching, including untracked forbidden files, a rename across a scope boundary, a deletion, and a modified verification script; unit tests for every base-and-head verify combination including missing and timed-out; a CLI test where an agent edits a file outside scope and the run is flagged; a CLI test proving that rejecting and redispatching does not reset the attempt counter, and that an infrastructure failure does not mark a task unsuitable; `make check` green.

Gate: none. Phase 1 stands alone and is worth having even if the rest is dropped.

Size: 4 sessions.

## Phase 2: worker adapter

Goal: agent-agnostic workers, per-run model selection, and an honest cost contract.

| Step | Change | Files |
|-|-|-|
| 2.1 | Done. An `agents` table, not a dotted config key: `config` is a flat `(key, value)` scalar table with a closed key list and a closed `slot` match, and `with_overrides` rejects an unknown stored key with a hard error on every command, so an open-ended name holding a list does not fit it. | `store.rs` (schema 11) |
| 2.2 | Done. `Worker` in `src/worker.rs`: command, args with `{prompt} {dir} {model} {budget}`, an allowlist flag and rule template, parser, and flags for cost reporting, budget enforcement, sandbox and session resume. An argument whose placeholder has no value is dropped together with the flag before it, so `--model {model}` disappears whole. `claude` is seeded by the migration with exactly the behaviour it had compiled in; `agent::claude` and the `AGENT` constant are gone. `pma agent` lists, `pma agent set` and `pma agent rm` edit. | new `src/worker.rs`, `agent.rs`, `main.rs` |
| 2.3 | Done. `Parser::report(output, exit_ok)` with `claude-json` and `text-tail`. `text-tail` takes its verdict from the exit status, its summary from the last 20 lines, and leaves the cost `None`. `codex-json` when a second real worker is actually used. | `worker.rs` |
| 2.4 | Done. Budget contract, stated rather than promised. `batch_budget` admits runs; it does not cap spend. `claude` checks its cap between turns and can overshoot, and a worker with no cap overshoots without limit. Reserve retry capacity when escalation is enabled. `agent_budget` applies per attempt; the per-task total is the sum over attempts and is recorded, not bounded. A route may require a worker with an enforceable bound; a timeout is not one, so `enforces_budget` is false for `claude` too. A run that exceeded its budget is a reason to read it, in `accept::review_reasons`. | `accept.rs`, `worker.rs` |
| 2.5 | Done as `agent` and `model` settings. Per-class overrides are not built: 4.1 gives each route its own agent and model, and a second place to say the same thing would have to be reconciled with it. | `config.rs` |

Phase 2 is complete. Acceptance met: a second worker with no structured output and no allowlist is dispatched, reviewed, shipped and reported through the same path, with no code that knows its name; its cost stays null through review and report; a removed worker fails its run by name rather than running nothing; the existing `claude` tests are unchanged.

Not covered by a test: concurrent attempts escalating at the batch limit, which needs the escalation of 4.3.

Gate met: a dispatch on a non-`claude` worker completes end to end, through ship.

## Phase 2b: containment and the channel

Goal: run the worker and its verify inside a container, and give the run a conversation the manager and the developer can both read while it happens.

Out of order by number, in order by dependency. Phases 3, 4a and 4b were built before this one. It gates 4c: publishing without a person, from an agent running on the host with the user's credentials, is the combination `design.md` already warns about under Agents -- the credential drop "makes an accidental push fail. It does not stop a determined process."

Three projects move and only the `pma` column is this plan's to sequence. `sanduk` holds containment, `minos` holds the channel, and [using-containers.md](using-containers.md) is the note behind the split.

| Step | Change | Files |
|-|-|-|
| 2b.1 | `sanduk --json`: one object per run with `ok`, `exit`, `cost_usd`, token counts, `report_path`, `mode`, `model`, `container`. Then `Parser::SandukJson` beside `claude-json`. Without it `sanduk` reformats `total_cost_usd` into a prose stats line, `claude-json` finds no result line and falls back to `text-tail`, and every contained run records cost as unknown. | `sanduk`; `worker.rs` |
| 2b.2 | Preflight `sanduk --version` against a minimum, as `gh` is checked, then register the worker with `pma agent set` and prove it on one project. `worker.allow` goes unused: `sanduk` passes `--dangerously-skip-permissions`, and a container that denies egress does not also need a `Bash()` allowlist. Do not ship both and believe in both. | `main.rs`, `dispatch.rs` |
| 2b.3 | `verify` runs in the container, its status reported separately from the agent's, and `base_verify` moves with it. One combined status cannot separate a failed edit from a failed test, and a base run in a different environment is not that run's baseline. This is what answers `design.md` Agents: "Isolation would need a container around both." The cost is toolchain images -- the stock ones carry no Rust, Go or C, and `sealed` blocks the fetch -- which is the bulk of this phase, not the integration. | `sanduk` images; `dispatch.rs` |
| 2b.4 | The mailbox: request and decision schemas under the workspace, read and answered by `pma`. Keeps a single fire-and-forget run serverless, and is the degraded path when `minosd` is absent. Dispatch must not depend on a server this tool did not start. | new `src/mailbox.rs`, `dispatch.rs` |
| 2b.5 | The supervisor loop. `pma dispatch` blocks until every run finishes and holds an exclusive `flock` for its whole run (4.11), so a mid-run question has nowhere to be answered. Either the loop reads the channel inside that call, or dispatch stops blocking. The first keeps the lock contract and the pid ownership record true. | `dispatch.rs`, `agent.rs` |
| 2b.6 | In `minos`: a grant `pma` mints per run, carrying its rooms, no admin group, expiring at `min(run timeout, task bound)`; `/vfs` and `/settings` refused to a grant; separable listeners, the agent-facing one bound to the sealed network and to loopback. Without the first two, an agent in the container holds a 7-day credential and a 100 MiB file channel. | `minos` |
| 2b.7 | `sanduk --network <name>` so a run joins the network `minosd` is already on, and a `pma` client for the wire: `open`, `send`, `history`, `submission.*`, `grant.*`. Escalation posts to a channel, with `/approve` and `/reject <why>` mapped to `review --approve` and `--rework`. Policy stays in `route.rs`; minos transports and records. | `sanduk`; new `src/minos.rs`, `route.rs` |
| 2b.8 | Retention for task rooms at least equal to the relay's log retention, and a task id and a run id on every message. Two records over different periods cover neither whole, and without the ids, reconstructing what an agent did means matching timestamps across three clocks. | `minos`; `store.rs` |

This phase builds the channel, not the manager. 3.7 stays gated on the pilot; what 2b adds is a place where a manager and a worker can talk at all, and a record of it a person can read while it happens.

Acceptance: a contained dispatch completes end to end through ship, with cost recorded and verify run at base and head inside the container, reported separately from the agent's status; a run whose `minosd` is down completes through the mailbox; a mid-run request is answered from policy with no person involved, and an escalated one blocks its own run and no other; a revoked grant closes the socket, and the container is stopped with it rather than left running against the worktree.

Gate, in two parts. 2b.1 to 2b.3: ten contained runs through ship, cost recorded, no run failed on a missing toolchain. Then 2b.4 to 2b.8, and 4c stays closed until unattended routes run contained.

Size: 4 sessions for the `pma` column. The image work is unsized until the toolchains are named, and the `sanduk` and `minos` columns are not this plan's to size.

## Phase 3: eligibility, specification and complexity

Goal: decide what an agent may take, improve what it is told, and produce the complexity value phase 4 matches on.

| Step | Change | Files |
|-|-|-|
| 3.1 | Done. `rank::Task::eligible`: the `ci` and `deps` signals at any tier, plus items tagged `#agent`. `activity` and `hygiene` stay undispatchable, so "signals at any tier" does not admit them. `--auto` walks tasks in matrix order and takes the eligible ones; `dispatch_quadrants` and `overflow_quadrants` are retired. | `rank.rs`, `config.rs`, `main.rs` |
| 3.2 | Done. `config::RETIRED` names each retired key and what replaced it. Migration 12 deletes the stored rows; `with_overrides` skips any that survive, because a row written by another binary must not make every command fail; `pma config <old>` explains instead of saying the key is unknown. | `config.rs`, `store.rs` (schema 12) |
| 3.3 | Done. `default_tier`, unset by default so nothing changes until it is set. With it set, the untiered count goes to zero and those projects rank. | `config.rs`, `main.rs` |
| 3.4 | Done. `Urgency::Stale` and the five `stale_after` settings are gone; urgency is a deadline, a blocking signal or `#urgent`. Age remains the queue's tiebreak and fills `pma stale`, a list to prune rather than a reason to work. The measured consequence in the test portfolio: the old rule put a 100-day `High` item in Q1; it now sits in Q2 with the rest. | `rank.rs`, `config.rs`, `report.rs`, `main.rs` |
| 3.5 | Done, on the run rather than on the task. `Features` are measured at dispatch: names a path or symbol, text words, description lines, tracked files, the base verify duration, and the project's decided and accepted runs. Not stored on `tasks`: routing matches at dispatch, so a scan-time copy would be a second source to keep in step, and the run's snapshot is what 4a replays. | `complexity.rs`, `dispatch.rs`, `store.rs` (schema 13) |
| 3.6 | Done. `complexity::estimate(class, features)` returns 1 to 5 as a sum of named adjustments, so a recorded estimate can be argued with. `ESTIMATOR` is `v1` and is stored on each run with the features it read; replaying a decision means re-running the rule named there. | new `src/complexity.rs` |
| 3.7 | Not built, and correctly so: its gate is measurement 3, which needs the phase 0 pilot. 3.6 is the applied rule and phase 4 has its producer either way. | new `src/judge.rs` |

Steps 3.1 to 3.6 are complete; 3.7 waits on its gate. Acceptance met: deterministic tests for 3.6 over a fixture set, independent of 3.7; ranking tests updated for the urgency change; a store carrying a retired key opens and reports it; a CLI test showing two tasks in one repository estimated 2 and 4 from their own text.

3.7 is the manager layer's first LLM step, per `design.md` goal 5. It classifies; instructing a worker mid-run needs the channel of phase 2b.

Gate for 3.7: measurement 3, from the phase 0 pilot. If detail predicts success, the classifier is worth its cost; if not, 3.6 stands and routing keeps to deterministic features. The golden set of 50 hand-labelled items belongs with 3.7, not before it.

## Phase 4a: policy and replay, no autonomy

Goal: policy as an artifact, applied deterministically, with nothing new published.

| Step | Change | Files |
|-|-|-|
| 4.1 | Done, as JSON rather than TOML: this crate parses JSON already, and adding a TOML parser plus `serde` derive for one document is a larger change than the syntax is worth. Match on class, complexity and tier; set agent, model, escalation, scope and approval. First match wins; no match refuses the dispatch by name. A revision is stored normalised, so two revisions diff by what they mean rather than by how they were typed. `unattended` on class A-, C or D, or with no class list at all, is refused where the document is read. | new `src/route.rs`, `store.rs` (schema 14) |
| 4.2 | Done. `pma route propose` stores a draft and routes nothing; `pma route activate <rev> [--shadow]` puts one into effect and records who did it. Each run records the revision and route it was dispatched under, so a later activation does not rewrite a past decision. | `store.rs`, `main.rs` |
| 4.3 | Done. One retry at the route's `escalate.model`, only where the check itself refused the work, with the previous attempt still in the worktree. A route that may escalate reserves both attempts against `batch_budget`, so the second is not refused after the first spent the room. Each attempt is appended with the model it ran and consumes one against the task, so two refused checks exhaust it exactly as a rework would. | `dispatch.rs`, `route.rs` |
| 4.4 | Done. `pma route replay <file\|revision>` reads each run's own snapshot, so a task edited, retiered, rescanned or reworked since does not change the answer. No cost is projected at all: what a different model would spend, or whether it would succeed, is not in this data, and the output says so rather than printing a number with assumptions attached. Shadow records the computed route and applies nothing, escalation included. | `route.rs`, `main.rs` |

Phase 4a is complete. Acceptance met: replaying the active revision over the runs it routed reports no difference for them, and reports the runs dispatched before it and under shadow as differences, which is what those two states mean. Replay reads snapshots, so editing, retiering, rescanning or reworking cannot change a past answer.

Gate met. Nothing here approves, ships or merges: a route names an approval mode and the run records it; 4b and 4c are what act on one.

## Phase 4b: batch approval

Goal: reduce review cost with a human still approving every publication.

| Step | Change | Files |
|-|-|-|
| 4.5 | Done. `propose` refuses approval by name: it produces a patch and a summary, and nothing is published. `each` is the existing path. `batch` is `pma review --approve <ids...>`, which checks every named run before approving any, so a list with one run to read changes nothing. `unattended` cannot reach a run that would ship: no automatic path exists until 4c. | `main.rs`, `dispatch.rs` |
| 4.6 | Done. Approval records the tree it was given for, as `git write-tree` names it, plus the head it was taken at and who gave it. The base, the verify command and the scope globs were already immutable on the run, so the tree and the approver were the whole gap. A rework clears them: the approver read another tree. | `store.rs` (schema 15), `dispatch.rs` |
| 4.7 | Done. Ship refuses a worktree whose tree no longer matches, and says to read it and approve again; approving an approved run re-takes the evidence, which is that recovery. After the rebase and the tick, `verify` runs on the integrated tree: two changes that each pass against the same base can fail together, and a clean rebase is not a semantic one. Ship's own `TODO.md` edit is admitted because it happens after the tree check, and because `TODO.md` is now outside every class's scope, so an agent that edits it is caught at review. | `ship.rs`, `class.rs` |

Phase 4b is complete in code. Acceptance met: a worktree edited after approval is refused and the branch is untouched; an upstream commit that the approved change breaks is caught on the integrated tree, not after publication; an agent that edits `TODO.md` is outside every class's scope and is read at review; a batch approval with one run to read in it approves nothing.

Gate outstanding, and it is not a code gate: at least 10 runs shipped through `batch` approval with no stale-evidence refusal that turned out to be spurious. That needs the phase 0 pilot.

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

Gate: phase 2b contained, replay clean (4a), plus shadow mode until it has observed at least 20 routed runs, not for a fixed week. A week during which no task matched proves nothing. The first enabled unattended routes are the canary: tier 4 and 5 repositories, class A only, with a stated minimum of completed runs and tolerated failures before autonomy widens. Widening is an explicit approval, recorded like a policy activation.

Size: 3 sessions.

## Phase 5: campaigns

Goal: one task definition across many repositories.

| Step | Change | Files |
|-|-|-|
| 5.1 | Done. A campaign is a task, an optional description, a class and a fixed set of projects. Membership is stored rather than recomputed, so a rescan cannot move work under a campaign in flight, and a restart can tell which members already have a run. `pma campaign add\|show\|run\|rm`. | new tables in `store.rs` (schema 16), `main.rs` |
| 5.2 | Done. `pma campaign run` dispatches the members with no live run, one worktree each, verified individually, reviewed as a batch with `pma review --approve <ids>`. A campaign task has no item in any `TODO.md`, so there is nothing to check on origin and nothing to tick at ship; it states its class instead of reading tags. A member whose run is not final is named rather than dispatched over: a second worktree would be left behind and a second pull request could be opened for the same work. | `dispatch.rs`, `ship.rs`, `main.rs` |
| 5.3 | Deferred, unchanged. Manifest dependency graph, identity resolution, cycle handling, readiness barriers, and how a dependent resolves the intended upstream revision without changing the user's checkout. | `scan.rs`, `campaign.rs` |

Creating pull requests in topological order does not make a library land before its dependents. A merged library may still not be consumable until a release or a pinned-revision update, and a dependent can verify against its old lockfile while never exercising the upstream change. Ordering alone is insufficient, which is why 5.3 is a barrier problem and not a sort.

Acceptance for 5.2 met: a CLI test runs one definition over 3 independent repositories, each with its own worktree and its own base and head verification; two are approved in one batch; the member whose agent gave up is named on a restart and dispatched only after it is rejected, so no second run is opened for work that already has one.

Acceptance for 5.3, when built: a fixture keeping the upstream pull request open, proving that dependent execution waits, then that the dependent verifies against the intended upstream version; plus an upstream failure and a restart.

Gate outstanding: phase 4b in use, and a campaign you would actually run. The first candidate from the review is the one the acceptance test models: a minimal CI workflow for the repositories without one, class A-, under `batch` approval, never unattended. Those repositories are independent, so 5.2 alone covers it. `unattended` for A- is refused where a policy document is read, so that limit needs nothing further.

5.3 is unsized until a campaign needs it.

## Phase 6: workflows

Goal: workflows as typed, parameterised functions over bags of units, composed into a graph, each node routed to its own agent and model. Specified in [workflows.md](workflows.md).

A signature is `in: [T] -> [U]` plus parameters plus an effect set, all checked. A node applies a function: one of five primitives -- `map` with a declared output bound, `reduce`, `edit`, `check`, `emit` -- or another workflow through `call`. Each primitive earns its place by changing what `pma` must check. Parallelism is derived from the graph, conditions live on edges as rules, and an agent produces data while a rule decides routing.

Three properties keep a composable document from becoming an unbounded engine: calls are flattened at propose time so the runtime holds one flat graph, every bound is a constant or a parameter with a declared maximum so the worst case is computable, and guards are rules rather than models. Iteration is a node retry, a bounded lap edge, or a bounded self-edge for decomposition, all drawing on one attempt counter per lineage, so none is a hole through 1.9's limit. Waiting needs no construct: a `check` node is not ready, and the pass ends.

| Step | Change | Files |
|-|-|-|
| 6.1 | The document: `types`, `params` with defaults and maxima, nodes, edges with guards, `@input`, `@output`, per-instance caps, and a declared effect set. Every refusal named where it is read, including a cycle that is not a bounded lap, a `0..n` node with no unit cap, a parameter used as a bound with no maximum, an edge whose types disagree, and declared effects that differ from the graph's. | new `src/workflow.rs`, `store.rs` (schema 20) |
| 6.2 | Units, bags, moves and verdicts: immutable units carrying lineage, depth and lap; a bag derived from the units a node wrote; one move row per unit and edge holding the guard that refused it. The frontier is re-derived from these, so a killed pass resumes without a cursor. | `store.rs`, `workflow.rs` |
| 6.3 | `map` at `0..n`, `1` and `0..1`, by agent or by rule, with the checks that make each bound worth declaring: the type, the cap, kept ids being a subset of the input with a reason per drop, and no field changed outside `writes`. A validator that could rewrite its input could launder work past the reviewer. | `workflow.rs`, `accept.rs` |
| 6.4 | `check` and `emit`: the rule-only ends of a graph. Checks write `passed`, `failed` or `unknown`, and guards read them; `unknown` is distinct because a check that could not run is not evidence. Sinks are `todo` with `add`, `tick` and `remove`, `note`, and `doc`; `issue` is refused until reconciled with `pma sync`. No node writes `TODO.md` itself. | new `todo::insert`, `workflow.rs` |
| 6.5 | `node` and `lap` as match dimensions on a route, with `*/leaf` matching one node wherever it was called from. A route stating no node matches only a dispatch that names none, so an existing policy keeps its behaviour and replay over earlier runs reports no difference. `lap` makes "a second attempt at a stronger model" a policy statement rather than a document one. | `route.rs`, `dispatch.rs` |
| 6.6 | The bound: worst-case runs and cost per node, walked over the flattened graph at every parameter's declared maximum. `pma workflow propose` prints it; `pma workflow activate` refuses a document above `workflow_budget`. A rules-only document costs zero. | `workflow.rs`, `config.rs` |
| 6.7 | `pma workflow propose\|activate\|run\|show\|stop`, arguments through `--set k=v` recorded on the instance, and `pma report --by node` and `--by workflow`. One pass per invocation, holding `session.lock` like dispatch. | `main.rs`, `report_runs.rs` |
| 6.8 | `call`, resolved by flattening at propose time: a callee's nodes inlined under the call site's name, so one workflow may be called twice, the cost composes, and the runtime never nests. Workflow-level recursion is refused, since flattening could not terminate. | `workflow.rs` |
| 6.9 | `edit` as a node: one unit in, a patch out, through `dispatch::prepare` and every phase 1 gate. `retry` makes the existing attempt series declarative; `escalate` is the case `retry.max: 1` with a model change. | `dispatch.rs`, `workflow.rs` |
| 6.10 | `reduce`, by rule (dedupe, rank, limit) or by agent (synthesise, choose), with provenance on every output unit; and lap edges with their terminal path. A `reduce` is the only barrier the design needs. | `workflow.rs` |
| 6.11 | Recursion: a self-edge bounded by `max_depth`, terminating on the producing node's own empty answer. | `workflow.rs` |

| 6.12 | Done. Documents as Rhai scripts, an alternative to JSON rather than a replacement: a script applies one combinator per primitive to a graph value that carries its own output port, so wiring is application rather than a repeated node name, and a reusable piece of a graph is a function. Its value becomes JSON and the same reader validates it. It builds a document and never runs during a pass, so the bound still holds and a proposed revision is still data. The engine is compiled without a clock or modules, with `eval` disabled and every limit set. | new `src/script.rs`, `Cargo.toml` |

Built so far: 6.2's units, moves and verdicts, with the frontier re-derived from them and a pass that runs every node a rule decides and stops, priced, at the first that an agent would; 6.1 whole, including migration 20; 6.5's `node` and `lap` on a route, with `*/leaf` matching one node wherever it was called from; 6.6's bound, stored per unit of input and weighed by `activate` against `workflow_budget`; 6.12's script form; and the `check`, `propose`, `activate` and `show` half of 6.7. The pass, the units and moves of 6.2, and every node's execution are not built.

A library of nine small workflows and three compositions is in [workflows.md](workflows.md#15-a-library-and-three-compositions): review then confirm then dedupe, an ensemble of three reviewers joined by a reducer, fix with an audit lap, issue triage with no model on the read, specification, recursive decomposition, implementation, an effectful identity that records items, settling pull requests at zero cost, and three callers that compose them. Two of the compositions differ only in which reviewer they call, because the signatures agree.

Acceptance, in full, in [workflows.md](workflows.md#16-acceptance). Refused shapes with what each would cost: [workflows.md](workflows.md#18-limits). The largest are a workflow as a value, which flattening forbids, and the ship-time conflict between two `edit` nodes, which is 5.3's barrier problem.

Gate: 6.1 to 6.8 change no repository, so they need none. 6.9 needs phase 0 measured and phase 4b's gate met, because it multiplies runs per human decision. 6.10 needs 6a in use. 6.11 needs evidence that one level of decomposition helps at all. No node is `unattended` before phase 4c's gate.

Size: 4 sessions for 6.1 to 6.7, 1 for 6.8, 2 for 6.9, 2 for 6.10, 1 for 6.11.

## Schema and migrations

Current `user_version` is 5. `runs` already carries `agent`; only `model` is new.

| Version | Contents | Phase |
|-|-|-|
| 6 | `attempts` table; transition timestamps and `review_seconds` on `runs`; `started_at` moves to `attempts`. Applied | 1 |
| 7 | Decision snapshot on `runs`: class, scope globs, tier, description, `agent_budget`, `timeout`. Applied | 1 |
| 8 | `verify_base` cache table; `verify_base_ok` and `verify_base_seconds` on `runs`. Applied | 1 |
| 9 | `changed_paths` and `scope_error` on `runs`. Applied | 1 |
| 10 | `exhaustion` counter per project and task revision. Applied | 1 |
| 11 | `agents` table; `model` on `runs` and `attempts`. Applied. Retired-key drops move to 3.2, which is what retires a key | 2 |
| 12 | Retired-key drops. Applied | 3 |
| 13 | `complexity`, `features` and `estimator` on `runs`. Applied | 3 |
| 14 | `routes` revisions with provenance; `route_revision`, `route` and `approval` on `runs`. Applied | 4a |
| 15 | `approved_tree`, `approved_head` and `approved_by` on `runs`. Applied | 4b |
| 16 | `campaigns` and `campaign_members`. Applied | 5 |
| 17-19 | `owner/name` on a project, absence, project tags. Applied, outside this plan | - |
| 20 | `workflows` revisions, `workflow_instances`, `workflow_units`, `workflow_moves`, `workflow_verdicts`; `workflow_instance`, `node`, `unit` and `lap` on `runs` | 6 |

Each migration is additive to existing tables, applied on open in one transaction, and raises `user_version` so an older binary refuses the file rather than misreading it.

## Configuration compatibility

Database migrations do not define configuration compatibility. `with_overrides` turns an unknown stored key into a hard error on every command, so retiring a key breaks any store that set it.

Rules for every retirement:

- The migration deletes the stored row.

- The name stays in a rejected-key list with a reason and a replacement, so `pma config <old>` explains rather than errors.

- A key whose meaning changes gets a new name. Do not reinterpret an existing one.

Retired so far: `dispatch_quadrants`, `overflow_quadrants`, `stale_after.1` through `stale_after.5`, and `model`, which became a preset by way of the worker record. The `weights.*` group follows when the health score is cut.

## Amendments to design.md

Update alongside the relevant phase, marking each superseded decision rather than rewriting it silently.

| Section | Superseded by |
|-|-|
| Quadrant actions, `dispatch_quadrants`, `overflow_quadrants` | 3.1 eligibility |
| Urgency, `stale_after` | 3.4 sequencing |
| Ship, step 4 publish | 4.7 reverification and 4.8 the merge lifecycle |
| Agents, host-run verify and the credential drop | 2b.3 containment |
| Dispatch, step 2 and the blocking loop | 2b.5 the supervisor loop |
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

Phase 2b puts two projects outside this plan on its critical path. A `sanduk` or `minos` change that does not land leaves the contained path unfinished, and 2b.4's mailbox is what keeps dispatch working meanwhile.

Every phase adds code to a tool that is itself one of the repositories being maintained. The cut list is not optional.
