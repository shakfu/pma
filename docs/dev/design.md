# pma design (draft)

Status: stages 1, 2, 4 and 5 are built. Stage 3 is built for `claude` only.

This document describes what `pma` does today. `implementation-plan.md` changes several of these decisions. Sections it supersedes carry a **Superseded** note naming the phase; the text below them remains the current behaviour until that phase lands.

## Problem

94 repositories under `~/projects/personal`, all on GitHub. 64 keep a root `TODO.md`. Checking what needs attention means opening each one.

Measured on 2026-09-14:

- 34 repos last committed on 2026-09-06. The commits only touched `.github/workflows/**` (for example `cysox`: "update wflws"). Last-commit date is therefore not a maintenance signal.

- Dirty working trees: `gnotes` 50 files, `playr` 36, `avocat` 12.

## Goals

1. Place every task across all projects in one Eisenhower matrix.

2. Dispatch coding agents to tasks, isolated from the user's work.

3. Review agent output, then commit and push in batches from `pma`.

4. Portfolio notes that belong to no single project.

5. Run the manager layer as an agent, under the rule that an LLM step proposes and code acts.

Non-goals: replacing GitHub Issues, and a general task manager.

**Two agent layers, and "agent" alone does not distinguish them.** A **worker** is one sub-agent on one task in one worktree: the command templates under [Agents](#agents). **`pma-agent`** is the manager layer. It classifies a task, writes its specification, reads a worker's output and instructs it mid-run, and proposes what to approve. `pma` the tool executes; `pma-agent` mints nothing, commits nothing and pushes nothing.

Goal 5 is not built. The manager is deterministic today, in scoring formulas and route matching. `design-review.md` states the split under "Manager and workers"; implementation plan 3.7 is its first LLM step and phase 2b is the channel it needs to instruct a worker at all.

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

Quadrants, as defined in [Columbia SPS, The Eisenhower Matrix](https://sps.columbia.edu/sites/default/files/2023-08/Eisenhower%20Matrix.pdf):

| | Urgent | Not urgent |
|-|-|-|
| **Important** | Q1 Do right away | Q2 Schedule for later |
| **Not important** | Q3 Delegate or avoid | Q4 Remove |

The two axes are independent. `TODO.md` stores importance. Urgency is computed, because it changes with time: a task due in 3 months becomes urgent without anyone editing the file. A stored quadrant would go stale.

### Importance

`importance = tier[t] * priority[p]`. A task is important when `importance >= important_threshold` (default 0.4).

With the default weights:

| Priority (weight) | Tier 1 (1.0) | Tier 2 (0.8) | Tier 3 (0.6) | Tier 4 (0.4) | Tier 5 (0.2) |
|-|-|-|-|-|-|
| Critical (1.0) | 1.0 | 0.8 | 0.6 | 0.4 | 0.2 |
| High (0.5) | 0.5 | 0.4 | 0.3 | 0.2 | 0.1 |
| Medium (0.2) | 0.2 | 0.16 | 0.12 | 0.08 | 0.04 |
| Low (0.05) | 0.05 | 0.04 | 0.03 | 0.02 | 0.01 |

Values at or above 0.4 are important: `Critical` in tiers 1-4, `High` in tiers 1-2. Nothing at `Medium` or `Low` is ever important. This is the direct consequence of the multiplier and the threshold.

### Urgency

**Superseded by implementation plan 3.4.** The `stale_after` rule is replaced by sequencing: `due:`, a blocking signal, and `#urgent`. With no due dates in the portfolio, "older than 30 days" admits most tier-1 tasks within a month, and the queue already sorts by age. Age becomes a tiebreak and a rotting-backlog report. The five `stale_after` settings are retired.

A task is urgent when any of these hold:

- `due:` is past or within `urgent_within` days (default 7).

- It carries `#urgent`.

- It is a signal task marked urgent (see Signals).

- It has no `due:` and has been open longer than `stale_after[tier]` days.

Default `stale_after`: tier 1 30 days, tier 2 60, tier 3 90, tiers 4 and 5 never. Without the last rule, Q1 would depend on remembering to write `due:`.

Age is the earlier of two times: the commit where the item's text first appeared in `TODO.md`'s history, and the first scan that saw it. Text is compared without list markers, checkbox, heading marks, trailing tokens, case or repeated spaces. An item therefore keeps its age when it moves between sections, gains a tag, or changes from a `###` heading into an item. Rewording resets it, so a reworded item counts as a new task.

`git blame -M` was the first choice and was rejected on real data. Migrating a file to v1 adds a tag to every item line, and blame then dates each line to the migration commit. Every migrated repo's ages restarted at 0.

Days are UTC calendar days, so a due date turns at UTC midnight.

The rule also applies to tasks that are not important. An old `Low` item in a tier-1 project moves from Q4 to Q3 after 30 days.

### Ordering and limits

Within a quadrant: importance, then days to due (dated tasks first), then age. The matrix view shows the top `quadrant_limit` tasks (default 10, as the source advises) and a count of the rest.

### Quadrant actions

**Superseded by implementation plan 3.1.** Eligibility replaces quadrant gating. An agent may take the `ci` and `deps` signals at any tier, plus items tagged `#agent`; `activity` and `hygiene` stay undispatchable. Importance orders the queue, eligibility decides what may be dispatched. `dispatch_quadrants` and `overflow_quadrants` are retired. The 2x2 remains a view.

| Quadrant | `pma` action |
|-|-|
| Q1 | Dispatched first. |
| Q2 | Dispatched after Q1. |
| Q3 | Dispatched only when `overflow_quadrants` includes it and Q1 and Q2 are exhausted. |
| Q4 | Never dispatched. Offered for removal from `TODO.md`, shipped as a batch. |

`dispatch_quadrants` (default `["Q1", "Q2"]`) sets what `pma dispatch --auto` draws from. `overflow_quadrants` (default `[]`) is drawn from only when every task in `dispatch_quadrants` is dispatched, in review, or not dispatchable.

This departs from the source, which says to do Q1 yourself and delegate Q3. Agents here work on the important tasks, so the review stage is the control.

## TODO.md format, v2

```markdown
# TODO

## Critical

- [ ] segfault on empty input #bug gh:42

## High

- [ ] support ggml 0.9 due:2026-10-01

## Medium

- [ ] flaky test on linux #urgent

## Low

- [x] drop python 3.9
```

Rules. `pma lint` reports errors (E), which exit 1, and warnings (W), which do not.

- The first non-blank line is `# TODO` (E). No second `#` heading (E).

- Priority comes from the enclosing `##` section: `Critical`, `High`, `Medium`, `Low`. Names are exact, including case (E), and each appears at most once (E).

- Other `##` sections are allowed and ignored. A checkbox item in one, or before the first `##`, is ignored (W). A file with no items but with plain bullets in other sections is flagged once (W): its tasks are invisible to `pma`.

- A `###` heading sets the group of the items below it, until the next `###` or `##`. `pma matrix` shows it. A bare `###` clears the group. `####` and deeper headings change neither section nor group.

- An item is one line: `- [ ] text` or `- [x] text`. Other checkbox spellings (`* [ ]`, `- [X]`, `1. [ ]`) (E). A plain bullet in a known section (E). Prose in a known section (W).

- Indented lines under an item are its description, carried verbatim, including blank lines between them. An indented line with no item above it (W).

- Unindented fenced code blocks are skipped.

- `- [x]` marks a finished item, which stays in its section. `pma prune` removes finished items and their descriptions; a file with errors is skipped.

- `## Done`, from v1, is an unknown section whose checkbox items warn (W). `pma prune` removes the section whole, whatever it holds, and lists any open item in it.

- Trailing tokens, read from the end of the line until a word is not a token:

  - `#tag`: a letter, then letters, digits, `-` or `_`. `#urgent` is a tag.

  - `due:YYYY-MM-DD`: a real calendar date (E), at most one (E).

  - `gh:N`: a positive issue number (E), at most one (E).

  - A token-shaped word earlier in the line is text. A trailing `#42` is text, with a hint to write `gh:42` (W).

- An item needs text besides tokens (E).

- No per-item ids. `gh:N` is the durable id for synced items. Unsynced items are identified by file and text, compared case-insensitively. Two open items with the same text (E), or two items with the same `gh:N` (E), break that identity. An agent that rewords an item is caught at review, where the diff is shown.

Headings over inline priority markers (todo.txt `(A)`): a human moves an item by cut and paste, and the file renders cleanly on GitHub.

The parser is line-based and edits lines in place. A markdown AST parser would re-render the file and reformat content `pma` does not own.

`pma lint` validates the format. Migrating the existing files is a scripted agent run, checked by `pma lint`.

Baseline, `pma lint */` on 2026-09-14: 29 of 94 repos have no root `TODO.md`. Of the 65 files, 3 are clean and none of those 3 has an item. The rest report 63 errors and 1492 warnings. 1415 of the warnings are checkbox items under sections other than the five known ones, so about 1400 existing tasks need moving into priority sections. 25 files have no items at all, only plain bullets.

## Signals

Project-level conditions become signal tasks in the matrix, so one view covers written tasks and maintenance. They are never written to `TODO.md`.

| Signal | Source | Signal task | Urgent | Priority | Dispatchable |
|-|-|-|-|-|-|
| ci | `gh run list` on the default branch | fix CI | when failing | High | yes |
| deps | per ecosystem: `cargo`, `go list -u -m all`, `uv` | update dependencies | no | Medium | yes |
| activity | `git log`, excluding commits whose paths all match `activity.ignore` | review project, once idle beyond the tier's horizon | no | Low | no |
| hygiene | changed files (`git status`), unpushed commits, leftover `pma/` branches | resolve local changes | no | Medium | no |

A review-project task is as old as the time since the horizon was crossed, not the time since the last commit. Otherwise it would reach `stale_after` on the day it appears.

CI looks at each workflow's latest run on the default branch that succeeded or failed. Cancelled and skipped runs are passed over.

Priorities are configurable per signal. With `High`, failing CI is important in tiers 1 and 2 (0.5, 0.4) and urgent, so it lands in Q1. In tiers 3-5 it lands in Q3.

Hygiene and activity tasks concern the user's working tree or judgement, which agents never touch, so they are not dispatchable.

Every `pma` worktree is made with a `pma/` branch, so counting branches finds worktrees too, and branches whose worktree is gone. A branch is leftover when no open run of that project owns it at scan time. Ship and reject remove the branch before a run becomes final, so leftovers come from interrupted dispatches or a lost database.

A fix-CI dispatch includes, for each workflow failing at the last scan, the `gh run view --log-failed` output of that workflow's latest decisive run, the run scan judged. A workflow that passes by dispatch time refuses the task until the next scan. A startup failure has no job log; the prompt says so. `pma` fetches it, because the agent environment has no GitHub credentials. The local `verify` passing does not prove CI passes on other platforms. With `publish = "pr"`, CI runs before merge.

The deps signal counts outdated dependencies as each tool reports them. The tools write nothing to the project:

| Ecosystem | Applies with | Command | Counts |
|-|-|-|-|
| cargo | `Cargo.lock` | `cargo update --dry-run` | semver-compatible lock updates, transitive included |
| uv | `uv.lock` | `uv tree --frozen --outdated --depth 1` | direct dependencies with a newer release |
| go | `go.mod` | `go list -u -m all`, direct modules | modules with a newer version |

The counts are not comparable across ecosystems: cargo's includes transitive crates. On 2026-09-15 across 52 repos, cargo projects reached 147 while uv projects stayed under 15. The score saturates at 10 outdated, so the difference does not dominate health.

Measuring takes 0.6s (uv) to 10s (go) per project, and 44s for the 52 repos. It runs only with `pma scan --deps`. Other scans keep the last measurement and its date, which `status --explain` shows. A project with none of the three files, or whose tools all fail, is unmeasured. A tool's failure is kept in the detail. A deps task is dispatched with that detail as the list to update.

### Project health

**Under review; see the cut list in `implementation-plan.md`.** Five weights and a saturation curve yield a number with no action attached. Removal waits on a replacement for what it alone covers: `pma report` (plan 1.10) describes runs that were attempted, whereas this view also describes repositories where nothing was ever dispatched.

A separate per-project view ranks projects rather than tasks:

`health = tier[t] * sum(w_i * s_i) / sum(w_i)`, each `s_i` in 0..1 (1 = needs attention). The sums run over measured signals only, so an unmeasured signal neither lowers nor raises the score.

| Signal | `s` |
|-|-|
| tasks | `1 - exp(-x / 3)`, `x` the summed priority weights of open items |
| activity | days idle / tier horizon, capped at 1; 1 when no counted commit exists |
| ci | failing 1, passing 0, no runs 0.5; unmeasured when unknown or `--offline` |
| deps | outdated count / 10, capped at 1; unmeasured when never measured |
| hygiene | 0.5 each for changed files, unpushed commits and leftover `pma/` branches, capped at 1 |

`pma status --explain` prints each signal's contribution. Weights cannot be calibrated without it.

## Storage

SQLite at `~/.config/pma/projects.db`, via `rusqlite`. It holds projects, tiers, weights, notes, the scanned task index, the dispatch queue, and agent run history. The directory may be a git repo for sync between machines.

This assumes one user. Commands that run agents or change worktrees (`dispatch`, `ship`, `review --reject`, `review --rework`) hold an exclusive `flock` on `session.lock` for their whole run. A second one refuses to start and names the holder's pid. The kernel releases the lock when the process exits, so a crash leaves no stale lock, unlike a pid file. Other commands run alongside, and SQLite waits up to 5s for a write lock. `pma review` holds the lock for milliseconds to fail runs of an ended session, so a command that needs the lock waits up to 2s before refusing.

- `journal_mode=DELETE`, not WAL. The file on disk is complete whenever no transaction is open, so it can be committed as is. WAL lets readers run during a write; here writes are single rows, and the busy timeout covers them.

- `pma store sync` commits and pushes the database, and pulls before the session writes. Scans do not commit, so the repo gains a blob only on an explicit sync.

- A session refuses to write when the remote has a newer database than the last one pulled. This catches a sync forgotten on another machine before the two copies diverge. Git cannot merge them afterwards.

Tiers, weights and notes are edited through `pma` commands, not a text editor.

Notes are portfolio-wide, with no project field: a note about one project belongs in its repo. `pma note add`, `pma note edit <id>`, `pma note rm <id>`, and `pma note` to list.

Schema changes are applied in order on open, inside one transaction, from the file's `user_version`.

## Configuration

```toml
important_threshold = 0.4
urgent_within       = 7                  # days
quadrant_limit      = 10
dispatch_quadrants  = ["Q1", "Q2"]
overflow_quadrants  = []                 # ["Q3"] to continue past Q1 and Q2
publish             = "pr"               # or "push"
attribution         = "user"             # or "co-author"
max_parallel        = 2                  # agents at once
batch_budget        = 5.0                # USD per dispatch batch
agent_budget        = 1.0                # USD per agent run
timeout             = 30                 # minutes per agent run, and per verify

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

[weights]                                # project health
tasks    = 5
activity = 1
ci       = 3
deps     = 1
hygiene  = 2

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
ignore  = [".github/**", "*.lock", "TODO.md"]
horizon = { 1 = 30, 2 = 60, 3 = 120, 4 = 240, 5 = 365 }   # days

[projects.cyllama]
tier    = 1
verify  = "make test"                     # default: detected; "none" disables
publish = "push"                          # overrides the global value
```

Shown as TOML for readability. The values are stored in `projects.db` and set with `pma config <key> <value>`, using dotted keys such as `tiers.2` or `activity.horizon.1`. `activity.ignore` takes a comma-separated list.

`TODO.md` is in `activity.ignore` so that editing the list is not maintenance. Adding an empty `TODO.md` to 19 repos in one run would otherwise have marked all of them active.

A glob without `/` matches a file name in any directory. `*` matches within one path segment, and `**` matches any number of segments.

Projects are the git repos directly under each root, named by directory. A project under a root with no tier is listed as untiered and left out of the matrix. A full `pma scan` marks projects it no longer finds as absent and says so, keeping the row, its tier and its tasks; commands that need a working tree refuse an absent project. `pma forget` deletes an absent project's record when the repository is gone for good; it refuses one with an unsettled run, whose worktree may still exist. Leaving a root is usually a move, and a rescan cannot rebuild a tier or a task's `first_seen`. Tags group projects for selection: a project carries several, and `--tag` on a name-taking command adds every project carrying it. They are private to the database, since GitHub topics describe a repository for search rather than organise a portfolio. `PMA_HOME` overrides `~/.config/pma`.

## Agents

The worker layer. For the manager layer, see goal 5.

Each agent is a command template run in the worktree. Flags below were checked against each tool's `--help` on 2026-09-14.

| Agent | Headless invocation | Budget cap |
|-|-|-|
| `claude` | `claude -p <prompt> --output-format json --permission-mode acceptEdits --max-budget-usd <n>` | native |
| `codex` | `codex exec -C <dir> -s workspace-write --json -o <file> <prompt>` | timeout only |
| `cursor-agent` | `cursor-agent -p --output-format json --workspace <dir> --force <prompt>` | timeout only |
| `opencode` | `opencode run --dir <dir> --format json --auto <prompt>` | timeout only |

Unverified: which permissions each agent needs to run the project's tests unattended, and whether each one's JSON output reports cost.

`claude` reports `total_cost_usd`. Its budget cap is checked between turns, so a run can exceed it: a one-word reply under a $0.05 cap cost $0.09 on 2026-09-15. `batch_budget` therefore bounds how many runs start, not what they spend.

Only `codex` has a sandbox. `cursor-agent --force` and `opencode --auto` auto-approve every command; opencode's own help calls this "dangerous". A worktree limits where an agent starts, not what it can reach.

`claude` runs with `--permission-mode acceptEdits` and one exact `Bash(...)` allow rule per subcommand of the verify command, so it can run the check `pma` runs afterwards. Claude Code checks each part of `a && b` against the rules separately ([permissions](https://code.claude.com/docs/en/permissions.md#compound-commands)), hence one rule per part. In `-p` mode other shell commands are denied, unless the user's own settings allow them. The permission mode is not an isolation boundary: the agent edits the files that verify executes, so allowing verify to the agent adds no reach that verify lacks. Isolation would need a container around both.

Because `pma` owns commits and pushes, the agent environment drops push credentials: `GH_TOKEN` and `GITHUB_TOKEN` unset, `GH_CONFIG_DIR` pointed at an empty directory, `SSH_AUTH_SOCK` unset, and `GIT_TERMINAL_PROMPT=0`. Through `GIT_CONFIG_COUNT`, credential helpers are cleared and `remote.origin.pushurl` is set to an unusable URL. This makes an accidental push fail. It does not stop a determined process. The verify command runs in the same environment.

## Dispatch

`pma dispatch cynn:31` names a TODO.md line from the last scan, and `pma dispatch cynn:ci` names failing CI. `pma dispatch --auto -n N` takes the top N dispatchable tasks of tiered projects, as ordered in the matrix. A task with a run that is not shipped or rejected is skipped.

1. `git fetch`, then `git worktree add` from the remote default branch into `<data>/worktrees/<project>/<slug>`, on branch `pma/<slug>`. The user's working tree is never touched, so a dirty tree does not block dispatch. The slug is the task text's first words, up to 40 bytes, with `-2`, `-3` on collision with a worktree, a local branch, or a remote `pma/` branch left by a pull request. The item must be open in the remote `TODO.md`, checked before the worktree exists; otherwise ship could not mark it done. A refusal names the cause: not in the remote file (commit and push it), or already done there (pull the clone). With `--auto`, a refused task gives its place to the next one in matrix order. A project-level failure, such as a failed fetch, skips that project's other tasks.

2. Run the agent template with a timeout and, where supported, a budget, allowed to run the verify command (see Agents).

3. The prompt carries the task, its description, and "do not commit". The project's own `CLAUDE.md` or `AGENTS.md` still applies.

4. If the branch has commits beyond its base, `pma` flags the run. The output was meant to be uncommitted.

5. `pma` runs the project's `verify` command itself. The agent's report is not trusted as proof. Without `projects.<name>.verify`, the first match wins: a Makefile `test:` target, `Cargo.toml`, `go.mod`, a `package.json` with a `test` script, `pyproject.toml` (`uv run pytest` with `uv.lock`, else `python3 -m pytest`). A failed verify still leaves the run ready; the reviewer decides.

6. Record: agent, diffstat, verify result, agent summary, cost where reported, duration.

Limits: `max_parallel` agents, and `batch_budget`. A run starts only while the batch's spent cost plus `agent_budget` for each running and starting run stays within `batch_budget`. A run that does not start is marked failed, with its worktree kept for rework or rejection.

`pma dispatch` blocks until every run finishes. A run left queued or running by an interrupted session is marked failed at the next dispatch or review, but only by a command that holds the session lock or finds it free. A run of a live session is left alone.

## Review

States: `queued -> running -> ready | failed`, then `approved -> shipped`, or `rejected`. With `publish = "pr"`, `approved -> pr-open`, then `shipped` when the pull request is merged, or `rejected` when it is closed unmerged. Rework returns a ready, failed or approved run to `running`.

`pr-open` is not final, so the task is not dispatched again while its pull request is open. The item is still open on the default branch until the merge, which alone would admit it. `pma review` and `pma dispatch` read each open pull request's state with `gh pr view`. A run whose state cannot be read stays `pr-open`.

`pma review` lists runs not shipped or rejected. `pma review <id>` shows task text, quadrant, verify result, cost, agent summary, and `git diff` against the base, untracked files included. Actions:

- `--approve`, from ready

- `--reject`, removing the worktree and branch; not from `pr-open`, where the pull request is closed on GitHub instead

- `--rework <feedback>`, running the agent again in the same worktree, with the feedback after the original prompt

## Ship

**Superseded by implementation plan 4.7 and 4.8.** Ship publishes without rerunning verification after approval, so an edited worktree, or a clean but semantically incompatible rebase, can publish content no verify run ever saw. Approval will carry evidence (verified tree, base SHA, verify command, scope revision, approver) that the integrated tree is rechecked against after the rebase. With `publish = "pr"`, the run will also gain a merge lifecycle gated on positive green CI for the current head, rather than only observing whether a person merged it.

`pma ship` processes approved tasks, per project:

1. `git add -A`, then commit with the task text as the subject. Add `Closes #N` when `gh:N` is set. Author is the user. With `attribution = "co-author"`, a trailer names the agent.

2. With `publish = "push"`, rebase onto the remote default branch.

3. Mark the item `[x]` in place and amend the commit. This follows the rebase. Git treats changes to adjacent lines as a conflict, so two tasks ticked before the rebase could conflict. A fix-CI run has no item and skips this step.

4. Publish per `publish`:

   - `push`: push to the default branch.

   - `pr`: push `pma/<slug>` and `gh pr create`. `gh` is required up front. The run becomes `pr-open`.

5. Remove the worktree and branch.

A failure in one project, such as a rebase conflict, stops that project and does not stop the batch. Its runs stay approved, so `pma ship` can be run again. The report lists each outcome.

Ship resumes where it stopped. The outcome is saved as soon as the publish step succeeds, and a failure to remove the worktree afterwards is a warning; the leftover branch then shows in the hygiene signal. With `push`, a worktree whose commits are already in the fetched default branch was pushed by an earlier, interrupted ship, and is recorded as shipped. With `pr`, an open pull request for the branch is reused instead of creating another.

The user's clone is not updated. After a push it is behind its remote, and an uncommitted `TODO.md` edit there can conflict on pull.

## Sync (hybrid)

- `TODO.md` to Issues: `Critical` items without `gh:N` get an issue labelled `pma:critical`. `pma` writes `gh:N` back into the line.

- Issues to `TODO.md`: an issue closed on GitHub marks its line `[x]`.

- Conflicts: `TODO.md` wins on text and priority. GitHub wins on closed state. A linked open item whose issue title differs retitles the issue. A linked item moved out of `Critical` loses the label; one moved in gains it.

- Issues opened by other people are listed as untriaged, not imported.

- A `gh:N` that names no issue is a warning. A finished item whose issue is still open is left alone; `Closes #N` from ship closes it.

Sync keys on the `Critical` heading, not on Q1. A quadrant shifts as due dates approach, which would open and close issues without any edit.

`pma sync` lists the plan; `pma sync --apply` carries it out. Every project whose last scan recorded a GitHub `owner/name` is synced, tiered or not: `Critical` is a property of the file, tiers only rank. A `TODO.md` with lint errors is skipped, since duplicate text or `gh:N` breaks item identity.

Each `gh:N` is written to the file right after its issue is created. A sync that stops between the two leaves an open, labelled issue with the item's title and no link. The next sync links that issue instead of opening another.

Write-backs are uncommitted edits to `TODO.md` in the user's clone, not a `pma ship` batch: ship commits worktrees, and the clone is the file the matrix reads. `scripts/commit_todo.py` commits them.

Adding `gh:N` changes an item's key from its text to the issue number. Dispatch, ship, and the one-run-per-task check therefore match a task by key or by text.

## Implementation

- CLI: `clap`. Config: `serde` and `toml`. Store: `rusqlite` (bundled).

- Git: run the `git` binary, not `git2` or `gix`, so the user's config, hooks and credentials apply.

- GitHub: run `gh`, not `octocrab`, so its existing authentication is reused.

- Processes: `std::process` and threads. The parallelism is child processes, so an async runtime is not needed.

- TUI: `ratatui` with only its crossterm backend, 69 crates rather than the default features' 152. `pma tui` shows the matrix as a 2x2 layout, Q1 top left, and the selected task with its `pma dispatch` target. It reads the last scan and changes nothing. Focus is a thick border and selection a `> ` marker in reverse video, so neither depends on colour.

## Stages

Each stage is used before the next one is built.

1. Format spec, `pma lint`, and migration of the 64 files.

2. `pma scan`, `pma matrix`, `pma status --explain`, with tiers and weights.

3. `pma dispatch`, `pma review`, `pma ship`, with `claude` first, then the other three agent templates. Built: `claude`, and leftover worktrees in the hygiene signal. Not built: the other three agents, and offering Q4 items for removal.

4. `pma sync` with Issues. Built.

5. Notes commands, deps signal, TUI. Built. The deps signal covers cargo, uv and go; npm (2 repos here) is not covered.

## Open questions

Open questions, what to cut, and the sequencing model for urgency: `design-review.md`. Sequenced work, with the phase that supersedes each decision above: `implementation-plan.md`. Review of that plan against this code: `plan-review.md`. Stage sequences over one project: `workflows.md`.
