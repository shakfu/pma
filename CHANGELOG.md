# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.1.0]

### Added

**Item groups.** An item's group is the nearest `###` heading above it within
its section, shown as a column in `pma matrix`. A group comes from the
item's position in the file rather than a tag. Migrated files can keep their
original sub-headings without a long per-item tag. The trade-off: an item moved
to another place changes group. The database schema is now version 2, and a
version 1 database gains the column when opened.

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
file lints clean while `pma` sees none of its tasks: 25 of 65 existing files
are like this.

### Fixed

**`due:` accepted a sign.** Date fields were parsed with `u32::parse`, which
accepts a leading `+`, so `due:2026-+1-01` passed lint.
