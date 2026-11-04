# Views and panes: justification and classification

Status: written 2026-11-03, against the tree after `1029b30`. Follow-ups:
`usage-log.md` (measuring use) and `views-refactor.md` (the proposal). Covers the eight
tabs on `1`-`8`, the project view, and the details opened from a row. TUI and
web show the same panes (`pma_core::screen`), so one entry covers both.

## Evidence

Usage figures come from the author's store (`~/.config/pma/projects.db`),
read 2026-11-03. They span 9 days and one user, so they rate frequency for
that user only. Colleagues' use is not measured.

| Measure | Count |
|-|-|
| Runs, 2026-10-26 to 2026-11-03 | 21 (15 pushed, 6 rejected), over 12 projects |
| Jobs | 43: dispatch 23, scan 6, shell 5, push 4, sync 2, commit 2, verify 1 |
| Jobs naming a `project:ci` target | 13 of 43 |
| Scans | 57 |
| Projects / absent / tiered / tagged | 119 / 5 / 0 / 8 |
| Open tasks | 1,335; matrix Q1 0, Q2 0, Q3 1, Q4 1,342 |
| Notes, campaigns, workflow instances, workflow revisions, routes, presets | 0 each |
| Agents / stored config rows | 5 / 2 |

The October review (`261101-review.md`, section 5) found the last five
dispatches came from a project view or by target, not from the inbox.

## Scales

Frequency, per working session:

- **F1** every session: opened, or in view, most times `pma tui` runs.
- **F2** most days: when a run or a CI failure happens.
- **F3** weekly or on demand: answers a question asked now and then.
- **F4** setup: when configuration changes.

Importance:

- **I1** the dispatch-review-publish loop needs it; no other pane shows it.
- **I2** saves a trip to another tab or to the CLI; the data exists elsewhere.
- **I3** optional: the CLI covers it, or the pane is empty under current use.

## 1 Inbox

Purpose: what needs a decision now, across every project. The default tab.

- **Waiting on you.** Runs in `ready`, `failed`, `approved`, `staged`,
  `committed`, `pr-open`, and jobs in `awaiting-approval`. It is the only
  cross-project list of pending decisions. Review is the slowest human step
  (median 256 s, ready to decided), so the list that starts it belongs on the
  first tab. Acts: `a` `x` `u` `m` `p` `X` `R` `w` `v` `T` `H`.
- **Working now.** Running agents and queued or running jobs, with each
  agent's last tool call. It answers "is anything under way" without leaving
  the inbox. It repeats the under-way rows of Activity.
- **Tasks.** Tasks an agent may take (`#agent`), then critical or urgent tasks
  only a person may take. Justified only if tasks are tagged: 8 of 1,335
  carry `#agent` or `#manual`, so it holds 3 rows. Its future is the open
  question in `261101-review.md` section 5.

## 2 Projects

Purpose: every project, worst state first; where a project is opened from.

- **Projects, worst first.** One row per present project: tier, tags, state,
  open count, idle days, CI, deps, local changes, TODO.md lint, next task for
  an agent. It is the portfolio overview and the place a set is formed: `/`,
  `*`, then `e`, `z`, `d` or `I` over the marks. `O` now reaches one project
  faster, so this pane's job is the set, not the single lookup.
- **Failing CI.** One row per failing workflow on a default branch, with its
  run link. `d` or `I` dispatches `project:ci`. 13 of 43 jobs name a `:ci`
  target, so this pane is a main source of dispatches. The `ci` column of the
  pane above shows the same fact without the workflow name or the target.
  Shown only while a workflow fails (2026-11-04).
- **Changed at the last scan.** What the latest scan found new, gone, failing,
  mended, opened or closed. It shows what moved since you last looked, which
  the worst-first order does not. The same data is `pma history last`.
- **Recent scans.** One row per scan, with counts per fault. Enter opens the
  scan. It answers "when did this start", and it explains the pane above.
  `pma history` gives the same rows.

## 3 Activity

Purpose: everything under way, and every run that finished.

- **Activity.** Running agents and jobs from the last 3 days, under way
  first. Acts: `s` stops a job, `a` approves a plan, `H` opens the pane.
- **Workflow instances.** Open and recent instances; `r` resumes, `s` stops,
  `f` starts. 0 instances exist. Kept because workflows are a built feature
  and this is their only front-end list.
- **Finished runs.** The newest 100 final runs, filtered by `b`. The only
  front-end record of what agents did and how it ended. It supports the
  question "did this change already land" before a re-dispatch, which run 9
  in the October review needed.

## 4 Matrix

Purpose: open tasks in four quadrants of urgency by importance.

- **Q1 Do right away, Q2 Schedule for later, Q3 Delegate or avoid, Q4
  Later.** Importance comes from tier and priority. Tier 5 makes nothing
  important (`rank.rs`, test `!important(5, Critical)`), and all 114 projects
  are untiered. So the matrix today is Q1 0, Q2 0, Q3 1, Q4 1,342. It is
  justified as the ranked view of tasks under tiers. With no tiers it shows
  one list in four panes, and the October review measured it as the one
  slow web screen.

## 5 Library

Purpose: the reusable definitions an agent runs from.

- **Workflow revisions.** Stored workflow documents, draft or active, with
  worst-case cost. 0 exist. Shows what `f` can start; `pma workflow` covers it.
- **Campaigns.** Named task texts over a set of projects; `r` runs, `a`
  adds, `x` removes. 0 exist. Tag sets and `z` over marks now cover the same
  case: one prompt over several projects.

## 6 Report

Purpose: how runs went, grouped by `b` (agent, project, class, and others).

- **Runs by X.** Counts, pass rate, cost and review time per group, with a
  total row. The header adds review speed. It is the only place cost and
  agent success are compared. It needs volume: 21 runs over 12 projects
  gives most groups 1 or 2 runs.

## 7 Notes

Purpose: free text that belongs to no project or TODO.md.

- **Notes.** `n` adds, `e` edits, `x` removes. 0 notes exist. A note that
  concerns a project belongs in its TODO.md, which `E` opens. Justified only
  for cross-project notes, which no record shows a need for.

## 8 Settings

Purpose: configuration without editing `settings.toml`.

- **Settings.** Every key, its value and its source (default, set, unknown).
  `e` edits, `x` resets. 2 keys are set. Its main value is showing defaults
  and flagging unknown keys.
- **Roots.** Directories a scan reads; `a` adds, `x` removes. Changes once.
- **Agents.** Workers and their fields; `e`, `x`, `u` makes one the default.
  5 agents exist, and the 2026-11-02 `interactive_args` work changed them.
  The most-used pane on this tab.
- **Presets.** Named worker, model and arguments. 0 exist.
- **Route revisions.** Policy documents that pick a worker per task class;
  draft, shadow or active. 0 exist.

## Project view (no key; `o`, `O`, or Enter on a row)

Purpose: one project's state, tasks and activity. The October review found
it is where dispatches start.

- **Summary line.** State and its reasons, CI, scan age, verify command,
  tags. It answers "can I dispatch here" before a key.
- **Open tasks.** The project's ranked tasks with priority, when, who may
  take them, and the issue. Acts: `d` `D` `I` `v` `Enter`. The main dispatch
  source.
- **Activity.** This project's runs and jobs, under way first, finished runs
  after. It answers "has this been tried" and is where its runs are reviewed.
- **TODO.md.** Lint errors, warnings and prunable items, by line. `f` fixes
  or prunes. Needed because a lint error hides an item from ranking.

## Details (opened over a tab)

- **Run.** Fields, items, changelog, diff, gates. Approval requires the tree
  it showed. I1: review happens here.
- **Job log.** A job's output. Where a failed push or dispatch is read.
- **Task.** The placed task, its TODO.md text, signals and runs.
- **Scan, Route revision.** Records opened from their panes; F3 or F4.

Views with no tab (`Next`, `Runs`, `Jobs`, `Agents`, `Workflows`,
`Campaigns`, `Stale`) are CLI renderings. The 2026-10-26 merge folded them
into the tabs above (`frontends.md`, phase 4).

## Classification

| Pane | Tab | F | I | Basis |
|-|-|-|-|-|
| Waiting on you | Inbox | F1 | I1 | only cross-project decision list |
| Run detail | - | F1 | I1 | review and approval |
| Open tasks | Project | F1 | I1 | where dispatches start |
| Activity | Project | F1 | I1 | review of this project's runs |
| Projects, worst first | Projects | F1 | I1 | way in to a project or a set |
| Working now | Inbox | F1 | I2 | repeats Activity's live rows |
| Summary line | Project | F1 | I2 | facts also in the projects row |
| Failing CI | Projects | F2 | I1 | 13 of 43 jobs are `:ci` targets |
| Job log | - | F2 | I1 | only front-end view of a failure's text |
| Activity | Activity | F2 | I2 | Inbox and project views show the live part |
| Finished runs | Activity | F2 | I2 | history before a re-dispatch |
| TODO.md | Project | F2 | I2 | `pma lint` covers it |
| Task detail | - | F2 | I2 | full text and runs of one item |
| Changed at the last scan | Projects | F3 | I2 | `pma history last` |
| Recent scans | Projects | F3 | I3 | `pma history` |
| Runs by X | Report | F3 | I2 | sole cost and success comparison; low volume |
| Agents | Settings | F4 | I2 | 5 agents, changed this week |
| Settings | Settings | F4 | I2 | defaults and unknown keys |
| Roots | Settings | F4 | I3 | set once |
| Tasks | Inbox | F3 | I3 | 3 rows of 1,335 tasks |
| Q1-Q4 | Matrix | F3 | I3 | degenerate without tiers |
| Workflow instances | Activity | F4 | I3 | 0 instances |
| Workflow revisions | Library | F4 | I3 | 0 revisions |
| Campaigns | Library | F4 | I3 | 0; tags and `z` cover it |
| Notes | Notes | F4 | I3 | 0 notes |
| Presets | Settings | F4 | I3 | 0 presets |
| Route revisions | Settings | F4 | I3 | 0 routes |
| Scan, Route revision detail | - | F4 | I3 | records |

By tab: Inbox, Project and Projects hold every F1/I1 pane. Activity is F2.
Report, Matrix, Library, Notes and Settings hold no I1 pane and no F1 or F2
pane.

## Findings

1. **Six panes are empty under current use**: workflow instances, workflow
   revisions, campaigns, notes, presets, route revisions. Each backs a built
   feature, so each is justified as the feature's only front-end list.
   None is justified by use.
2. **The matrix depends on tiers nobody set.** Either tier projects, derive
   importance from priority alone when every project is untiered, or drop
   the tab to a key. As built it costs a tab for one list.
3. **Overlap.** Inbox `Working now` repeats Activity's live rows; the
   projects `ci` column and `Failing CI` show one fact twice; `Changed at the
   last scan` and `Recent scans` repeat `pma history`. The first two overlaps
   save a tab switch on F1/F2 paths, so they earn their place. The scan
   panes put two F3 panes on an F1 tab.
4. **Inbox `Tasks` is the weakest I1-tab pane.** It holds 3 rows because
   tagging is rare. Options: drop it, or fill it from the `#` scope's top
   tasks. Still the open question from 2026-11-01.

## Another way to group the panes

The tabs group panes by kind of record. An alternative groups them by step
in the loop:

- **Find:** Projects, worst first; Failing CI; project Open tasks; Inbox
  Tasks; Matrix; `O`.
- **Run:** Working now; Activity; Workflow instances.
- **Review:** Waiting on you; Run detail; Job log.
- **Learn:** Finished runs; Report; Changed at the last scan; Recent scans.
- **Configure:** Settings tab; Library; Notes.

Under that grouping every F1/I1 pane sits in find, run or review. 7 of the
11 I3 rows fall into learn or configure; the other 4 are Inbox Tasks, the
Matrix, Workflow instances and the Scan and Route revision details. That
suggests three primary tabs and a secondary set, against eight equal ones
now. The data above is one
user over 9 days, so this is a hypothesis to test with colleagues before any
tab moves.
