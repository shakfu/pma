# pma cheatsheet

Assumes the `claude` worker at its default model. Workflow and reasoning: [guide.md](guide.md). Full reference: [README](../README.md).

## The daily loop

```sh
pma                       # what agents can take, and what waits on you
pma dispatch proj:31      # one TODO.md line to an agent
pma review                # runs awaiting a decision
pma review 4              # task, test result before/after, cost, denials, diff
pma stage 4               # into the clone's index; `git diff --cached`
pma push 4 --approve      # approve, commit and push in one step
```

## Setup, once

```sh
pma root add ~/projects                 # git repos directly under it
pma project tier 1 alpha beta           # 1 most important; untiered = 5
pma project tag add rust alpha beta     # groups for --tag, `#`, `O`
pma scan                                # TODO.md, git, CI; rarely needed again
pma scan --deps                         # lock staleness; every <= 30 days
pma config projects.alpha.verify "make check"   # when detection is wrong
pma verify alpha                        # run the check where a dispatch would
pma preset set claude-haiku claude haiku   # cheap option for mechanical work
```

## Targets

| Target | Meaning |
|-|-|
| `proj:31` | `TODO.md` line 31 at the last scan |
| `proj:31 proj:40` | one run over both (a bundle); `--separate` for two |
| `proj:critical` | every open item under `## Critical`, one run |
| `proj:q1` .. `proj:q4` | one Eisenhower quadrant |
| `proj:ci` | failing CI |
| `proj:deps` | stale dependency locks |
| `proj` | pick from a list |
| `proj --prompt "..."` | free prompt, no item ticked |
| `--auto -n 3` | top 3 eligible: `ci`, `deps`, items tagged `#agent` |

## Dispatch flags

| Flag | Effect |
|-|-|
| `-m haiku` / `-p claude-haiku` | model or preset for this run |
| `--worktree` | edit a worktree of the remote default branch, not the clone |
| `--detach` | queue and return; follow with `pma job`, `pma watch` |
| `--push` | push each run whose gates are clean; others wait in review |
| `--retry` | reset used attempts on a target and dispatch again |
| `--interactive` | drive `claude` yourself in a herdr pane |

Precedence: `-a`/`-m` > `-p` > active route > default preset.

## Deciding on a run

| Situation | Command | TUI |
|-|-|-|
| Good | `pma review 4 --approve` | `a` |
| Good, several | `pma review 4 5 6 --approve` | |
| Every run with clean gates | | `b` |
| Close, needs a change | `pma review 4 --rework "use the existing parser"` | `w` |
| Wrong | `pma review 4 --reject` | `x` |
| Failed; try again unchanged | `pma review 4 --retry` | `R` |
| Check or machine broke | `pma review 4 --reject --infra` | |
| Already landed / clone moved | `pma close 4` | `X` |
| Bundle, item 3 not done | `pma review 4 --approve --drop 3` | `a` asks |

## Publishing

Each step runs the ones before it that the run has not done.

| Command | Result | TUI |
|-|-|-|
| `pma stage 4` | in the clone's index, item ticked | `s` |
| `pma commit 4 --subject="..."` | committed in the clone | `m` |
| `pma push 4` | run's commit pushed | `u` |
| `pma push --all-approved` | every approved run pushed | `U` |
| `pma pr 4` | pull request from a branch | `p` |
| `pma merge 4` / `--fix` | merge the PR / agent resolves conflicts | `M` / `F` |

## One-shot jobs: `pma run`

```sh
pma run                             # list names
pma run review alpha                # review + confirm findings (opus)
pma run security-review alpha
pma run test-gaps alpha
pma run doc-drift alpha
pma run todo-audit alpha            # open items the code already does
pma run fix-ci alpha                # verified edit lands unstaged in the clone
pma run deps-upgrade --tag rust --dry-run   # bulk: plan first, then --yes
pma run review alpha -w claude:sonnet       # override the model
```

## Unattended

```sh
pma dispatch --auto -n 3 --detach
pma job                             # queued, running, finished
pma job log 7 ; pma job stop 7
pma watch                           # each agent's tool calls, live
pma config notify 'osascript -e "on run a" -e "display notification (item 1 of a)" -e "end run" "$PMA_JOB_SUMMARY"'
pma workflow run review-fix alpha --local --unattended --detach
```

## Settings worth knowing

| Key | Default | Note |
|-|-|-|
| `workspace` | `clone` | `worktree` for parallel runs per project |
| `max_parallel` | 2 | agents at once |
| `agent_budget` | 1 | USD per run; admits and flags, see guide |
| `batch_budget` | 5 | USD per batch |
| `timeout` | 30 | minutes per run |
| `scan.every` | 30 | minutes between full scans in TUI/web |
| `default_tier` | 5 | rank of untiered projects |
| `projects.<p>.verify` | detected | the test command `pma` trusts |
| `projects.<p>.sandbox` | off | confine the check's writes |

`pma config <key> <value>` sets one; `--reset` clears it.

## TODO.md

```markdown
# TODO

## Critical
- [ ] segfault on empty input #bug gh:42

## High
- [ ] support ggml 0.9 due:2026-12-01 #agent
  context lines are indented; the agent gets them verbatim

## Low
- [x] drop python 3.9
```

| Token | Effect |
|-|-|
| `#agent` | eligible for `--auto` |
| `#manual` | agents never take it |
| `#urgent` | urgent regardless of date |
| `due:YYYY-MM-DD` | urgent within 7 days |
| `gh:N` | linked issue |
| `^k3f9q` | stable id; `pma lint --ids --apply` adds them |

`pma lint ~/projects/*/` checks; `--fix --apply` repairs; `pma prune --apply` removes ticked items.

## TUI and web keys

`pma tui` or `pma web`. `?` lists the keys for the current row.

| Key | Action | Key | Action |
|-|-|-|-|
| `1`-`8` | inbox, projects, activity, matrix, library, report, notes, settings | `Enter` | open row |
| `O` | jump to project by name or tag | `o` | open the row's project |
| `/` | filter list | `#` | scope everything to a tag |
| `Space` | mark | `*` | mark all shown |
| `d` | dispatch marked or selected | `D` | dispatch, push if clean |
| `I` | dispatch to drive in herdr; stays behind | `Tab` | next list, e.g. `Failing CI` |
| `P` | prompt an agent (project view) | `z` | `pma run` a name |
| `v` | diff in pager (run) / verify (task) | `e` | tier, tag, verify, sandbox |
| `t` | record review minutes | `c` | scan |
| `H` / `T` | herdr pane / take over | `Esc` | back |
| `E` | project in `$EDITOR`, re-read on exit (TUI only) | `S` | shell in project, herdr pane if integrated; re-read on exit |

## Where things are

- `~/.config/pma/settings.toml`: settings, roots, workers, presets, tiers.
- `~/.config/pma/projects.db`: scans, runs, workflows.
- `~/.local/state/pma`: worktrees, logs, artifacts.
- `.pma/changelog.md` in the project: the agent's draft changelog entry. `.pma/` is listed in `.git/info/exclude`, so `git status` ignores it.
