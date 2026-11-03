# TODO

## Critical

## High

- [ ] Add `pma integrate <name>`: pma should integration adapters to integrate with [herdr](https://herdr.dev/tui/docs/cli-reference/) and also see <https://herdr.dev/tui/docs/socket-api/>; and also with [minos](https://github.com/shakfu/minos). Such integrations are mutually exclusive.

- [ ] Finish the pilot cohort and apply its gate #manual 1 of 20 items shipped since 2026-09-24 (`docs/dev/pilot.md`). Fix the `report` count above first, or the shares will mislead. (REVIEW.md U1)

- [ ] Decide whether pma is the dashboard or the dispatch loop #manual The evidence supports `scan`/`status`/`history`; most of the code serves dispatch and workflows. pma is for other developers, one per install, so the adoption items below are in scope; multi-user use comes later. (REVIEW.md, Questions)

- [ ] npm or pnpm dependency signals, and a better Python verify fallback deps covers `cargo`, `uv` and `go` (`crates/pma-core/src/deps.rs:24-26`). Without `uv.lock`, verify detection picks `python3 -m pytest` (`crates/pma-core/src/agent.rs:263-268`), likely without the project's dependencies (unverified). The README's Requirements now state what is supported. (REVIEW.md U7)

- [ ] Install pma with schema 49 and the agent entries #manual Install this `pma`, then check that the upgrade wrote `agents.claude.interactive_args` to `~/.config/pma/settings.toml` and nothing else. `make install` gilda, minima, hax, myra and hilda: the installed minima is 0.4.0, and none of the installed five takes a positional prompt. Then append `docs/dev/agent-entries.md`. Each agent item below needs this first.

- [ ] Test each agent as a pma worker #manual For every agent below, on a scratch project, check four things. A headless `pma dispatch` gives a run with a non-empty summary, a cost where the entry says `reports_cost`, and a failed verdict when the model name is wrong. `--interactive` (`I`) opens a pane where the task is already the first turn, and the run ends when the agent exits. `T` on a finished run opens that agent with the take-over prompt. herdr raises "an agent needs you" when the agent waits for input; unknown for every agent but claude. Record the results in `docs/dev/agent-entries.md` and correct the entries.

- [ ] Test claude after the schema 49 upgrade #manual `--interactive` and `T` built their command from the headless one; both now read `interactive_args`. `T` now passes the run's model, which it did not.

- [ ] Test codex as a pma worker #manual Unverified: that `codex` and `codex exec` take `--` before the prompt; what `--approve-for-me` asks of a person in a pane; whether `exec`'s stdout gives a readable `text-tail` summary. Cost is unknown: `exec --json` exists, but its result shape has not been read.

- [ ] Test pi as a pma worker #manual Each positional word is a separate message and `@word` reads a file, so the entry passes `--` and the task as one word; check a task that starts with `@` or `-`. pi has no permission mode, so the worktree is its only bound. Cost is unknown.

- [ ] Test opencode interactively #manual `interactive_args` comes from `opencode --help`: `{dir} --auto --prompt`. The TUI takes no `--model`, so an interactive run ignores the preset's model.

- [ ] Decide omp's entry #manual omp is not installed. Install it and write `interactive_args`, or remove the worker.

- [ ] Test gilda as a pma worker #manual Parser `json:text:usage.cost:error`. A `truncated` outcome exits 0 with a null error, so pma counts it as a success. Headless `--permissions auto` asks before touching secrets or writing outside the root; check that with no terminal it denies rather than hangs.

- [ ] Test minima as a pma worker #manual Parser `json:text:cost:error`; a failed run's message is only in `.error`, which pma now shows as the summary. The entry leaves `--sandbox` off. With it on, `git` inside a linked worktree is probably denied, since the worktree's `.git` points outside the root (read from the code, untested).

- [ ] Test hax as a pma worker #manual Parser `json:text:cost:error`. `.cost` is omitted when spend is 0, which pma records as unknown. hax has no permission mode. A first word that names a hax preset is taken as `--preset`; pma's args start with `--model=`, so check a run with no model chosen and a one-word task.

- [ ] Test myra as a pma worker #manual Parser `json:text:cost:error`, with `-C {dir}` and `--json`. `truncated` covers a length cut and the 100-call cap, and exits 0. Cost is null on a local server. The Python port was tested only under Python 3.15.

- [ ] Test hilda as a pma worker #manual Parser `json:result:usage.cost:error`; cost is in OpenRouter credits. The entry runs `-M yolo`, the only mode that works unattended, so bash runs with the user's permissions. hilda has no `-C`; pma sets the directory. Exit 2 (`--max-turns`) and 3 (`--max-cost`) are failures to pma.

## Medium

- [ ] Retest live updates in the web after the blank-page fix #manual The event stream existed but was never opened (see High). Many of the out-of-sync views reported on 2026-10-27 likely came from that. Retest before building more.

- [ ] Show a hint of a workflow's state machine when it is selected in `z` The server would send a short form of the graph with each choice.

- [ ] Project settings as one field per setting, not one typed line Fields need a new `Ask` kind in `keys`, built in both front ends. Cheaper: one row per setting in the project view, each edited alone, as the settings tab does.

- [ ] Make the worktree option easier to find It exists: "edit in a worktree" in `z`, `--worktree`, and `pma config workspace worktree`. Colleagues asked to revisit the clone default; `docs/dev/clone-runs.md` holds why it was chosen. Ask which of its reasons fails for them before changing the default.

- [ ] Undo the last rework: return to the previous attempt rather than rejecting the run Reject reverts every attempt. Attempts are append-only rows, but a clone run reverts one patch, so this needs each attempt's patch kept. Open: is the need "discard the latest rework, keep the earlier one approvable"?

- [ ] Scheduled runs within a budget, finding issues and adding them to TODO.md with duplicates merged Proposal and open decisions: `docs/dev/scheduling-work.md`. Step 1, `instance_budget`, built 2026-10-27. Next: a week of `pma run review` by hand on 3 projects (cost per project, share of findings kept), then step 2, confirm by severity. Tapping into an agent's session is the session-id item below.

- [x] Click through the web page after the drawer removal #manual Done 2026-10-27 in headless Chromium (Playwright's cache), against a scratch store with two runs: tabs, the address, the chooser, prompts, a run's detail, the commit prompt and its notice's Open button, reject, the activity filter, More, and 1100 and 720 px. No console errors.

- [ ] Show open pull requests and issues opened by others as signal tasks `scan` reads only CI from GitHub (`crates/pma-core/src/scan.rs:680`), and `sync` lists issues by others as untriaged but ranks none. A project with unanswered contributor pull requests shows as `clear`. Read them with `gh pr list` and `gh issue list` during `scan`, rank them by age, and do not make them dispatchable by default. (REVIEW.md U6)

- [ ] A task source that is never pushed, for public repositories Dispatch needs the item in `TODO.md` on origin (`crates/pma-core/src/dispatch.rs:601`), so every task an agent takes on an OSS repository is published first. Options: a gitignored `.pma/TODO.md`, or items held in the database. This conflicts with "TODO.md is the only task file" (`docs/dev/design.md`); decide that first, or document the exposure. (REVIEW.md U5)

- [ ] A per-project setting that refuses host mode For private code, `projects.<name>.worker_mode = "container"` would stop a dispatch from running unconfined by accident. Host mode leaves `~/.ssh`, reads and the network open. (REVIEW.md U10)

- [ ] Propose `#agent` tags across the portfolio 15 of 1,201 open items carry `#agent`, so `dispatch --auto` has little to take. A read-only workflow, as `todo-audit` is, could propose tags; or a per-project default could mark items that have a description. (REVIEW.md U4)

- [ ] Per-project session locks One agent command and one job run at a time across all projects (`docs/dev/async_dispatch.md`, open question 1), so a detached job on one project blocks a dispatch on another. Needs a shared `batch_budget` ledger. (REVIEW.md U8)

- [ ] `pma merge` refuses a pull request with failing checks or a moved head #agent `publish::merge` refuses only a conflict, so it merges a pull request with failing or pending checks, and commits pushed after review. Refuse failing or pending checks; allow none. Refuse a head that is not the commit pma pushed: record that commit at `pma pr`, which today records only the URL, and pass it to `gh pr merge --match-head-commit`. Tests in `tests/cli.rs` with the fake `gh` (`FAKE_GH_PR`): each refusal. Also from the original item and not built: several ids in one `pma merge`, and `--delete-branch`.

- [ ] The process-group kill does not reach a child that calls `setsid` (`src/agent.rs:105-106`, `119`). `docs/dev/design.md:377` says no agent outlives the session. A survivor's writes to the worktree no longer reach an approval or a publish: an approval must match the tree last shown, and publishing starts from the approved tree. It still runs, reads what the user can, and writes outside the worktree. A cgroup or PID namespace would bound it; `sanduk` does. (REVIEW.md M10)

- [ ] Record each run's agent session id, so `resumes` does something `Worker::resumes` is stored, settable and listed by `pma agent`, but no dispatch path reads it (`crates/pma-core/src/worker.rs:210`). `opencode()` sets it `true`, so the listing claims a feature pma lacks. A `{session}` placeholder in worker args (`claude --session-id <uuid>`, as wallfacer does) plus a session column on the run would let a rework continue the earlier session instead of re-prompting, and let `pma review N --open` hand the terminal to `claude --resume <id>` in the worktree. Takeover must run under the `agent::restrict` environment, or it gains push credentials. Open: does a `-p` session resume interactively; does a `sanduk` transcript survive its container; what is opencode's flag. If neither use is wanted, delete the field.

- [ ] Calibrate bundles after 10 bundled runs #manual A bundle runs under one task's `agent_budget` ($1.00) and `timeout` (30 min), holds at most 8 items (`dispatch::BUNDLE_MAX`), and a failure uses an attempt of the bundle, not of each item. All four are defaults no run has tested (`docs/dev/bundled-dispatch.md`, "Defaulted, open"). For each bundled run record cost, time, items, items dropped at approval, and whether one item's failure failed the run. Then decide whether budget and timeout scale with item count. If most bundles end with items dropped, reconsider one commit per item, which the one-diff limit rules out today.

## Low

- [ ] Agent logs are re-read whole on every 2 s refresh `watch::agents` reads and parses each running agent's `stream-json` log in the TUI, the web server and its event thread (`crates/pma-core/src/watch.rs`). Read the tail for the last tool and text, and cache the tool count by file offset. (REVIEW.md U7, 2026-11-01)

- [ ] A run detail in the `margo` clone takes 0.65 s where one elsewhere takes 0.10 s `pma review 9` against `pma review 10`, release build, 2026-11-01. `Query::Run` runs `dispatch::tree` twice and `dispatch::diff` on the clone; the TUI's one thread waits on it when the detail opens. Measure which call, then cache the tree by the clone's index mtime or compute it once.

- [ ] Mark commands without a recorded pilot as experimental in `--help` and the README 34 top-level commands; routes and campaigns have no recorded runs. (REVIEW.md U11)

- [ ] Date or range the README's scan time It says 95 repositories in about 15 s; 80 took 24.6 s on 2026-10-25. (REVIEW.md U12)

- [ ] Record a hand estimate of each task's time, so a time-saved figure exists `--minutes` records review time only. (REVIEW.md, Questions)

- [ ] Citations of the earlier REVIEW.md (`M10`, `L5`, `D4`, ...) in TODO.md and `docs/dev/workflows.md` point at a file that now holds another review Rewrite them, or keep earlier reviews under `docs/dev/`.

- [ ] Dispatched runs likely leave untracked transcripts in `~/.claude/projects/<encoded-worktree>/` Each worktree is a new cwd, so each run probably adds a session pma neither records nor removes, and it shows in `claude --resume`. Unverified: check after a real run. Then choose: keep them as an audit trail of what the agent did, which the diff does not show, or remove them with the worktree. wallfacer's `CompanionFiler` handles the same cleanup problem.

- [ ] Accept an item id, or a unique prefix of one, as a dispatch target: `myproject:^k3f` Targets are `project:line` (`crates/pma/src/main.rs:230`), and a line number goes stale when TODO.md is edited after the scan; an id does not. An ambiguous prefix lists the candidates rather than guessing, as wallfacer resolves its refs.

- [ ] `retry` on `edit`, then lap edges, then recursion (`docs/dev/workflows.md` section 19, parts 6c-6e). Gated: 6c needs 10 runs published through batch approval.

- [ ] **Identity by slug rather than by directory name**: `discover` skips a repository whose basename another root already supplied, so two roots cannot both hold a `py`. Worth doing when a second root exists, not before.

- [ ] `user_version` is read before the migration transaction opens (`src/store.rs:658-693`). Two processes starting after an upgrade both replay the steps; the second fails with "already exists". (REVIEW.md L5)

- [ ] `project import --apply` and `project tier` apply many rows without a transaction (`src/main.rs:1196-1227`). (REVIEW.md L8)

- [ ] `pma pr` pushes `run.branch`; if the agent switched branches, the commit is elsewhere (`src/publish.rs:195`). (REVIEW.md L11)

- [ ] `prune` and `lint --ids` do not re-read TODO.md before saving, so an edit made between the read and the save is lost (`crates/pma/src/main.rs`). The shared temp name is fixed. (REVIEW.md L13)

- [ ] Errors in `lint`, `prune`, `sync` and publish go to stdout (`crates/pma/src/main.rs`). The `prune --apply` exit code is a pilot cohort item in Medium. (REVIEW.md L14)

- [ ] Open: does `cargo update --dry-run` ever print lines `parse_cargo` (`crates/pma-core/src/deps.rs`) misses? The counting bug beside it is a pilot cohort item in Medium. (REVIEW.md L15)

- [ ] Smaller review items, unverified: run scope stored comma-joined (`src/store.rs:1149`); the CI prompt has no byte cap and can exceed the argument limit (`src/dispatch.rs:726`); cost over budget is judged on cumulative cost across reworks (`src/accept.rs:91`); table widths count characters, not display columns. (REVIEW.md)

- [ ] Windows support pma-core assumes Unix, so a Windows build stops at a `compile_error!`; `release.yml` builds Linux and macOS only. Sites: the `sh` watchdog and `kill -KILL -<pgid>` (`crates/pma-core/src/agent.rs:121`, `:134`; `scan.rs:357`; `jobs.rs:186`), `process_group(0)` (`agent.rs:147`, `scan.rs:314`, `jobs.rs:63`), `Command::new("sh")` in `agent.rs`, `jobs.rs:152`, `scan.rs:1081` and `dispatch.rs:1401`, and mode bits in `agent.rs:70` and the hook snapshot (`dispatch.rs:375`). Needs a job object to kill a process tree (`windows-sys`), `CREATE_NEW_PROCESS_GROUP` for the runner, a Windows rule for which hooks git runs, and a Windows CI job. sanduk-sandbox has no Windows backend, so confined verify fails closed there. Open: is WSL, which runs the Linux binary unchanged, enough? Until then, a `compile_error!` in `pma-core/src/lib.rs` names the limit; remove it with the port.

- [ ] `pma task <project:line>` does not list a bundle among the runs holding the item. The busy check sees it through `run_items` (`api::has_run`); the task detail's `held_by` (`api::task_view`) matches `runs.task_key` alone, which a bundle sets to `bundle`. #agent

- [ ] Approving a bundle from its run detail in the TUI returns to the list, not the detail. The checklist replaces `Mode::Detail` (`App::checking`), so Enter or Esc leaves both.

- [ ] `make web-check` on Linux: the default `CHROME` in `scripts/web-check.sh` is a macOS path, and headless Chromium aborts with "No usable sandbox" where unprivileged user namespaces are blocked (Ubuntu's AppArmor rule). It ran on 2026-10-28 through a wrapper adding `--no-sandbox`. Options: a `CHROME_FLAGS` variable, or detect the Linux path in Playwright's cache.
