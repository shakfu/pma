# pma implementation plan

Derived from `design-review.md`, 2026-09-17. Sequenced by dependency and by evidence: gates before autonomy, data before policy, cuts throughout.

Each phase states what it changes, how it is accepted, and the gate that must hold before the next phase starts. Sizes are rough estimates, in sessions of work, not commitments.

## Phase 0: close the loop once

Goal: produce the evidence the rest of the plan depends on. No new features.

| Step | Detail |
|-|-|
| 0.1 | Put this plan's own items in `pma/TODO.md`, in priority sections. `pma` gets managed by `pma`. |
| 0.2 | Tier 3 repositories you know well, plus `pma`. Leave the rest untiered for now. |
| 0.3 | Run `pma scan` with `gh` authenticated, so CI state is real rather than unknown. |
| 0.4 | Run each project's `verify` once at HEAD by hand. Record pass, fail and duration. |
| 0.5 | Dispatch 20 tasks across those repositories. Approve, reject or rework each one as you would in normal use. |

Acceptance: 20 runs in the `runs` table with verify result, cost, duration and final state; a note of review minutes per run.

Gate: measurements 1 and 2 from the review. Accepted share below roughly 40%, or review above 10 minutes per run, changes the plan: fix specification quality (phase 3) before automation (phases 4 and 5).

Size: 1 session plus agent cost. No code.

## Phase 1: strengthen the gate

Goal: make a clean run provable without reading the diff. Everything autonomous rests on this, and none of it depends on the queue changes.

| Step | Change | Files |
|-|-|-|
| 1.1 | Run `verify` at the base commit before the agent starts; store `verify_base`, `verify_base_ok`. Cache per project and base sha, so a batch pays once. | `dispatch.rs`, `store.rs` (schema 6) |
| 1.2 | Path scope per task class: store the route's allowed globs, compute `git diff --name-only` against the base, flag files outside. Reuse `scan::glob_match`. | `dispatch.rs`, `report.rs` |
| 1.3 | Attempt counter and a `not-suitable` state after two failed attempts, so `--auto` stops re-picking a task. | `store.rs`, `dispatch.rs`, `main.rs` |
| 1.4 | `pma report`: accepted share, cost, duration, verify outcomes, grouped by project, class and agent. Reads `runs` only. | new `src/report_runs.rs` or extend `report.rs` |

Acceptance: unit tests for scope matching and for the base and head verify combinations; a CLI test where an agent edits a file outside scope and the run is flagged; `make check` green.

Gate: none. Phase 1 stands alone and is worth having even if the rest is dropped.

Size: 2 sessions.

## Phase 2: worker adapter

Goal: agent-agnostic workers, and per-run model selection, before any routing needs them.

| Step | Change | Files |
|-|-|-|
| 2.1 | Config records `agents.<name>`: command, args template with `{prompt} {dir} {model} {budget}`, allowlist form, output parser, capability flags. `claude` becomes one entry, not the code path. | `config.rs`, `agent.rs` |
| 2.2 | Normalized `Report {ok, summary, cost, error}` with parsers `claude-json` and `text-tail`. `codex-json` when a second worker is actually used. | `agent.rs` |
| 2.3 | `agent` and `model` columns recorded per run, and per attempt. | `store.rs` (schema 7), `dispatch.rs` |
| 2.4 | Settings: default agent, default model, per class overrides later read by the matrix. | `config.rs` |

Acceptance: two fake workers in `tests/cli.rs`, one JSON and one plain text, both dispatched and reviewed through the same path; the existing `claude` tests unchanged.

Gate: a dispatch on a non-`claude` worker completes end to end.

Size: 2 sessions.

## Phase 3: classes, eligibility and specification

Goal: decide what an agent may take, and improve what it is told.

| Step | Change | Files |
|-|-|-|
| 3.1 | Class per task: from the signal type for `ci` and `deps`, from `#agent`, `#manual` and class tags for items. Class A- for `.github/**` work. | `todo.rs`, `rank.rs` |
| 3.2 | Eligibility replaces quadrant gating: signals at any tier, plus tagged items. Retire `dispatch_quadrants` and `overflow_quadrants`. | `config.rs`, `main.rs` |
| 3.3 | `default_tier`, so untiered projects appear at all. | `config.rs`, `main.rs` |
| 3.4 | Urgency becomes sequencing: `due:`, blocking signals, `#urgent`. Drop `stale_after` and its 5 settings. Age moves to tiebreak and to a rotting-backlog report. | `rank.rs`, `config.rs`, `report.rs` |
| 3.5 | Deterministic complexity features stored per task: names a file or symbol, text and description length, repo size, verify duration, prior success in that repo. | `scan.rs`, `store.rs` (schema 8) |
| 3.6 | Optional: one LLM step, classify-and-specify, schema-checked and cached per item revision, producing `{class, complexity, acceptance, expected_paths}`. | new `src/judge.rs` |

Acceptance: a golden set of 50 of your items, labeled by hand, with measured agreement for 3.6; ranking tests updated for the urgency change.

Gate: measurement 3. If detail predicts success, 3.6 is worth its cost; if not, stop at 3.5 and keep routing on deterministic features.

Size: 3 sessions, of which 3.6 is 1.

## Phase 4: routing matrix and approval modes

Goal: policy as an artifact, applied deterministically.

| Step | Change | Files |
|-|-|-|
| 4.1 | `routing.toml` in the store: match on class, complexity, tier; agent, model, escalation, scope, approval. Versioned, with provenance for who proposed and approved it. | new `src/route.rs`, `store.rs` |
| 4.2 | Matcher and escalation: retry once at the next model on a failed verify, within budgets, recording each attempt. | `route.rs`, `dispatch.rs` |
| 4.3 | `pma route replay`: apply a candidate matrix to recorded runs, report differences and cost. Shadow mode logs the computed route without using it. | `route.rs`, `main.rs` |
| 4.4 | Approval modes per route: `propose`, `each`, `batch`, `unattended`. `pma review --approve` over many ids for `batch`. | `main.rs`, `dispatch.rs` |
| 4.5 | Code-level limits no matrix may raise: never unattended for class C, D or A-, never merge on red CI, never exceed `batch_budget`, cap per repository per day, stop after K consecutive failures. | `dispatch.rs`, `ship.rs` |
| 4.6 | Digest after an unattended pass: what shipped, what was refused, what it cost. | `report.rs` |

Acceptance: replay reproduces the routes of recorded runs; a CLI test proves each hard limit refuses; an unattended route ships only when every gate is clean.

Gate: shadow mode for a week, then canary on tier 4 and 5 repositories, before any unattended route is enabled.

Size: 3 sessions.

## Phase 5: campaigns

Goal: one task definition across many repositories, in dependency order.

| Step | Change | Files |
|-|-|-|
| 5.1 | Manifest dependency graph between projects: path and git dependencies in `Cargo.toml`, `go.mod` requires of your own repos, `pyproject.toml` siblings. | `scan.rs` |
| 5.2 | Campaign definition: task text or spec, repository selector, class, scope, approval mode. | new `src/campaign.rs` |
| 5.3 | Run a campaign: one worktree per repository, verified individually, ordered so a library lands before its dependents; batched review; N pull requests. | `campaign.rs`, `dispatch.rs`, `ship.rs` |

Acceptance: a CLI test running a campaign over 3 fake repositories, one depending on another, checking order and per-repo verification.

Gate: phase 4 in use, and at least one campaign you would actually run. The first candidate from the review: add a minimal CI workflow to the 13 repositories without one, under `batch` approval, never unattended.

Size: 3 sessions.

## Continuous: cuts

Do these as each area is touched, not as a separate project.

| Cut | When |
|-|-|
| Eisenhower as dispatch policy; keep the 2x2 as a view | with phase 3.2 |
| `stale_after` | with phase 3.4 |
| Health score and `status --explain` | after phase 1.4 replaces it with run outcomes |
| Deps counter, in favour of Renovate or Dependabot | when the deps class proves it adds nothing over bot PRs |
| Issues sync | if no one else files issues on these repositories |
| Notes, TUI | freeze now, remove if unused after phase 4 |

## Not yet

| Deferred | Trigger |
|-|-|
| MCP server, so an outside agent drives `pma` | phase 4 done and the CLI shape stable |
| Second agent reviewing a matrix revision | replay proves insufficient on its own |
| Per-item ids for dependency edges (`id:a7`, `needs:`) | real tasks block each other often enough to notice |
| Security advisories, bot pull requests, release lag as signals | after phase 1, and only if the `ci` and `deps` signals earn their place |
| Agents beyond `claude` and one other | phase 2 proves the adapter on a second worker |

## Schema and compatibility

Four migrations are planned: 6 base verify, 7 agent and model per attempt, 8 complexity features, 9 routing and approval. Each is additive, applied on open, and raises `user_version` so an older binary refuses the file rather than misreading it.

`runs` is the calibration corpus. Never delete rows; add columns.

## Risks to the plan

Phase 0 may show that agents cannot close these tasks, or that review is too slow. Then phases 4 and 5 are premature and the work moves to specification quality and the queue.

Phases 1 and 2 are worth building in either case: the first proves a run is clean, the second removes a vendor from the core.

Every phase adds code to a tool that is itself one of the repositories being maintained. The cut list is not optional.
