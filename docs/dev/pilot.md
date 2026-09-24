# Pilot

Phase 0 of [implementation-plan.md](implementation-plan.md): the first shipped run, and the measurements the later phases are gated on. Drafted 2026-09-24.

## Setup

Worker: `claude` on the host, no container. pma runs it with `--permission-mode acceptEdits`, which approves edits and a fixed set of file commands (`mkdir`, `rm`, `mv`, `cp`, `sed`) only for paths inside the working directory, here the worktree ([security docs](https://code.claude.com/docs/en/security), "Working directory boundary" and "Accept Edits mode"). Any other Bash command needs a rule. pma adds one rule for the verify command, and in `-p` mode there is no one to approve the rest. `opencode --auto` and `omp` approve every command, so they are not in this pilot.

Two limits. Read-only Bash commands such as `cat` run without asking, so reads outside the worktree are not bounded. Allow rules in your own Claude Code settings apply to these runs too, and widen the bound.

```sh
pma project tier 1 pma pkgdb margo
pma config projects.pma.verify "make check"
pma config projects.pkgdb.verify "make test"
pma config projects.margo.verify "go test ./pkg/... ./internal/... ./cmd/..."
pma scan
pma verify pma pkgdb margo
```

`pma verify` runs each check where a dispatch would, under the agent's environment. Run it before the first dispatch, and again after any change to a verify command. See [initial_run.md](initial_run.md) for what skipping it cost.

`publish` stays `pr`, so each approved run opens a pull request, and `pma review` settles it once merged or closed. Defaults: `agent_budget` $1.00, `timeout` 30 minutes.

Verify, measured on a fresh clone of each repository in a plain shell. `pma verify` measured `pkgdb` at 113 s and `margo` at 8 s under the agent's environment:

| Project | Command | Result | Time |
|-|-|-|-|
| `pma` | `make check` | passes; needs `uv` for the pytest step | about 50 s, cold build included |
| `pkgdb` | `make test` | 704 passed, 8 skipped | 2 min 6 s |
| `margo` | `go test ./pkg/... ./internal/... ./cmd/...` | passes | about 25 s |

Each run pays for verify three times: at the base, at the head, and on the integrated tree at ship. A base result is cached per commit.

`margo`'s own `make test` runs `go test ./...`, which fails on a fresh checkout: `main.go` embeds `frontend/dist`, which is built, not tracked. The command above leaves out the root package (`main.go`, `app.go`), so a change there is not verified. The cohort avoids such items.

## Cohort

Every item is class B: `Class::of` gives B to an item with no class tag, and `#agent` marks eligibility only. Difficulty is a draft estimate from 1 (trivial) to 5 (a design task). Correct it before the first dispatch. The plan requires the estimate to exist before the outcome is known.

| # | Project | Item | Detail | Difficulty | Run | 1st pass | Accepted | Merged | Review min |
|-|-|-|-|-|-|-|-|-|-|
| 1 | pma | `pma sync` checks an item's text before writing `gh:N` | described | 2 | | | | | |
| 2 | pma | `pma lint` reports an unclosed code fence | described | 3 | | | | | |
| 3 | pma | `pma scan` bounds each git and gh call with a timeout | described | 3 | | | | | |
| 4 | pma | `##Critical` without a space is a lint error | one-liner | 1 | | | | | |
| 5 | pma | a dotted project name can take `projects.<name>.*` | one-liner | 2 | | | | | |
| 6 | pma | `review 3 4 --approve --minutes 5` drops the minutes | one-liner | 2 | | | | | |
| 7 | pma | `pma config <retired key>` says unknown | one-liner | 1 | | | | | |
| 8 | pma | `project import` refuses a CSV with a byte-order mark | one-liner | 1 | | | | | |
| 9 | pma | a PR closed unmerged does not count as an attempt | one-liner | 2 | | | | | |
| 10 | pma | `dispatch --retry` resets the count before the run starts | one-liner | 2 | | | | | |
| 11 | pkgdb | backup/restore | one-liner | 3 | | | | | |
| 12 | pkgdb | import packages from `pyproject.toml` | one-liner | 2 | | | | | |
| 13 | pkgdb | report CI failures as `pkgdb check` events | one-liner | 3 | | | | | |
| 14 | pkgdb | track packages you don't own | one-liner | 3 | | | | | |
| 15 | pkgdb | auto-discover packages from your repos | one-liner | 3 | | | | | |
| 16 | margo | 11.5 untested packages | described | 2 | | | | | |
| 17 | margo | 10.4 Ollama / local model support | described | 3 | | | | | |
| 18 | margo | 10.3 OpenRouter live model fetch | one-liner | 3 | | | | | |
| 19 | margo | 10.2 per-workspace MCP server scoping | one-liner | 5 | | | | | |

19 items rather than 20: the rest of `pkgdb` and `margo` failed the exclusions below.

Confounds, stated so the result is not over-read:

- Detail tracks the repository. `pkgdb`'s items are all one-liners; `pma`'s descriptions were written from a code review, with file and function names.

- `margo`'s one-liners have full entries in `docs/dev/notes.md` under the same number. An agent may find them. Record whether it did; if so, the item was effectively described.

- 19 tasks over three repositories give a pilot reading, not a calibrated rate.

## Excluded

| Items | Why |
|-|-|
| `pkgdb`: spikes and drops, milestones, groups/tags, CI status column, server/API mode | already built (`checks.py`, `server.py`, `reports.py`) but still open in `TODO.md`. Tick or remove them. |
| `pkgdb`: publish to GitHub Pages | publishing is class D work |
| `margo`: 10.6, 10.19, 10.21, 10.22, 10.25, 10.27, 11.1, 11.7 | frontend or root package; verify does not compile them |
| `margo`: 10.26 | moving `internal/config` can break the root package, which verify cannot see |
| `margo`: 11.2, 11.3, 11.4 | need a product decision or external data |
| `margo`: 10.23 | CI files are outside class B's scope |

## Procedure

1. Commit and push `TODO.md` in all three repositories. Dispatch refuses an item that is not open on origin.

2. Run the setup above.

3. Dispatch one item at a time: `pma dispatch pkgdb` opens a list to pick from.

4. Read `pma review <id>`: task, both verify results, scope, cost and diff. Time only the reading and deciding.

5. Decide: `pma review <id> --approve --minutes N`, `--rework "<feedback>" --minutes N`, or `--reject --minutes N`. Write one line below for every rework and rejection.

6. `pma ship`, then merge or close the pull request on GitHub. The next `pma review` settles it.

7. Fill the table from `pma review <id>`. `pma report --by project` totals it.

pma records attempts, cost, both verify results, changed paths and review time. The table and the notes below hold what it cannot know.

## Notes per run

(one line per rework or rejection: run id, why)

## Gate

From the plan: accepted share below about 40%, or review above 10 minutes per run, moves the work to specification quality before any automation. Report first-attempt passes, acceptance after rework, and merges as three numbers. An open pull request counts in no numerator.
