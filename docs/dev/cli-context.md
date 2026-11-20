# CLI context, confirmation and help

Decision, 2026-11-20. A command acts on the project you are in, asks before it does what cannot be undone, and its help says what it would act on. The CLI, the TUI and the web follow the same rules.

## Scope

For `pma <command> [<flags>] [<project>...]`, the scope is, in order:

1. The projects named.
2. `--tag`, or `-A/--all-projects`.
3. The context: `$PMA_PROJECT`, else the project whose directory holds the cwd. A front end's project page runs its `:` jobs in that project's directory; its other jobs run in pma's state directory, inside no project.
4. Every project.

`--tag` overrides the context, so a flag means the same wherever it is typed. `-A` ignores the context. `--all` keeps its older, per-command meanings (more rows, untiered projects, every project for `run`), so the context flag needed another name.

A command that takes one project (`shell`, `project show`, `project tier`, `deps refresh`) fails with no scope. `lint` and `prune` take the context project's TODO.md.

| Kind | Commands | Without a project, before | Now, in a project |
|-|-|-|-|
| one project | `shell`, `project show/tier/tag/forget`, `deps refresh`, `task` | error | that project |
| targets | `dispatch`, `run <name>`, `workflow run <name>`, `verify` | error, or a list | that project |
| portfolio | `next`, `status`, `scan`, `matrix`, `stale`, `sync` | every project | that project; `-A` for all |
| files | `lint`, `prune` | `./TODO.md` | the project's TODO.md |
| runs | `review`, `stage`, `commit`, `push`, `pr`, `merge`, `close` | run ids | unchanged |

### References

| Ref | Means |
|-|-|
| `.` | the context project; an error without one, except as a path |
| `:12`, `:ci`, `:critical` | that target in the context project |
| `%` | the selected row, on a front end's `:` line |

## Confirmation

| Class | Commands | Asks |
|-|-|-|
| Leaves the machine | `push`, `pr`, `merge`, `sync` | yes |
| Deletes pma state | `review --reject`, `close`, `project forget`, `root rm`, `agent rm`, `preset rm`, `campaign rm` | yes |
| Many at once | `run --all` or `--tag`, `project clone`, `project import` | yes |
| Rewrites TODO.md | `prune`, `lint --fix`, `lint --ids` | only to remove text that `HEAD` lacks |
| Settings, reads | the rest | no |

- No flag: print the plan, ask `[y/N]`.
- `-y/--yes`: act without asking.
- `-d/--dry-run`: print the plan and stop. `-n` was taken: it is `--count` on `stale`, `dispatch` and others.
- No terminal and no `-y`: refuse. A job counts as approved: whoever queued it confirmed, in a front end's dialog or with `-y`.

`--apply` is gone. Default-to-act over default-to-preview: the common case is one command, and a prompt over every act trains a reflexive `y`. `prune` needs no prompt because git restores what it removes, except text that was never committed; that case asks.

`PMA_JOB` is removed from an agent's environment, so an agent a job starts is not taken as approved. `PMA_PROJECT` is removed from agents, the job runner, and the help and plan processes a front end starts: a job's context is its directory.

## Front ends

- A `:` line naming a `pma` command runs it as a job. On a project page the job runs in the project's directory, which sets the context, and the job is the project's.
- `%` is replaced by the selected row: a run or job id, a task's target, a project's name.
- A bare name the view also has (`:push`) is the view's act on the selection, as its key is. With arguments it is the CLI. Bare `:push` must not become `pma push`, which acts on no row.
- The confirm dialog shows the command's `-d` plan where it has one.

## Help

One function gives, for a command and a context, the scope it acts on and why. Commands, help and the front ends call it.

- `pma help <cmd>` and `pma <cmd> --help` start with `Here: acts on argdec (cwd); -A for every project.`
- Bare `pma help` starts with `Context: argdec (cwd)` or `Context: none (every project)`.
- `:help` lists the commands that apply on this page, with the context filled in.
- `:help <cmd>` is the CLI's help run in the page's context, with the selection named.

## Not done

Keys as CLI templates, which would remove `Act`, `Action` and `screen::action_in` as separate layers. It is not needed for the above; decide after use.
