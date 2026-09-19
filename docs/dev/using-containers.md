# Using containers

Design note, 2026-09-19. Whether to run dispatched agents inside containers, how the pieces divide, and what `pma` needs from each.

Three projects are in scope:

- `pma` (Rust): portfolio scan, ranking, worktrees, dispatch, verify, ship.

- [`sanduk`](https://github.com/shakfu/sanduk) (Python): a coding agent in a disposable container, with a host-side relay holding the API key.

- [`minos`](https://github.com/shakfu/minos) (Go): a conversation server with rooms, channels, moderation and an archive.

## Ownership

State the boundary or the layers grow into each other.

| Layer | Owns |
|-|-|
| `pma` | what work exists, what it is worth, and what policy applies |
| `sanduk` | how one run is contained: image, mode, relay, budget, teardown |
| `minos` | who sees a decision and who answers it |

`pma` decides which task, `sanduk` decides which box, neither schedules the other's work.

## Requirements

1. Contain both the agent and the verify command. `pma` runs `verify` itself, on the host, in the agent's environment (`src/dispatch.rs:764`). The agent edits `Makefile`, `build.rs` and `conftest.py`, then the host executes them. Containing only the agent leaves the build uncontained. Already recorded in [design.md](design.md#agents): "Isolation would need a container around both."

2. Let a run ask a question mid-flight. The current `sanduk` pattern is fire and forget. `pma` should answer most requests from policy and escalate the high-cost or high-risk ones to a person.

3. Make that traffic observable to a person while it happens, not only in a report afterwards.

4. Carry a conversation between two agents. `pma-agent` reads a worker's output and instructs it; the developer reads both and intervenes. Driving `pma-agent` is what the dispatcher is for, so this is the requirement the others serve, not an extension of them. It rules out a report-only channel.

`minos` answers 2, 3 and 4 together: `minosd` on the agent's own network is a bidirectional channel for the agent and a readable room for a person. See [Reaching minosd from a sealed container](#reaching-minosd-from-a-sealed-container).

An earlier version of this note said two agents never need to converse, and treated dispatch as fan-out and review as fan-in with nothing in between. That was wrong. What is open is the shape of `pma-agent`, not whether it exists: see open question 5.

## Stage (a): the agent contained, verify on the host

`pma`'s agent is a record, not a code path: `Worker { command, args, allow, parse, ... }` in `src/worker.rs:66`, set through `pma agent set`. `sanduk` registers with no Rust changes:

```text
command = "sanduk"
args    = ["run", "{prompt}", "-w", "{dir}", "--mode", "key-safe",
           "--model", "{model}"]
```

`pma` supplies the git worktree, `sanduk` bind-mounts it. No overlap.

What (a) buys: the key never enters the container, egress is held to an exact path allowlist, and the filesystem is scoped. What it does not buy: containment of the build, per the requirement above.

Two corrections it needs:

- Cost is lost. `sanduk` consumes `claude`'s `stream-json` and reformats `total_cost_usd` into a prose stats line (`sanduk/src/sanduk/agents/claude.py:44`). `pma`'s `ClaudeJson` parser finds no `{"type":"result"}` line and falls back to `TextTail` (`src/worker.rs:20`), recording cost as unknown.

- `worker.allow` is dead weight. `sanduk` passes `--dangerously-skip-permissions` by default. A container that already denies egress does not also need a `Bash()` allowlist. Do not ship both and believe in both.

`--budget` is OpenRouter-only (`sanduk/README.md`), so `agent_budget` stops being enforced against Anthropic. The timeout remains the only bound.

## Stage (b): both contained

`sanduk` runs the agent and then `verify`, inside the box. `pma` reads the result rather than re-running the command.

The cost is toolchain images. `sanduk`'s stock images carry `git`, `ripgrep`, `curl`, `jq` and `python3`, with no Rust, Go or C, and `sealed` mode blocks `cargo fetch`. Each project language needs a kit, a recipe, or a named `--image`. For a Rust-heavy portfolio this is the bulk of the work, not the integration.

`pma` must still distrust the agent's own claim. In (b) the trusted signal is `sanduk`'s exit status for the verify step, reported separately from the agent's step. One combined status cannot tell a failed edit from a failed test.

A regression check needs two runs: `base_verify` against the base tree already exists (`src/dispatch.rs:306`) and has to move into the container too, or the comparison is between two different environments.

## Reaching minosd from a sealed container

`--internal` blocks routes off the host. It does not block traffic between containers on that network. `sanduk` already relies on this: a placeholder container is attached to the sealed network so the vmnet host bridge exists at all (`sanduk/src/sanduk/runtime.py:252`).

So `minosd` attached to `sanduk-net` is reachable from the agent container with `sealed` intact. The agent is not reaching across the boundary; the server is inside it. This is the design, and it needs no change to the relay.

An earlier draft of this note rejected the option. The rejection was wrong.

The contract is also less of an obstacle than it looked. Section 12 of [wire-contract.md](https://github.com/shakfu/minos/blob/main/docs/wire-contract.md) puts the message bus, its topics, storage, worker leases, liveness locks and their sweep outside the contract. What is frozen is the wire shapes, not the server's internals, so queueing for agent traffic can be added without touching the OS.js format.

Four real problems remain. None is a reason not to do it; all are work. What `minosd` needs in order to take them is specified in [minos_agent_reqs.md](https://github.com/shakfu/minos/blob/main/docs/dev/minos_agent_reqs.md), beside minos's own [agents.md](https://github.com/shakfu/minos/blob/main/docs/dev/agents.md).

### 1. minos has no per-run credential

Authentication is `POST /login {username, password}`, answered with a cookie session: 12 hours, refreshed per request, 7 days maximum. There is no token grant and no machine account. `system` is server-written only and cannot be impersonated.

A password in the container is what the relay exists to avoid. `sanduk` gives a container a per-run token that is worthless anywhere else; minos has no equivalent, so an agent would hold a credential valid for 7 days against every room its user can enter.

One property helps. `groups` is the only authority channel, and a server must read the role from the issued session rather than from its user table (wire-contract.md:55). A session issued to a non-admin keeps non-admin rights for its life. So a scoped short-lived session is compatible with the model; what is missing is a way to mint one.

Needed in minos: a grant that `pma` can mint per run, carrying one room, no admin group, and an expiry matching the run's timeout.

### 2. The VFS is a 100 MiB exfiltration channel

`home:` is writable per user, and a `writefile` body may be 100 MiB. One session covers chat and the whole of `/vfs`. An agent granted chat is therefore granted file transfer, and a sealed run's central property -- that source cannot leave -- is gone. The agent writes the tree to `home:/`, and a person on the host reads it out.

Detection exists, prevention does not: a successful VFS mutation posts an event to `system` (wire-contract.md:371), so every write announces itself to every account. That makes exfiltration noisy, not impossible.

Needed in minos: agent sessions answer 403 at `/vfs`. `groups` is the authority channel, so a group whose sessions are refused there is the smallest change that fits the existing model.

### 3. minosd is dual-homed, so minosd is the boundary

A person has to reach minosd, and the agent's network has no route off the host. So minosd sits on both, which makes it a bridge across the containment boundary by construction.

That is the right place for the bridge: minosd is one Go server, it is inspectable, and its archive is a record. But the property changes, and the change has to be stated rather than assumed. `sealed` stops meaning "nothing leaves" and starts meaning "nothing leaves except what minosd accepts". Every control that matters then lives in minosd, which is why (1) and (2) are prerequisites and not refinements.

### 4. The audit trail splits

Today a sealed run's record is complete because the relay sees every call, and `--log-bodies` writes the bodies. A second channel the relay does not see ends that. minos keeps its own record -- the archive, with a retention period and search -- so nothing is lost, but the record is now in two stores with two retention policies and two clocks.

Decide which one is authoritative for a dispute about what an agent did, and set the minos retention for agent rooms to at least the relay's log retention.

## Where minos fits

Two roles, and only the second was in the earlier draft.

**A worker to `pma-agent`, inside the sealed network.** What requirements 2 and 4 ask for. A room per task, per minos design.md D5. The worker posts a request or a report; `pma-agent` answers within policy; `pma` the tool executes and escalates what policy will not decide. A person reads the room while it happens, which is requirement 3, and reads it later from the archive. The developer writes into the same room, which is how a correction reaches both at once and how a worker stays reachable when `pma-agent` is down.

**`pma` to a person.** Escalation to a channel, answered with `/approve <id>` or `/reject <id> [why]`, which already match `pma review --approve` and `--rework "feedback"`. `/queue` is the pending set.

What minos brings over a polled mailbox directory: a push and delivery model (wire-contract.md sections 7 and 8), a moderation queue whose verbs already match `pma`'s, an archive with retention and search, and one surface for both audiences rather than two.

Keep the mailbox anyway, for the case it is better at: a single fire-and-forget `sanduk run` needs no server, no account and no room. Use the bind mount there and minos for anything long-lived. The decision rule is container lifetime, not preference.

The escalation policy stays in `pma` the tool, which already holds the inputs: `route.rs` (`Policy`, `Subject`), `class.rs`, `complexity.rs`, tiers and `agent_budget`. `pma-agent` proposes within that policy and holds no authority to execute (minos design.md D2, D3). minos transports and records decisions; it does not make them.

`sanduk` stays out of it. It ships no messaging adapters and holds no messaging credential, and it should keep holding none: the minos grant is minted by `pma` and passed in as an argument, like the model or the budget.

## Three gates, three points

Do not merge these, and do not let two of them decide one thing.

| Gate | Point | Owner |
|-|-|-|
| mid-run request | during the run | `pma` policy, over the run's `minos` room, escalating to a person |
| result delivery | after a wakeup | `sanduk` `approval = true`, `sanduk approve` |
| ship | after verify | `pma review`, `pma ship` |

Rule for the overlap: a `pma` task never goes through a `sanduk` assistant. Two schedulers would race one task. Assistants stay for standing non-repo work such as triage and docs watch; `pma` calls `sanduk run` only.

## Long-lived containers

`sanduk` deletes the container at the end of every run, and an assistant wakeup is one `sanduk run`. An assistant's identity persists through `workspace/` and `assistants.db`; its container does not. So "long-lived agent" today means a long-lived workspace and a short-lived container.

A container that outlives one invocation changes the threat model: the run token stays valid for the container's life, the relay must stay up for it, and the orphan sweep in `sanduk/src/sanduk/runs.py` becomes load-bearing rather than a safety net.

Two ways to get there:

1. `pma` holds one long `sanduk run` process. It already runs children in their own process group under a timeout (`src/agent.rs:38`). Cheapest, and the pid ownership record stays true.

2. `sanduk` grows a session verb: start, attach, stop, with an explicit owner record. The honest version of the requirement, and a `sanduk` feature whoever calls it.

Prefer (1) until a task needs a container across two dispatches. A `minos` channel raises the odds that it will: a room outliving one `sanduk run` is only useful if the peer on the other end outlives it too. Expect (2) in stage (b).

`minosd` itself is a third container, on the same network, with a lifetime of its own. It is not swept by `runs.py`, because no run owns it. Whoever starts it owns stopping it.

## Tool, not a Rust library

Keep `sanduk` as a subprocess. Reasons, strongest first.

1. The relay is the security core and is already tested. `proxy.py` is 645 lines with non-obvious invariants: `Accept-Encoding` narrowed to gzip because the standard library decodes no brotli, `stream_options.include_usage` injected into streamed Chat Completions, and the network holder outliving the run by five minutes because vmnet keeps the host bridge up only while a container is attached. A port re-derives all of it in the one place a bug leaks a key.

2. The process boundary is a property. `runs.py` makes a pid the owner of its containers, swept by the next run. Linked in, `pma`'s pid owns them, so a `pma` crash leaks a container holding a live run token until `pma` reimplements the sweep.

3. Startup cost is irrelevant: about 50ms of Python against seconds of container boot.

4. `sanduk` has callers other than `pma`: eight agents, entry-point plugins, assistants, `shell`, `ps`. A Rust port either forks the contract or makes the Python package a binding, which is a third project.

Against: `pma` stops being one `cargo install`. It already requires `git`, `gh`, `claude` and a container engine, so the marginal cost is small but real.

Every feature the plan needs belongs to one of the three projects already: `sanduk` (`--json`, `--network`, a session verb), `minos` (a minted grant, `/vfs` denied to agent sessions), or `pma` (escalation policy, one parser, the minos client). None of them is a port.

## Work items

Stage (a):

1. `sanduk --json`: one object per run with `ok`, `exit`, `cost_usd`, token counts, `report_path`, `mode`, `model`, `container`. `Outcome` already carries `ok`, `text`, `error` and `stats`.

2. `pma`: a `sanduk-json` parser beside `Parser::ClaudeJson` in `src/worker.rs`. Around 30 lines, and the only Rust stage (a) needs.

3. `pma`: check `sanduk --version` against a minimum at preflight, as `gh` is checked.

4. Register the worker with `pma agent set` and prove it on one project.

Stage (b):

5. Kits or images carrying the Rust, Go and C toolchains, with the fetch that `sealed` blocks done at build time.

6. `sanduk` runs `verify` in the container and reports its status separately from the agent's. `base_verify` moves with it.

7. Mailbox protocol: request and decision schemas under the workspace, and the `pma` side that reads, decides and answers. Keeps single runs serverless, and is the fallback when `minosd` is not up.

The `minos` channel, in dependency order. Items 8 and 9 gate the rest: without them an agent holds a 7-day credential and a 100 MiB file channel.

8. `minos`: a grant `pma` can mint per run -- one room, no admin group, expiring with the run's timeout. No password reaches the container.

9. `minos`: `/vfs` answers 403 for agent sessions, keyed on the group.

10. `sanduk`: `--network <name>` so a run joins a network `minosd` is already on, and the agent image carries the `minos` client.

11. `pma`: a `minos` client. Open the room, mint the grant, read requests, apply policy, answer, and escalate what policy declines. `pma-agent` speaks over the same room under a grant of its own, minted by `pma` and never shared with a worker.

12. `pma`: escalation as a channel post, with `/approve` and `/reject <why>` mapped to `review --approve` and `--rework`.

13. Retention for agent rooms set to at least the relay's log retention, so the two records cover the same period.

## Open questions

1. What classifies a mid-run request as high risk? Cost has a number; "risk" needs a definition `route.rs` can evaluate.

2. What happens to a run whose escalation nobody answers? A timeout that fails the run is honest but wastes the spend; one that proceeds defeats the gate.

3. In (b), does `pma` still diff the worktree, or does it trust what the container wrote into the bind mount? The worktree is on the host either way, so the diff stays available and should stay authoritative.

4. Is a room per run, per task or per project? Per run is out: a rework reuses the worktree and the second run should read what the first was told. minos design.md D5 says per task; [pma_feedback.md](https://github.com/shakfu/minos/blob/main/docs/dev/pma_feedback.md) argues per project on the count, 1031 open tasks against 96 projects, against a model with no deletion rule.

5. Is `pma-agent` one context across the fleet, or one instance per task? A fleet context reads every task room, which makes it a channel between workers and puts every worker inside every repository's content (minos design.md section 3, D10). One instance per task keeps the isolation dispatch already has, and gives up a cross-project judgement `rank.rs` and `route.rs` already compute from stored scores.

6. Which record is authoritative for a dispute: the relay's bodies or the minos archive?

7. Does `pma` run `minosd`, or is it an operator's service `pma` finds? The second is cleaner and means `pma` must degrade to the mailbox when it is absent.
