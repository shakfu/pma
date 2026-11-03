# Usage log: scope

Status: built 2026-11-03, on by default. Feeds the F column of `views.md`.

Where the build departs from the scope below:

- `--json` is not built; the file is JSON lines already.
- `pma usage` lists each tab's panes by rendering every tab's query now.
  The inbox query settles runs whose process died, as opening the tab does.
- The project view's panes are listed only once an event names them: the
  view needs a project.
- A page's event is checked at `/api/usage`: a known kind other than
  `act`, a real tab, a name of 1 to 32 characters from `[A-Za-z0-9_/#-]`.
- No route-level web test: `pma-web` has no request harness. The check
  is unit-tested; your `make web-check` drives the page.

## Question it answers

Which tabs, panes and details a person opens and acts in, per front end, per
day. `views.md` rates frequency from the store's records. Records show what
was created, not what was looked at: 0 notes says the Notes tab made nothing,
not that nobody opened it.

The `jobs` table already records every long act (43 rows). It does not
record short acts, tab opens, detail opens or the front end used.

## Events

One line per event. Nothing typed and no project name is recorded.

| `ev` | When | Fields |
|-|-|-|
| `tab` | a tab is chosen by key or click; not a refresh | `tab`, `via` (`key`, `click`, `back`) |
| `project` | the project view opens | `via` (`o`, `O`, `enter`, `sidebar`) |
| `pane` | focus moves to a pane (`Tab`, `h`, `l`, click) | `tab`, `pane` |
| `open` | a detail opens | `kind` (`run`, `job`, `task`, `scan`, `route`, `campaign`) |
| `act` | an act is sent, after its confirm | `tab`, `pane`, `act` (`Act` name), `ok` |
| `find` | `/`, `O` or `#` is used | `tool` |

Common fields: `at` (unix seconds), `fe` (`tui` or `web`). `pane` is the
pane title. Titles with counts (`Finished runs: ..., newest 100 of 212`,
`Scan #57`, `Q1 Do right away`) are recorded by a fixed key, not the title.

Left out:

- Selection moves (`j`, `k`): about 10 per act, and they say nothing a pane
  focus or an act does not.
- Refresh ticks: they are not a person's choice.
- Dwell time. A TUI left open overnight would dominate it. Add later, capped
  at a few minutes since the last key, if counts prove too coarse.
- The CLI. Its subcommands could be counted at one point in `main.rs`; that
  would test the "CLI covers it" claims in `views.md`. Phase 2.

## Storage

`<state>/usage/YYYY-MM.jsonl` under `store::state_home()`, appended with
`OpenOptions::append`, one write per event.

A table in `projects.db` is rejected: `Watch::poll` compares the store's
`data_version`, so a usage write from the web page would make every open
TUI reload, and the reverse. A separate SQLite file avoids that but adds a
schema for append-only data.

Size: about 100 bytes an event. At 500 events a day, 1.5 MB a month. One
file a month makes deleting old data a file removal.

## Hook sites

- **TUI** (`crates/pma-tui/src/tui.rs`). `App::key` already returns a
  `Request` and stays store-free for its tests. Add `App::events:
  Vec<Event>`, pushed inside `key` for `Command::Tab`, `NextPane`,
  `PrevPane`, `Open`, `OpenProject`, `Jump`, `Filter`, `Scope` and each
  `Act` sent. The run loop drains it after each key. Refresh loads are
  built in the loop, not by `key`, so they are not counted.
- **Web** (`crates/pma-web/assets/app.js`). Tab, pane, open, project and
  find events: `POST /api/usage` from the handlers for those keys and
  clicks. `load(ifChanged)` with `ifChanged` true is a refresh and sends
  nothing. Acts: `/api/act` records them server-side, since it has `tab`
  and `act`. The page adds `pane` to its body.
- **Core** (`crates/pma-core/src/usage.rs`, new). `Event` (serde),
  `record(&Event)`, and `summary(since) -> Vec<Row>`. Both front ends call
  `record`. A failed write is ignored: losing an event must not fail a key.

## Reading it

`pma usage [--since 30d] [--json]` prints:

1. Tab opens per day, by front end.
2. Per pane: focuses and acts.
3. Per act: count and failures.
4. Panes and tabs with zero events in the period, listed by name.

Item 4 is the input the refactor needs. A colleague can paste the plain
output; it holds no project names.

## Setting

`usage_log = on | off` in `settings.toml`. Default `on`: the file stays on
the machine, and an off default would leave the colleagues' data empty.
This is your call. The alternative is `off`
with a one-line note in the README asking testers to turn it on.

## Tests

- `usage.rs`: an event round-trips; `summary` counts and lists zero panes
  from a fixture file.
- `tui.rs`: a `2` key yields one `tab` event; a refresh `Load` yields none;
  an act yields one `act` event with its pane.
- `pma-web` tests: `/api/act` appends one line; `/api/usage` refuses an
  unknown `ev`, and a body over the 64 KB cap as today.
- The web key paths are covered by your `make web-check`.

## Cost

About 250 lines: 120 in core, 50 in the TUI, 40 in the web, 40 in the CLI
command. No schema change, no dependency.

## Decision rule for the data

After 14 days, with at least two users:

- A pane with no focus and no act, on a tab opened fewer than 2 times a
  week, moves to a secondary tab.
- A tab with fewer than 1 open a week, and no I1 pane, loses its number key.
- A pane acted on more often than its tab is opened is in the wrong place:
  the acts start elsewhere (for example from `O`).
