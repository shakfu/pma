# Deps refresh

Status: built, 2026-10-21.

Where the build departs from the design below:

- No route is seeded. Activating a policy is the user's gate. A policy without a catch-all refuses every other dispatch, and any applied policy refuses workflow nodes no route names. Name the worker (`-a refresh`), or add a `"class": "A"` route to your own policy.

- A `refresh` run counts its attempts under its own key. A refused refresh says an update broke the check, which is the agent's task; counting it against the task left the agent no attempt.

- A `refresh` run is charged nothing against `batch_budget`. Another worker that reports no cost is charged its whole `agent_budget`.

## Problem

The stale-deps state fires on almost every project, and nothing clears it without judgement.

Measured from `projects.db` on 2026-10-21:

- 37 of 39 measured projects are in the state. The trigger is any count above zero (`rank.rs:192`).

- Every measurement dates from 2026-09-15. rezon is stored at 147; `cargo update --dry-run` gives 72 today, after a lock refresh on 2026-10-18. The state reads an old number as current.

- The three tools answer different questions (`deps.rs`). cargo and go report what the lock can move to within declared constraints. uv's `uv tree --outdated` reports any newer release on the index.

- Of uv's 166 listed bumps, 76 change the major version or a 0.x minor version. Of cargo's 420, 28 do. The classification is a regex over version strings, so treat it as approximate.

- cargo's parser keeps `Updating` lines only. It drops `Adding`, `Removing`, and the note `N unchanged dependencies behind latest`.

## Decision

Stale means: the lock can move within the project's declared constraints, and the lock has not changed for 30 days. A deterministic worker refreshes the lock; the project's check decides whether the refresh is kept.

This is uv's documented workflow ([uv#6692](https://github.com/astral-sh/uv/issues/6692)): constraints state a floor, the lock pins versions, `uv lock --upgrade` moves them, tests gate the commit. An upper bound is added per package where one is needed, not as a blanket policy.

Rejected:

- **Any newer release.** It grows with upstream release cadence, not with the project. No command clears it; moving a constraint is judgement.

- **pma's own semver rule for uv.** It adds a policy uv does not have. Python packages do not follow semver reliably.

- **Upper bounds on every dependency.** It changes packaging policy for published libraries and causes resolver conflicts for their users ([Schreiner](https://iscinumpy.dev/post/bound-version-constraints/)).

- **Lock age alone.** An old lock with nothing to move needs no action.

- **Count alone.** Patch releases are daily, so a lock is almost never fully current. The state would return on the next scan.

## Measurement

Each measure is its fix run as a dry run, so the fix clears the state by construction.

| Ecosystem | Marker | Measure | Fix |
|-|-|-|-|
| cargo | `Cargo.lock` | `cargo update --dry-run` | `cargo update` |
| uv | `uv.lock` | `uv lock --upgrade --dry-run` | `uv lock --upgrade` |
| go | `go.mod` | `go list -u -m all`, direct | `go get -u ./... && go mod tidy` |
| npm | `package-lock.json` | lock diff after `npm update --package-lock-only` on a copy | `npm update` |
| pnpm | `pnpm-lock.yaml` | lock diff after `pnpm update --lockfile-only` on a copy | `pnpm update` |

npm and pnpm, added 2026-11-20: `npm update --dry-run` compares against `node_modules`, so in a clone without it every package reads as an addition, and with `--package-lock-only` it reports nothing. Updating the lock alone on a copy of `package.json` and the lock measures the fix without touching the project. Workspaces are refused: the copy lacks the members' manifests.

uv changes from `uv tree --frozen --outdated --depth 1`. The new count includes transitive packages, like cargo's. cargo and go are unchanged.

`Adding` and `Removing` lines count as movement. A lock that would only add or remove a package can still be refreshed.

Held back is a second number, read from cargo's `behind latest` note. It is shown in `status --explain` and the report. It places a project in no state and creates no task. uv and go have no free equivalent; add one when someone needs it.

## Lock age

Lock age is the commit time of the newest change to each lock (`Cargo.lock`, `uv.lock`, `go.sum`, `package-lock.json`, `pnpm-lock.yaml`) on HEAD or its upstream; the upstream counts a refresh published but not yet pulled. With several locks, the oldest decides. It is one `git log` per lock with no network, so every scan computes it, not only `--deps`.

A lock with uncommitted changes counts as fresh: someone is refreshing it. A lock never committed has no age.

## State

A project is in stale deps when all three hold:

1. the last `--deps` measurement is at most 30 days old;

2. that measurement found movement above zero;

3. the lock is more than 30 days old.

A measurement older than 30 days makes the project unmeasured, which places it in no state. It cannot say whether the lock is within the window.

One setting, `deps.days = 30`, sets both windows. They are one policy: how often a lock is expected to move.

## The fix

A worker named `refresh`, seeded beside `claude`, runs `pma deps refresh {dir}`. That subcommand runs the fix for each marker present and exits. It takes no prompt and no model, and reports no cost.

Everything else is the existing dispatch path:

- a worktree on the default branch;

- `Class::Mechanical`, whose scope is the manifests (`class.rs:38`), so a refresh that touches source is refused;

- `accept.rs:115`: mechanical work must leave a green tree green;

- review, `pma pr` or `pma push`.

```sh
pma dispatch cynn:deps -a refresh             # one project
pma dispatch a:deps b:deps c:deps -a refresh  # several
```

With a route that sends class `A` to `refresh`, `pma dispatch --auto` sends deps to it and `#agent` items to their own routes.

A refresh that fails the check is rejected. The project keeps its state, and its run records the failing verify output. Hand it to an agent with `pma dispatch x:deps -a claude`. The deps prompt then carries that output beside the movement list.

## Not in scope

- **Advisories.** `cargo audit`, `pip-audit`, `govulncheck` or OSV would be a separate state ranked above stale deps. Add when security tracking is wanted here.

- **Raising constraints.** uv's hidden `uv upgrade <pkg>` ([uv#19738](https://github.com/astral-sh/uv/pull/19738), in 0.13.1) rewrites one constraint and leaves the lock. It is unfinished; [uv#6794](https://github.com/astral-sh/uv/issues/6794) tracks the design.

- **A failed tool counted as zero.** Cohort item 9 in `pilot.md` owns it. This change leaves the summing in `measure` as it is.

## Open questions

1. Escalation changes the model, not the worker (`dispatch.rs:1496`). Should a route escalate `refresh` to `claude` automatically, or stay manual? Automatic needs `escalate` to name a worker.

2. `go get -u ./...` also moves indirect modules. `go get -u=patch` is narrower. Which is go's compatible range for this purpose?

3. Do unattended runs (`--local --unattended`) apply to `refresh`? Mechanical work may run unattended today, and the check gates it, but a lock refresh that passes tests can still change runtime behaviour.

## Plan

1. `deps.rs`: uv measure, movement lines, cargo held-back count. Unit tests on captured output.

2. Lock age in `scan.rs`, stored beside `deps_at`; a migration.

3. `rank.rs`: the three-part state; `deps.days`.

4. `pma deps refresh <dir>`; the seeded worker; the deps prompt carries a refused refresh's verify output. CLI test with a fake `uv`.

5. Docs: README, `design.md` deps section, CHANGELOG.
