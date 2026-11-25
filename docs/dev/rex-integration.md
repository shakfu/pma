# Integrating with Rex

Design note, 2026-11-25. `pma integrate rex` runs agents in [Rex](https://www.superlogical.com/rex/docs/automate/server-api), a terminal multiplexer from Superlogical. Rex is the next-generation backend. herdr and Herdr.app stay; the three exclude each other, as `integration` takes one value. Measured against the Rex server 0.1.0-preview.21 (API revision 18) and Rex.app 0.1.0-preview.34 on macOS.

## What Rex is

A server owns sessions, windows (tabs) and blocks (panes), and outlives the app. Clients reach it over a Unix socket, `~/Library/Application Support/rex/server.sock`. The `rex` CLI wraps the socket and prints JSON with `--json`; `rex api list` prints every method (89 here). The CLI is not on `PATH` by default. The server's copy is at `~/Library/Application Support/com.superlogical.rex/RexServer/Versions/Current/Rex Server.app/Contents/MacOS/rex`, and each pane names that directory in `REX_BIN_DIR`.

Each pane gets:

| Variable | Value |
|-|-|
| `REX_SERVER` | `unix:///Users/sa/Library/Application%20Support/rex/server.sock` |
| `REX_SESSION`, `REX_BLOCK` | the pane's session and block IDs |
| `REX_BIN_DIR` | the CLI's directory |
| `TERM_PROGRAM` | `rex` |

`rex skill setup` installs a skill (`~/.claude/skills/rex`) and a `SessionStart` hook (`rex __internal session-start --agent claude`) into `claude`.

## 1. Environment hygiene

This mirrors herdr's case (`herdr-integration.md`, section 1). A dispatched agent inherited `REX_*` from a `pma` started in a Rex pane. It could then run `rex send -b <pane> 'git push'` into a pane that holds credentials. Removing the variables is not enough: `rex ls` with no `REX_SERVER` found the default socket. `REX_SERVER=unix:///nonexistent/rex.sock` made it fail without starting a server.

`agent::restrict` removes `REX_SESSION`, `REX_BLOCK`, `REX_BIN_DIR` and `REX_CLIENT`, and sets `REX_SERVER` to a dead path. The `SessionStart` hook then fails, and its `|| true` hides that.

## 2. Runs as Rex tabs

`rex.sessions` picks the layout. `single`, the default, is one `pma` session with a tab per run titled `<project>: <task>`. `project` is one `pma: <project>` session per project, as for herdr's workspaces and Herdr.app's spaces. The task is cut to 40 characters before the project is put in front, so the project is never cut.

`single` is the default over `project` because Rex's sidebar lists sessions, and a session per project stays after its runs are reviewed. The cost is that finished runs awaiting review collect in one tab bar, and one project's tabs cannot be closed together. Every run's follower also reads the whole session's events. That is negligible at 10 runs and unmeasured at 50.

`pma` drives the CLI, not the socket, and always passes `--autostart=false`, so it never starts a server.

| pma | Rex |
|-|-|
| `check` | `rex whoami --json`; `api_revision` 18 or later |
| session by label | `rex ls --json`, matched on `label` |
| first run in a session | `rex new <label> --window <title> -c <cwd> --json -- <argv>` |
| later runs | `rex window new <title> -s <session> -c <cwd> --focus=false --json -- <argv>` |
| `close` | `rex block close <block>` |
| `focus` | `rex focus <block> -s <session>`, then `rex -C <app> do session.select session_id=<session>` for each connected app |
| closed | `rex events -s <session> --json`: `block_closed`, or `block_event` `child_exited`, for the run's block |
| still there | `rex block inspect <block> --json`; `not found` means gone |
| state | `block_event` `program_status_changed`, `.record.state` |

A pane is stored as `rex:<session>/<block>`. `rex events -s` refuses a block ID, so the session must be kept. `close`, `focus` and `follow` pick the backend from that prefix, so a run outlives a change of setting.

Measured:

- A block runs argv directly, with the login shell's environment, and closes when it exits. `argv` is wrapped as for Herdr.app (`zsh -lic '"$@"; exec "$0" -li'`), so a shell stays for reading the output, as in the other two backends. `--keep-open` leaves a pane that takes no input.
- `rex wait <block>` exits with the program's code, and `child_exited` carries `exit_code`. The status file stays anyway: a headless run's pipeline ends in `pma pane-view`, whose code is not the agent's.
- `window new --focus=false` leaves the session's active window as it was, and does not switch the app.
- `rex events` sends no snapshot. `follow` checks the block after each subscription ends. A close between starting the subscriber and its connecting is not seen. The status file and the timeout end such a run anyway.
- `session.select` fails unless Rex.app has Remote Control on (Settings, General): "Rex.app is not accepting actions". `focus` then selects the tab within the session and returns that error, which callers print as a warning.

## 3. State through OSC 7501

Rex keeps per-terminal status records that programs report with the [Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status.md): `ESC ] 7501 ; state=<s>:app=<name> ESC \`, with `state` one of `idle`, `working`, `done`, `blocked`, `error` or `clear`. Rex.app shows them and raises its own activity notices, so `pma` sends no notice, as for Herdr.app.

A headless run's wrapper reports `working` before the script and `done` or `error` after it, read from the status file. This takes the place of herdr's `pane.report_agent`. `done` and `error` survive the next prompt; `working` does not. `follow` passes each record's state to `on_state`, with `error` read as `done`, since herdr has no `error` and the verdict comes from the status file.

Captured:

```json
{"type":"session_event","payload":{"session_id":"session:...","event":{"type":"block_event","payload":{"block_id":"block:...","creator_name":"com.superlogical.terminal","name":"program_status_changed","payload":{"id":"","record":{"state":"blocked","kind":"permission","app":"pma","effective_app":"pma","updated_at":"...","owner":{"pid":37563,"name":"bash"}},"reason":"report"}}}}}
```

Untested: whether `claude` or another agent reports OSC 7501 itself in an interactive run; nothing in Rex's docs says so.

## Not built

- **Actions.** A `rex.action` in Rex's `init.lua` can put `pma next`, `pma tui` or `pma review` in the palette and on a key ([actions](https://www.superlogical.com/rex/docs/customize/actions)). Config actions have 30 seconds, so long work belongs in a window. Deferred by the user.
- **Remote hosts.** `rex -S ssh://host` reaches another machine's server. Paths and the status file are local to `pma`, so this does not apply as is.
- **Detection.** As for herdr-app, `pma integrate` reports a running Rex server but never switches to it.

Against a live server: the ignored test `rex_live_round_trip` (`cargo test -p pma-core -- --ignored rex`), and `make rex-check`, a dispatch through a scratch store. `make rex-manual-check` asks a person at Rex.app to confirm an interactive run, `H` and take-over, each in front.
