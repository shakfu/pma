# Fix-CI loop

Design and build, 2026-11-27. Code: `crates/pma-core/src/ci_loop.rs`.

A headless fix-CI run loops on a pull request until its checks pass or its rounds run out. pma pushes and waits; the agent never stays alive or holds push credentials. You review once, at the end.

## Evidence for the need

- 51 fix-CI runs in the store, 40 interactive.
- CI judged 12 of them, all `fixed`.
- The 2026-11-15 backfill (`ci-verdict.md`) found 15 of 19 pushed fixes passed first time, 1 failed, and 3 had no CI run.

A second round is needed about 1 time in 16. The loop takes the waiting out of the headless path; the success rate is no reason to build it.

## Preconditions, checked at dispatch

- Task `ci`, headless, and neither `--local` nor `--push`. `ci_loop.max` (3) above 0. Every such dispatch loops, `--auto` included; `ci_loop.max 0` turns it off.
- The run gets its own worktree on a `pma/` branch, whatever `workspace` says. The clone is not touched.
- `gh` on `PATH`, origin on GitHub.
- A workflow file's top-level `on:` names `pull_request`, read from its text. Otherwise the dispatch prints `no CI loop: <why>` and the run goes ahead as before.
- The repository's default `GITHUB_TOKEN` permission is `read` (`gh api repos/<slug>/actions/permissions/workflow`). Otherwise the loop is refused, with the setting to change. See "Secrets".
- No workflow a round can start names a secret (bar `GITHUB_TOKEN`), `secrets: inherit` or a `write` permission. A round starts workflows on `push`, `create`, `pull_request` and `workflow_run`; branch filters are not read. Quotes are dropped and `secrets[...]` and `toJSON(secrets)` count. The repository default does not cap a workflow's own `permissions:`. This is a text match, not a YAML parse.

## A round

1. **Dispatch.** CI is read fresh (`refresh::ci`). The prompt carries the failed logs (`failed_ci_logs`). The worktree starts at `origin/<default>`.
2. **Attempt.** The agent runs as today, followed by verify.
3. **Auto-gate.** Each of these hands the run to you as `ready`:
   - verify failed;
   - the run's route is `propose`;
   - the diff is empty;
   - the diff touches `.github/`;
   - the diff deletes a test file or adds a skip marker. The checks read the tree that would be published, new files included, and a rename counts as a deletion. `|| true` counts only in a Makefile, justfile, `package.json`, `tox.ini`, `noxfile.py` or a file named for tests.

   The run's own gates are `accept::loop_reasons`: `review_reasons` without the class B rule that a base which already passed proves nothing. A fix-CI run's local verify usually passes at the base, as on 8 of the 11 headless runs with both results recorded; the pull request's checks judge it instead. From round 2, a round whose tree equals the branch's also stops.

   If none applies, the run is approved with `approved_by = ci-loop`.
4. **Publish.** The `pma pr` path builds one commit of the approved tree on the base. In round 1 it pushes the branch and opens the pull request. It is not a draft: `pma merge` already refuses failing or unfinished checks. In later rounds it force-pushes with a lease (`pr_tip_ref`). `pr_head` records the commit.
5. **Wait.** The dispatch job reads `statusCheckRollup` every 60 s. It releases the project lock while it waits. It waits until every check on `pr_head` has finished. `ci_loop.wait` (30 min) bounds the round's whole wait, re-run included. Up to 2 failed reads in a row are retried. A head still at the round before's commit is GitHub not yet updated, not a push by someone else.
6. **Decide.**

| Checks on `pr_head` | Next |
|-|-|
| all pass | Say so with the URL. The loop ends; pma does not merge. |
| a failure, rounds left | `gh run rerun --failed` once per Actions run. A higher `attempt` number says the re-run started, so the old failure is not read as its result. If it still fails, go to the next round. |
| a failure, no rounds left | The run stays `pr-open` with the error, under "For you" in `pma next`. |
| no check within 10 min | Stop: CI does not run on this pull request. |
| `ci_loop.wait` passes | Stop with that error. |
| the head moves to another commit | Stop: someone else pushed. |

An error, such as a `gh` failure or the project lock busy for 10 min, stops the loop and is recorded on the run. The dispatch then exits non-zero. A publish that fails takes back the loop's approval, so no unreviewed change stays approved.

A later round whose agent fails or times out returns the run to `pr-open`, with the error. The pull request still holds the round before's change.

The next round is `dispatch::fix_checks`, modelled on `fix_conflicts`:
- remove the last round's worktree if publishing left it, then rebuild it at the base, with the branch's tree uncommitted, as round 1 started. At the branch tip, pma's own commit would read as the agent's;
- record that tip as the lease;
- set the feedback to the failing run's `--log-failed` tail and "your previous fix is in the tree";
- set the run to `running`, then repeat steps 2 to 6.

The agent learns the history from the tree and the feedback, not from a resumed session.

## Budgets

- `ci_loop.max` caps the rounds.
- A loop that ends unfixed consumes one task attempt (`ATTEMPT_LIMIT`). A loop that ends fixed consumes none.
- A later round whose verify fails consumes one, as in `fix_conflicts`.
- Each round's cost, diff and log stay in `attempts`.

## Secrets

A pull request from a branch in the same repository runs its workflows with the repository's secrets and `GITHUB_TOKEN`. Each round runs agent code in CI that nobody has reviewed.

Measured 2026-11-27 over the 90 projects with workflows:

- Named secrets appear only in release workflows that run on `push: tags` or `workflow_dispatch`: hax, entangled-rs, py-js. None runs on `pull_request`.
- `GITHUB_TOKEN` is in every run. `default_workflow_permissions` is `read` on myra, timu and hax, and `write` on gopherwiki. gopherwiki also allows approving pull request reviews. There, a round's code could push to the repository and approve pull requests.

Hence the `read` precondition. A push made with `GITHUB_TOKEN` triggers no further workflow, so a token that can only read cannot reach the release secrets.

## Built and new

| Reused | New |
|-|-|
| worktree runs, `pma pr` publish, lease push | `ci_loop.max`, `ci_loop.wait` |
| `fix_conflicts` re-attempt with feedback | auto-gate, `approved_by = ci-loop` |
| `attempts` records | wait for checks in the dispatch job; lock released while waiting |
| merge gate on checks and `pr_head`, `failed_ci_logs` | `publish::failed_checks`, re-run by attempt number |
| | preconditions: `pull_request` trigger, token permission, workflow secrets and permissions |

No schema change. Each round is a row in `attempts`.

The dispatch job stays alive for the whole loop. Under the job runner it holds that project's job slot throughout, so later jobs in the project wait; the project's lock is free while CI runs.

## Rejected

- **The agent pushes and watches CI itself.** It needs push credentials, which `agent::restrict` removes on purpose. Each failed round lands unreviewed on the default branch. The agent idles for the CI time, about 9 min for timu, under its run timeout.
- **Re-dispatch after each landed fix, reviewed each round.** No branch handling is needed, but you review every round. It stays as the fallback where CI does not run on pull requests.
