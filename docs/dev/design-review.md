# pma design review

2026-09-17, against `design.md` at commit `2d3a90f` plus the fixes in the working tree. The tool is five days old, so this reviews the design, not its maturity.

Purpose under review: maintain many repositories from one place, by ranking work and dispatching agents to it. `pma` is a Project Management Agent: an orchestrator that delegates to sub-agents, not only a command-line tool that runs one.

Sequenced work from this review: `implementation-plan.md`.

## Verdict

Build the core: one ranked queue across repos, and a safe path from task to merged change (worktree, no push credentials, `pma` runs the tests, review, ship). Nothing off the shelf does both for 94 personal repos.

Cut the scoring machinery around it. The Eisenhower grid, the health score, the deps counter, Issues sync, notes and the TUI are separate subsystems with 40 settings between them. `pma` is itself a repository to maintain; each subsystem is a cost against the tool's own purpose.

The decisive question is not in the code: what share of these tasks can an agent close, at what review cost. Measure it before adding features. Below roughly 40% accepted, `pma` is a portfolio viewer with a dispatcher attached, and the queue is the product.

## Measured, 2026-09-17

From a read-only copy of `~/.config/pma/projects.db`, root `~/projects`. This machine holds 13 repositories, not the 94 the design describes.

| Measure | Value |
|-|-|
| Projects scanned | 13, of which 2 have a tier |
| Largest backlogs | `gopherwiki` 51 tasks, `playr` 40; neither has a tier, so neither is in the matrix |
| Open tasks by priority | 2 critical, 16 high, 70 medium, 66 low |
| Tasks with `due:`, `#urgent` or `gh:N` | 0 |
| Task age | 57 between 164 and 224 days, 97 under 30 days |
| Task text | median 22 words; 2 of 160 have a description |
| CI status | unknown for all 13; dependencies never measured |
| Agent runs | 0 |

Simulated matrix over these 154 tasks, with every project at one tier:

| Tier | Q1 | Q2 | Q3 | Q4 | Reachable by `dispatch --auto` |
|-|-|-|-|-|-|
| 1 | 2 | 16 | 55 | 81 | 18 (12%) |
| 3 | 0 | 2 | 57 | 95 | 2 (1.3%) |

Two consequences of the defaults, not of the data: `Medium` and `Low` are never important at any tier (`design.md:69`), and a deps task is `Medium` and never urgent, so it lands in Q4 and `--auto` never dispatches it.

## Urgency is sequencing, not decay

Urgency answers: what must happen before other work can proceed. The current rule (`design.md:83`) makes a task urgent once it is older than `stale_after[tier]`. With no due dates in the portfolio, urgent reduces to "older than 30 days", and within a month most tier-1 tasks qualify. Age then carries no ordering information, since the queue already sorts by age.

Replace it with three sources:

1. **External deadline.** `due:` within `urgent_within`. Unchanged.

2. **Blocking other work.** A task that other tasks wait on. Failing CI is the clearest case: nothing in that repo merges until it passes, which is why it is urgent as a signal already.

3. **Explicit marking.** `#urgent`, for what the other two rules miss.

The inverse state matters as much. A blocked task cannot start, so it is neither urgent nor dispatchable, and it should leave the queue until its blocker closes. Today such a task sits in the queue and can be dispatched.

Age becomes two things: a tiebreak within equal score, and a separate report of what is rotting. A backlog item untouched for 224 days is a candidate for deletion, not for immediate work.

### What dependency links cost

Dependencies need stable item ids. The format says no per-item ids, and identifies unsynced items by text (`design.md`, TODO.md format). Text identity cannot carry an edge: rewording a task breaks it.

Three options, in order of cost:

1. **Signals only.** No format change. Failing CI is urgent because it blocks; deps and hygiene are not. Cross-project edges come from manifests (below). Covers the common case at zero cost to the file format.

2. **Short ids written back.** `pma` assigns `id:a7` the way sync writes `gh:N`, then `needs:a7` and `#blocked` become expressible. Reverses one decision, and makes the format carry a graph.

3. **Issue links only.** `needs:gh:42`, available only for synced items, which today means `Critical` alone.

Recommendation: option 1 now, option 2 only if real tasks turn out to block each other often enough to notice.

### Cross-project sequencing comes free

A portfolio has a dependency graph already, in the manifests: path and git dependencies in `Cargo.toml`, `go.mod` requires of `github.com/<user>/*`, `pyproject.toml` on a sibling. Where project A is used by B, work on A first, and a breaking change in A makes work in B wait.

This is derivable in a scan, needs no format change, and orders work across repositories rather than inside one. It fits the tool's purpose better than any per-task rule, and no per-repo tool can do it.

## Deploying agents on discrete tasks

Importance is the wrong gate for delegation. Three properties decide whether a task suits an agent:

1. **Machine-checkable acceptance.** Something other than the agent's own report says the change works.

2. **Bounded blast radius.** The files a correct change touches can be stated in advance.

3. **Cheap rollback.** A revert or a force-push undoes it.

All three hold, and review is a skim. None hold, and the agent generates work for the reviewer.

### Classes of maintenance work

| Class | Examples | Acceptance | Scope | Deployment |
|-|-|-|-|-|
| A. Mechanical | dependency bumps in range, formatter and linter fixes, dead code the compiler flags, broken doc links | existing suite and build | lock files, manifests, or whole-file formatting | auto-dispatch, merge on green CI, review by sample |
| A-. Mechanical, privileged | CI action bumps, workflow edits | existing suite and build | `.github/**` | never unattended; see Risks |
| B. Specified | fix failing CI, fix a bug with a reproduction, add a missing test, small feature with stated acceptance | a test failing at base and passing at head | the files the task names, plus tests | pull request, diff skimmed |
| C. Judgment | API shape, architecture refactor, performance trade-offs, public interfaces | none automatic | unbounded | agent drafts a patch or proposal; the user decides |
| D. Never | releases and publishing, credentials, license changes, removing tasks from `TODO.md` | n/a | n/a | not dispatched |

`pma` treats every task as class B and gives them all one prompt. Its signal tasks already know their class: `ci` is B, `deps` is A.

### Mechanisms this needs

**Verify at base as well as at head.** One verify run proves only that the tree is green now. Running it at the base commit first separates three cases: already broken, broken by the agent, fixed by the agent. For a bug fix it is also the acceptance test: the new test must fail at base and pass at head. Highest value per line of code here, at the cost of one extra verify run per dispatch.

**Path scope as policy.** Each class declares the paths a correct change touches, and `pma` checks the diff against them. A deps task that edits `src/` is wrong whether or not the tests pass. `pma` enforces it; the prompt only requests it.

**A prompt per class.** Deps: update within the constraints, do not touch code. CI: reproduce the failing log locally first. Bug: write the failing test before the fix.

**Review by exception.** Auto-approve when every check is clean: verify as expected at base and head, diff inside scope, no commits made, cost under threshold, no security-sensitive path touched. Everything else reaches the reviewer. This attacks review time, which is the throughput limit, rather than agent parallelism.

### Cross-repo campaigns

One task definition applied to 20 repositories is worth more than one task in one repository: migrate workflows to a reusable one, bump a pinned action, adopt a lint rule, drop Python 3.9. The portfolio shows the pattern already: 34 repos last committed on the same day, all touching `.github/workflows/**` (`design.md:11`).

A campaign uses what `pma` has: the repo list, each repo's state, a worktree per run, verification, batched ship. It yields N pull requests from one definition, and the reviewer holds one context across them, so marginal review cost per repo is low. Run a campaign in manifest dependency order, so a library lands before its dependents.

`multi-gitter` and `all-repos` cover the scripted version. An agent earns its place where the edit differs per repo, which is where scripted fleet tools fail. Nothing in the design covers this.

### Where the agent runs

Local `claude -p` in a worktree, as now. One machine covers 94 repos overnight, and no credentials leave it. Running agents in CI would add parallelism that is not the bottleneck, and would put tokens in 94 repositories.

## Manager and workers

Two agent layers, with different requirements.

The manager is `pma` itself: what to work on, what to delegate, to whom, and whether a result is acceptable. Today this layer is entirely deterministic, in scoring formulas and quadrant lists.

The worker is one sub-agent on one task in one worktree. Today it is `claude`, hardcoded: the `AGENT` constant, the `claude()` command builder, `parse_claude`, and the `--allowedTools` syntax.

Both must become agent-agnostic, and the rule that keeps that safe is the one the design already applies to results: **an LLM step proposes, code acts**. `pma` runs `verify` itself and does not trust an agent's report. The same holds for the manager's own judgments.

### Where judgment earns its cost

| Decision | Made by | Why |
|-|-|-|
| Task inventory, git and CI facts | code | deterministic, cacheable, testable |
| Score and ordering | code | an inspectable formula beats an unrepeatable judgment |
| Class and agent-suitability of an item | LLM | needs to read the item and the repository |
| Complexity estimate | LLM, with deterministic features | drives routing; see below |
| Turning a one-line item into a spec with acceptance criteria | LLM | the largest gap in the input: 2 of 160 items carry a description |
| Which worker and model to use | code, from the estimate and recorded outcomes | routing is a table, not a judgment |
| Whether a diff is acceptable | code gates, then an LLM summary | verify, path scope and commit count are objective; the summary saves reading time |
| Commit, push, merge | code only | side effects stay deterministic |

### Routing by complexity, the differentiator

Requirement: `pma` estimates each task's complexity and dispatches it to an agent and model matched to it. A single-agent tool cannot do this. A fleet tool without agents cannot do it either. This is the reason for the manager layer to exist.

Estimation inputs, cheapest first:

1. Deterministic features: task class, whether the item names a file or symbol, length of its text and description, repository size, whether a `verify` command exists and how long it takes, and the recorded success rate for that class in that repository.

2. An LLM estimate over the item and repository context, returning `{complexity: 1-5, expected_paths, rationale}` under a schema.

3. The outcome of previous attempts at the same task.

Routing policy, config-driven rather than compiled in:

| Complexity | Typical work | Worker and model | Acceptance |
|-|-|-|-|
| 1-2 | class A chores: dependency bumps, formatting, action bumps, doc links | cheapest capable model, e.g. Haiku 4.5 | automatic when gates are clean |
| 3 | class B with a clear acceptance test | mid model, e.g. Sonnet 5 | pull request, diff skimmed |
| 4 | class B touching several files, or an unclear failure | strongest model, e.g. Opus 5 | pull request, diff read |
| 5 | class C judgment work | strongest model, drafting only | proposal to the user, never auto |

`claude` takes `--model` and `--fallback-model`, so per-run selection needs no new mechanism on that worker; other workers pass their own flag through the adapter.

Escalation, not one shot: a failed attempt at complexity 2 may retry once at the next model up, within `agent_budget` and `batch_budget`. Record each attempt with its model. Two failures mark the task not suitable for an agent, which is also the fix for `--auto` re-picking a task forever.

The objective is not the cheapest tokens. Review time is the scarce resource, so routing optimizes accepted work per review minute. A cheap model is worth using only where a failure costs money and nothing else, which means class A with automatic acceptance behind verify-at-base and path scope. Where a human reads the diff, a failed cheap attempt costs more than the price difference between models.

Calibration is measurable and belongs in the tool: store class, complexity estimate, agent, model, attempt number, verify result and final state, then report accepted share and cost per model and class. Routing thresholds move on that evidence. Without it, model choice is superstition.

### The routing matrix is an artifact, not a per-task call

An agent proposes the matrix; the tool applies it. One judgment per revision, not one per task. Over 1400 items the difference is a single call against 1400, and the applied policy is inspectable, diffable and repeatable.

The matrix is a file in the store, versioned, with provenance: which agent and model proposed it, from which outcome data, when, and who approved it.

```toml
[[route]]
match    = { class = "A", complexity = "1-2" }
agent    = "claude"
model    = "haiku"
escalate = { on = "verify_failed", model = "sonnet", attempts = 1 }
scope    = ["Cargo.lock", "Cargo.toml", "uv.lock", ".github/**"]
approval = "unattended"

[[route]]
match    = { class = "B", complexity = "3-4" }
agent    = "claude"
model    = "sonnet"
escalate = { on = "verify_failed", model = "opus", attempts = 1 }
approval = "batch"
```

Everything the matching needs, the tool computes without an LLM: class from the signal type or the item's tag, complexity features from the item text, repository size, `verify` presence and duration, and the recorded success rate for that class in that repository. A free-text item with no tag is the one case needing a classifier, and that call is cheap and cacheable per item revision.

Adopting a revision has three cheap checks, in order:

1. **Replay.** Apply the candidate matrix to the recorded runs and report what it would have routed differently, and at what cost. The `runs` table is the corpus.

2. **Shadow.** Compute the route, log it, dispatch by the current matrix. Compare over a week.

3. **Canary.** Apply it to tier 4 and 5 repositories first, then wider.

A second agent may review a proposed revision for risk, and report `{approve, deny, notes}` against the autonomy rules below. It reviews the policy, not each task. The user approves any revision that raises autonomy.

### Approval modes

Autonomy is a property of a route, not of the tool. Four modes, from least to most autonomous:

| Mode | Behaviour | Fits |
|-|-|-|
| `propose` | the run produces a patch and a summary; nothing is published | class C, new repositories, a new worker or model |
| `each` | the user approves each run before ship | class B on tier 1 and 2 |
| `batch` | the user approves a list, filtered to runs whose gates are clean | class B at scale |
| `unattended` | the tool ships clean runs and reports afterwards; the user reads a digest and samples | class A behind full gates |

Unattended is safe because of the gates, not because of the model. A routing mistake wastes money. A gate mistake breaks a default branch. So `unattended` requires all of: verify failing or passing as expected at base and at head, the diff inside the route's `scope`, no commits by the agent, cost under the run budget, no security-sensitive path touched, and for `publish = pr`, green CI before merge.

Limits the tool enforces regardless of the matrix, in code rather than config: never unattended for class C or D, never merge a pull request whose CI is not green, never exceed `batch_budget`, at most N unattended changes per repository per day, and stop the batch after K consecutive failures or rejections. A matrix revision cannot raise these; only a change to `pma` can.

Every unattended change is reversible and recorded: the pull request or commit id in `outcome`, the route and model that produced it, and a digest listing what shipped, what was refused, and what it cost.

### The worker adapter

Replace the `claude` constant with a capability record per agent, in config:

```toml
[agents.claude]
command = "claude"
args    = ["-p", "{prompt}", "--output-format", "json",
           "--permission-mode", "acceptEdits",
           "--model", "{model}", "--max-budget-usd", "{budget}"]
allow   = "--allowedTools Bash({cmd})"   # one rule per subcommand
parse   = "claude-json"                  # result, is_error, total_cost_usd
reports = ["cost"]

[agents.codex]
command = "codex"
args    = ["exec", "-C", "{dir}", "-s", "workspace-write", "--json", "{prompt}"]
parse   = "codex-json"
sandbox = true
reports = []
```

The record must carry what the design's agent table already shows differing: how prompt and directory are passed, how a model is named, whether a budget cap exists, how success and cost are parsed, whether a sandbox or a command allowlist exists, and whether sessions resume, which decides whether rework re-prompts or continues.

Consequences:

- One normalized `Report {ok, summary, cost, error}`, as `parse_claude` already produces. A worker that reports no cost gets `None`, and `batch_budget` then bounds runs started rather than spend.

- The trust boundary must not depend on the worker. No allowlist support means the worktree, the stripped credentials and the path-scope check carry it. `pma` verifies either way.

- Routing can then mix vendors, not only models, and the recorded outcomes say which combination earns which class.

### Manager architecture

Keep the control plane deterministic and call an LLM for the judgment steps, each with a schema-checked result, logged and cached by content hash. The alternative, an LLM loop that decides when to scan, dispatch and ship, puts a nondeterministic process in charge of `git push` across 94 repositories, and is the part that cannot be unit-tested.

Then make the head swappable: expose `pma` over MCP or a stable CLI so any capable agent session can act as the interactive manager while `pma` keeps the gates. Agent-agnostic at the manager layer means not writing a manager per vendor.

Start with two LLM steps, both testable against a hand-labeled set of your own items: classify-and-specify at scan time, and review summary with risk flags.

## Keep

- `TODO.md` as the source: offline, diffable, next to the code, no API limits.

- The line-based parser. Editing one line without re-rendering a file `pma` does not own is what makes ship and sync safe.

- Worktrees from the remote default branch, with push credentials removed.

- `pma` running `verify` itself. Not trusting the agent's report is the most valuable check in the pipeline.

- `git` and `gh` as binaries, for the user's own authentication and configuration.

- The run lifecycle now that a run ends at merge rather than at ship.

## Cut or park

| Subsystem | Why |
|-|-|
| Eisenhower as dispatch policy | Keep the 2x2 as a view. One score plus an eligibility rule replaces 4 quadrants and their 2 quadrant lists. "Q4 Remove" currently labels 53-62% of the backlog, including recent `High` items at tier 3. |
| Health score, `status --explain` | 5 weights and a saturation curve yield a number with no action attached. Tiering already states which projects matter. |
| Deps counter | Renovate and Dependabot report this and open the pull request. The count is not comparable across ecosystems, and costs 44s per scan for 52 repos. |
| Issues sync | The largest subsystem per unit of value, and it creates a second store plus the `gh:N` identity rule. It pays off only when other people file issues. |
| Activity signal | The design's own measurement says last-commit date is not a maintenance signal (`design.md:11`). |
| Five tiers | Tiering 94 repositories by hand is setup work. A focus list of 5 to 10 plus `default_tier` gives the same ordering. |
| Notes, TUI | Useful, but outside the loop. Freeze them. |

## Three measurements before more features

1. **Agent success.** Dispatch 20 tasks across 3 repos. Record the share passing `verify` and the share approved. The `runs` table holds both already.

2. **Review cost.** Time those 20 reviews. At 10 minutes each, parallelism is irrelevant and the review gate is the design problem to solve.

3. **Does task text predict success?** Compare the 10 most detailed items against 10 one-liners. If detail decides, then eligibility means "specified well enough", and the format needs an acceptance line.

## Changes worth making now

1. Verify at base as well as at head, and store both results. Independent of every queue change, and it is what makes class A auto-approval safe.

2. Path scope per class, checked against the diff at review.

3. Separate eligibility from importance. Importance orders the queue. Eligibility decides what an agent may take: CI and deps tasks at any tier, plus items tagged `#agent`.

4. Replace `stale_after` with the sequencing model above. Five settings go.

5. Add `default_tier`, so untiered projects appear at all.

6. Decide what `pma review` is for when `publish = pr`. Today the diff is reviewed twice, once in `pma` and once on GitHub.

7. Record the defaults as guesses, each with the measurement that would settle it. The design presents them as decided.

8. Add a state for a task an agent failed twice, so `--auto` stops choosing it, and record which model each attempt used.

9. Move the worker from a constant to a config record, with `claude` as one entry. Add `model` and `route` to `runs` before routing exists, so the data to calibrate a matrix accumulates from the first dispatch.

10. Extract the routing features deterministically, before any classifier exists: class from the signal type or tag, text and description length, whether the item names a file or symbol, repository size, `verify` presence and duration, prior success in that repository.

11. Add approval mode per route, and the code-level limits that no matrix may raise.

## Risks

### The verification gate is missing where CI is

Measured on the 26 repositories here: 25 have a detectable `verify` command and 22 have test files, so the gate class A depends on mostly exists. 13 have no workflow at all.

Consequences: the `ci` signal is unmeasurable for half the portfolio, absent rather than passing; and "green CI before merge" cannot gate an unattended route in those repositories.

A detected `make test` target is not proof of a gate. It may be trivial, slow, or need the network. Run every project's `verify` once at HEAD and record result and duration. That also seeds the base-verify data.

First campaign candidate: add a minimal workflow to the 13 repositories without one. Mechanical, repetitive, slightly different per repository, and it builds the gate the rest of this design assumes.

### Workflow files are a privilege escalation path

A workflow runs with repository tokens, so a change under `.github/workflows/**` can exfiltrate secrets on its next run, and CI validates the changed workflow rather than checking it. The green-CI gate is worth least exactly where the change is most dangerous.

Rule: workflow edits are never unattended, whatever their complexity or class. Class A- above exists for this reason.

### Prompt injection reaches 94 repositories

`pma` already puts fetched CI logs into prompts, and the same would hold for issue text. On public repositories that content is influenced by other people. Under an unattended route with auto-merge, a successful injection lands on default branches across the portfolio.

Present defences: the worktree, stripped push credentials, the path scope check, and `pma` running `verify` itself rather than trusting the agent.

Missing defences: fetched text marked as untrusted data in the prompt rather than instructions, and the workflow rule above.

### The tool competes with the work it saves

`pma` is 8k lines of Rust plus tests, and it is one of the 94 repositories. Every subsystem kept is maintenance that the portfolio must pay for. This is the argument behind the cut list; it is also the argument against building campaigns, routing and a classifier before the loop has closed a single task.

### It does not manage itself

`pma`'s own `TODO.md` has four empty sections. The cheapest validation available is to run the loop on `pma` and on 2 or 3 repositories you know well, using the change list in this review as the tasks. A design that cannot route and close its own chores will not hold for 94 repositories.

## Open questions

1. Is the unit of work the task or the project? Tasks feed agents. Projects decide attention. `pma` models both and keeps two rankings.

2. If measurement 1 fails, is the cross-repo queue still worth the scan pipeline? Probably yes, but it is a much smaller tool.

3. Does a maintenance portfolio need signals this tool does not have: security advisories, open bot pull requests, unreleased commits since the last tag? Unverified; each needs a tool check.

4. Is auto-merge acceptable for class A on low-tier repositories? If not, review stays the bottleneck and the classes matter less.

5. Are campaigns the product? If they would run weekly, the per-task queue is secondary and the design should say so.

6. What is the blast radius rule for `publish = push`? A direct push to the default branch has no review step; class A may still suit it, but the rule needs stating.

7. Which gates are mandatory for `unattended`, and which are per route? The list above is a proposal; each one costs something to compute.

8. Is `pma` callable by an outside agent, over MCP, or does the manager live inside `pma`? This decides whether the CLI or a tool schema is the primary interface.

9. When is a matrix revision proposed: on a schedule, or when replay over recent runs shows a better route? The second needs a measure of better, which means agreeing the objective is accepted work per review minute.

10. Does a second agent's review of a revision add anything over replay on recorded runs? Replay uses evidence; the reviewer uses judgment about cases not yet seen.
