# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

**Deps signal.** `pma scan --deps` counts outdated dependencies with
`cargo update --dry-run`, `uv tree --outdated` and `go list -u -m all`, adds
an "update dependencies" task, and scores deps in health. It is opt-in because
it took 44s for 52 repos against 1s for a plain scan. Plain scans keep the last
measurement. Counts differ in kind between tools: cargo's include transitive
crates.

**`pma note`** adds, edits, removes and lists portfolio notes.

**`pma tui`** shows the matrix as a 2x2 layout with the selected task's
details. `ratatui` is built with only its crossterm backend, which adds 69
crates instead of 152.

**`pma sync`.** Opens an issue labelled `pma:critical` for each `## Critical`
item and writes `gh:N` into its line, marks items done when their issue is
closed, retitles and relabels linked issues to match TODO.md, and lists open
issues by other people. A dry run unless `--apply`. Write-backs stay
uncommitted in the clone rather than going through `pma ship`, which commits
worktrees only.

Each `gh:N` is saved as soon as its issue exists, and an open labelled issue
with an unlinked item's title is linked rather than duplicated, so an
interrupted sync does not open a second issue. Adding `gh:N` changes an item's
key, so dispatch and ship now match a task by key or by text.

**`pma dispatch`, `pma review`, `pma ship`.** Run `claude` on tasks in
worktrees of the remote default branch, verify the result with the project's
own tests, review the diff, then commit, push or open a PR, and mark the item
done. Settings: `max_parallel`, `batch_budget`, `agent_budget`, `timeout`,
`publish`, `attribution`, `dispatch_quadrants`, `overflow_quadrants`, and
per project `projects.<name>.verify` and `projects.<name>.publish`. The
database schema moves to version 2; version 1 databases are upgraded on open.

The item is marked done after the rebase, not before. Two tasks from one
project each insert at the top of `## Done`, so editing first made the second
rebase conflict. An item must be open in the remote `TODO.md` to be
dispatched; otherwise ship would have no line to mark.

`claude --max-budget-usd` is checked between turns and was exceeded in use
($0.09 under a $0.05 cap). `batch_budget` limits which runs start, not their
total spend.

### Changed

**`pma lint` accepts bullets outside the priority sections in a migrated
file.** The "no items" warning now fires only when a file also has no priority
section. A file with `## Critical` to `## Low` and no open items is valid, and
its declined or deferred work can stay as plain bullets instead of becoming
tasks.

## [0.1.0] - 2026-09-14

### Added

**Item groups.** An item's group is the nearest `###` heading above it within
its section, shown as a column in `pma matrix`. A group comes from the
item's position in the file rather than a tag. Migrated files can keep their
original sub-headings without a long per-item tag. The trade-off: an item moved
to another place changes group.

**`pma scan`, `pma matrix`, `pma status`.** Scan the git repos under the roots
into SQLite, place the tasks of tiered projects in an Eisenhower matrix, and
rank projects by health. `status --explain` prints each signal's share. Tiers,
roots and settings are managed with `pma tier`, `pma root` and `pma config`.

An item's age is taken from the commit where its text first appeared in
`TODO.md`, not from `git blame`. On real repos, blame dated every migrated item
to the migration commit, because migration added a tag to each line. Every age
restarted at 0, and no item could become urgent by age for a month.

**`pma lint`.** Checks TODO.md files against format v1, defined in
`docs/dev/design.md`. The parser works line by line, not through a markdown
AST, so later stages can edit one line without re-rendering the file. A file
with no items but with plain bullets elsewhere is flagged. Otherwise such a
file lints clean while `pma` sees none of its tasks.

[Unreleased]: https://github.com/shakfu/pma/compare/0.1.0...HEAD
[0.1.0]: https://github.com/shakfu/pma/releases/tag/0.1.0
