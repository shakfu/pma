# Integrating with sanduk-rs

Design note, 2026-09-25. How far `pma` should integrate with [sanduk-rs](https://github.com/shakfu/sanduk-rs)'s crates, and in what order. A proposal; nothing here is implemented. It builds on [using-containers.md](using-containers.md), whose stages (a) and (b) it keeps.

sanduk-rs is the Rust rewrite of sanduk, published as three crates. Its CLI takes the flags `Worker::sanduk()` passes (`src/worker.rs:394`), so the `sanduk` worker needs no code change to use it, only the binary on `PATH`. The question here is whether `pma` should link any of the crates as well.

## What each crate offers `pma`

| Crate | Adds to `pma`'s build | What `pma` gains |
|-|-|-|
| `sanduk-sandbox` | `landlock` (Linux), `libc` | confinement of the processes `pma` runs on the host: `verify`, and later host workers |
| `sanduk-container` | `serde_json`, `libc` | engine primitives only: `run_argv`, `list_containers`, `destroy`. The relay, image build, run records and teardown are in `sanduk`, not here |
| `sanduk` | tokio, hyper, rustls; `pma` has no async stack today | the whole `run`, with a typed outcome |

## 1. `sanduk-sandbox` on the host

`sanduk-sandbox` confines a child process's writes to one root, plus `$TMPDIR` and the toolchain caches, with Landlock on Linux and Seatbelt on macOS. Reads and the network stay open. It is the implementation of minima's `--sandbox`.

### Where it applies

- **`verify`.** Every verify run goes through `verify_once` (`src/dispatch.rs:1111`): the head check (`dispatch.rs:1080`), the base check (`dispatch.rs:1162`) and the check before publishing (`src/publish.rs:161`). It runs the project's command with `sh -c` in the worktree, so a `Makefile`, `build.rs` or `conftest.py` the agent edited runs unconfined on the host. That is the gap [using-containers.md](using-containers.md) records for stage (a).
- **Host workers.** `claude` runs on the host under `--permission-mode acceptEdits`. `opencode --auto` and `omp` approve every command, which is why [pilot.md](pilot.md) left them out. Under `sanduk-sandbox`, with the worktree as the root, their writes are bounded, which may be enough to admit them.
- **`scan`** runs `git` and `gh`, which only read. Not worth confining.

### Four design problems

1. **`spawn_guarded` would drop the Linux sandbox without an error.** It rebuilds the command as `sh -c WATCHDOG <program> <args>` from `get_program()` and `get_args()` (`src/agent.rs:124`). On macOS the sandbox is an argv prefix (`sandbox-exec -p ...`) and survives the rebuild. On Linux, Landlock is applied in a `pre_exec` hook on the `Command`, which the rebuild does not carry, so verify would run unconfined. The policy has to wrap the outer command, `policy.command("sh")` with the watchdog's arguments, so the watchdog and everything under it inherit it. A test must write outside the worktree on Linux and check the disk, not the exit status.

2. **A git worktree writes outside itself.** A worktree's `.git` is a file pointing at `<repo>/.git/worktrees/<name>/`. `git add` writes the index there and objects into `<repo>/.git/objects/`. Confined to the worktree, an agent cannot stage a file, and any verify that runs `git` fails. `pma` has to grant that worktree's admin directory and `objects/`. Granting `objects/` lets the confined process write objects into the real repository. It cannot move a ref, so nothing it writes becomes history without `pma`, but the grant should be stated where it is made.

3. **`Worker.sandbox` is a boolean, and there would be two strengths of sandbox.** Today `sandbox = true` means the worker runs in a container (the `sanduk` worker). A host worker under `sanduk-sandbox` has its writes bounded but its reads, its network and the API key open. One flag for both would admit the weaker one under the stronger one's gates. A field such as `sandbox = "none" | "writes" | "container"` keeps them apart; `src/store.rs:231` holds the column, so this is a schema migration.

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

## 4. The alternative: extend the CLI

Most of what the library would give can cross the process boundary instead:

- **`sanduk run --verify CMD`**: stage (b). The agent runs, then `CMD` in the same image, mount and network, and the two exit statuses are recorded separately, since one combined status cannot tell a failed edit from a failed test. `base_verify` would run the same way against the base tree, so the regression comparison is between one environment and itself.
- **A richer `--stats-file`.** It records `exit`, `ok`, `stats`, `error` and `report` today. Adding the verify status, the relay's request count and spend, and token counts as numbers gives `pma` typed results without linking anything.

This gets stage (b) without tokio in `pma`. The toolchain images stay the real cost of stage (b), whichever form it takes: the stock images carry no Rust, Go or C, and `sealed` cannot `cargo fetch`.

## Order

1. Install the `sanduk` binary (`cargo install sanduk`). Update the comment on `Worker::sanduk()`, which describes the Python tool, and add a test that its flags parse against the binary.
2. `sanduk-sandbox` for `verify`: the policy wrapping the watchdog, the git worktree grants, a per-project opt-out, and the Linux disk test.
3. `Worker.sandbox` as three values, then host workers under `sanduk-sandbox`.
4. `sanduk run --verify` and the richer stats file, in sanduk-rs; `pma` reads them for stage (b).
5. A library API, when `pma` needs something the CLI cannot carry.

## Open questions

These decide the order:

- **The threat.** An agent that errs, or an injected prompt? `sanduk-sandbox` covers the first. If the second matters now, step 4 comes before steps 2 and 3.
- **Where `pma` runs unattended.** A macOS laptop, or a Linux server? That decides whether Landlock's kernel floor and Seatbelt's nesting limit bite.
- **Host workers.** Should `opencode` and `omp` become usable on the host, or is the container worker the only one meant for anything untrusted?
