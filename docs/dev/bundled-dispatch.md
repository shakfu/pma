# Bundled dispatch and project prompts

Status: built, 2026-10-28.

Where the build departs from the design below:

- The key is `bundle`, not `bundle:<n>`. Nothing reads a bundle's number from its key.

- `run_items` has `n` and `dropped INTEGER NOT NULL DEFAULT 0`, not `done`. An item is ticked unless dropped, so an approval without `--drop` ticks every item.

- `--drop` takes the item numbers `pma review <id>` prints and the prompt uses, not TODO.md lines. A line goes stale when the file is edited; the number does not.

- `pma stage` takes no `--drop`, and refuses a ready bundle, as commit, push and a pull request from a view do (`dispatch::needs_approval`). Approve with `--drop`, then publish.

- Step 2 came with step 1. Without the clean-run exclusion, `--push` would tick every item of a bundle.

- A bundle holds TODO.md items only. A signal named beside them keeps its own run.

- `Ask::Check` serves every approval from the TUI and the web. A run with no items gives an empty list, which asks nothing, so a plain run is approved as before. The input is the numbers left unchecked, as `--drop` takes them. A re-approval starts with the earlier choice.

- `pma task <project:line>` does not list a bundle among the runs holding the item. The busy check does see it.

- A prompt's key is `prompt`, not `prompt:<n>`, as a bundle's is `bundle`. Its whole text is kept in `runs.description`, which `--retry` dispatches again. Its attempts count against its title, the first line cut at 80 characters.

- Marks are a front end's, per view: a `Pick::Tasks` built from them is the pick `d` acts on (`screen::marked`). They clear on a dispatch, on Escape and on another view. A refresh drops a mark whose row is gone, so nothing unseen is dispatched. Retried when any marked task would be on its own.

## Problem

A common session: open a project, see 5 High and 2 Medium items, ask one agent to "fix the highs and the top two mediums". pma cannot express this.

- The TUI and web dispatch one task: `Act::Dispatch` fits `Pick::Task` only (`crates/pma-core/src/screen.rs:143`).

- The CLI expands `proj:high`, `proj:q1` or a checked list (`crates/pma-tui/src/choose.rs:89`) into one run per task.

- The default workspace is the clone (`crates/pma-core/src/config.rs:144`), and a clone holds one run (`clone_busy`, `crates/pma-core/src/dispatch.rs:729`). `pma dispatch proj:high` with 5 items queues 1 run and refuses 4.

- In worktree mode the 5 runs overlap in files and need 5 reviews.

- A run carries one item: `runs.task_key`, `text`, `gh`. Publishing ticks one item (`todo::mark_done` in `crates/pma-core/src/publish.rs`, three sites).

A second need: a prompt that is not a TODO.md item, such as "audit error handling".

## Decisions

1. Several tasks of one project, named in one dispatch, become one run: a bundle. This is the default in both workspaces. A bundle has one workspace (the clone, or one worktree and branch), one agent session and one diff. `--separate` restores one run per task.

2. At approval the reviewer sees the bundle's items, all checked, and unchecks those not done. Only checked items are ticked and closed.

3. This document precedes any code.

## Design

### 1. Bundles

**Store.** A new table, one row per item:

```sql
CREATE TABLE run_items (
    run INTEGER NOT NULL REFERENCES runs(id),
    key TEXT NOT NULL,
    text TEXT NOT NULL,
    gh INTEGER,
    done INTEGER,          -- NULL until approval; 1 checked, 0 unchecked
    PRIMARY KEY (run, key)
);
```

A single-task run writes no rows and is read as today. A bundle's `runs.task_key` is `bundle:<n>`, so code that reads one key keeps working. `without_item` treats `bundle:` as itemless, and each tick site then reads `run_items` instead. `runs.text` is the item texts joined with `; `, which `revision` and `attempt_key` already read.

**Grouping.** `run_dispatch` groups picks by project. A project with 2 or more picks yields one bundle pick; one pick stays a plain run. Targets over 2 projects yield 2 runs. `--auto` is unchanged: it draws one task per run across the portfolio, which is a different policy.

**Origin check.** `prepare` checks each item against origin, as `on_origin` does for one. A done or absent item is dropped with a warning, as a selector passes over a blocked task today. A bundle with no items left is refused. One left becomes a plain run.

**Prompt.** One `Task:` section per item, each with its description lines, numbered. The agent ends with its summary per item number. The summary informs the reviewer; it does not decide what is ticked.

**Busy check.** `has_run` and `blocked` (`crates/pma/src/main.rs`) also match an open run's `run_items`. A task inside a running bundle cannot be dispatched again.

**Approval.** `approve` takes the checked keys and writes `run_items.done`. Unchecked items stay open in TODO.md.

- CLI: `pma review N --approve --drop 31,40`, by TODO.md line at the last scan, or `--drop <key>`.

- `pma stage` takes a ready run without approval (`crates/pma-core/src/publish.rs`, "staging is the review"), so it takes `--drop` too.

- TUI and web: a new `Ask::Check` kind, a checklist with every row on. Both front ends build it. The approve binding uses it for a bundle and `Ask::Confirm` otherwise.

**Unattended paths.** A bundle never counts as clean. `--push` and batch approval (`ApproveRuns`, `Pick::Run::clean`) pass over it, because no reviewer has chosen which items are done.

**Publishing.** The three `mark_done` sites tick every checked item. `message` writes one `Closes #N` per checked item with an issue. The subject is prefilled with `<k> TODO items` and edited with `m` as today; the body lists the checked item texts.

**Reject, rework, retry.** Reject reverts the whole run. Rework keeps the item list. `--retry` on a failed bundle re-dispatches its items from `run_items`, still subject to the origin check.

**Front ends.** Space marks a task row in the TUI tasks list; the web gets a checkbox per task row. `d` with marks sends `Action::Dispatch { targets }` with one target per mark. The action already takes a list (`crates/pma-core/src/api.rs:394`), and the CLI does the grouping, so the front ends need no bundle logic. `project_of` (`api.rs:1781`) must return the project when every target names the same one, so the job shows in that project's view.

### 2. Project prompts

`pma dispatch proj --prompt "<text>"`. In the TUI and web, `P` in a project view asks for the text with `Ask::Text`.

- The key is `prompt:<n>`; `without_item` covers it, as it covers `campaign:`. No origin check, nothing ticked.

- `runs.text` is the first line, at most 80 characters. The details are the whole prompt.

- Verify, review, approve and publish are unchanged. The attempt limit counts against the text, as for any task.

- The prompt rule "Do not edit TODO.md" stays. A prompt that asks for work on TODO.md items should be a bundle.

`Ask::Text` is one line. A multi-line prompt needs the multi-line editor the commit-message item in TODO.md already names.

## Defaulted, open

Not answered on 2026-10-28; these defaults hold until changed.

- **Attempts.** A failed bundle counts against the bundle's text, not each item's. A failed bundle says little about any one item. Dispatch checks the bundle's count after grouping, so the same items stop at `ATTEMPT_LIMIT` until `--retry`.

- **Budget and timeout.** Not scaled with item count. A bundle is capped at 8 items; a project named with more is passed over with a warning, and the other targets run. Revisit after 10 bundled runs, from their cost and time against single runs.

- **Ordering.** No `proj:medium:2` selector. "Top two" is chosen by hand with marks. A selector needs a defined order (file order or `rank::place`), and marks need none.

## Known limits

- Unchecking an item leaves it open in TODO.md; its changes stay in the diff. A bundle is approved, reworked or rejected whole. To drop one item's partial work, send rework feedback asking the agent to revert it. Per-item diffs would need one commit per item, which the prompt forbids.

## Failure modes to test

- A bundle item is ticked on origin between scan and dispatch: dropped, the rest dispatched.

- Every item is unchecked at approval: refused. Reject the run instead.

- `--drop` names a key not in the bundle: refused, listing the bundle's items.

- TODO.md was edited during the run so an item's line moved: `mark_done` matches by key and text, as today.

- Two marked tasks in two projects: two runs, each project's clone busy check applies separately.

- A bundle in clone mode while the clone holds a run: refused once, not once per item.

## Build order

1. `run_items`, grouping in `run_dispatch`, the per-item origin check, the prompt, the tick sites and `message`. CLI only, approval by `--drop`. Tests in `crates/pma/tests/cli.rs`.

2. Busy check and the unattended-path exclusion.

3. Marks in both front ends and `project_of`.

4. `Ask::Check` in both front ends.

5. `--prompt` and `P`.
