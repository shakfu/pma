# Using pma well

How to get the most work out of `pma` per minute of your attention. It assumes the `claude` worker at its default model. Commands in brief: [cheatsheet.md](cheatsheet.md). Full reference: [README](../README.md).

## The model

`pma` has one loop:

1. `scan` reads every project's `TODO.md`, git state and CI.
2. `next` ranks tasks across all projects.
3. `dispatch` hands a task to an agent.
4. `pma` runs the project's check before and after.
5. You `review` the diff and decide.
6. `stage`, `commit`, `push` or `pr` publishes it.

Your time goes into steps 5 and 6. Everything below aims to make those steps short and rare to repeat.

## Setup that pays for itself

### Tier only what matters

Untiered projects rank at tier 5. Tier the 5-15 projects you actually maintain. Leave the rest untiered; they still scan and stay out of the way.

```sh
pma project tier 1 core-lib
pma project tier 2 cli site
pma project export tiers.csv    # bulk edit, then: pma project import tiers.csv --apply
```

Tag projects by kind (`rust`, `work`, `py`). Tags drive `--tag`, the TUI's `#` scope and `O` jump.

### Make the check trustworthy

The check is the only evidence `pma` records that a change works. Its gates (`b`, `--push`, `--local`) trust it. A weak check makes every gate weak.

```sh
pma verify core-lib cli site    # runs each check where a dispatch would
pma config projects.site.verify "uv run pytest -x"
```

Fix any project whose check fails on a clean tree before dispatching to it. Otherwise every run starts red and the before/after comparison says nothing.

### Add a cheap preset

```sh
pma preset set claude-haiku claude haiku
```

Use it for mechanical tasks with `-p claude-haiku`. Keep the default model for anything that needs judgement.

## Writing tasks an agent finishes

The item text and its indented lines are the whole prompt. The agent sees nothing else from you.

- One outcome per item. "Add `--json` to `list`" finishes. "Improve the CLI" does not.
- Put context in indented lines: the file, the fixture, the constraint, what done looks like.
- Name the test that should pass, if one exists.
- Tag `#agent` on items you trust to an unattended pick. `--auto` takes only those, plus `ci` and `deps`.
- Tag `#manual` on items that need you: design calls, credentials, releases.
- Run `pma lint --ids --apply` once. Ids keep a task's age and runs when you reword it.

```markdown
## High

- [ ] `list --json` emits one object per project #agent
  shape: {"name", "tier", "tags"}; see tests/cli_list.rs
  keep the text output unchanged
```

## Clone or worktree

| | `clone` (default) | `worktree` |
|-|-|-|
| Starts from | your working tree, as it is | remote default branch |
| Runs per project | one at a time | several |
| Your checkout | edited, change left unstaged | untouched |
| Task must be | in local `TODO.md` | pushed to the remote `TODO.md` |
| Best for | one project you are working in | many projects in parallel, PRs |

Use the clone when you want to see the change in your editor and keep working around it. Use `--worktree` or `pma config workspace worktree` for batches across projects.

In the clone, a rejection reverts only the agent's recorded patch. It refuses once you commit, or once you edit the same lines. Use `pma close` then.

## Choosing the model

| Work | Choice |
|-|-|
| Ordinary tasks, bug fixes, features | default model |
| Lock updates | `-a refresh` (no agent, no tokens) |
| Renames, lint fixes, small mechanical edits | `-p claude-haiku` |
| Reviews via `pma run review` and similar | opus, chosen by `pma run` |
| Failed twice on the default model | `--rework` with feedback, or a stronger `-m` |

Reviews default to opus because weaker models produced false findings in the pilot. Override with `-w claude:sonnet` if cost matters more than precision.

To set this once, write a routing policy and activate it: see "Stale dependencies" in the README for the shape. `pma route replay` shows what a candidate would have changed over past runs.

### Cost limits

- `agent_budget` (USD 1): per run. It admits runs into a batch and flags runs that cost more. Treat it as a signal, not a hard stop.
- `batch_budget` (USD 5): per batch of dispatches.
- `timeout` (30 min): per run.
- `max_parallel` (2): agents at once.

Raise `max_parallel` only with `workspace = worktree`. The clone allows one run per project.

## Reviewing fast

`pma review 4` (or `Enter` on a run) shows, in order of what to read:

1. The check before and after. Red after green is a reject.
2. The review reasons: why `pma` will not treat the run as clean. See "Let clean runs through" below.
3. Denied actions, among those reasons. `claude` runs in auto mode; a classifier refuses some actions without stopping the run. Read what was refused: the agent may have worked around it badly.
4. The diff. `v` opens it in your pager.
5. The changelog draft, in `.pma/changelog.md`.

Then decide:

- **Approve** when the diff is right.
- **Rework** when it is close. Feedback is one or two sentences naming the fix: "use `Config::load`, not a new parser". The agent continues in the same worktree.
- **Fix it yourself** when the fix is quicker to make than to describe. `E` on the run opens the project in your editor. In a clone run the diff stays live until you commit, so your edit is part of what you approve and publish ([`clone-runs.md`](dev/clone-runs.md)). A later reject then refuses if you edited the agent's lines; use close.
- **Reject** when the approach is wrong. Rewrite the task's indented lines before the next attempt.
- **Retry** when it failed for no reason in the task: a flaky test, a timeout.
- **`--reject --infra`** when the check or the machine failed. It uses no attempt and does not count against the agent.
- **Close** when the change already landed or you took it over by hand.

A task gets two attempts. `--retry` on dispatch resets them.

### Bundles

Several items from one project, named together, become one run. Review it once. If it finished only some items, approve with `--drop` listing the rest; they stay open. Use `--separate` when items are unrelated; a bad item then sinks only its own run.

## Publishing

| You want | Command |
|-|-|
| To write the commit yourself | `pma stage 4`, then `git commit` |
| `pma` to commit and push | `pma push 4 --approve` |
| A pull request | `pma review 4 --approve`, then `pma pr 4` |
| Every approved run out | `pma push --all-approved` |

`stage`, `commit` and `push` need the clone at the run's base. If you committed since, they refuse; `pma pr` still works.

`push` sends the run's commit only, never your other local commits.

## Throughput

### Batch, then review in one sitting

```sh
pma dispatch --auto -n 4 --detach --worktree
pma watch                       # optional
# later
pma review
```

Reviewing six runs in one sitting costs less attention than six interruptions. Set `notify` so you know when a job ends:

```sh
pma config notify 'osascript -e "on run a" -e "display notification (item 1 of a)" -e "end run" "$PMA_JOB_SUMMARY"'
```

Pass the summary as an argument, as above. It contains task text, which must not be parsed as AppleScript or shell.

### Let clean runs through

`--push` on `dispatch` and `run` pushes every run whose gates are clean. `D` in the TUI does the same. A run is not clean if any of these hold ([`accept.rs`](../crates/pma-core/src/accept.rs)):

- the check failed, or the project has no verify command;
- the run is a bundle;
- the task's class needs judgement (classes C and D; A is mechanical work such as deps);
- it changed a path outside its class, or a file that implements the check (`Makefile`, `conftest.py`, ...);
- the agent committed, was denied an action, or overspent `agent_budget`;
- it changed nothing.

Clean means nothing `pma` checks objects. It does not mean the change is correct. Use `--push` only on projects whose check you trust.

### Work in the TUI

`pma tui` (or `pma web`) keeps the loop on one screen.

1. `1` inbox: runs to review sit at the top.
2. `Enter` a run, `v` the diff, then `a`, `w`, `x` or `u`.
3. A decision opens the next ready run.
4. `O` jumps to a project; `Space` marks tasks; `d` dispatches the marked ones.
5. `#` scopes every tab to a tag.
6. `E` opens the selected row's project in `$VISUAL`, else `$EDITOR`. When the editor exits, `pma` re-reads the project, so an unstaged fix or a ticked item shows at once. TUI only.
7. `S` opens a shell in the project: a herdr pane when integrated, else this terminal. The project is re-read when the shell exits. `pma shell <project>` does the same from the command line.

On the projects tab, `d` or `I` on a project row dispatches the task in its `next for an agent` column. `Tab` to the `Failing CI` list, shown while a workflow fails, dispatches `project:ci` instead. Mark several rows and press `I` once: each project gets its own herdr session, opened in the background. `H` on a run brings its pane forward.

### Portfolio sweeps

`pma run` runs one named job across many projects. Bulk selections stop at a plan until `--yes`.

```sh
pma run fix-ci --all --dry-run
pma run fix-ci --all --yes
pma run todo-audit --tag rust --yes     # prune items already done
pma run doc-drift --tag work --yes
```

For one change across many repositories, use a campaign:

```sh
pma campaign add ci "add a CI workflow" --projects a --projects b
pma campaign run ci
```

For multi-step work (review, confirm, fix), use a workflow. `pma workflow library` lists nine. Each run prints a priced plan and spends nothing until you approve it by id.

### Skip the agent

Some tasks take less time to do than to dispatch and review: a typo, a version bump, a one-line config change. Do those by hand. `E` on the task's row, or `S` for a shell, make the edit, tick the item in `TODO.md`, and commit. A run costs a check before and after, plus your review; a hand edit costs neither.

## Measure, then adjust

```sh
pma report --by project     # also --by class, --by agent
```

Watch three numbers:

- **First-attempt pass rate.** Low in one project: its check or its task text is weak.
- **Cost per accepted run.** High on simple tasks: route them to haiku.
- **Review minutes.** Measured from first reading to decision. Record your own with `t` or `--minutes` when you reviewed outside `pma`.

## When something refuses

| Message or symptom | Cause | Action |
|-|-|-|
| Another command holds the lock | one agent-running command at a time | wait, or `pma job stop` |
| `stage` refuses | the clone has staged changes | commit or unstage yours first |
| `stage`/`push` refuses, base moved | you committed since the run | `pma pr 4`, or `pma close 4` and redo |
| `reject` refuses in the clone | you committed, or edited the same lines | `pma close 4`, revert by hand if needed |
| Run fails on a hook or git setting | the repository's git config changed during the run | restore it, or `--reject --keep-git-changes` |
| Worktree dispatch cannot find the item | item not pushed to the remote `TODO.md` | push `TODO.md` first |
| Item not offered to `--auto` | no `#agent` tag, or attempts used | tag it, or `--retry` |
| CI reads unknown | `gh` missing or not logged in | `gh auth login` |
