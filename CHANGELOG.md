# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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
