# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

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
