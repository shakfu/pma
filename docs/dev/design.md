# pma design (draft)

Status: draft for review. Nothing here is built.

## Problem

94 repositories under `~/projects/personal`, all on GitHub. 64 keep a root
`TODO.md`. Checking what needs attention means opening each one.

Measured on 2026-09-14:

- 34 repos last committed on 2026-09-06. The commits only touched
  `.github/workflows/**` (for example `cysox`: "update wflws"). Last-commit date
  is therefore not a maintenance signal.
- Dirty working trees: `gnotes` 50 files, `playr` 36, `avocat` 12.

## Goals

1. Place every task across all projects in one Eisenhower matrix.
2. Dispatch coding agents to tasks, isolated from the user's work.
3. Review agent output, then commit and push in batches from `pma`.
4. Portfolio notes that belong to no single project.

Non-goals: replacing GitHub Issues, and a general task manager.

## Decisions taken

| Question | Decision |
|-|-|
| Task source of truth | `TODO.md` in each repo |
| Task view | Eisenhower matrix |
| Importance | Project tier (1 most important to 5) multiplied by item priority |
| Thresholds and weights | Configurable; the defaults below are the starting point |
| Agent quadrants | Q1 and Q2; Q3 optional once Q1 and Q2 are exhausted |
| Signal tasks | In the matrix alongside written tasks |
| Portfolio store | SQLite at `~/.config/pma/projects.db`, optionally git-tracked; one user, one session |
| Agents | `claude`, `codex`, `cursor-agent`, `opencode` |
| Agent output | Uncommitted changes, reviewed, then committed and pushed by `pma` |
| Publish | Configurable: push to the default branch, or push a branch and open a PR |
| Attribution | Configurable, default the user only |
| Issues sync | Hybrid, `Critical` items only |
| Language | Rust |

## The matrix

Quadrants, as defined in
[Columbia SPS, The Eisenhower Matrix](https://sps.columbia.edu/sites/default/files/2023-08/Eisenhower%20Matrix.pdf):

| | Urgent | Not urgent |
|-|-|-|
| **Important** | Q1 Do right away | Q2 Schedule for later |
| **Not important** | Q3 Delegate or avoid | Q4 Remove |

The two axes are independent. `TODO.md` stores importance. Urgency is computed,
because it changes with time: a task due in 3 months becomes urgent without
anyone editing the file. A stored quadrant would go stale.

### Importance

`importance = tier[t] * priority[p]`. A task is important when
`importance >= important_threshold` (default 0.4).

With the default weights:

| Priority (weight) | Tier 1 (1.0) | Tier 2 (0.8) | Tier 3 (0.6) | Tier 4 (0.4) | Tier 5 (0.2) |
|-|-|-|-|-|-|
| Critical (1.0) | 1.0 | 0.8 | 0.6 | 0.4 | 0.2 |
| High (0.5) | 0.5 | 0.4 | 0.3 | 0.2 | 0.1 |
| Medium (0.2) | 0.2 | 0.16 | 0.12 | 0.08 | 0.04 |
| Low (0.05) | 0.05 | 0.04 | 0.03 | 0.02 | 0.01 |

Values at or above 0.4 are important: `Critical` in tiers 1-4, `High` in tiers
1-2. Nothing at `Medium` or `Low` is ever important. This is the direct
consequence of the multiplier and the threshold.

### Urgency

A task is urgent when any of these hold:

- `due:` is past or within `urgent_within` days (default 7).
- It carries `#urgent`.
- It is a signal task marked urgent (see Signals).
- It has no `due:` and has been open longer than `stale_after[tier]` days.

Default `stale_after`: tier 1 30 days, tier 2 60, tier 3 90, tiers 4 and 5
never. Without the last rule, Q1 would depend on remembering to write `due:`.

Age is the earlier of `git blame -M` on the line and the first time `pma` saw
the task. `-M` keeps the date when an item moves between sections. Rewording an
item resets both, so it counts as a new task.

The rule also applies to tasks that are not important. An old `Low` item in a
tier-1 project moves from Q4 to Q3 after 30 days.

### Ordering and limits

Within a quadrant: importance, then days to due, then age from `git blame`.
The matrix view shows the top `quadrant_limit` tasks (default 10, as the source
advises) and a count of the rest.

### Quadrant actions

| Quadrant | `pma` action |
|-|-|
| Q1 | Dispatched first. |
| Q2 | Dispatched after Q1. |
| Q3 | Dispatched only when `overflow_quadrants` includes it and Q1 and Q2 are exhausted. |
| Q4 | Never dispatched. Offered for removal from `TODO.md`, shipped as a batch. |

`dispatch_quadrants` (default `["Q1", "Q2"]`) sets what `pma dispatch --auto`
draws from. `overflow_quadrants` (default `[]`) is drawn from only when every
task in `dispatch_quadrants` is dispatched, in review, or not dispatchable.

This departs from the source, which says to do Q1 yourself and delegate Q3.
Agents here work on the important tasks, so the review stage is the control.

## TODO.md format, v1

```markdown
# TODO

## Critical

- [ ] segfault on empty input #bug gh:42

## High

- [ ] support ggml 0.9 due:2026-10-01

## Medium

- [ ] flaky test on linux #urgent

## Low

## Done

- [x] drop python 3.9
```

Rules. `pma lint` reports errors (E), which exit 1, and warnings (W), which do
not.

- The first non-blank line is `# TODO` (E). No second `#` heading (E).
- Priority comes from the enclosing `##` section: `Critical`, `High`, `Medium`,
  `Low`. `Done` holds finished items. Names are exact, including case (E), and
  each appears at most once (E).
- Other `##` sections are allowed and ignored. A checkbox item in one, or before
  the first `##`, is ignored (W). A file with no items but with plain bullets in
  other sections is flagged once (W): its tasks are invisible to `pma`.
- `###` and deeper headings group items without changing their section.
- An item is one line: `- [ ] text` or `- [x] text`. Other checkbox spellings
  (`* [ ]`, `- [X]`, `1. [ ]`) (E). A plain bullet in a known section (E). Prose
  in a known section (W).
- Indented lines under an item are its description, carried verbatim, including
  blank lines between them. An indented line with no item above it (W).
- Unindented fenced code blocks are skipped.
- `- [ ]` under `## Done` (E). `- [x]` in a priority section (W).
- Trailing tokens, read from the end of the line until a word is not a token:
  - `#tag`: a letter, then letters, digits, `-` or `_`. `#urgent` is a tag.
  - `due:YYYY-MM-DD`: a real calendar date (E), at most one (E).
  - `gh:N`: a positive issue number (E), at most one (E).
  - A token-shaped word earlier in the line is text. A trailing `#42` is text,
    with a hint to write `gh:42` (W).
- An item needs text besides tokens (E).
- No per-item ids. `gh:N` is the durable id for synced items. Unsynced items are
  identified by file and text, compared case-insensitively. Two open items with
  the same text (E), or two items with the same `gh:N` (E), break that identity.
  An agent that rewords an item is caught at review, where the diff is shown.

Headings over inline priority markers (todo.txt `(A)`): a human moves an item by
cut and paste, and the file renders cleanly on GitHub.

The parser is line-based and edits lines in place. A markdown AST parser would
re-render the file and reformat content `pma` does not own.

`pma lint` validates the format. Migrating the existing files is a scripted
agent run, checked by `pma lint`.

Baseline, `pma lint */` on 2026-09-14: 29 of 94 repos have no root `TODO.md`.
Of the 65 files, 3 are clean and none of those 3 has an item. The rest report
63 errors and 1492 warnings. 1415 of the warnings are checkbox items under
sections other than the five known ones, so about 1400 existing tasks need
moving into priority sections. 25 files have no items at all, only plain bullets.

## Signals

Project-level conditions become signal tasks in the matrix, so one view covers
written tasks and maintenance. They are never written to `TODO.md`.

| Signal | Source | Signal task | Urgent | Priority | Dispatchable |
|-|-|-|-|-|-|
| ci | `gh run list` on the default branch | fix CI | when failing | High | yes |
| deps | per ecosystem: `cargo`, `go list -u -m all`, `uv` | update dependencies | no | Medium | yes |
| activity | `git log`, excluding commits whose paths all match `activity.ignore` | review project | no | Low | no |
| hygiene | dirty files, unpushed commits, leftover `pma` worktrees | resolve local changes | no | Medium | no |

Priorities are configurable per signal. With `High`, failing CI is important in
tiers 1 and 2 (0.5, 0.4) and urgent, so it lands in Q1. In tiers 3-5 it lands in
Q3.

Hygiene and activity tasks concern the user's working tree or judgement, which
agents never touch, so they are not dispatchable.

A fix-CI dispatch includes `gh run view --log-failed` output in the prompt.
`pma` fetches it, because the agent environment has no GitHub credentials. The
local `verify` passing does not prove CI passes on other platforms. With
`publish = "pr"`, CI runs before merge.

The deps signal is deferred: it needs one adapter per language.

### Project health

A separate per-project view ranks projects rather than tasks:

`health = tier[t] * sum(w_i * s_i) / sum(w_i)`, each `s_i` in 0..1 (1 = needs
attention), over `critical`, `activity`, `ci`, `deps`, `hygiene`.

`pma status --explain` prints each signal's contribution. Weights cannot be
calibrated without it.

## Storage

SQLite at `~/.config/pma/projects.db`, via `rusqlite`. It holds projects, tiers,
weights, notes, the scanned task index, the dispatch queue, and agent run
history. The directory may be a git repo for sync between machines.

This assumes one user and one session at a time. Under that assumption:

- `journal_mode=DELETE`, not WAL. The file on disk is complete whenever no
  transaction is open, so it can be committed as is. WAL only helps readers run
  alongside a writer, which one session never does.
- `pma store sync` commits and pushes the database, and pulls before the session
  writes. Scans do not commit, so the repo gains a blob only on an explicit sync.
- A session refuses to write when the remote has a newer database than the last
  one pulled. This catches a sync forgotten on another machine before the two
  copies diverge. Git cannot merge them afterwards.

Tiers, weights and notes are edited through `pma` commands, not a text editor.

## Configuration

```toml
important_threshold = 0.4
urgent_within       = 7                  # days
quadrant_limit      = 10
dispatch_quadrants  = ["Q1", "Q2"]
overflow_quadrants  = []                 # ["Q3"] to continue past Q1 and Q2
publish             = "pr"               # or "push"
attribution         = "user"             # or "co-author"

[tiers]
1 = 1.0
2 = 0.8
3 = 0.6
4 = 0.4
5 = 0.2

[priorities]
critical = 1.0
high     = 0.5
medium   = 0.2
low      = 0.05

[stale_after]                            # days open without due: before urgent
1 = 30
2 = 60
3 = 90

[signals]
ci       = "high"
deps     = "medium"
activity = "low"
hygiene  = "medium"

[activity]
ignore  = [".github/**", "*.lock"]
horizon = { 1 = 30, 2 = 60, 3 = 120, 4 = 240, 5 = 365 }   # days

[projects.cyllama]
tier    = 1
verify  = "make test"                     # default: detected
publish = "push"                          # overrides the global value
```

Shown as TOML for readability. The values are stored in `projects.db` and set
with `pma config`. A project under a root with no tier is listed as untiered and
left out of the matrix.

## Agents

Each agent is a command template run in the worktree. Flags below were checked
against each tool's `--help` on 2026-09-14.

| Agent | Headless invocation | Budget cap |
|-|-|-|
| `claude` | `claude -p <prompt> --output-format json --permission-mode acceptEdits --max-budget-usd <n>` | native |
| `codex` | `codex exec -C <dir> -s workspace-write --json -o <file> <prompt>` | timeout only |
| `cursor-agent` | `cursor-agent -p --output-format json --workspace <dir> --force <prompt>` | timeout only |
| `opencode` | `opencode run --dir <dir> --format json --auto <prompt>` | timeout only |

Unverified: which permissions each agent needs to run the project's tests
unattended, and whether each one's JSON output reports cost.

Only `codex` has a sandbox. `cursor-agent --force` and `opencode --auto`
auto-approve every command; opencode's own help calls this "dangerous".
A worktree limits where an agent starts, not what it can reach.

Because `pma` owns commits and pushes, the agent environment drops push
credentials: `GH_TOKEN` and `GITHUB_TOKEN` unset, `GH_CONFIG_DIR` pointed at an
empty directory, `SSH_AUTH_SOCK` unset, and `GIT_TERMINAL_PROMPT=0`. This makes
an accidental push fail. It does not stop a determined process.

## Dispatch

1. `git fetch`, then `git worktree add` from the remote default branch into
   `<data>/worktrees/<project>/<slug>`, on branch `pma/<slug>`. The user's working
   tree is never touched, so a dirty tree does not block dispatch.
2. Run the agent template with a timeout and, where supported, a budget.
3. The prompt carries the task, its description, and "do not commit". The
   project's own `CLAUDE.md` or `AGENTS.md` still applies.
4. If the branch has commits beyond its base, `pma` flags the run. The output
   was meant to be uncommitted.
5. `pma` runs the project's `verify` command itself. The agent's report is not
   trusted as proof.
6. Record: agent, diffstat, verify result, agent summary, cost where reported,
   duration.

Limits: `max_parallel` agents and a USD budget per batch.

## Review

States: `queued -> running -> ready | failed -> approved | rejected | rework`.

`pma review` shows each ready task: task text, quadrant, verify result, agent
summary, and `git diff` in the worktree. Actions:

- approve
- reject (removes the worktree)
- rework with feedback (dispatches again in the same worktree)

## Ship

`pma ship` processes approved tasks, per project:

1. In the worktree, mark the item `[x]` and move it to `Done`.
2. `git add -A`, then commit with the task text as the subject. Add `Closes #N`
   when `gh:N` is set. Author is the user. With `attribution = "co-author"`, a
   trailer names the agent.
3. Publish per `publish`:
   - `push`: rebase onto the remote default branch and push.
   - `pr`: push `pma/<slug>` and `gh pr create`.
4. Remove the worktree.

A failure in one project, such as a rebase conflict, stops that project and does
not stop the batch. The report lists each outcome.

## Sync (hybrid)

- `TODO.md` to Issues: `Critical` items without `gh:N` get an issue labelled
  `pma:critical`. `pma` writes `gh:N` back into the line.
- Issues to `TODO.md`: an issue closed on GitHub marks its line `[x]`.
- Conflicts: `TODO.md` wins on text and priority. GitHub wins on closed state.
- Issues opened by other people are listed as untriaged, not imported.

Sync keys on the `Critical` heading, not on Q1. A quadrant shifts as due dates
approach, which would open and close issues without any edit.

Write-backs are uncommitted changes, shipped with the next batch.

## Implementation

- CLI: `clap`. Config: `serde` and `toml`. Store: `rusqlite` (bundled).
- Git: run the `git` binary, not `git2` or `gix`, so the user's config, hooks and
  credentials apply.
- GitHub: run `gh`, not `octocrab`, so its existing authentication is reused.
- Processes: `std::process` and threads. The parallelism is child processes, so
  an async runtime is not needed.
- TUI (later): `ratatui`, with the matrix as a 2x2 layout.

## Stages

Each stage is used before the next one is built.

1. Format spec, `pma lint`, and migration of the 64 files.
2. `pma scan`, `pma matrix`, `pma status --explain`, with tiers and weights.
3. `pma dispatch`, `pma review`, `pma ship`, with `claude` first, then the other
   three agent templates.
4. `pma sync` with Issues.
5. Notes commands, deps signal, TUI.

## Open questions

None at present.
