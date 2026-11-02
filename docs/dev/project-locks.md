# Project locks and parallel jobs

Decision, 2026-11-01. A run takes a lock on its project. Before this, one global `session.lock` was held for a run's whole life, so one `I` could stop all other work for 8005 s. See `REVIEW.md`, S2.

## Locks

| Holder | `session.lock` | `project-<name>.lock` |
|-|-|-|
| dispatch with named targets, publish, reject, rework, merge, `verify <name>` | shared | exclusive, each project named |
| workflow, campaign, `dispatch --auto`, `--tag`, `verify --tag` | exclusive | none |

Shared and exclusive exclude each other, so work over every project still runs alone.

**Liveness.** `fail_interrupted_runs` marks queued and running runs failed, which is safe only if no live process owns them. `flock` is released when a process dies, so holding a project's lock proves nothing live owns that project's runs. The call now fails only the projects its lock covers. This needs no pid record, which could be reused after a crash.

Over a pid or a per-run file: the lock already existed, and a crash cannot leave it stale.

## Jobs

The runner runs jobs on threads:

- A job about one project needs that project. Jobs in a project keep their order.

- `scan` needs nothing and runs beside anything.

- Anything else needs everything, and later jobs cannot overtake it, so it cannot wait forever.

- `max_parallel` caps headless project jobs. An interactive job does not count, since a person drives it.

A dispatch whose clone holds another run stays queued instead of failing. A later job in the project still runs, since it may be the push that frees the clone.

## Limits

- `max_parallel` caps jobs and also agents within a job, so a job with several projects can exceed it overall.

- `settle_committed` looks at every project while holding none of their locks, so it writes a run only if the run is still in the state it read.

- A dispatch waiting for its clone keeps the runner polling, and the front ends' `working` indicator on, until the run is decided.

- Two runs in one project still run one after the other, worktree runs included.
