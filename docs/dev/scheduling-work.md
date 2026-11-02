# Scheduled work

Status: proposed, 2026-10-27. Step 1, `instance_budget`, is built; the rest is not. Decided and open points are at the end.

## Problem

Runs start by hand. Colleagues asked for runs on a schedule, such as every night, within a set budget. Each run finds issues and adds them to `TODO.md`, with duplicate findings merged into one item.

`pma web` and `pma tui` are not daemons, and the job runner exits once its queue is empty (`crates/pma-core/src/jobs.rs`). A schedule inside `pma` would need a resident process.

## What exists

The library already holds the pipeline:

- `portfolio-sweep` (`crates/pma-core/workflows/library.json:534`) calls `ensemble-review` with `breadth: 8`, then `record`.

- `ensemble-review` (`:211`) runs three reviewers (bugs, missing tests, misusable interfaces), merges their findings, then confirms each against the code.

- `record` (`:422`) emits each finding to the `todo` sink. The sink (`crates/pma-core/src/pass.rs:1699`) writes the item with an `^id` and adds no tag.

- `pma workflow run ... --unattended` approves each plan as it comes.

| Need | Exists | Gap |
|-|-|-|
| Find issues | `ensemble-review` | none |
| Write to `TODO.md` | `record` and the `todo` sink | none |
| Run unattended | `--unattended` | none |
| Budget | `workflow_budget` per project, at activation; `batch_budget` per plan | Nothing caps a whole pass. See Budget below. |
| Merge duplicates in one night | `merge`: `reduce via rule dedupe`, grouped by exact `title` (`:255`) | Three reviewers word one defect three ways, so exact titles rarely match. |
| Skip what `TODO.md` holds | the sink skips an open item with the same normalised text (`pass.rs:1732`) | A reworded finding is added again. |
| Skip what was rejected | nothing | An item deleted or ticked returns the next night. |

## Budget

Verified against the code on 2026-10-27.

- **`workflow_budget` is per unit of input, at activation only.** `pma workflow propose` and the library's first `pma run` price each workflow over one unit (`crates/pma/src/main.rs:3139`, `crates/pma/src/run.rs:262`). Activation refuses a figure over `workflow_budget`. A pass checks nothing against it (`main.rs:3312`).

- **A pass's worst case grows with its targets until `caps.max_units` stops it.** Each node's input is cut to the instance cap (`crates/pma-core/src/workflow.rs:2888`). From `pma workflow check --units N` at `agent_budget` $1.00:

  | Projects | `portfolio-sweep` | `find-issues` |
  |-|-|-|
  | 1 | 27 runs, $27 | 41 runs, $41 |
  | 10 | 270 runs, $270 | 70 runs, $70 |
  | 100 | 700 runs, $700 | - |

  `find-issues` flattens early because its `max_units` is 60; `portfolio-sweep`'s is 400.

- **`batch_budget` bounds one plan, not a pass.** A plan holds `floor(batch_budget / agent_budget)` runs, 5 at the defaults (`crates/pma-core/src/pass.rs:557`). `--unattended` approves each plan in turn until the instance finishes or waits (`pass.rs:1006`). No running total of spend is kept. The help text, "`batch_budget` sizes each plan", is accurate but bounds a night only by the pass's worst case.

- **`agent_budget` is not a hard bound for any built-in worker.** `claude` gets `--max-budget-usd {budget}` (`crates/pma-core/src/worker.rs:419`) but checks it between turns: a $0.05 cap once cost $0.09. Every built-in worker has `enforces_budget: false` (`worker.rs:431`). The dollar figures above assume each run stays within `agent_budget`.

- **A pass-wide check existed and was removed.** A pass once compared its total against `workflow_budget`; that stopped any revision running over two projects (CHANGELOG, `workflow_budget is weighed at activation only`). A cap for a night needs its own setting, not that one.

So a night is bounded today, but only by the pass's worst case: $270 for `portfolio-sweep` over 10 projects. Nothing holds it to a figure the user sets.

## Proposal

Confirmation is spent from the top: critical findings first, then high, then medium and low while budget is left. Confirmed findings go to a triage list, not to `TODO.md`; the user accepts each into `TODO.md` or rejects it. The steps are in build order.

### 1. `instance_budget` (built)

A setting: the most one workflow instance may spend, in dollars, over every target and every resume. `off` by default. Named for the instance rather than the pass, because a resumed instance must not spend its budget again.

- Before each plan of an unattended pass, the instance's spend so far is the sum of `cost_usd` over its runs (`runs.workflow_instance`); a run with no reported cost counts at `agent_budget`. If that plus the plan's worst case would exceed `instance_budget`, the plan is not approved. The pass prints why and the plan with its approve command, as an attended pass does, and a detached job waits for approval.

- A plan approved by its id runs whatever the cap: the developer sees its price. So does `pma job approve` on a job that stopped at the cap.

- It admits plans; it does not cap spend. A plan's runs can each overshoot `agent_budget` (see Budget), so the last plan admitted can carry the instance past the cap by that overshoot.

- An instance whose worst case is over the cap starts anyway, with a line saying so. A refusal before start was proposed as an opt-in and is not built: worst cases assume every run spends its full `agent_budget`, so it would refuse most useful nights.

**Calibration.** The starting value is $10, a placeholder. What $10 buys depends on the agent and model pair and on the provider's prices, so the value is set from measured cost per run, not from worst cases. `pma report` breaks cost down by project, class and agent, not by model (`--by model` is refused), so calibrating per model needs that dimension added.

### 2. Confirm by severity, from the top

`find-issues` and `ensemble-review` confirm every finding at the same cost, whatever its severity. Under a budget, a medium finding confirmed early can cost the critical one found later its confirm run.

- **`rank` by a named field.** `{"rule": "rank", "by": "severity"}` orders a group by the field's declared values: critical, high, medium, low. Today `rank` reads only `priority` (`crates/pma-core/src/pass.rs:1843`), and a check refuses it on a type without that field (`crates/pma-core/src/workflow.rs:1542`). `priority` stays the default.

- **A `rank` node before `confirm`**, in `find-issues`, and after `merge` in `ensemble-review`. A reduce runs once every node before it has finished, so with every nightly project in one instance the ranking spans projects: a critical finding in one is confirmed before a medium one in another.

- **Plans take units in ranked order.** A plan takes waiting units in order (`pass.rs:557`), and a reduce mints its kept units in the order it sorted them. Read from `plan_of` and `mint`, not yet tested; a test holds it.

- **`instance_budget` decides where a night stops.** Once the next plan would cross it, no further confirm runs.

- **Findings left unconfirmed are dropped, with no fingerprint.** A night with budget left can find and confirm them. Not listed as unconfirmed: in the pilot, haiku's unconfirmed claims were 0 of 7 true (`docs/dev/workflow-pilot.md`).

- **`confirm` may rewrite `severity`**, as well as `reason`. The ranking trusts the reviewer's severity, which nothing has checked; a reviewer that marks everything critical would make it useless. A confirmed finding then reaches triage with a severity the confirm checked. The confirm task must ask for it.

Rust: `rank`'s `by`, and its check. The rest is the workflow documents and a test.

A night whose critical and high findings use the whole budget confirms no medium or low one. That is the intent. A cheaper model for low-severity confirms would stretch the budget, but routes match on node and lap, not on a unit's fields; it would need a route on a field, or a confirm node per severity. Deferred.

### 3. A triage list

A nightly finding goes into a store table, not into `TODO.md`. Each row holds a fingerprint (the normalised title and the sorted `paths`), the project, the finding and its state: waiting, accepted or rejected. A later night skips any fingerprint the table already holds, whatever its state, so a rejected finding does not return.

- Both front ends list waiting findings in an inbox pane. One key accepts a finding into `TODO.md` through the existing `todo` sink; another rejects it.

- An accepted finding is written without `#agent`, so `dispatch --auto` does not take it until the user tags it.

Rust: a table and a migration, a sink that writes to it, two actions and their keys, and a pane.

### 4. Schedule with launchd

A launchd job runs:

```sh
pma workflow run find-issues --tag nightly --unattended
```

The `nightly` tag selects the projects. `StartCalendarInterval` runs a missed job once the machine wakes, and coalesces several missed intervals into one run ([`launchd.plist(5)`](https://keith.github.io/xcode-man-pages/launchd.plist.5.html)). cron skips a missed run. On Linux, a systemd timer with `Persistent=true` does the same.

No Rust: a plist and its documentation. A `pma schedule` command can follow if writing the plist by hand proves error-prone.

### Deferred

- An agent `merge` in place of exact-title dedupe (`via: agent`, `crates/pma-core/src/workflow.rs:999`). `find-issues` has one reviewer, so rewordings of one defect are rarer than under `ensemble-review`'s three.

- An agent check of each finding against `TODO.md` before it is listed. The fingerprint skip and the triage step may make it unnecessary.

Add either if the triage list proves noisy.

## Risks

- **Public repositories.** A `TODO.md` that is pushed is published. A nightly `security-review` finding there discloses a vulnerability. Keep `security-review` out of scheduled runs, or send its findings to notes.

- **Auto-dispatch.** A filed finding must not carry `#agent`, or `dispatch --auto` hands unreviewed findings to an agent. The sink adds no tag today; keep it so. A `#found` tag would make filed findings filterable.

- **Cost.** `portfolio-sweep` runs `ensemble-review` on opus: 3 reviewers, then a confirm run per finding, up to 27 runs per project. See Budget for the totals.

- **Session lock.** A sweep holds `session.lock`, so a morning dispatch waits if the sweep overruns. Per-project locks are an open TODO item.

## Rejected for now

- **Findings straight into `TODO.md`.** Exposes findings in public repositories, lets unreviewed findings reach auto-dispatch, and leaves no record of what the user rejected. Revisit once most nightly findings are accepted, such as 80% over two weeks.

- **Triage for public repositories, direct for private ones.** Needs pma to know each repository's visibility. Not before the direct path is worth having.

## Decisions

Decided, 2026-10-27:

1. Findings go through the triage list in every repository; no split by visibility yet.

2. `instance_budget` starts at $10 as a placeholder, to be calibrated per agent and model pair and provider pricing.

3. Confirmation is spent by severity, from the top. Findings left unconfirmed when the budget runs out are dropped without a fingerprint, and `confirm` may rewrite `severity`.

Recommended, not yet decided:

4. `find-issues` rather than `ensemble-review`. With ranking and `instance_budget`, cost no longer decides: either is capped and spends from the top. What remains is fewer rewordings with one reviewer against wider coverage with three. `ensemble-review` weekly, or by hand, once an agent `merge` folds its rewordings.

5. `security-review` stays out of scheduled runs and runs by hand.

6. Before building, run `pma run review` by hand on 3 projects for a week, recording cost per project and the share of findings kept. The pilot shipped 1 of 20 items (`docs/dev/pilot.md`); a low share argues against a nightly run.

7. Which projects get `nightly`.
