# pma

Maintenance across many repositories from one place.

Status: every stage of the [design](docs/dev/design.md): lint, scan, matrix,
health, agent dispatch with review and ship, Issues sync, notes, the deps
signal, and a TUI. `claude` is the only agent.

## Install

```sh
cargo install pma
```

From a clone: `make install`.

Requirements:

- macOS or Linux. The database location is derived from `HOME`.
- Rust 1.88 or later, and a C compiler: SQLite is compiled in.
- `git` on `PATH`.
- `gh`, authenticated, for CI status. Without it, CI is recorded as unknown and
  left out of health; `pma scan --offline` skips GitHub on purpose. Also for
  fix-CI dispatch, `publish = pr`, and `pma sync`.
- `claude`, logged in, for `pma dispatch`.

## TODO.md format

```markdown
# TODO

## Critical

- [ ] segfault on empty input #bug gh:42

## High

- [ ] support ggml 0.9 due:2026-10-01

## Medium

## Low

- [x] drop python 3.9
```

The section gives the priority. A finished item is ticked where it stands;
`pma prune` removes finished items. A `###` heading inside a section groups the
items below it. Trailing `#tag`, `due:YYYY-MM-DD` and `gh:N` (a linked issue)
are optional. The full rules are in the
[design](docs/dev/design.md#todomd-format-v2).

## Commands

```sh
pma lint                        # ./TODO.md
pma lint ~/projects/*/          # every project; a directory means its TODO.md
pma prune ~/projects/*/         # plan: finished items and `## Done` to remove; --apply
```

Output is `path:line: severity: message`. Exit status is 1 when any file has an
error or cannot be read. Warnings alone exit 0.

```sh
pma root add ~/projects/          # git repos directly under it are projects
pma tier cyllama 1                # 1 (most important) to 5, or none
pma scan                          # TODO.md, git state, CI via gh; about 15s for 95 repos
pma scan --offline cyllama        # one project, without GitHub
pma scan --deps                   # also count outdated cargo, uv and go dependencies
pma matrix                        # tiered projects' tasks in the Eisenhower matrix
pma matrix -q q1 --all
pma status --explain              # projects by health, with each signal's share
pma config                        # every setting; `pma config tiers.2 0.7`, `--reset`
pma tui                           # the matrix in the terminal; q quits
pma note add "move CI to reusable workflows"   # portfolio notes; `pma note` lists
```

`matrix` and `status` read the last scan; they do not rescan. Only tiered
projects appear in them.

```sh
pma dispatch cynn:31              # a TODO.md line from the last scan; or cynn:ci, cynn:deps
pma dispatch --auto -n 3          # the top 3 dispatchable tasks in the matrix
pma review                        # runs not yet shipped or rejected
pma review 4                      # task, verify result, cost, summary, diff
pma review 4 --approve            # or --reject, or --rework "feedback"
pma ship                          # commit, push or open a PR, remove worktrees
```

Each run gets a worktree of the remote default branch under
`~/.config/pma/worktrees`, so a dirty clone is never touched. The agent runs
without push credentials. `pma` then runs the project's tests itself: set the
command with `pma config projects.cynn.verify "make check"`, or let it be
detected. Limits: `max_parallel`, `batch_budget`, `agent_budget`, `timeout`.
`publish` is `pr` by default; `pma config publish push` pushes to the default
branch instead.

```sh
pma sync                          # plan: issues to open, items to mark done
pma sync --apply cyllama           # carry it out for one project
```

Each `## Critical` item gets an issue labelled `pma:critical`, and `gh:N` is
written into its line. An item whose issue is closed is ticked. TODO.md
edits are left uncommitted; `scripts/commit_todo.py` commits them.

The database is `~/.config/pma/projects.db`, or `$PMA_HOME/projects.db`.
Dates are compared as UTC calendar days.

## Development

```sh
make test     # cargo test, then pytest on scripts/
make check    # fmt check, clippy -D warnings, test
```
