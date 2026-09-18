# Implementation plan review

2026-09-18. Reviewed `design.md`, `design-review.md`, and especially `implementation-plan.md`, with the current dispatch, store, and ship code checked for feasibility. This is a document and implementation inspection; no agents were dispatched and no portfolio measurements were repeated.

## Assessment

The direction is sound: measure completed work, separate eligibility from priority, enforce gates in code, and introduce routing only after collecting outcomes. Keeping human approval while the evidence accumulates is the right starting point.

The plan is not yet specific enough to authorize unattended shipping. Its largest gaps concern what constitutes verified output, how that evidence survives publication, and what historical data replay actually needs. Phase 1 also depends on classes and routes introduced later, despite claiming to stand alone. Resolve the high-priority findings below before implementing autonomy; the manually reviewed pilot can proceed with the measurement adjustments described here.

Priority meanings: **P1** blocks the claimed unattended guarantees or a core phase acceptance criterion; **P2** leaves a significant implementation or measurement ambiguity.

## Findings

### 1. P1 — Verification must bind to the final published tree

References: implementation plan steps 1.1–1.2 and 4.4–4.5; design sections “Review” and “Ship”; `src/dispatch.rs::approve`, `src/ship.rs::ship_one`.

The plan adds base verification and scope checks but does not specify when their evidence expires. Today approval changes a state, while ship stages the worktree again, rebases direct pushes, marks the task complete, and publishes without rerunning verification. A passing run can therefore publish different content after an edit or a clean but semantically incompatible rebase. Two individually passing runs against the same base can also fail when combined.

Record the verified tree, base SHA, verification command, scope policy revision, and approval evidence. Invalidate approval when agent-owned content changes. Recheck the final integrated tree after rebase and define how the tool's own `TODO.md` edit is admitted without granting agents unrestricted access to that file. For PRs, require evidence tied to the current PR head and an explicit policy for integration with a moving base.

Acceptance should cover a worktree edited after approval, two incompatible changes that rebase cleanly, and a changed PR head. None may publish unattended using stale evidence.

### 2. P1 — The unattended publishing and CI contract is incomplete

References: implementation plan steps 4.4–4.6 and phase 4 gate; design-review sections “Approval modes” and “Open questions”; `src/ship.rs::ship_one` and `settle`.

“Never merge on red CI” is weaker than requiring green CI: missing, pending, cancelled, or unreadable checks are not red. The existing shipping path opens PRs and later observes whether someone merged them; it does not implement CI-gated merging. The plan does not explicitly schedule that lifecycle, and `publish = push` bypasses PR CI entirely.

Define whether unattended means opening a PR or completing its merge. If it includes merge, add explicit handling for pending checks, required-check selection, head changes, API failures, closure without merge, and restart recovery. Require positive success for the exact revision; absent or unknown evidence must leave the PR pending. Initially restrict unattended publication to PRs, or separately specify the direct-push gate before enabling it.

Also define canary promotion by evidence: minimum completed runs, tolerated failures, and an explicit approval to widen autonomy. A week of shadow mode with no matching tasks proves nothing. Clarify that the first enabled unattended routes are the canary; currently the gate places canary before any unattended route is enabled.

### 3. P1 — The hard budget promise contradicts the worker contract

References: implementation plan steps 2.1–2.2, 4.2, and 4.5; design section “Agents”; design-review section “The worker adapter”.

Step 4.5 promises never to exceed `batch_budget`. The design explicitly records that a worker's native cap can overshoot, and the adapter permits workers that report no cost. Reserving a nominal run budget cannot enforce an absolute spend ceiling in either case. Automatic retries make the distinction more important.


Choose an enforceable contract. Either describe `batch_budget` as an admission budget with disclosed overshoot and unknown spend, or restrict strict-budget routes to workers with a genuinely enforceable bound. Preserve unknown costs as unknown, reserve retry capacity, and specify whether `agent_budget` applies per attempt or across the whole task. A hard spend guarantee must not be inferred from timeout support.

Acceptance should include a worker exceeding its reservation, a worker returning no cost, and concurrent attempts plus escalation at the batch limit.

### 4. P1 — Add immutable attempt and decision records before collecting calibration data

References: implementation plan phase 0 acceptance, steps 1.3–1.4, 2.3, 3.5, 4.3, and “Schema and compatibility”; `src/store.rs::update_run`; `src/dispatch.rs::attempt`.


The current run row is mutable. Rework replaces the summary, verify result, feedback, and start time while accumulating cost and duration. Keeping run rows forever does not preserve individual attempts. The proposed migrations mention values “per attempt” without defining an attempt entity. Class and complexity features stored only on scanned tasks also cannot reconstruct historical decisions after a rescan or rewording.

Introduce append-only attempts associated with a run, and snapshot decision inputs at dispatch: task revision and description, class, tier, raw features, derived complexity, estimator version, route revision, worker/model, budgets, and gate evidence. Record review decisions and minutes separately from execution duration. Keep the run row as a lifecycle summary. The existing schema already contains `agent`; migration 7 should extend it rather than add it again.

For the no-code pilot, use a small sidecar ledger keyed by run and attempt to retain these observations until the schema exists. Without that, phase 0 can lose first-attempt failures during normal rework, and phase 1 cannot report by class from `runs` alone.

Replay can reproduce routing decisions when inputs and policy versions are preserved. It cannot establish what a different model would actually cost or whether it would succeed. Report alternative costs as estimates with assumptions, and validate outcomes through canaries. Test replay after task edits, tier changes, rescans, and rework.

### 5. P1 — Base verification and path scope do not yet define an acceptance gate

References: implementation plan steps 1.1–1.2 and 3.6; design-review sections “Classes of maintenance work” and “Mechanisms this needs”; `src/dispatch.rs::diff`.

Passing the existing suite at base and head demonstrates no observed regression; it does not demonstrate that a bug was fixed. A newly added regression test is absent when step 1.1 runs. Conversely, a failing base may be unrelated to the requested fix. “Make a clean run provable without reading the diff” overstates what these checks establish, particularly when the worker can edit the tests or scripts that implement verification.

Specify an acceptance table by class and verify status, including missing verification, timeout, and infrastructure failure. For bug fixes, require the focused regression test to fail against the base implementation and pass against the candidate, or retain human review. Freeze the selected verification command and define when changes to its implementation require review. Key baseline caches by the command and relevant execution context as well as repository and SHA; do not reuse them indefinitely after configuration changes.

Scope enforcement must examine the complete candidate change. Plain `git diff --name-only` misses untracked files unless they are included first; the existing display helper already stages intent-to-add for that reason. Specify NUL-delimited paths, both sides of renames, deletions, and failure to enumerate changes. Apply privileged-path exclusions to actual changed paths regardless of the predicted class or route scope. Otherwise a misclassified workflow edit can bypass the A- restriction.

Add tests for untracked forbidden files, renames across scope boundaries, modified verification scripts, missing checks, and a new regression test. Treat the resulting gate as bounded evidence, not proof of arbitrary task correctness.

### 6. P2 — Phase dependencies and deterministic routing inputs need resolving

References: implementation plan steps 1.2, 1.4, 2.4, 3.1–3.6, and 4.1.

Phase 1 expects a task class and the route's globs, but classes arrive in phase 3 and routes in phase 4. Phase 3 extracts features but defines no deterministic conversion to the complexity value that phase 4 matches. If the optional classifier is skipped, routing has no specified complexity producer. Neither `#agent` nor `#manual` identifies a maintenance class by itself.

Move a minimal class and scope contract into phase 1, with conservative defaults, without requiring the full routing matrix. Define tag precedence, unknown classes, an initial versioned complexity rule, and behavior for unmatched or overlapping routes. Keep `ci` and `deps` as the eligible signals: “signals at any tier” must not accidentally admit the activity and hygiene tasks the design explicitly excludes. Class D must remain undispatchable, not merely ineligible for unattended shipping.

Define policy activation as a separate action from editing `routing.toml`, including approval of autonomy increases and the policy revision governing in-flight runs. Add deterministic acceptance tests for phase 3 even when step 3.6 is omitted; its present acceptance criterion primarily tests the optional classifier.

### 7. P2 — Retry exhaustion needs task-revision identity and failure categories

References: implementation plan steps 1.3 and 4.2; design sections “TODO.md format” and “Dispatch”; `src/main.rs::has_run`, `src/dispatch.rs::rework`.

An attempt counter on a run does not by itself prevent a rejected task from starting a fresh run. A permanent counter keyed only by task text can instead block a materially revised specification. Signal identities such as `ci` recur across independent incidents. The plan also leaves “failed attempt” ambiguous: budget refusal, spawn failure, timeout, red verification, and human rejection have different implications for suitability.

Define exhaustion per task revision or signal incident, retain counters across runs for that revision, and specify which failures consume an attempt. Count manual rework and automatic escalation consistently, while allowing an explicit reset. Define whether escalation resumes the failed tree or starts from a clean base, and how the exhausted state permits cleanup of retained worktrees.

Acceptance should prove that rejecting and redispatching does not reset the limit, an infrastructure failure does not label the task unsuitable, and a new CI incident or revised task can become eligible.

### 8. P2 — Phase 0 does not collect all the evidence used by later gates

References: implementation plan steps 0.1–0.5, phase 0 acceptance, and phase 3 gate; design-review “Three measurements before more features”.

Phase 3 depends on comparing detailed tasks with one-liners, but phase 0 specifies only 20 arbitrary tasks. The review reports just two items with descriptions in its measured sample, so a suitable comparison set cannot be assumed. Task class and difficulty can also confound any observed relationship between detail and success. Twenty tasks are useful pilot evidence, not a reliable universal threshold.

Preselect and record the cohort, including specification detail, class, repository, and task difficulty where practical. Distinguish first-attempt success, eventual acceptance after rework, and merged completion. Define which review minutes count and how pending PRs affect the denominator. Treat the 40% and ten-minute thresholds as provisional decision rules.

For self-management, include bounded tasks with acceptance criteria rather than whole multi-session phases. Commit and push the pilot items before dispatch, because the existing dispatcher requires them to exist in the remote default branch. Measure the exact fetched base where possible; a manual verify at a different local HEAD is not that run's baseline.

### 9. P2 — Campaign ordering does not establish dependency readiness

References: implementation plan steps 5.1–5.3 and phase 5 acceptance; design-review “Cross-project sequencing comes free”.

Creating PRs in topological order does not make a library land before dependents. Even a merged library may not be consumable until a release, registry publication, or pinned revision update. A dependent can verify successfully against its old lockfile while never exercising the campaign's upstream change. Path dependencies can also resolve outside the isolated worktree into a sibling checkout.

Start with independent repositories if that is sufficient for the CI-workflow campaign. For dependent campaigns, define manifest identity resolution, cycle handling, merge/readiness barriers, and how each dependent resolves the intended upstream revision without changing the user's checkout. Add persistent campaign membership and restart semantics so failures block only downstream work and retries do not duplicate PRs.

The acceptance fixture should keep the upstream PR open and prove that dependent execution waits, then verify against the intended upstream version. Include an upstream failure and a restart. Ordering alone is insufficient.

## Suggested revision order

1. Amend phase 0 with a reproducible cohort and per-attempt sidecar measurements; keep it manually reviewed.

2. Move minimal class/scope definitions and immutable attempt recording into phase 1. Specify acceptance outcomes and verification invalidation before implementing the checks.

3. Build the adapter with explicit cost and capability semantics. Preserve the current worker as the compatibility baseline.

4. Define deterministic eligibility and complexity, then collect enough versioned routing inputs to make replay meaningful.

5. Split phase 4 into policy/replay, manually approved batch shipping, and unattended PR merging. Give each its own acceptance gate, including restart and stale-evidence cases.

6. Begin campaigns with independent repositories; add dependency barriers only for a concrete campaign that needs them.

Update `design.md` alongside the relevant phases, marking superseded decisions explicitly. It currently describes quadrant eligibility, no task IDs, and manual publication behavior while the implementation plan changes those contracts. Document how retired configuration keys migrate or fail validation; additive database migrations alone do not define configuration compatibility.

Keep the cut list, but evaluate removal separately from successful-run reporting: run outcomes describe attempted work, whereas portfolio views also describe repositories where nothing has been dispatched. The report is not automatically a functional replacement for those views.
