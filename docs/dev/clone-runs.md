# Runs in the clone

Status: built, 2026-10-26. The default since; `workspace = worktree` keeps the design in `stage-commit-push.md`.

## Problem

A run edited a worktree of the remote default branch. Reviewing it meant reading `pma review`'s diff rather than the change in the project with its own tools, verify rebuilt the project from nothing per run, and a run in a worktree could not use the build cache in the clone. A `--local` run that failed verify was discarded, and its patch was lost with the worktree.

## Decision

The agent edits the clone, from the working tree as it is. Nothing is staged, committed or pushed by anyone but `pma` after approval.

| | Clone run | Worktree run |
|-|-|-|
| `base` | `HEAD` | fetched default branch, or the working tree with `--local` |
| `start` | a commit of the working tree, untracked files included | none |
| the diff | `start` against a commit of the working tree, built in a temporary index | base against the worktree |
| verify | in the clone | in the worktree |
| reject | `git apply --reverse` of the patch, working tree only | the worktree is removed |
| stage | `git apply --cached` of the patch, then the task tick in the file and the index | `git apply --index` in the clone |
| at a time | one per project; a second is refused until the first is decided | any number |

- Every clone run ends in review, whatever verify said. A verify that fails for a reason the agent did not cause (see the sandbox entry in the CHANGELOG) still leaves a readable, approvable change.

- `remove_worktree` refuses the repository itself. A clone run's `worktree` is `repo`.

- The clone's index is never written outside `stage`. `tree`, `diff`, the scope check and the patch read the working tree through a temporary index (`working_tree_commit`).

- A failed run, interrupted or budget-refused, holds the clone until rejected: its agent may have left edits that only the revert takes out.

- A revert that no longer applies, because the user edited the same lines since, is refused with the patch's path; the run stays as it was.

- Each attempt records its change as `runs/<id>/ready.patch`. A reject reverses that record, never the tree as it stands, which holds whatever came later. Once `HEAD` leaves the base, a reject is refused, and `--no-revert` or `X` closes the run with its files as they are. The diff reads the record from then on; until then it stays live, so a reviewer's tweak is in what they read and publish. Found 2026-10-31: rejecting a run whose change had been committed by hand reversed every later commit and all uncommitted work in the tree.

## Rejected

- **Agent commits, pma reverts the commit.** Moves `HEAD` under the user, and the agent is told never to commit.

- **Stage by blob rather than by patch.** `update-index --cacheinfo` with the reviewed blobs would stage the user's own unstaged hunks in the same file. The patch stages what was reviewed, and refuses when the index differs from what the run started from.

- **Allow several clone runs per project.** Their changes would overlap in one working tree and the second's diff would include the first's.
