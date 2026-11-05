# pma and minos integration

State as of 2026-11-05, against minos `18d527e`. The proposal is minos `docs/dev/pma-integration.md` ([github](https://github.com/shakfu/minos/blob/main/docs/dev/pma-integration.md)). This file records pma's side: what is built, what is verified, and what is open. Room filing is proposed in minos `docs/dev/run-rooms.md`.

## Status

Built 2026-11-05: minos 4.2 on the host, as `pma integrate minos` (`crates/pma-core/src/minos.rs`). A developer reads, corrects and interrupts a run from its minos room. User-facing description: README, "Talking to a run in minos". The decisions below record what was chosen.

Verified against a real `minosd`, `minosb` and `minosa` built from `18d527e`, without pma: `room ensure` with `-invite worker`, a correction posted mid-turn arriving as the second stdin frame, the linger closing stdin, exit 0, and the room holding both replies. A second `worker` broker in the same room exits 2 with no `ready` event and never starts its agent. `make minos-check` runs the same through `pma dispatch`; it needs a git fixture, so a person runs it.

## Built in minos

| Flag | Purpose |
|-|-|
| `minosb -task TEXT` | Writes the task as the agent's first stdin frame. A pma template cannot express a wrapper script. |
| `minosb -events FILE` | Moves control events to FILE. stdout then carries the agent's stream unchanged, for pma's log, `pma watch` and `parse_claude`. |
| `minosb -linger DUR` | Closes the agent's stdin after DUR idle between turns. Without it a run never ends by itself (issue 1). |
| `minos room ensure -project NAME -task KEY` | Prints the id of room `task/KEY`, founding the project and room if missing. Needs an administrator, so pma calls it, not `minosb`. |
| One run per room | `MINOS_ROOM_AGENTS` (default 1) caps agent runs in a room. `minosb` fails at start if refused (issue 11). |

`make host-run` in minos drives a real `claude` (2.1.299) through all three, plus held messages and both interrupt lanes. It also saves the agent's stream as `agent.jsonl`.

## Command

pma builds this from the `minos.*` settings and the worker's `relay_args`:

```text
minosb -server URL -user worker -room ROOM -socket SOCK -events EVENTS \
  [-interrupters a,b] -agent claude -task PROMPT -linger 120s -- \
  claude -p --input-format stream-json --output-format stream-json --verbose ...
```

`minos.linger` (default 120 s) is unvalidated. It sets how long a finished run holds its job runner waiting for corrections.

pma's template decides what runs and how it is launched. `minosb` runs whatever follows `--` as a child, with `MINOS_PASSWORD` and `MINOS_USER` removed from its environment. It has no code that chooses an agent and none that names sanduk, so replacing `claude` with `sanduk run` changes only the template. `minosb` knows only the agent's stream format, through the adapter `-agent` selects (`claude`, the only one): how to frame a message on stdin, that a `result` record ends a turn, and how to frame an interrupt. `minosd` never touches the agent process.

## Issues

1. **Run end. Resolved in minos.** `claude --input-format stream-json` reads stdin until it closes, and `minosb` never closed it. A run held the job runner until the timeout and counted as killed. `-linger` fixes it; a real `claude` exits 0 on end of input.

   `-linger DUR` waits until the agent is between turns with nothing held, then for DUR more. Any message or interrupt restarts the wait, and a turn is never cut short. On expiry `minosb` posts `[closed: this run reads no more messages and is ending]` to the room, closes stdin, and reports `closed`. After that, a room message is reported as `undelivered`, and an interrupt as outcome `closed`. The default, 0, never closes stdin, so a template without the flag hangs until pma's timeout.

2. **Summary. Resolved.** `parse_claude` (`agent.rs`) takes the last `result` record. In a corrected run that is the reply to the last correction, not the task's outcome. A relayed run is read by `parse_relayed`, which reports every turn's reply in order.

3. **Success. Resolved.** An interrupted turn ends with `subtype: error_during_execution`. If it is the last turn, `parse_claude` reports failure even when the work is done. `parse_relayed` does not count it as a failure, so verify decides.

4. **Cost.** `total_cost_usd` is cumulative per session, so the last record gives the right total. An interrupted turn adds $0 and reports 0 tokens, though output was in progress. Whether that output is billed is unknown, so the total may undercount.

5. **Credential exposure.** `minosb` needs `MINOS_PASSWORD`. It removes it from the agent's environment, but the agent runs as the same uid. Under Yama `ptrace_scope=1` a same-uid process can read `/proc/<pid>/environ` (checked on this host). The agent can therefore read the worker account's password. Mitigation: give the worker account only the run's room and channel. Only a container run (minos 4.3) keeps the credential out of reach.

6. **Credential storage. Resolved.** A worker's `env` (`worker.rs`) is applied after `agent::restrict` and would carry the password. Stored there, it sits in plain text in `settings.toml`. pma reads `MINOS_PASSWORD` and `MINOS_ADMIN_PASSWORD` from its own environment instead, else, under `minos.dev_passwords` (on by default), uses the account name (minosd's development fixture, `go/internal/config/config.go`), and `restrict` removes both from every agent. Under herdr the pane script stays on disk, so the password goes to a 0600 file the script reads and deletes. This avoids the file, not issue 5.

7. **Sandbox. Not a blocker.** The `sanduk_sandbox` policy confines verify only (`dispatch.rs`, `verify_once`). Agent runs pass none, so `minosb` reaches the server and creates its socket.

8. **Who can steer.** Anyone who can post in the run's room can direct an unsandboxed agent. Room membership is a trust decision.

9. **Record split.** After a correction, the stored prompt no longer describes what the agent was told. The room holds the rest, and the run row holds the room id (`runs.room`, schema 50).

10. **Stale pma docs. Resolved 2026-10-14.** `using-containers.md` and `implementation-plan.md` 2b.6-2b.8 described `minosd` on the sealed network, a minos client in the container, and a Rust wire client in pma. minos rejected all three. Both now follow the broker design.

11. **One run per room.** With one room per task, a second run of that task is refused while the first holds the room. This covers `max_parallel` above 1 and a rework dispatched before the first run lingers out. pma retries a refused run without the room, since the broker started no agent, and warns. A run that loses its seat for 45 s reports `unseated` and stops after its agent started. Inference, not tested: pma records that run as failed.

12. **Other agents.** `minosb` has a `claude` adapter only. A worker without `relay` runs beside a broker that serves only the socket, with `MINOS_SOCKET` set and a prompt line naming `minosa messages`. A correction reaches it only if it polls. pma posts its start and end to the room through `minosa say`. An adapter in minos for another agent's stdin protocol makes it a `relay` worker with no pma change.

## pma work for 4.2

All built 2026-11-05.

1. minos: `minos room ensure`.

2. `runs.room`, schema 50; `pma review` and the front ends show it.

3. Account names in `minos.user` and `minos.admin`; passwords from the environment (issue 6). `ensure` runs before each run that is not interactive.

4. `{room}`, `{socket}` and `{events}` placeholders in `worker.rs`. A template that names `{room}` or `{socket}` starts its own broker. `{socket}` is outside every worktree; a path over 103 bytes moves to `/tmp/pma-<uid>`.

5. Issues 2 and 3, in `parse_relayed`.

6. Exit test: `make minos-check`, and three CLI tests with fake `minos`, `minosb` and `minosa`.

## Decisions

Decided by the user 2026-11-05.

1. **Are mid-run corrections needed?** Yes, beside takeover after a run (`T`, through herdr).

2. **Room grain.** `minos.rooms`: `task` by default, `run` when set. One room per task gives up to 1031 rooms, and minos cannot delete one. One per run bounds the count by work done.

3. **Interrupter.** `minos.interrupters`, empty by default. pma cannot write `minosb`'s stdin: the watchdog gives the worker `/dev/null`.

4. **Server down.** The run goes on without a room and warns.

5. **Shape.** A setting of its own, `minos.*`, not a third `integration` value. herdr chooses where the agent process runs and `minosb` wraps its command, so the two combine. Non-`claude` workers get the room through `minosa` (issue 12).
