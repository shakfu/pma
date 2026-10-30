# pma

Maintain many repositories from one place.

If you keep dozens of small projects, the work is not hard, it is scattered. Each repository has a `TODO.md`, failing CI, outdated dependencies and a dirty working tree, and finding out means opening all of them. `pma` reads them all in one pass, ranks every task across every project in one list, and can hand a task to a coding agent in your clone, or in a throwaway worktree. You read the diff, and `pma` commits it and opens a pull request or pushes.

Scanning 95 repositories takes about 15 seconds.

Nothing is published without you. An agent runs with no push credentials in its environment, `pma` runs the project's tests itself rather than trusting the agent's report, and every change waits unstaged until you approve it.

```sh
pma root add ~/projects        # git repos directly under it are projects
pma project tier 1 myproject   # 1 is most important, 5 least
pma scan                       # TODO.md, git state, CI
pma                            # next: tasks for agents, and what waits on you
pma run review myproject       # a review, confirmed, findings printed
pma run fix-ci myproject       # fix failing CI in the clone; the edit waits for review
pma dispatch myproject:31      # hand line 31 to an agent
pma review 1                   # the diff, the test result, the cost
pma stage 1                    # into your clone's index; `git diff --cached`
pma push 1                     # commit and push it; or `pma pr 1` after --approve
```

## Features

**One view of everything**

- `pma next`, or `pma` alone: one list of tasks for agents, in the order `pma dispatch --auto` takes them, and one of what waits on you: runs to review, publish or merge, then critical or urgent tasks no agent may take.

- Every task in every project in one Eisenhower matrix, ranked by project importance and item priority.

- Failing CI, outdated dependencies, idle repositories and dirty working trees appear as tasks beside the written ones.

- Two front ends with the same views and actions: `pma tui` in a terminal, over SSH too, and `pma web` in a local browser, on 127.0.0.1 behind a one-time token. Both act on what is selected: dispatch a task, approve or reject a run, approve a plan, stop a job. Space marks tasks, and `d` dispatches the marked ones, one run per project. `P` in a project view dispatches a prompt you type.

- A view of each project: its state and why, CI, dependencies, local changes, open tasks and every run, with how old the scan behind it is. `pma project show <name>`, or `o` on any row.

- Projects worst first by what needs attention (`pma status`), and an oldest-first list to prune (`pma stale`).

- Each scan says what changed since the one before: items opened and closed, CI failing or fixed, a fault found or mended, projects new. `pma history` keeps the last 100 scans, and `pma status --html` writes the view as one page.

- Portfolio notes that belong to no single project.

**Tasks in plain Markdown**

- A root `TODO.md` per repository is the only task file. Nothing to keep in step, and no lock-in: remove `pma` and the files still read.

- `pma lint` checks the format; `pma lint --fix --apply` rewrites the mechanical errors. `pma prune` removes what is finished.

- `pma sync` mirrors `Critical` items to GitHub Issues and ticks items whose issue was closed.

**Agents, with the brakes on**

- `claude`, `opencode` and `omp` come as templates. Any coding agent with a headless mode is a record you edit, not a code change.

- `sanduk` is the same agent in a disposable container: the key stays on the host, the worktree is mounted at its own path, and the box is the bound. Needs [sanduk](https://github.com/shakfu/sanduk-rs) (`cargo install sanduk`) and a container engine; `pma agent show sanduk` prints what it runs.

- By default an agent edits your clone, from the working tree as it is, and its change waits there unstaged: `git diff` shows it, `pma review` approves it, and a rejection reverts it. Your own files are left alone. One run per project at a time. `pma config workspace worktree`, or `--worktree` on one command, gives each run a worktree of the remote default branch instead, so your checkout is never touched and runs can share a project.

- The agent's environment holds no push credentials: tokens, the SSH agent, askpass helpers and the control sockets of terminal multiplexers (herdr, tmux, screen, zellij, kitty, WezTerm, Neovim) are removed, and git's pushes are redirected. Credentials on disk, such as `~/.ssh` keys or `~/.git-credentials`, stay readable, and a terminal scripted through AppleScript (Ghostty, iTerm2, Terminal.app) has no variable to remove; this stops an accidental push, not a determined one. `pma` runs the project's test command itself, before and after, and reports both ends.

- With `pma config projects.<name>.sandbox on`, that test command runs able to write only to the worktree, the temp directories and the toolchain caches: Landlock on Linux 6.2 and later, Seatbelt on macOS. Reads and the network stay open. Off by default: a suite that writes elsewhere fails under it for no fault of the agent's.

- A hook or git setting added to the repository during a run fails the run, and `pma` runs no git in that repository until it is restored, or until `pma review <id> --reject --keep-git-changes` accepts it as yours.

- Every run records the diff, the test result, the cost, the duration and the paths it touched. A run that changed a file its task had no business changing is flagged.

- Limits you set: agents at once, dollars per batch, minutes per run. The per-run dollar figure admits runs into a batch and flags a run that cost more; no shipped worker stops at it.

- `pma report` says what dispatching has produced: first-attempt passes, reworks, merges, cost, review time.

- An agent drafts a changelog entry per change in `.pma/changelog.md`, not in `CHANGELOG.md`, so changes running together do not conflict; `pma review` shows the draft, or says there is none.

**Running unattended**

- `--detach` on `pma dispatch` and `pma workflow run` queues the work and returns at once; a runner takes it and outlives the terminal. `pma job` lists jobs, `pma watch` shows what each running agent is doing, and `pma config notify '<command>'` runs when a job ends.

- `pma workflow run --local --unattended` approves each plan as it comes and leaves every edit that passed the project's tests as unstaged changes in your clone; one that failed is discarded. Nothing is committed or pushed.

**Which model, where**

- A preset names a worker, a model and the arguments that configure it: `pma preset set omp-luna-high omp gpt-5.6-luna --thinking high`.

- `pma` runs agents and is not an API client. Provider keys stay the agent's, reached through its own configuration, and `pma` stores none of them.

- A routing policy decides the worker, the model and how much autonomy per kind of task. It is a document you propose, activate, and can replay over past runs to see what it would have changed.

**Repeating work**

- A campaign applies one task definition across many repositories, each verified on its own.

- A workflow is a graph of agents over a project: review, then validate the review, then fix what it confirmed. Written as JSON or as a script, and costed before it runs. `pma workflow library` prints nine ready-made ones: reviews, issue triage, specification, a `TODO.md` audit and a documentation check.

- `pma workflow run` prints a plan: each agent node it would run, over which units, with the worker, the model and a ceiling. Nothing is spent until you approve that plan by its id, and an approval runs that plan and nothing else. Its target is `dispatch`'s: a project, a `TODO.md` line, a heading or a signal, whichever type the workflow reads.

- One run of a workflow is an instance. It is frozen at the revision it started under, so activating a new one does not change work already under way, and `--instance <id>` resumes it where the last pass stopped. A node runs once every node before it has finished, so a join sees every branch. A node that edits a repository leaves a run in `pma review` like any other.

- Each node can run on its own worker and model: a route that names the node, such as `{"match": {"node": "*/review"}, "model": "haiku", "approval": "each"}`, picks them.

## Install

```sh
cargo install pma
```

From a clone: `make install`, which builds the release binary and copies it to `~/.local/bin`; set `PREFIX` to install elsewhere: `make install PREFIX=/usr/local`. Or `cargo install --path crates/pma`. `--no-default-features` builds without `pma web` and its HTTP server.

Requirements:

- macOS or Linux. Rust 1.89 or later, and a C compiler: SQLite is compiled in.

- `git` on `PATH`.

- A coding agent for `pma dispatch`: `claude`, `opencode` or `omp`, logged in or holding its own provider key.

- `gh`, authenticated, for CI status, for opening pull requests and for `pma sync`. Without it CI reads as unknown, and `pma scan --offline` skips GitHub on purpose.

What is supported:

- **Forge: GitHub.** CI status, `pma pr` and `pma sync` need a GitHub remote. A repository elsewhere is still scanned, and dispatched to with `stage`, `commit` or `push`; its CI reads as unknown.

- **Dependencies: Cargo, uv and Go modules.** `pma scan --deps` and the `refresh` worker read `Cargo.lock`, `uv.lock` and `go.mod`. Other projects show no deps state.

- **Test command detection:** a `test:` target in the Makefile, then `cargo test`, `go test ./...`, `npm test` when `package.json` has a `test` script, then `uv run pytest` with a `uv.lock` or `python3 -m pytest` without one. For anything else, or a Python project that needs an environment, set `pma config projects.<name>.verify "<command>"` and check it with `pma verify <name>`.

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

- Trailing tokens, read from the end of the line: `#tag`, `due:YYYY-MM-DD`, `gh:N` for a linked issue, and an optional id, `^k3f9q`: five characters of `0-9` and `a-z` without `i`, `l`, `o` or `u`. A token-shaped word earlier in the line is text, and so is a caret word that is not an id, such as `^1.2`.

- `#urgent` makes an item urgent whatever its date. `#agent` marks one an agent may take. `#manual` keeps agents off it entirely.

- A finished item is ticked where it stands. `pma prune` removes it.

Two open items with the same text, or two items with the same `gh:N` or id, are errors. An item is identified by its id, else its issue number, else its text. Ids are optional and `pma` writes them: `pma lint --ids --apply` gives one to every open item, and an item a workflow adds gets one. With an id, rewording an item keeps its age and any run it has.

## Commands

Projects and tasks:

```sh
pma lint                        # ./TODO.md; a directory means its TODO.md
pma lint ~/projects/*/          # every project
pma prune ~/projects/*/         # what would be removed; --apply to do it
pma root add ~/projects/        # also `pma root` and `pma root rm`
pma project                     # every project: tier, tags, last scanned
pma project tier 1 alpha beta   # or `none`; untiered projects rank at default_tier, 5
pma project export tiers.csv    # every project's tier and tags, to edit
pma project import tiers.csv --apply   # read them back
pma project tag add rust myproject     # private groups; --tag <t> elsewhere
pma project forget gone         # an absent project's record
pma project clone --tag work    # check out absent projects again; --apply to do it
pma scan                        # TODO.md, git state, CI via gh
pma scan --offline myproject    # one project, without GitHub
pma scan --deps                 # also measure how far each cargo, uv and go lock can move
pma next                        # for agents and for you; --all for every row
pma matrix                      # the ranked 2x2; -q q1 for one quadrant
pma status --explain            # projects worst first, with what placed each
pma status --all                # untiered projects too, when default_tier is never
pma status --all --html s.html  # the same, with the last scan's changes, as one HTML page
pma history                     # past scans; `pma history last` for what the latest changed
pma stale                       # open items by age, oldest first
pma task myproject:12           # one task in full; Enter on it in the TUI and web
pma tui                         # every view and action; 1-8 switch tabs, ? lists keys, q quits
pma web                         # the same in a browser, on 127.0.0.1 behind a one-time token
pma web --app                   # as an app window, in Chrome or Chromium
pma project show myproject      # one project: state, open tasks, runs and jobs; `o` in the TUI and web
pma web --no-open --port 9000   # for `ssh -L 9000:127.0.0.1:9000 host` from another machine
pma note add "move CI to a reusable workflow"
```

`next`, `matrix`, `status`, `history` and `tui` read the last scan. They do not rescan.

Agents, models and presets:

```sh
pma agent                       # the workers; `pma agent show claude` for one
pma preset set claude-haiku claude haiku
pma preset set omp-luna-high omp gpt-5.6-luna --thinking high
pma preset                      # every named combination; * is the default
pma preset use omp-luna-high
pma config                      # every setting; `pma config <key> <value>` sets one
pma verify myproject            # run its check where a dispatch would, before one does
```

Highest wins: the `-a` and `-m` flags, then `-p <preset>`, then an applied route, then the default preset.

Dispatch, review, publish:

```sh
pma dispatch myproject:31       # a TODO.md line from the last scan
pma dispatch myproject:ci       # failing CI; or :deps for dependencies
pma dispatch myproject:critical # every open item under that heading, as one run
pma dispatch myproject:31 myproject:40  # one run over both; --separate for one each
pma dispatch myproject          # pick from a list
pma dispatch myproject --prompt "audit error handling"  # your own prompt, no item
pma dispatch --auto -n 3        # the top 3 dispatchable tasks
pma dispatch -p claude-haiku myproject:31
pma review                      # runs not yet published or rejected
pma review 4                    # task, test result, cost, summary, diff
pma review 4 --approve          # or --reject, or --rework "feedback"
pma review 4 --approve --drop 3 # a bundle: leave its item 3 open
pma review 4 --reject --infra   # the check or machine failed, not the agent
pma review 4 5 6 --approve      # a batch; every run is checked before any is approved
pma stage 4                     # into your clone's index, item ticked; no approval needed
pma commit 4                    # stage if not yet staged, then commit in your clone; --subject=<text>
pma push 4                      # commit if not yet committed, then push the commit
pma pr 4                        # or commit in the worktree and open a pull request
pma push --all-approved         # every approved run; also on stage, commit and pr
pma report --by project         # also class or agent
```

A target naming one task fails if that task cannot run. A target naming many passes over each with its reason, and dispatches the rest.

The `TODO.md` items named in one project are one run, a bundle; `--separate` gives each its own. Its rules: `docs/dev/bundled-dispatch.md`.

A run edits the clone, or with `pma config workspace worktree` a worktree of the remote default branch under `~/.local/state/pma/worktrees`, on a `pma/` branch; in a worktree, the item must be open in the remote `TODO.md`, so commit and push before dispatching. Set the test command with `pma config projects.myproject.verify "make check"`, or let it be detected. `pma config projects.myproject.sandbox on` confines what the check may write. `docs/dev/clone-runs.md` has the clone run's rules.

In a project with a `CHANGELOG.md`, the agent leaves that file alone and drafts one entry per separable change. `pma review <id>` shows the entries, or says none was drafted.

`stage`, `commit` and `push` work in your clone, and each runs the steps before it that the run has not done. The clone must be at the run's base, the commit its check ran on; one that has moved is refused, and `pma pr` remains. `stage` refuses a clone with staged changes. `commit` and `push` need `--approve` for a run not yet staged. `push` pushes the run's commit only. Design: `docs/dev/stage-commit-push.md`.

A run opened with `pma pr` is `pr-open` until the pull request is merged or closed; `pma review` settles it and shows review activity meanwhile. `pma merge <id>` merges it, or `M` in the TUI and web. If it conflicts, `pma merge <id> --fix` or `F` has an agent resolve the conflicts and returns the run to review; `pma pr` then updates the same pull request, and `stage`, `commit` and `push` refuse it. Rejecting the fix returns the run to `pr-open`.

Stale dependencies:

```sh
pma scan --deps                          # measure every lock; at least once per deps.days
pma status --explain                     # stale deps  153 to move, lock 51d old, ...
pma dispatch a:deps b:deps -a refresh    # move each lock in a worktree; no agent
pma review 4                             # the lock diff and the check's verdict
pma stage 4                              # then `pma push 4`, or `pma pr 4`
pma review 5 --reject                    # a refresh the check refused
pma dispatch b:deps -a claude            # the agent gets the check's output
```

A project's deps are stale when a `--deps` measurement no older than `deps.days` (30) found that a lock can move within its declared constraints, and that lock has not changed for `deps.days`. An older measurement places the project in no deps state, so run `pma scan --deps` at least that often.

The `refresh` worker runs `pma deps refresh`: `cargo update`, `uv lock --upgrade`, or `go get -u ./...` with `go mod tidy`. Under `>=` constraints, `uv lock --upgrade` takes new major versions; the project's check decides whether the result is kept. Reject a refused refresh before another worker takes the task. The next agent's prompt then carries the check's output. A refresh uses none of the agent's two attempts and none of `batch_budget`. The worker runs the `pma` on your `PATH`, so install the build that has `pma deps refresh`.

For `pma dispatch --auto` to send deps to `refresh` and everything else to your default, propose and activate a policy:

```json
{"route": [
  {"name": "locks", "match": {"class": "A"}, "agent": "refresh", "approval": "each"},
  {"name": "rest", "approval": "each"}
]}
```

Class `A` is mechanical work, which is the deps task alone. An applied policy refuses any workflow node that no route names, so add a `node` route for each workflow you run. `pma workflow run` warns of each node it would refuse, and `pma route replay <file>` shows what the policy changes over the runs already recorded. Design and rationale: [docs/dev/deps-refresh.md](docs/dev/deps-refresh.md).

Repeating work:

```sh
pma campaign add ci-workflow "add a CI workflow" --projects a --projects b
pma campaign run ci-workflow    # one worktree per repository, verified apart
pma route propose routing.json  # worker, model and autonomy per kind of task
pma route activate 2            # --shadow records the decision without applying it
pma route replay routing.json   # what a candidate would have done differently
pma workflow library | pma workflow propose -   # the shipped library, as a draft
pma workflow run todo-audit myproject           # open items the code already does, as notes
pma workflow check lib.rhai     # read a document and print its worst case
pma workflow propose lib.rhai   # store it as a draft revision
pma workflow activate 1         # refused if its worst case is over budget
pma workflow run review myproject --dry-run   # plan and price, starting nothing
pma workflow run review myproject -p claude-haiku
pma workflow run triage myproject:critical --set severity=high
pma workflow run triage --instance 3 --approve 3fa2c1e09b7d   # the plan it printed
pma workflow run review-fix myproject --local --unattended   # no approvals; verified fixes left unstaged
pma workflow run review-fix myproject --local --unattended --detach   # returns at once
pma job                         # queued, running and finished jobs; `job log <id>`, `job stop <id>`
pma job approve 3               # run the plan a detached job stopped at
pma watch                       # each running agent's tool calls and last words
pma config notify 'notify-send pma "$PMA_JOB_SUMMARY"'   # told when a job ends
# design and known gaps: docs/dev/async_dispatch.md
pma workflow run triage --instance 3 --cap max_units=80   # resume a capped instance
pma workflow stop 3             # no further node; its runs stand
pma workflow                    # revisions, and every instance with its state
```

A pass runs every node a rule decides, then stops and prints a plan for the agent nodes that are ready: as many runs as `batch_budget` admits at `agent_budget` each. `--approve <plan>` runs exactly that plan, `max_parallel` agents at a time, then stops at the next plan. A plan that changed since it was printed has another id and is refused. A unit a node could not handle is named on stderr and goes no further than a default edge; a check waiting on a pull request keeps its unit until the next pass.

GitHub Issues:

```sh
pma sync                        # what would change
pma sync --apply myproject      # carry it out
```

Each `## Critical` item gets an issue labelled `pma:critical`, and `gh:N` is written into its line. An item whose issue is closed is ticked. Edits to `TODO.md` are left uncommitted, for you to read first.

## Where things live

- The database is `~/.config/pma/projects.db`, or `$PMA_HOME/projects.db`. It serves one machine: two copies cannot be merged.

- Worktrees, logs and artifacts sit under `~/.local/state/pma` (`$XDG_STATE_HOME/pma`), or `$PMA_HOME/state` when `PMA_HOME` is set; `PMA_STATE` overrides both. Worktrees are removed when a run is published or rejected. A workflow's agent nodes read the code in a worktree of their own and leave nothing behind; what they read and wrote stays in `artifacts/<instance>/<node>/<n>/`, numbered per run.

- Settings live in `~/.config/pma/settings.toml`, beside the database: `config` keys, roots, workers, presets, and each project's tier and tags. Edit it by hand, or with `pma config <key> <value>`, `pma root`, `pma project`, `pma agent` and `pma preset`, which keep its comments. `pma config` lists the settings and `--reset` clears one. An invalid file stops every command and names the key. Workflow and route revisions stay in the database, because activating one checks its cost and records who did.

One command that runs agents or edits worktrees runs at a time. `dispatch`, `stage`, `commit`, `push`, `pr`, `workflow run` and `review --reject` take a lock and name the process holding it. Reading commands run alongside.

## Development

```sh
make test     # cargo test, then pytest on scripts/
make check    # format check, clippy with warnings as errors, then test
```

The code is a cargo workspace. `crates/pma-core` holds the store, scanning, dispatch, workflows and the `api` module every front end uses; it prints nothing. `crates/pma-tui` is the terminal UI, and `crates/pma` the binary with the CLI. Design: `docs/dev/frontends.md`.

## Licence

MIT.
