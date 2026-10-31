# Integrating with herdr

Design note, 2026-10-29. Whether `pma` should integrate with [herdr](https://github.com/herdrdev/herdr), and how. Checked against herdr 0.9.5 on this machine (protocol 22). Sections 4 and 6 are built; the recommendation below predates them.

## What herdr is

A terminal multiplexer for coding agents: one Rust binary, Apache-2.0, pre-1.0. A background server owns workspaces (`w1`), tabs (`w1:t1`) and panes (`w1:p1`), and survives the client. It classifies the agent in each pane as `idle`, `working`, `blocked`, `done` or `unknown`. Clients drive it through a Unix socket with newline-delimited JSON (`herdr api schema --json` prints 130 methods) or through the `herdr` CLI, which wraps the socket and prints JSON.

Every pane gets these variables:

| Variable | Value here |
|-|-|
| `HERDR_ENV` | `1` |
| `HERDR_SOCKET_PATH` | `~/.config/herdr/herdr.sock` (mode 0600) |
| `HERDR_PANE_ID`, `HERDR_TAB_ID`, `HERDR_WORKSPACE_ID` | the pane's IDs |
| `HERDR_BIN_PATH` | `/opt/homebrew/bin/herdr` |
| `SSH_AUTH_SOCK` | `~/.config/herdr/herdr.sock.agent`, herdr's forwarded agent |

`herdr integration install` writes a state hook into each agent's own configuration. Here `claude`, `codex`, `opencode` and `pi` have one; for `claude` it is `~/.claude/hooks/herdr-agent-state.sh`, wired in `~/.claude/settings.json`.

## Where the two overlap

Both run coding agents and both persist across the terminal, but they own different things.

| | `pma` | herdr |
|-|-|-|
| Agent mode | headless (`claude -p`, `stream-json`) | interactive TUI in a pane |
| Unit | a run: task, diff, verify, cost, review | a pane and its lifecycle state |
| Persistence | queue and runner (`pma job`) | server holding PTYs |
| Guarantees | no push credentials, verify by `pma`, nothing published unreviewed | none about what the agent may do |

herdr has no notion of a task, a diff or a verdict. `pma` has no live terminal. The integration points are therefore at the edges: environment hygiene, notices, and where `pma`'s own front ends run.

## 1. Environment hygiene (do regardless)

Done 2026-10-29: the six variables below are removed, and so are the control-socket variables of tmux, screen, zellij, kitty, WezTerm and Neovim.

`agent::restrict` (`crates/pma-core/src/agent.rs:20`) removes push credentials but not the herdr variables. When `pma` runs inside a herdr pane, which is the case for this session, every agent it spawns inherits them. Three consequences:

1. **A route around the push guard.** `HERDR_SOCKET_PATH` gives the agent `pane.run`, `pane.send_text` and `agent.prompt` on every pane in the session. Another pane is a shell holding herdr's `SSH_AUTH_SOCK` and `gh` credentials, so `herdr pane run <pane> "git push"` succeeds where the agent's own `git push` is blocked. `claude` under `--permission-mode acceptEdits` with `--allowedTools Bash(<verify>)` (`worker.rs:415`, `:426`) cannot run `herdr`. `opencode --auto` and `omp`, which approve every command, can.

2. **The herdr skill activates.** `~/.claude/skills/herdr/SKILL.md` gates on `HERDR_ENV=1`. A dispatched `claude` sees the variable and is told it may control the session.

3. **State reported against the wrong pane.** The `claude` hook sends `pane.report_agent_session` with the inherited `HERDR_PANE_ID` on `SessionStart`. That pane is the one running `pma tui` or `pma dispatch`, not an agent pane. Inference, not tested: whether `claude -p` fires `SessionStart` here, and whether herdr then offers to resume a headless run's session in that pane after a restart.

Fix: add `HERDR_ENV`, `HERDR_SOCKET_PATH`, `HERDR_PANE_ID`, `HERDR_TAB_ID`, `HERDR_WORKSPACE_ID` and `HERDR_BIN_PATH` to the removal list in `restrict`, with a case in the existing scrub test (`agent.rs:442`). `SSH_AUTH_SOCK` is already removed.

Limit, measured 2026-10-31 and more serious than first written: herdr needs no variable. Its [socket lookup](https://herdr.dev/tui/docs/socket-api/) ends at a default path, and `herdr workspace list` succeeded in a shell with no `HERDR_*` variable. `claude` now runs under `auto`, so no allowlist stops it running `herdr`; only the classifier might. `HERDR_SOCKET_PATH=/nonexistent/herdr.sock` makes the same command fail with `server_not_running`, since that variable outranks the default. Setting it in `restrict`, over removing it, closes the accidental route. Untested: what `claude`'s state hook does with a dead path.

Earlier wording: the socket path is fixed and readable by the user, so a process that looks for `~/.config/herdr/herdr.sock` still reaches it. This matches the README's stated bound, "this stops an accidental push, not a determined one". The `sanduk` worker is the boundary for a determined one, since the container does not mount the socket.

On macOS, Ghostty (1.3.0 and later, on by default), iTerm2 and Terminal.app accept `input text` or its equivalent over AppleScript. No variable carries this, so `restrict` cannot remove it; an agent that may run `osascript` (`opencode --auto`, `omp`) can try it, gated only by macOS Automation permission. Ghostty's `macos-applescript = false` turns it off.

## 2. Notices (works today, no code)

`pma config notify` runs a shell command when a job ends, with `PMA_JOB_ID`, `PMA_JOB_STATE` and `PMA_JOB_SUMMARY` set (`jobs.rs:148`). herdr's toast takes a title and a body:

```sh
pma config notify 'herdr notification show "pma job $PMA_JOB_ID $PMA_JOB_STATE" --body "$PMA_JOB_SUMMARY"'
```

`herdr notification show "pma test" --body "..."` returned `{"shown":true}` on this machine. The runner reaches the socket only if it inherited `HERDR_SOCKET_PATH` or herdr falls back to its default path; the second is unverified. A README line would cover this; no code.

## 3. Status in herdr's sidebar (small, optional)

`workspace.report_metadata` and `pane.report_metadata` set up to 16 display tokens on a workspace or pane, with a `ttl_ms` of at most 24 h. `pma` could report counts such as `review=2 failed=1` from the runner after each job, or from `pma tui` on refresh. Cost: a socket client or a `herdr` subprocess, gated on `HERDR_ENV`, about 40 lines. Open: how herdr renders the tokens, which the schema does not say. Value is low while `pma tui` already shows the same counts.

## 4. Runs as herdr panes (built 2026-10-31; the objections below are the first analysis)

An agent in a pane gets `PATH`, `GIT_*` and `PMA_*` from `pma`, and the rest from the pane's shell, not `pma`'s full environment: the script is a file, and a provider key must not land in it. So a key set only where `pma` started does not reach the agent; it needs to be in the shell herdr starts.

Socket API, not the CLI. A connection takes one request. Each run holds an `events.subscribe` connection for its pane. Captured from 0.9.5, and not in the schema: `pane.closed` arrives as `pane_closed`, `pane.agent_status_changed` needs a `pane_id`, and `pane.report_agent` refuses `done`. The exit status stays in a file, since herdr reports none for a line typed into a shell. A named session outside a pane is not handled.

The user chose this. A live test showed most objections do not hold when the pane runs a generated script rather than `agent.start`: the script unsets what `restrict` removed, `claude -p` keeps its event stream through `tee`, and `pane close` kills the process. A guard process closes the pane if `pma` dies. The last point, coupling, still holds: this depends on a pre-1.0 CLI. An interactive run does lose cost, budget and summary, and it needs a person at Claude's folder-trust prompt in a directory not yet trusted, so its tab comes to the front. `claude --help` says `-p` skips that prompt.


`pma` could start an agent with `pane.split` and `agent.start` instead of a child process, so the user watches and steers it live. This breaks five things `pma` relies on:

- **Environment.** `pane.split` takes an additive `env` map. It cannot remove what the server injects, so `restrict` cannot be applied.

- **Process control.** `spawn_guarded`'s process group, watchdog and timeout have no counterpart; `pma job stop` could not kill the agent.

- **Output.** Cost, tokens and the event stream for `pma watch` come from `stream-json` on stdout. A pane yields rendered screen text (`pane.read`).

- **Headless operation.** The runner works with no terminal and no herdr server; this path needs both.

- **Coupling.** Protocol 22 on a pre-1.0 tool. `async_dispatch.md` already rejected "caller is the async layer" because it "ties the workflow to one orchestrator".

`pma watch` in a herdr pane gives most of the visibility with none of the cost.

## 5. Front ends in herdr panes (works today)

`pma tui` and `pma watch` run in herdr panes like any TUI, and the server keeps them alive across detach and SSH drops. `--remote`/`--machine` reach `pma tui` on another host. No code.

## 6. `pma integrate herdr` (built 2026-10-31)

Takeover: open a run's clone or worktree in a new herdr pane with an interactive agent, so a person can question or correct it. This answers the weakness a headless run has, and keeps section 4's guarantees because the run itself stays headless. `pane split --cwd`, `agent start --kind claude` and `agent prompt` exist in 0.9.5; none was driven for this note. Edits made there change the tree, so `dispatch::approve` refuses the run until it is read again. The pane holds the user's credentials and no `restrict`: a person is at the keyboard. `pane.split` has only an additive `env`.

Also available: notices (section 2, which can report `no_foreground_client`, so `osascript` still suits detached jobs) and `workspace report-metadata` tokens (section 3, display-only, lost on a server restart).

minos gives steering during a run and herdr after one, which is the likely reason they exclude each other. Inference: `TODO.md` does not say why.

## Alternative framing

Treat herdr as a user of `pma`, not a dependency. An interactive agent in a herdr pane calls `pma next --json`, `pma dispatch`, `pma review` like a person. That needs stable JSON and exit codes on those commands, which the front-end parity work already pushes toward, and no herdr-specific code at all.

## Recommendation

1. Do section 1 now. It is a hygiene fix whether or not herdr is ever integrated.
2. Document section 2 in the README under notices.
3. Defer section 3 until there is a reason the TUI's counts are not enough.
4. Do not do section 4. Overruled: the user chose it, and it is built.
