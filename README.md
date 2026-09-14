# pma

Maintenance across many repositories from one place.

Status: stages 1 and 2 of the [design](docs/dev/design.md): lint, scan, matrix and
health. Agent dispatch is not built yet.

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

```sh
pma root add ~/projects/personal  # git repos directly under it are projects
pma tier cyllama 1                # 1 (most important) to 5, or none
pma scan                          # TODO.md, git state, CI via gh; about 15s for 95 repos
pma scan --offline cyllama        # one project, without GitHub
pma matrix                        # tiered projects' tasks in the Eisenhower matrix
pma matrix -q q1 --all
pma status --explain              # projects by health, with each signal's share
pma config                        # every setting; `pma config tiers.2 0.7`, `--reset`
```

`matrix` and `status` read the last scan; they do not rescan. Only tiered
projects appear in them.

The database is `~/.config/pma/projects.db`, or `$PMA_HOME/projects.db`.

## Development

```sh
make test     # cargo test, then pytest on scripts/
make check    # fmt check, clippy -D warnings, test
```
