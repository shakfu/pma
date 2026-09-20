# pma

Maintain many repositories from one place.

If you keep dozens of small projects, the work is not hard, it is scattered. Each repository has a `TODO.md`, failing CI, outdated dependencies and a dirty working tree, and finding out means opening all of them. `pma` reads them all in one pass, ranks every task across every project in one list, and can hand a task to a coding agent in a throwaway worktree. You read the diff, and `pma` commits and publishes it.

Scanning 95 repositories takes about 15 seconds.

Nothing is published without you. An agent runs with no push credentials, `pma` runs the project's tests itself rather than trusting the agent's report, and every change waits in a worktree until you approve it.

```sh
pma root add ~/projects        # git repos directly under it are projects
pma project tier myproject 1   # 1 is most important, 5 least
pma scan                       # TODO.md, git state, CI
pma matrix                     # every project's tasks, ranked, in one view
pma dispatch myproject:31      # hand line 31 to an agent
pma review 1                   # the diff, the test result, the cost
pma review 1 --approve
pma ship                       # commit, push or open a pull request
```

## Features

**One view of everything**

- Every task in every project in one Eisenhower matrix, ranked by project importance and item priority.

- Failing CI, outdated dependencies, idle repositories and dirty working trees appear as tasks beside the written ones.

- A terminal browser (`pma tui`), a per-project health ranking (`pma status`), and an oldest-first list to prune (`pma stale`).

- Portfolio notes that belong to no single project.

**Tasks in plain Markdown**

- A root `TODO.md` per repository is the only task file. Nothing to keep in step, and no lock-in: remove `pma` and the files still read.

- `pma lint` checks the format. `pma prune` removes what is finished.

- `pma sync` mirrors `Critical` items to GitHub Issues and ticks items whose issue was closed.

**Agents, with the brakes on**

- `claude`, `opencode` and `omp` come as templates. Any coding agent with a headless mode is a record you edit, not a code change.

- Each run gets its own worktree of the remote default branch, so your dirty checkout is never touched.

- The agent runs without push credentials. `pma` runs the project's test command itself, before and after, and reports both ends.

- Every run records the diff, the test result, the cost, the duration and the paths it touched. A run that changed a file its task had no business changing is flagged.

- Limits you set: agents at once, dollars per run, dollars per batch, minutes per run.

- `pma report` says what dispatching has produced: first-attempt passes, reworks, merges, cost, review time.

**Which model, where**

- A preset names a worker, a model and the arguments that configure it: `pma preset set omp-luna-high omp gpt-5.6-luna --thinking high`.

- `pma` runs agents and is not an API client. Provider keys stay the agent's, reached through its own configuration, and `pma` stores none of them.

- A routing policy decides the worker, the model and how much autonomy per kind of task. It is a document you propose, activate, and can replay over past runs to see what it would have changed.

**Repeating work**

- A campaign applies one task definition across many repositories, each verified on its own.

- A workflow is a graph of agents over a project: review, then validate the review, then fix what it confirmed. Written as JSON or as a script, and costed before it runs.

- `pma workflow run` prints what it would run, the worker, the model and a ceiling, and spends nothing until you approve it.

## Install

```sh
cargo install pma
```

From a clone: `make install`, which builds the release binary and copies it to
`~/.local/bin`. Set `PREFIX` to install elsewhere: `make install PREFIX=/usr/local`.

Requirements:

- macOS or Linux. Rust 1.89 or later, and a C compiler: SQLite is compiled in.

- `git` on `PATH`.

- A coding agent for `pma dispatch`: `claude`, `opencode` or `omp`, logged in or holding its own provider key.

- `gh`, authenticated, for CI status, for opening pull requests and for `pma sync`. Without it CI reads as unknown, and `pma scan --offline` skips GitHub on purpose.

## TODO.md format

```markdown
# TODO

## Critical

- [ ] segfault on empty input #bug gh:42

## High

- [ ] support ggml 0.9 due:2026-10-01

## Medium

### parser

- [ ] accept a trailing comma
  the fixture in tests/bad.json covers it

## Low

- [x] drop python 3.9
```

The rules, which `pma lint` checks:

- The first non-blank line is `# TODO`, and there is no second `#` heading.

- Priority comes from the enclosing `##` section: `Critical`, `High`, `Medium` or `Low`, spelled exactly, at most one of each. Other `##` sections are allowed and ignored.

- An item is one line, `- [ ] text` or `- [x] text`. No other spelling of the checkbox.

- Indented lines under an item are its description, carried to the agent verbatim.

- A `###` heading groups the items below it, until the next heading.

- Trailing tokens, read from the end of the line: `#tag`, `due:YYYY-MM-DD`, and `gh:N` for a linked issue. A token-shaped word earlier in the line is text.

- `#urgent` makes an item urgent whatever its date. `#agent` marks one an agent may take. `#manual` keeps agents off it entirely.

- A finished item is ticked where it stands. `pma prune` removes it.

Two open items with the same text, or two items with the same `gh:N`, are errors. An item is identified by its text until an issue number is written into it.

## Commands

Projects and tasks:

```sh
pma lint                        # ./TODO.md; a directory means its TODO.md
pma lint ~/projects/*/          # every project
pma prune ~/projects/*/         # what would be removed; --apply to do it
pma root add ~/projects/        # also `pma root` and `pma root rm`
pma project                     # every project: tier, tags, last scanned
pma project tier myproject 1    # or `none`; untiered projects are not ranked
pma project tag add rust myproject     # private groups; --tag <t> elsewhere
pma project forget gone         # an absent project's record
pma scan                        # TODO.md, git state, CI via gh
pma scan --offline myproject    # one project, without GitHub
pma scan --deps                 # also count outdated cargo, uv and go deps
pma matrix                      # the ranked view; -q q1 for one quadrant
pma status --explain            # projects by health, with each signal's share
pma stale                       # open items by age, oldest first
pma tui                         # browse the matrix; q quits
pma note add "move CI to a reusable workflow"
```

`matrix`, `status` and `tui` read the last scan. They do not rescan.

Agents, models and presets:

```sh
pma agent                       # the workers; `pma agent show claude` for one
pma preset set claude-haiku claude haiku
pma preset set omp-luna-high omp gpt-5.6-luna --thinking high
pma preset                      # every named combination; * is the default
pma preset use omp-luna-high
pma config                      # every setting; `pma config <key> <value>` sets one
```

Highest wins: the `-a` and `-m` flags, then `-p <preset>`, then an applied route, then the default preset.

Dispatch, review, ship:

```sh
pma dispatch myproject:31       # a TODO.md line from the last scan
pma dispatch myproject:ci       # failing CI; or :deps for dependencies
pma dispatch myproject:critical # every open item under that heading
pma dispatch myproject          # pick from a list
pma dispatch --auto -n 3        # the top 3 dispatchable tasks
pma dispatch -p claude-haiku myproject:31
pma review                      # runs not yet shipped or rejected
pma review 4                    # task, test result, cost, summary, diff
pma review 4 --approve          # or --reject, or --rework "feedback"
pma review 4 5 6 --approve      # a batch; every run is checked before any is approved
pma ship                        # commit, push or open a PR, remove worktrees
pma report --by project         # also class or agent
```

A target naming one task fails if that task cannot run. A target naming many passes over each with its reason, and dispatches the rest.

Each run gets a worktree of the remote default branch under `~/.config/pma/worktrees`, on a `pma/` branch. The item must be open in the remote `TODO.md`, so commit and push before dispatching. Set the test command with `pma config projects.myproject.verify "make check"`, or let it be detected. `publish` is `pr` by default; `pma config publish push` pushes to the default branch instead.

Repeating work:

```sh
pma campaign add ci-workflow "add a CI workflow" --projects a --projects b
pma campaign run ci-workflow    # one worktree per repository, verified apart
pma route propose routing.json  # worker, model and autonomy per kind of task
pma route activate 2            # --shadow records the decision without applying it
pma route replay routing.json   # what a candidate would have done differently
pma workflow check lib.rhai     # read a document and print its worst case
pma workflow propose lib.rhai   # store it as a draft revision
pma workflow activate 1         # refused if its worst case is over budget
pma workflow run review myproject --dry-run
pma workflow run review myproject -p claude-haiku
```

GitHub Issues:

```sh
pma sync                        # what would change
pma sync --apply myproject      # carry it out
```

Each `## Critical` item gets an issue labelled `pma:critical`, and `gh:N` is written into its line. An item whose issue is closed is ticked. Edits to `TODO.md` are left uncommitted, for you to read first.

## Where things live

- The database is `~/.config/pma/projects.db`, or `$PMA_HOME/projects.db`. That directory can be a git repository, which is how two machines share it.

- Worktrees sit under the same directory and are removed when a run ships or is rejected.

- Settings live in the database rather than a file. `pma config` lists them, `pma config <key> <value>` sets one, and `--reset` clears one.

One command that runs agents or edits worktrees runs at a time. `dispatch`, `ship`, `workflow run` and `review --reject` take a lock and name the process holding it. Reading commands run alongside.

## Development

```sh
make test     # cargo test, then pytest on scripts/
make check    # format check, clippy with warnings as errors, then test
```

## Licence

MIT.
