# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

**`pma lint`.** Checks TODO.md files against format v1, defined in
`docs/dev/design.md`. The parser works line by line, not through a markdown
AST, so later stages can edit one line without re-rendering the file. A file
with no items but with plain bullets elsewhere is flagged. Otherwise such a
file lints clean while `pma` sees none of its tasks: 25 of 65 existing files
are like this.
