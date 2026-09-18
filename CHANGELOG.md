# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

**Per-attempt records.** Each agent invocation and the verification after it is now an append-only `attempts` row, carrying its own summary, cost, duration, verify result and error. `runs` keeps the lifecycle summary, so its cost and duration still sum across reworks. `pma review <id>` lists the attempts when there is more than one.

A run was one mutable row, so a rework overwrote the previous attempt's summary, verify result and start time. A task that failed twice and then succeeded recorded only the success, which is the case the calibration data most needs. The database schema moves to version 6; existing runs contribute their last attempt, whose outcome was never recorded and stays null.

**A task an agent cannot close is not chosen again.** Attempts are counted per project and task revision, and at two, `pma dispatch --auto` passes the task over and a named dispatch refuses. `pma dispatch <target> --retry` clears the count. The count survives the run that raised it, so rejecting a task and dispatching it again no longer starts from zero; it was keyed to nothing durable before, and `--auto` could pick the same task forever.

The revision is the task's normalised text. Rewording a task starts a fresh count, which is right: a materially revised specification has not been tried. It also separates recurring signals, since `fix CI: build` and `fix CI: build, test` are different incidents while the word `ci` is not.

A red verification and a rejection each consume one attempt. A run refused at the batch budget, an agent that could not start, and a run killed at the timeout consume none: none of them says anything about whether the task suits an agent. The database schema moves to version 10.

**Review by exception.** `pma review` marks each run `clean` or `N to read`, and `pma review <id>` lists why:

```
read this run because:
  - outside class B: .github/workflows/ci.yml
  - the base already passed, so no check discriminates this change
```

What the two verify results must show depends on the class. Class A must leave a green tree green, so a base that was already failing blocks. Class B is accepted by a check that fails at the base and passes at the head; green to green demonstrates no regression but does not demonstrate a fix. A missing command, a check that did not run, and a base that could not be measured are each distinct from a red result. A run that edited the files implementing its own verify command is read whatever else passed, since a worker that changes its own check can turn any tree green; the files are derived from the command, and test files are not among them.

Nothing is approved or shipped by this. Review time is the throughput limit, and reading the reasons is faster than reading the diff. The reasons are computed from the run rather than stored, so changing a rule re-reads the evidence.

**Path scope, checked against the whole change.** Every path a run touched is enumerated against its base and recorded, and `pma review <id>` reports what the task's class does not permit: `scope  1 of 5 files OUTSIDE: .github/workflows/ci.yml`. Untracked files are staged with `--intent-to-add` first, so a new file counts; the listing is NUL-delimited because a path may contain a newline, and `--name-status` names both sides of a rename. Paths that could not be enumerated are recorded as unknown with the reason, never as an empty list, which would read as a run that changed nothing.

`.github/**`, `LICENSE`, `COPYING` and `.netrc` are refused to every class but A-, judged on the paths actually changed rather than the class predicted at dispatch. A workflow runs with repository tokens and CI validates the changed workflow rather than checking it, so a task misread as a dependency bump must not be able to edit one. The run stores the paths, not the verdict, so changing the rules re-reads the evidence. The database schema moves to version 9.

**Verification at the base commit.** `pma` now runs the project's `verify` in the fresh worktree before the agent starts, and `pma review <id>` reports both results: ``make test`: base FAILED, head passed`. One run at the head proves only that the tree is green now; it cannot tell a regression from a repository that was already broken, which is what class A auto-approval will rest on. Results are cached per project, base commit, command and timeout, so every task of a project in one batch pays for one run, and changing the command or the timeout misses the cache rather than reusing it. A base check that could not be started is recorded as unknown, not as failing.

The verify command is chosen once, at dispatch, and no longer changes. It was re-detected on every attempt, so a rework could check the head with a command the base was never checked with. The database schema moves to version 8.

**Task class, and the decision behind a dispatch.** Each task gets a maintenance class from `docs/dev/design-review.md`: `deps` is A (mechanical), `ci` and any unclassified item are B (specified), and an item tagged `#manual` is D and is refused before a worktree is created. The run records the class, the globs that class allows, the project's tier, the item's description, `agent_budget` and `timeout` as they were at dispatch, and none of them is updated afterwards. A rescan, a reworded item or `pma config` would otherwise leave no way to say what a past dispatch decided. The database schema moves to version 7; runs dispatched before it have no snapshot.

The globs are recorded but not yet enforced.

**A timestamp per run transition:** `dispatched_at`, `ready_at`, `decided_at` and `published_at`. `runs` previously held only the current attempt's start time, which a rework moved, so no query could count changes published per day or bound a digest to one pass. `published_at` is when `pma` pushed or opened the pull request; a merge days later does not move it, and a pull request closed unmerged does not replace the approval in `decided_at`.

**`pma review <id> --minutes N`** adds reported review time to the run, summed over repeated reviews. Review time is the throughput limit this design assumes, and nothing measured it.

**Deps signal.** `pma scan --deps` counts outdated dependencies with `cargo update --dry-run`, `uv tree --outdated` and `go list -u -m all`, adds an "update dependencies" task, and scores deps in health. It is opt-in because it took 44s for 52 repos against 1s for a plain scan. Plain scans keep the last measurement. Counts differ in kind between tools: cargo's include transitive crates.

**`pma prune`** removes finished items and their descriptions from TODO.md files, and each v1 `## Done` section whole. A dry run unless `--apply`. Files with lint errors are skipped.

**Leftover worktrees in the hygiene signal.** A scan counts local `pma/` branches that no open run owns, and "resolve local changes" lists them. Each `pma` worktree has such a branch, so the count also finds branches whose worktree is gone. Hygiene scores 0.5 per condition, capped at 1, which keeps the existing values. The database schema moves to version 4.

**`pma note`** adds, edits, removes and lists portfolio notes.

**`pma tui`** shows the matrix as a 2x2 layout with the selected task's details. `ratatui` is built with only its crossterm backend, which adds 69 crates instead of 152.

**`pma sync`.** Opens an issue labelled `pma:critical` for each `## Critical` item and writes `gh:N` into its line, marks items done when their issue is closed, retitles and relabels linked issues to match TODO.md, and lists open issues by other people. A dry run unless `--apply`. Write-backs stay uncommitted in the clone rather than going through `pma ship`, which commits worktrees only.

Each `gh:N` is saved as soon as its issue exists, and an open labelled issue with an unlinked item's title is linked rather than duplicated, so an interrupted sync does not open a second issue. Adding `gh:N` changes an item's key, so dispatch and ship now match a task by key or by text.

**`pma dispatch`, `pma review`, `pma ship`.** Run `claude` on tasks in worktrees of the remote default branch, verify the result with the project's own tests, review the diff, then commit, push or open a PR, and mark the item done. Settings: `max_parallel`, `batch_budget`, `agent_budget`, `timeout`, `publish`, `attribution`, `dispatch_quadrants`, `overflow_quadrants`, and per project `projects.<name>.verify` and `projects.<name>.publish`. The database schema moves to version 2; version 1 databases are upgraded on open.

The item is marked done after the rebase, not before. Git treats changes to adjacent lines as a conflict, so ticking first could conflict for two tasks from one project. An item must be open in the remote `TODO.md` to be dispatched; otherwise ship would have no line to mark.

`claude --max-budget-usd` is checked between turns and was exceeded in use ($0.09 under a $0.05 cap). `batch_budget` limits which runs start, not their total spend.

### Changed

**The agent may run the verify command.** `claude` got `acceptEdits` only, so in `-p` mode it could not run the project's tests, while `pma` ran code the agent had edited anyway. It now gets one exact `Bash(...)` rule per subcommand of verify. Other shell commands stay denied. The design states that the permission mode is not an isolation boundary.

**TODO.md format v2: finished items stay in their priority section.** `## Done` is no longer part of the format. Ship and sync tick `- [ ]` to `- [x]` in place, and lint accepts `- [x]` in any priority section. A `## Done` section grows without limit; `pma prune` removes finished items when wanted. Each item in an existing `## Done` raises a warning that names `pma prune`.

**`pma lint` accepts bullets outside the priority sections in a migrated file.** The "no items" warning now fires only when a file also has no priority section. A file with `## Critical` to `## Low` and no open items is valid, and its declined or deferred work can stay as plain bullets instead of becoming tasks.

### Fixed

**A task shipped as a pull request was dispatched again before the merge.** The run became `shipped`, which frees its task, while the item stayed open on the default branch. The second run then failed to push over the first run's remote branch. Such a run is now `pr-open` until `gh pr view` reports it merged (`shipped`) or closed (`rejected`); `pma review` and `pma dispatch` check. A new run's slug also avoids remote `pma/` branches. The database schema moves to version 5, so an older `pma` refuses the file instead of reading `pr-open` as failed. The upgrade returns runs already shipped as pull requests to `pr-open`, so their merge state is checked once.

**`pma review` during a dispatch marked the running runs failed.** It assumed no other session was live. A failed run could then be rejected or reworked while its agent still ran. `dispatch`, `ship`, `review --reject` and `review --rework` now hold an `flock` on `session.lock`, and interrupted runs are failed only when that lock is free. A lock was chosen over a pid column in `runs`: the kernel releases it on exit, and pids are reused. A command that needs the lock waits up to 2s, since `pma review` holds it for milliseconds. `File::try_lock` raises the minimum Rust version to 1.89.

**`pma ship` could not resume after a partial success.** If removing the worktree failed after a push, the run stayed approved, and the retry reported "nothing to ship" for work already on the default branch. If `gh pr create` failed after the branch was pushed, a retry could fail on the existing pull request. The outcome is now saved before cleanup, and a cleanup failure is a warning. A retry records pushed commits found in the default branch, and reuses an open pull request for the branch.

**`pma dispatch --auto` could dispatch nothing when its top task was refused.** A refused pick kept its place, so `-n 1` with an unpushed item on top started no run. The next candidate now takes the place. The refusal says whether the item is missing from the remote `TODO.md` or already done there; it always said "commit and push it first".

**A fix-CI prompt could carry the wrong log.** It took the latest failed run of any workflow on the branch, which may be an old failure of a workflow that passes now. With no failed run, it called `gh run view null`. It now takes each failing workflow's latest decisive run, as scan does, and refuses the task when that workflow passes.

**CI detail names the cause of a `gh` failure.** It kept the last line of `gh`'s stderr, which is an alternative or an update notice. Without authentication every project read "Alternatively, populate the GH_TOKEN environment variable..." instead of "please run: gh auth login".

## [0.1.0] - 2026-09-14

### Added

**Item groups.** An item's group is the nearest `###` heading above it within its section, shown as a column in `pma matrix`. A group comes from the item's position in the file rather than a tag. Migrated files can keep their original sub-headings without a long per-item tag. The trade-off: an item moved to another place changes group.

**`pma scan`, `pma matrix`, `pma status`.** Scan the git repos under the roots into SQLite, place the tasks of tiered projects in an Eisenhower matrix, and rank projects by health. `status --explain` prints each signal's share. Tiers, roots and settings are managed with `pma tier`, `pma root` and `pma config`.

An item's age is taken from the commit where its text first appeared in `TODO.md`, not from `git blame`. On real repos, blame dated every migrated item to the migration commit, because migration added a tag to each line. Every age restarted at 0, and no item could become urgent by age for a month.

**`pma lint`.** Checks TODO.md files against format v1, defined in `docs/dev/design.md`. The parser works line by line, not through a markdown AST, so later stages can edit one line without re-rendering the file. A file with no items but with plain bullets elsewhere is flagged. Otherwise such a file lints clean while `pma` sees none of its tasks.

[Unreleased]: https://github.com/shakfu/pma/compare/0.1.0...HEAD [0.1.0]: https://github.com/shakfu/pma/releases/tag/0.1.0
