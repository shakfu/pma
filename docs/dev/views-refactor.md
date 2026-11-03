# Views: refactor proposal

Status: proposal A built 2026-11-03, on the user's decision, before the
usage log had data. Rests on `views.md`.

Where the build departs from the proposal below:

- Rule 1 was already true in the TUI: an empty pane took 3 rows, its
  borders and its hint. Only the web's empty pane was tall (28 px padding);
  it now uses the one-line style of the sidebar's empty list.
- Setup's `r`, `a` and `f` (add root, make default, propose route) fit any
  row, so a campaign row could not also take `r` and `a`. Run campaign is
  `z`, as a run is elsewhere, and add campaign is `C`. Start workflow stays
  on activity only.
- The matrix keeps key `6` always. Without a project above tier 5 it shows
  one pane whose hint names `pma project tier`.
- `Query::Status` still loads scans and changes: `pma status --html` puts
  the history tab's two scan panes before the project list.
- The CLI's composite views are unchanged; `pma report` reads
  `Query::Report`, the history tab `Query::History`.

## Problem

- 8 numbered tabs. Every F1 or I1 pane is on 3 of them (Inbox, Projects,
  project view). Report, Matrix, Library, Notes and Settings hold no F1,
  F2 or I1 pane.
- 6 panes are empty under current use: workflow instances, workflow
  revisions, campaigns, notes, presets, route revisions. Each shows its
  empty hint at full pane height.
- The matrix shows Q1 0, Q2 0, Q3 1, Q4 1,342, because tier 5 makes nothing
  important and no project is tiered.
- The Projects tab carries two F3 panes (`Changed at the last scan`,
  `Recent scans`), which take rows from the pane you came for.
- Inbox `Tasks` holds 3 rows of 1,335 tasks.

## Proposal A: five tabs by step

| Key | Tab | Panes | From |
|-|-|-|-|
| `1` | inbox | Waiting on you; Working now; Notes, when any exist | Inbox, Notes |
| `2` | projects | Projects, worst first; Failing CI | Projects |
| `3` | activity | Activity; Workflow instances; Finished runs | unchanged |
| `4` | history | Runs by X; Changed at the last scan; Recent scans | Report, Projects |
| `5` | setup | Agents; Settings; Roots; Presets; Route revisions; Workflow revisions; Campaigns | Settings, Library |
| - | project | unchanged | |
| `6` | matrix | Q1-Q4, bound only while a project has a tier | Matrix |

Rules that go with it:

1. **An empty feature pane collapses to one line.** A pane whose feature
   has no record shows its hint as a single row, not a full box. Applies to
   the 6 empty panes and to Notes. The feature stays reachable; it stops
   taking screen rows.
2. **Inbox `Tasks` goes.** The project view and `O` are where dispatches
   start (`261101-review.md` section 5). If you want a cross-project task
   list, the alternative is to fill it from the `#` scope only while a scope
   is set.
3. **The matrix needs tiers.** With no tiered project, `6` says so and
   names `pma project tier`, instead of showing four panes. The alternative,
   importance from priority alone when nothing is tiered, changes ranking
   for `pma next` and `pma matrix` too; it is a separate decision.
4. **Agents first in setup.** It is the one setup pane with recent use
   (5 agents, changed 2026-11-02).

Keys `1` to `3` keep their meaning, so the F1 paths do not change. `4` to
`8` change; `docs/cheatsheet.md`, `docs/guide.md` and the README's "1-8
switch tabs" change with them.

### Code impact

- `keys.rs`: `Tab` loses `Report`, `Library`, `Notes`, gains `History`,
  `Setup`; `Tab::query`, `BINDINGS` and the tab lists (`PANES`, `MARKS`,
  the `Enter` and `o` tab sets) follow. Acts bound to `Library` and `Notes`
  move to `Setup` and `Inbox`. The one-meaning test then checks the new
  pairs: `x` on setup must not mean both remove-campaign and remove-root
  for one pick; it does not, since `fits` splits them by pick.
- `api.rs`: `Query::History` (report rows plus scans and changes) and
  `Query::Setup` (settings plus revisions and campaigns). `Query::Status`
  stops loading scans and changes for the front ends; `pma status --html`
  keeps them.
- `screen.rs`: `changes_pane` and `scans_pane` move from `View::Status` to
  `View::History`; `revisions_pane` and `campaigns_pane` join
  `View::Settings`. A `Pane` gains `collapse: bool`, set when its feature
  has no record; the TUI gives it a height of 1, the web one row.
- `pma-tui`: `heights` honours `collapse`; `Matrix` key handling as rule 3.
- `pma-web`: the tab bar comes from the bindings already; the sidebar's
  scope and act groups need no change. Your `make web-check` covers it.
- Tests: tab and binding tests in `keys.rs` and `screen.rs` change; new
  tests for rule 1 and rule 3.

Estimate: 400 to 600 changed lines, most in `screen.rs` and tests.

### Risks

- Your colleagues' muscle memory for `4` to `8`. 1 to 3 do not move.
- `Finished runs` could sit in history instead of activity. It stays in
  activity because it is read before a re-dispatch, an F2 path.
- Merging Library into Setup puts definitions you run (campaigns,
  workflows) beside configuration. That is a fit only while they are rare.
  If workflows become common, Library returns as a tab.

## Proposal B: the project view as home

The 2026-11-01 review's reading: the product is the project view. Then:

- `pma tui` opens on the last project used, or the chooser `O` opens.
- A one-line strip on every screen shows the inbox count: runs waiting,
  plans waiting, agents running. `1` opens the inbox in full.
- The projects tab becomes the chooser's full form and the set builder.

B changes fewer panes but changes the start of every session. It fits a
user who works in a few projects; A fits one who works across many. The
usage log's `project` and `tab` events, by `via`, tell them apart: if most
project opens come from `O` and most tab opens are `1` to check the
inbox, B fits.

## Order

1. Build the usage log (`usage-log.md`). Collect 14 days, two users or more.
2. Rule 1 (collapse empty panes). It is independent of A and B and changes
   no key.
3. Decide A or B from the data, by the rule at the end of `usage-log.md`.
4. Rules 2 and 3 with whichever is chosen.
