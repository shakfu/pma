# pma-box: sanduk's library, copied into pma

Decision record, 2026-11-04. Reverted 2026-11-05: the crate was removed before anything called it, and pma runs the `sanduk` binary again ([sanduk-integration.md](sanduk-integration.md)). The copy is at commit `1af4a80`. Its divergence from sanduk was dependencies only (see "Changes since the copy"), so no pma-specific relay policy was lost.

## Decision

`crates/pma-box` is a copy of sanduk-rs's `sanduk` and `sanduk-container` crates at commit `031bb4e`, merged into one crate. pma maintains it; sanduk-rs continues separately, and changes are not synced in either direction. `sanduk-sandbox` stays a crates.io dependency.

A copy over linking a published `sanduk`: pma is installed with `cargo install pma`, so every library dependency must be on crates.io. Each API change would then cost a sanduk release and an `=` pin bump. A copy over moving the library into pma's workspace and having sanduk-rs depend on it: sanduk-rs is to stay a project of its own for now.

## What was copied

- `src/` of `sanduk`, with `sanduk-container` as `src/container/`. `testing::Fake` is always built, no longer behind `test-util`: a feature of the crate itself cannot be enabled for its own integration tests.
- `build.rs` and `resources/` (the shipped agents, recipes and kits).
- The tests, goldens included. `sanduk-container`'s tests are `tests/container_*.rs`.

Not copied: `sanduk-cli` (the binary, assistants, `rusqlite`, the stats file, the CLI parity tests).

## Names

Only host-side names that could collide with a sanduk installation changed:

| What | sanduk | pma-box |
|-|-|-|
| Containers, holders, image repositories | `sanduk-` | `pma-` |
| Mode networks | `sanduk-open`, `sanduk-net` | `pma-open`, `pma-net` |
| Directories | `<xdg>/sanduk` | `<xdg>/pma/box` |
| Clone branch | `sanduk/<run>` | `pma-box/<run>` (pma's own branches are `pma/<slug>`) |
| Reaper argv0, relay thread, temp dirs | `sanduk-*` | `pma-*` |

`sanduk clean` and `sanduk ps` select by the `sanduk-` prefix, so they never touch pma's containers, and pma's sweep never touches sanduk's. Images are built separately, which costs disk.

Names inside images are unchanged: the `sanduk` provider id in agent configs, `/run/sanduk/agent.sock`, `SANDUK_*` variables, `sanduk.*` labels. They cannot collide between installations, and renaming them would change every golden Containerfile.

## Verified, Linux, 2026-11-04

- `make test`: all pass. `cargo clippy -p pma-box --all-targets -D warnings`: clean.
- `make box-live`, Docker 29.9.3: `container_live` 6/6, `live_tls` 1/1, `live_run` 4/4. No `pma-` containers are left over; the mode networks are, by design.
- `live_run` covers an open run with an image build and `verify`, key-safe and sealed runs reaching the relay, a dropped `Live` whose container is running, and the uid refusal.

The uid check refuses only a workdir with no group or other write. Under a umask of `002`, the Ubuntu default, new directories are `775`, so it never fires.

## Changes since the copy

- `agent/stream.rs` removed: pma parses agent output itself. An agent's `stream` is checked against `agent::STREAMS`.
- `sha2` replaced by `ring::digest`, which TLS already links. Image tags are unchanged: the golden tags pass.
- `flate2` removed. The relay always sends `accept-encoding: identity` upstream. A coded response anyway is not read for usage, so it counts as unpriced and stops a budgeted run. The cost is uncompressed non-streaming bodies.
- `regex` replaced by the ASCII predicates in `src/check.rs`. Its test compares each with the pattern it replaced on mutated and random input. One deliberate difference: `uid` takes ASCII digits only, where `\d` took any Unicode digit.
- `sha2` and `regex` stay as dev-dependencies, as test oracles.

`cargo tree -e normal`: 54 crates, 30 not already in pma (was 72 and 48).

## Sharing with sanduk-rs

Kept separate for now. The candidate to share is `src/container/`: 4 sites carry the namespace, and its only dependencies are `serde_json` and `libc`. Sharing means publishing `sanduk-container` with a namespace parameter and pinning it with `=`, as `sanduk-sandbox` is. Count how often `src/container/` changes here first: each change would cost a sanduk-rs release.

The relay stays as copied, apart from names, so a fix in either copy ports as a diff. A blocking rewrite and `toml_edit` for agent specs are dropped for that reason.

## Next

1. The `sanduk` worker calls `pma-box` instead of the `sanduk` binary. Design first.
2. An owner for recipe and kit pins, which change when an agent releases.
