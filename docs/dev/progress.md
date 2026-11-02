# Progress display

Status: design, 2026-10-21. Nothing built.

## Problem

Long commands print nothing while they work. Measured on `pkgdb`, whose check is `make test` (704 tests):

| Command | Silent for | Then prints |
|-|-|-|
| `pma verify pkgdb` | 114 s | one line |
| `pma dispatch pkgdb:deps -a refresh` (#32) | 4 min 16 s: base check 135 s, refresh 1 s, head check 121 s | one line per run |
| the same with the base check cached (#33) | 1 min 55 s | one line per run |

The user cannot tell a working command from a hung one, nor which step it is on. The only progress display is the scan bar (`progress.rs`), a count of finished projects.

## What exists

- `agent::run_limited` runs every long subprocess (an agent, a check, a deps tool, a job) with its output in a log file. It is the one place a long step starts and ends.

- Run logs are `~/.local/state/pma/runs/<id>/agent-<n>.log` and `verify-<n>.log`; a base check's is `verify-base/<project>-<commit>.log`.

- `verify_base` records each check's duration per project, commit and command (`seconds`).

- `watch.rs` reads a running agent's log: a `claude` log's last tool call, or any other log's last line.

- `dispatch::execute` runs up to `max_parallel` runs on threads; only the main thread holds the store, and the threads report over a channel.

- The TUI and the web queue long acts as jobs: a runner process runs the CLI command (`frontends.md`, phase 2). They read jobs and running agents through `Query::Jobs`, as `pma watch` does.

## Decision

Each long step records its phase in a file beside the run's logs. The CLI draws a live line per active run from those files and the logs; `pma watch`, the TUI and the web read the same files. The phase is observed by `pma`, not reported by the worker, as in `async_dispatch.md`.

### What a line shows

```text
pkgdb   verify head  make test  1m12s / ~2m12s  [ 92%] tests/test_service.py ....
margo   agent        claude     0m41s           Edit src/server.go
```

| Field | Source |
|-|-|
| project | the run |
| phase | the step under way; see below |
| what runs | the verify command, or the worker's name |
| elapsed | since the phase began |
| estimate | the last `verify_base.seconds` for the project and command; checks only |
| last activity | a `claude` log's last tool call, or any log's last non-empty line |

Phases: `fetch`, `worktree`, `verify base`, `verify base (cached)`, the worker's name for the agent step, `verify head`, `commit`, `push`, `pr`. A step that runs no subprocess, such as `fetch`, shows its name and elapsed time only.

A percentage bar is not shown: `pma` cannot know how far through its work a test suite or an agent is. The estimate is the last measured duration, which a check repeats closely: `pkgdb` took 114 s, 113 s and 121 s. Agents vary too much for one, so they show elapsed time alone.

### Where the phase lives

`runs/<id>/phase`, one line: the phase, its start time, and the log it writes to. Overwritten at each phase change, through a rename. `pma verify` has no run, so it writes to `verify-base/<project>-<commit>.phase`.

A file over a column on `runs`: the worker threads hold no store, and every phase change would be a write to a database other commands are reading. The logs are files already, and `watch` reads them from the same directory.

### Drawing

- On stderr, only when stderr is a terminal, as the scan bar is. A pipe or a CI log gets the bytes it gets today.

- A thread redraws four times a second: one line per active run, cut to the terminal's width. The width comes from `ratatui::crossterm::terminal::size`, which the `pma` crate reaches through `pma-tui`; `pma-core`, which writes and reads the phase files, stays free of terminal code.

- A run that finishes prints its usual line above the live area, which stays below it.

- With no active run, the area is erased, so the command's own output starts at column 0.

## Rejected

- **Streaming the check's output.** Two parallel suites interleave, and 700 pytest dots bury the phase. Kept as a possible `--follow`, for one run.

- **Only `--detach` and `pma watch`.** The wait moves elsewhere, and `watch` today shows agents only, not checks or git.

- **A worker reporting its own progress.** An agent may not call a progress tool, and a check cannot. `async_dispatch.md` rejected this for the same reasons.

## Open questions

1. When stderr is not a terminal, stay silent, or print one plain line per phase change? Silent is the default here: scripts read the output, and a job's log already holds the step outputs.

2. Should `pma watch` list base checks and `pma verify`, which belong to no run? This design lets it, from the `.phase` files in `verify-base/`.

3. `git fetch` on a large repository can take a minute and writes no log. Is its name and elapsed time enough?

## Plan

1. `progress.rs`: the phase file, written and read; the live area; a phase recorded at each step of `dispatch::prepare`, `dispatch::attempt` and `dispatch::preflight`. `pma verify`, `pma dispatch` and `pma review --rework` draw it. Tests on the line format and on a phase file a run leaves.

2. `pma pr` and `pma push` (commit, push, `gh pr create`), `pma workflow run`, `pma campaign run`.

3. `pma watch`, the TUI's agents tab and the web read the phase files, for foreground and detached work alike.

4. Docs: README, CHANGELOG.
