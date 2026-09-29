# Integrating with sanduk-rs

Design note, 2026-09-25; decision and steps 1-2 added 2026-09-29. How far `pma` should integrate with [sanduk-rs](https://github.com/shakfu/sanduk-rs)'s crates, and in what order. It builds on [using-containers.md](using-containers.md), whose stages (a) and (b) it keeps.

sanduk-rs is the Rust rewrite of sanduk, published as three crates. Its CLI takes the flags `Worker::sanduk()` passes (`src/worker.rs:423`), so the `sanduk` worker needs no code change to use it, only the binary on `PATH`. The question here is whether `pma` should link any of the crates as well.

## Decision

2026-09-29: `pma` and `sanduk` stay two executables. `pma` does not link the `sanduk` crate; the `sanduk` worker runs the `sanduk` binary on `PATH`. Section 3 records the prototype and what it cost. Section 1, `sanduk-sandbox` for `verify`, is not covered by this decision.

## What each crate offers `pma`

| Crate | Adds to `pma`'s build | What `pma` gains |
|-|-|-|
| `sanduk-sandbox` | `landlock` (Linux), `libc` | confinement of the processes `pma` runs on the host: `verify`, and later host workers |
| `sanduk-container` | `serde_json`, `libc` | engine primitives only: `run_argv`, `list_containers`, `destroy`. The relay, image build, run records and teardown are in `sanduk`, not here |
| `sanduk` | tokio, hyper, rustls; `pma` has no async stack today | the whole `run`, with a typed outcome |

## 1. `sanduk-sandbox` on the host

`sanduk-sandbox` confines a child process's writes to one root, plus `$TMPDIR` and the toolchain caches, with Landlock on Linux and Seatbelt on macOS. Reads and the network stay open. It is the implementation of minima's `--sandbox`.

### Where it applies

- **`verify`.** Every verify run goes through `verify_once` (`src/dispatch.rs:1229`): the head check (`dispatch.rs:1196`), the base check (`dispatch.rs:1280`) and the check before publishing (`src/publish.rs:182`). It runs the project's command with `sh -c` in the worktree, so a `Makefile`, `build.rs` or `conftest.py` the agent edited runs unconfined on the host. That is the gap [using-containers.md](using-containers.md) records for stage (a).
- **Host workers.** `claude` runs on the host under `--permission-mode acceptEdits`. `opencode --auto` and `omp` approve every command, which is why [pilot.md](pilot.md) left them out. Under `sanduk-sandbox`, with the worktree as the root, their writes are bounded, which may be enough to admit them. Rejected 2026-09-29: see problem 3.
- **`scan`** runs `git` and `gh`, which only read. Not worth confining.

### Four design problems

1. **`spawn_guarded` would drop the Linux sandbox without an error.** It rebuilds the command as `sh -c WATCHDOG <program> <args>` from `get_program()` and `get_args()` (`src/agent.rs:124`). On macOS the sandbox is an argv prefix (`sandbox-exec -p ...`) and survives the rebuild. On Linux, Landlock is applied in a `pre_exec` hook on the `Command`, which the rebuild does not carry, so verify would run unconfined. The policy has to wrap the outer command, `policy.command("sh")` with the watchdog's arguments, so the watchdog and everything under it inherit it. A test must write outside the worktree on Linux and check the disk, not the exit status.

2. **A git worktree writes outside itself.** A worktree's `.git` is a file pointing at `<repo>/.git/worktrees/<name>/`. `git add` writes the index there and objects into `<repo>/.git/objects/`. Confined to the worktree, an agent cannot stage a file, and any verify that runs `git` fails. `pma` has to grant that worktree's admin directory and `objects/`. Granting `objects/` lets the confined process write objects into the real repository. It cannot move a ref, so nothing it writes becomes history without `pma`, but the grant should be stated where it is made.

   Resolved otherwise, 2026-09-29: nothing under `.git` is granted. `sanduk-sandbox` grants every right Landlock handles on a writable path (`AccessFs::from_all`), removal included, so a grant on `objects/` lets a check delete existing objects and lose history. `confine_path` refuses `.git` for the same reason. Reads work under the policy: `git status` and `git diff` pass in the disk test. `git add` fails, and a project whose verify needs it uses the opt-out.

3. **`Worker.sandbox` is a boolean, and there would be two strengths of sandbox.** Today `sandbox = true` means the worker runs in a container (the `sanduk` worker). A host worker under `sanduk-sandbox` has its writes bounded but its reads, its network and the API key open. One flag for both would admit the weaker one under the stronger one's gates. A field such as `sandbox = "none" | "writes" | "container"` keeps them apart; `src/store.rs:231` holds the column, so this is a schema migration.

   Closed 2026-09-29: `Worker.sandbox` stays a boolean, and host workers stay unconfined. The container worker is the only one meant for untrusted work. Two reasons:
   - Nothing reads `Worker.sandbox` as a gate, by design: which context a worker runs in is the user's choice, and every worker is admitted under the same gates (`src/worker.rs:6-9`). A third value would be a label no code acts on.
   - A host agent runs only with its state directory writable: `~/.claude` and `~/.claude.json` for `claude`, `~/.local/share/opencode` and `~/.config/opencode` for `opencode`, `~/.omp` for `omp`. Write access there lets a confined agent add a hook, a plugin or an MCP server that runs unconfined in the user's next session. The sandbox would bound mistakes in the worktree and leave that route open.

4. **Platform limits.**
   - Seatbelt refuses a nested profile, so a command that runs `sandbox-exec` itself fails under it. Claude Code's own sandbox mode does, and so does SwiftPM (`swift build` needs `--disable-sandbox`, as minima documents).
   - Landlock needs Linux 6.2 (ABI 3). Below it `sanduk-sandbox` refuses rather than run unconfined, so on an older kernel the project needs the opt-out.
   - A verify that writes outside the worktree and the caches (a global tool install, a build store elsewhere) fails. A per-project `sandbox = false` beside `verify`, or a list of extra writable directories, keeps such projects working.

### What it does not fix

Reads and the network. An adversarial edit to a build file can still read `~/.ssh` and send it anywhere. `sanduk-sandbox` bounds what a mistake can destroy; it does not contain a prompt injection. That is stage (b).

## 2. `sanduk-container` alone: little to gain

It wraps the engine's CLI. What makes a `sanduk` run safe is the relay, the token swap, the image build, the teardown and the sweep, and all of it lives in the `sanduk` crate. Linking `sanduk-container` alone would have `pma` rebuild `sanduk run`, which is the duplication the crates were extracted to end. The one real use is housekeeping: listing or cleaning the containers a dispatch left behind, which `sanduk ps` and `sanduk clean` already do.

## 3. `sanduk` as a library: premature

What it would give:

- typed outcomes: tokens and the relay's spend as numbers, rather than a parsed stream;
- cancellation and progress from inside `pma`;
- one binary to install, and no `PATH` or version skew.

What it would cost:

- **An API that does not exist.** `run` is a CLI command: it takes clap arguments, prints notes to stderr and the trace to stdout, installs process-wide signal handlers, and uses process-wide state (the caught-signal flag, thread-local directory overrides). Called inside `pma`'s TUI, each of those collides. A `sanduk::Run` builder returning an `Outcome`, with notes to a callback, is real refactoring in sanduk-rs.
- **The async stack** in `pma`'s build.
- **Crash isolation and version decoupling**, which the process boundary gives for free.

sanduk-rs's own plan says to wait until its interfaces settle.

### The embedded CLI, prototyped and reverted

A cheaper form avoids the refactoring: `pma` links `sanduk` and calls `sanduk::cli::main(argv)` from a `pma sanduk` subcommand. The `sanduk` worker runs `current_exe()` with `sanduk` prepended, so each run is still a child process under the watchdog. Measured against 0.5.0 on Linux:

| | 0.5.0 | With `sanduk = "=0.1.0"` |
|-|-|-|
| Clean release build | 45 s | 55 s |
| Release binary | 10.9 MB | 18.9 MB |
| Crates in the tree | 111 | 181 |

The prototype also changed `pma`'s output. sanduk enables `serde_json/preserve_order` so its relay keeps a request body's key order. Cargo unifies features, so `pma` cannot turn it off. Every `serde_json::Map` in `pma` then serialised in insertion order instead of sorted: `pma project export` failed its round-trip test, and route and workflow documents changed text. Any crate in the tree that enables the feature does this.

Reverted; the two stay separate executables. A test that `Worker::sanduk()`'s arguments parse against `sanduk run` belongs in the separate setup instead (step 1 below).

### In-process `run` would also need

For the record, if a library API is taken up again:

- **Signals.** `Signals::catch()` saves and restores process-wide handlers, and `CAUGHT` is one global. `pma` runs agents on parallel threads (`dispatch.rs:980`); two overlapping runs restore handlers out of order and leave sanduk's installed.
- **Output.** `run` prints the trace to stdout and notes to stderr. Parallel runs would interleave, and the TUI would break.
- **Cancellation.** `pma` stops a run by killing its process group. A call on a thread needs a cancel token and teardown in `Drop`, and still leaks containers if `pma` is killed.

## 4. The alternative: extend the CLI

Most of what the library would give can cross the process boundary instead:

- **`sanduk run --verify CMD`**: stage (b). The agent runs, then `CMD` in the same image, mount and network, and the two exit statuses are recorded separately, since one combined status cannot tell a failed edit from a failed test. `base_verify` would run the same way against the base tree, so the regression comparison is between one environment and itself.
- **A richer `--stats-file`.** It records `exit`, `ok`, `stats`, `error` and `report` today. Adding the verify status, the relay's request count and spend, and token counts as numbers gives `pma` typed results without linking anything.

This gets stage (b) without tokio in `pma`. The toolchain images stay the real cost of stage (b), whichever form it takes: the stock images carry no Rust, Go or C, and `sealed` cannot `cargo fetch`.

## 5. The `sanduk` worker, end to end

`scripts/sanduk-e2e.sh` dispatches one task through the `sanduk` worker in a scratch `PMA_HOME`, against the real engine and API. Run 2026-09-29 with `claude-haiku-4-5-20251001`: the container ran in `key-safe` mode, the edit reached the worktree `pma` reviews, the cost was read from the agent's stream, verify ran confined on the host, and no container was left.

`--kill-test` SIGKILLs `pma` while the agent's container runs. `pma`'s watchdog then SIGKILLs its process group, `sanduk` included, which runs no teardown; the engine's CLI is in a group of its own. With sanduk 0.1.0 the container kept running with the worktree mounted until a later `sanduk run` swept it. The key was not exposed: it lives in the relay, which died with `sanduk`.

Fixed in sanduk-rs (Unreleased): each run starts `sanduk reap` in its own process group, blocked on a pipe the run holds. The pipe closing, at teardown or at death, deletes what the run's record still names. The kill test passes against that build. Not covered: the reaper dying too (reboot, OOM), which the next run's sweep still handles. Until a release carries the fix, `cargo install sanduk` installs a version that leaks.

## Order

1. Done 2026-09-29: `cargo install sanduk`, and a test that the template's flags and agent parse against the installed binary. `make test` now needs `sanduk` on `PATH`.
2. Done 2026-09-29: `sanduk-sandbox` for `verify`, with the policy wrapping the watchdog, `projects.<name>.sandbox off` as the opt-out, the sandbox in the base cache's key, and a disk test. No git grants: see problem 2.
3. Closed 2026-09-29, not done: host workers stay unconfined. See problem 3.
4. Done in sanduk-rs 2026-09-29 (Unreleased): `run --verify CMD`, and `--stats-file` with `mode`, the relay's numbers and the verify result. `run --verify` with no task checks a base tree in the same container setup, in place of a separate verb. Toolchain kits followed the same day: `build`, `rust`, `go` and `uv`, which stack on one image. sanduk-rs's own suite passes inside `--kit rust`. `pma` does not read the results yet; that is the next step: a per-project opt-in with the project's kits, `--verify {verify}` in the `sanduk` template, head and base results from the stats file, and where verify ran in the base cache's key.
5. No library API. Anything `pma` needs from a run goes into the CLI or the stats file.

## Open questions

These decide the order:

- **The threat.** An agent that errs, or an injected prompt? `sanduk-sandbox` covers the first. If the second matters now, step 4 comes next.
- **Where `pma` runs unattended.** A macOS laptop, or a Linux server? That decides whether Landlock's kernel floor and Seatbelt's nesting limit bite.
- **Host workers.** Answered 2026-09-29: the container worker is the only one meant for untrusted work.
