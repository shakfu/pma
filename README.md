# pma

Maintenance across many repositories from one place.

Status: stage 1 of the [design](docs/dev/design.md). Only `pma lint` exists.

## Install

```sh
make install          # cargo install --path .
```

## TODO.md format

```markdown
# TODO

## Critical

- [ ] segfault on empty input #bug gh:42

## High

- [ ] support ggml 0.9 due:2026-10-01

## Medium

## Low

## Done

- [x] drop python 3.9
```

The section gives the priority. Trailing `#tag`, `due:YYYY-MM-DD` and `gh:N`
(a linked issue) are optional. The full rules are in the
[design](docs/dev/design.md#todomd-format-v1).

## Commands

```sh
pma lint                        # ./TODO.md
pma lint ~/projects/personal/*/ # every project; a directory means its TODO.md
```

Output is `path:line: severity: message`. Exit status is 1 when any file has an
error or cannot be read. Warnings alone exit 0.

## Development

```sh
make test     # cargo test
make check    # fmt check, clippy -D warnings, test
```
