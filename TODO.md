# TODO

## Critical

## High

- [x] `pma sync` checks an item's text before writing `gh:N` into its line #agent `link` in `src/sync.rs` checks only that the planned line still holds an unlinked item. If `TODO.md` gains a line above it while `gh issue create` runs, `gh:N` lands on the wrong item, and the next sync retitles that issue and opens a second one for the real item. Compare the item's `todo::normal_text` with the planned title, and refuse the write on a mismatch, as a changed file is refused now. Add a unit test that edits the file between plan and link.

- [x] `pma lint` reports an unclosed code fence #agent `src/todo.rs` toggles fence state on any line starting with three backticks or tildes. A four-backtick fence closes on three, a tilde block closes on backticks, and a fence never closed hides every later item from the parser while lint exits 0. Close a fence only on a line of the same character at least as long as the opening one, as CommonMark does, and report a fence still open at end of file as an error at its opening line. Unit tests for each case.

- [x] **Stable item ids**: an optional trailing id, `- [ ] text ^k3f9q`: 5 random lowercase Crockford base32 characters after a caret, unique within the file. Identity is the id, else `gh:N`, else the text, so rewording an item keeps its age and its open run. `pma lint --ids --apply` writes one into every open item, and the workflow `todo` sink gives one to an item it adds. Not minted at dispatch: publishing ticks the same line on origin, so an uncommitted id in the clone would conflict on every pull. The attempt counter stays keyed on the text.

- [x] `pma pr` and `pma push` check the approved tree after `HEAD` has moved. `publish_one` in `src/publish.rs` compares the worktree with `approved_tree` only while `HEAD` equals `approved_head`. A publish that commits and then fails, on a rebase conflict, a verify failure or a timeout, leaves the run approved with `HEAD` moved, and an edit made in the worktree afterwards is committed and published on the retry without anyone reading it. The same holds for a commit made after approval, or an approval that could not read `HEAD`. Compare the tree of the approved content with what is about to be published whatever `HEAD` is, and test a failed publish followed by an edit and a retry. Found by the Opus workflow trial (docs/dev/workflow-pilot.md). #agent

- [x] A retried publish commits files written by the publish-time verify. `uv run pytest` can rewrite `uv.lock`; a test run can write a coverage file or a snapshot. If that publish fails on a rejected push, the retry commits the verify output with no hostile agent involved. Same root cause as the item above. Record the commit id `pma` created and accept a moved `HEAD` only when it equals that id and the worktree is clean, or build the commit from the approved tree object with `git commit-tree`. (REVIEW.md H4)

- [x] **`lint` and `scan` see one level only**: `discover` keeps directories holding `.git` directly under a root, and `lint` maps a directory to `<dir>/TODO.md`. A nested TODO.md inside a repository is never read, so `pma lint <root>/*` exited 0 while `hax/rxa`, `pktpy/docs` and `py/source/tests/matrix` all had errors. Decide between recursion and a one-level rule stated in the help text.

- [x] The default preset outranks an applied route. `choose_worker` in `src/dispatch.rs:781-797` merges the `-p` preset and the default preset into `named` and reads it before the route. Stated order (`README.md:184`, `docs/dev/design.md:355`, `dispatch.rs:769`): flags, `-p`, applied route, default preset. With a default preset and a route naming `agent: codex, model: haiku`, `pma dispatch --auto` runs the preset's worker, and the run still records the route name. Migration 24 creates default presets, so upgraded databases are in this state. Keep the two presets apart and read the default one after the route; add a test that fails today. (REVIEW.md H1)

- [x] A shadow policy still refuses, scopes and sets approval. `src/dispatch.rs:575-591` gates only agent and model on `rev.shadow`; `README.md:217` says `--shadow` records without applying. Under shadow: a task matching no route is refused; `route.scope` replaces the class scope, and `"scope": []` removes the manifest bound of a `deps` run; `route.approval` is stored, and `propose` blocks `pma review --approve`. Under shadow, record the revision and route name only; add a test. (REVIEW.md H2)

- [x] Policy parsing ignores unknown keys. `parse_route` in `src/route.rs:262-387` reads known keys by index, and a missing key reads as `Null`, meaning no condition. `"complexty"` or `"clas"` in `match` widens the route to match everything; `"match": "A"` matches everything; `"model": 4` is dropped; `"attempts": 3` is clamped to 1 (`route.rs:341`). The stored document is the parsed form, so `pma route show` hides the typo. Contradicts `route.rs:191`. Reject keys outside the known set at route and `match` level, require `match` to be an object, reject non-string `agent` and `model`, bound `complexity` and `tier` to 1-5. (REVIEW.md H3)

## Medium

- [x] `pma scan` bounds each git and gh call with a timeout #agent `scan_project` in `src/scan.rs` runs `git` and `gh` through `Command::output()` with no deadline, so one hung `gh run list` hangs the whole scan. `src/deps.rs` already bounds its tools with `agent::run_limited`. Give scan's calls a deadline, record a timed-out call in the project's detail rather than failing the scan, and test it with a fake `gh` that sleeps.

- [x] A `##Critical` heading without a space is a lint error #agent

- [x] **`pma status` sorts by explicit state and drops the health score**: worst first, failing CI, then lint or scan errors, then unpushed commits or leftover `pma/` branches, then open Critical items, then stale deps, then idle past the tier's horizon, tier breaking ties. The score prompts no action, its 5 weights are uncalibrated, it double-counts signals the matrix already lists as tasks, and `status` is its only reader. `--explain` names the state that placed each project; the `weights.*` settings are retired.

- [x] **`pma next` as the default view**: one ordered list in two sections, "for agents" (eligible tasks in dispatch order, with why each is urgent) and "for you" (runs awaiting review, open pull requests, Critical or due items that are not eligible). `pma tui` shows it instead of the 2x2. `pma matrix` stays, with Q4 relabelled "Later", since under the default weights every `Medium` and `Low` item lands in "Remove".

- [ ] **Workflow iteration: retry, then laps, then recursion**: all three are refused at `propose` today. Start only when the single-pass engine has run a real review, confirm, fix workflow end to end several times on real repositories with no refusal caused by a runtime bug. Retry first: same unit, same worktree, one attempt counter per lineage (W23). Parallelism across nodes waits until the serial engine is stable; `max_parallel 1` makes a pass fully serial meanwhile.

- [x] `pma pr` and `pma push` refuse a run with no changes. With nothing staged and no agent commit, `publish_one` in `src/publish.rs` still commits the `TODO.md` tick, so the later `rev-list --count` sees one commit and `nothing to publish: no changes` never fires: the task is marked done and pushed or opened as a pull request with no work in it. Check for changes before the tick, and test approving and publishing a run whose changed paths are empty. Found by the Opus workflow trial (docs/dev/workflow-pilot.md). #agent

- [ ] A project whose name contains a dot can take `projects.<name>.verify` #agent

- [ ] `pma review 3 4 --approve --minutes 5` drops the minutes #agent

- [ ] `pma config <retired key>` says the key is unknown instead of explaining the retirement #agent

- [ ] `pma project import` refuses a CSV saved with a byte-order mark #agent

- [ ] A pull request closed without merging does not count as an attempt on its task #agent

- [ ] `pma dispatch --retry` resets the attempt count before it knows the run started #agent

- [ ] `pma merge <id>...` merges a run's pull request and settles it #agent Today a `pr-open` run is merged on GitHub by hand, and the next `pma review` settles it. `pma merge` runs `gh pr merge <url> --squash --delete-branch` for each named `pr-open` run, then settles it to `merged` at once, as `publish::settle` does for a merge it observes. Refuse a run that is not `pr-open`. Refuse one whose pull request has failing or pending checks; allow one with no checks. Refuse one whose head is no longer the commit pma pushed, since that merges commits no one approved: record the pushed commit at `pma pr`, which today records only the URL. Tests in `tests/cli.rs` with the fake `gh` (`FAKE_GH_PR`): a merge, each refusal, and the run merged afterwards.

- [ ] **Render `gh:N` as a link**: `report`, `rank`, `matrix` and `tui` print a bare issue number, and the project row now stores `owner/name` to resolve it against.

- [ ] **Absent projects still rank**: `status` and `matrix` score a project no scan can find, so its tasks compete for attention when nothing can be dispatched against them. Either drop them from ranking or mark them in the listing.

- [ ] **`pma clone --tag <tag>`**: create the checkout for a project whose record exists but whose working tree does not, from its stored `owner/name`. Needs a target root when several are registered, and a rule for a directory name already taken under another one.

- [ ] `update_run` can overwrite a newer run state. It writes 21 columns from an in-memory copy with no state guard and ignores the affected-row count (`src/store.rs:1178`). `review --approve` and `--minutes` hold no session lock (`src/main.rs:2068-2101`), so a stale copy can overwrite `pushed` with `approved`. Decide the concurrency model first: every writing command takes the session lock, or `BEGIN IMMEDIATE` with guarded, column-scoped updates. (REVIEW.md M1)

- [ ] `atomically` opens a deferred transaction and its callers read before they write (`src/store.rs:1617`). SQLite returns `SQLITE_BUSY` without waiting when a read lock is upgraded while another writer holds the reserved lock. At `src/pass.rs:2186` the agent has already run. Failure is inferred, not reproduced. Same decision as M1. (REVIEW.md M2)

- [x] Worker placeholders are substituted after the prompt is inserted (`src/worker.rs:259-273`). A task containing `{model}` with no model chosen drops the prompt argument and the flag before it; with a model chosen, `{model}`, `{dir}`, `{budget}` and `{timeout}` in task text are rewritten. Substitute before inserting the prompt. (REVIEW.md M3)

- [ ] The batch budget counts unknown cost as zero (`src/dispatch.rs:881-884`, `910`). `omp`, `opencode` and runs killed at the timeout report no cost, so `batch_budget` never stops them. (REVIEW.md M4)

- [ ] `reject` consumes an attempt for a run that never started (`src/dispatch.rs:1367-1372`). Two such rejections reach the attempt limit. Open: can a run refused by the batch budget be cleared without `reject`? (REVIEW.md M5)

- [ ] A project ranked through `default_tier` is routed with `tier: None`, so a route with a tier condition never matches it (`src/main.rs:1712`, `1848`). Unverified. (REVIEW.md M6)

- [ ] `route replay` compares only route name, agent and model (`src/route.rs:417-437`). A change to `approval` or `scope` prints "0 routed differently". Unverified. (REVIEW.md M7)

- [ ] `eligible` ignores `#manual` (`src/main.rs:1496`). An `#agent #manual` item is listed under "For agents" and missing from "For you"; dispatch does refuse it. (REVIEW.md M8)

- [ ] `names_a_place` matches `and/or`, `e.g.`, `Node.js` (`src/complexity.rs:86-96`). Complexity drops by one, which can select a weaker model and a higher autonomy band. Unverified. (REVIEW.md M9)

- [ ] The process-group kill does not reach a child that calls `setsid` (`src/agent.rs:105-106`, `119`). `docs/dev/design.md:377` says no agent outlives the session. A survivor's writes to the worktree no longer reach an approval or a publish: an approval must match the tree last shown, and publishing starts from the approved tree. It still runs, reads what the user can, and writes outside the worktree. A cgroup or PID namespace would bound it; `sanduk` does. (REVIEW.md M10)

- [x] Credentialed git runs in the worktree after the agent, with the hook and config snapshot checked only at publish (`src/dispatch.rs:1043-1054`, `1309-1320`). A `filter.*.clean` or `diff.external` entry written to `<repo>/.git/config` runs under `pma review`. Only `core.fsmonitor` is disabled. Unverified. (REVIEW.md M11)

- [x] A setting or hook an agent adds to the clone stays after its run fails. Dispatch, `pma verify` and workflow nodes now refuse the repository until it is restored or the run is rejected with `--keep-git-changes`.

- [x] `pma scan` still runs `git status` in a clone holding a setting a failed run found, which runs an added `core.fsmonitor`. Such a clone is now left unread; it keeps its last scan and records why as its scan error.

- [ ] `todo::insert` finds headings without tracking code fences (`src/todo.rs:694-706`). A `## example` line inside a fence receives the new item; the parser does not see it there, so each resumed pass inserts another copy. (REVIEW.md M12)

- [ ] README claims overstate the agent sandbox and budget. "No push credentials": `HOME` is unchanged, so `~/.ssh`, `~/.git-credentials` and `~/.netrc` stay readable, and `GH_ENTERPRISE_TOKEN`, `GIT_ASKPASS`, `SSH_ASKPASS` are not removed (`src/agent.rs:23-38`). "Dollars per run": no shipped worker enforces a budget (`src/accept.rs:89`). Reword to "no push credentials in its environment" and to what `accept.rs:89` says, or close the gaps. Open: does `gh` authenticate from the system keyring with `GH_CONFIG_DIR` set to an empty directory? (REVIEW.md, README claims)

## Low

- [ ] **Identity by slug rather than by directory name**: `discover` skips a repository whose basename another root already supplied, so two roots cannot both hold a `py`. Worth doing when a second root exists, not before.

- [ ] `prune` deletes an open checkbox nested under a finished item; the dry run lists only the parent (`src/todo.rs:636`, `src/main.rs:952`). (REVIEW.md L1)

- [ ] `sync::gh`, `publish::gh_in` and `wt_git` have no timeout, so a stalled fetch holds the session lock (`src/sync.rs:196`, `src/publish.rs:351`, `src/dispatch.rs:168`). Reuse the bound `scan::call` has. (REVIEW.md L2)

- [ ] `forget_project` leaves `projects.<name>.verify` (`src/store.rs:1029-1036`). A later unrelated repository of the same name runs the old shell command. (REVIEW.md L3)

- [ ] `home()` accepts an empty or relative `PMA_HOME` (`src/store.rs:617-623`), so the database and session lock depend on the working directory. `state_home()` already filters empty values. (REVIEW.md L4)

- [ ] `user_version` is read before the migration transaction opens (`src/store.rs:658-693`). Two processes starting after an upgrade both replay the steps; the second fails with "already exists". (REVIEW.md L5)

- [ ] `Slot::Count` has no upper bound; `timeout * 60` wraps in release and panics in debug (`src/config.rs:263`, `src/publish.rs:154`). (REVIEW.md L6)

- [ ] CSV export does not round-trip a tag containing a comma or a project named `#x` (`src/projects.rs:42-112`). (REVIEW.md L7)

- [ ] `project import --apply` and `project tier` apply many rows without a transaction (`src/main.rs:1196-1227`). (REVIEW.md L8)

- [ ] The agent log is read as strict UTF-8; one invalid byte gives an empty string, a failed run and no cost (`src/dispatch.rs:1028-1031`). (REVIEW.md L9)

- [ ] `parse_claude` takes the last line that parses as JSON, then checks its type, so a later JSON line on stderr hides the result (`src/agent.rs:182-186`). (REVIEW.md L10)

- [ ] `pma pr` pushes `run.branch`; if the agent switched branches, the commit is elsewhere (`src/publish.rs:195`). (REVIEW.md L11)

- [ ] The publish tick uses `std::fs::write` (`src/publish.rs:138`); every other TODO.md writer uses the atomic `todo::save`. (REVIEW.md L12)

- [ ] TODO.md writers share the temp name `.TODO.md.pma-tmp` (`src/todo.rs:758`), and `prune` and `lint --ids` do not re-read before saving. (REVIEW.md L13)

- [ ] Errors in `lint`, `prune`, `sync` and publish go to stdout, and `prune --apply` on a file with lint errors exits 0 (`src/main.rs:809-933`, `3405`, `3476`). (REVIEW.md L14)

- [ ] One failed dependency tool is counted as zero outdated when another succeeds (`src/deps.rs:38-47`). Open: does `cargo update --dry-run` ever print lines `parse_cargo` (`src/deps.rs:110`) misses? (REVIEW.md L15)

- [ ] A leading byte-order mark makes the whole TODO.md a lint error with no stated cause (`src/todo.rs:226`). (REVIEW.md L16)

- [ ] The route example at `README.md:78` has no `approval` and is refused by the parser (`src/route.rs:358`). (REVIEW.md L17)

- [ ] Smaller review items, unverified: run scope stored comma-joined (`src/store.rs:1149`); a code fence indented 1-3 spaces does not hide its content (`src/todo.rs:205`); `gh:N` cast from `u64` to `i64` (`src/scan.rs:404`); the CI prompt has no byte cap and can exceed the argument limit (`src/dispatch.rs:726`); cost over budget is judged on cumulative cost across reworks (`src/accept.rs:91`); table widths count characters, not display columns. (REVIEW.md)

- [ ] `docs/dev/design.md` drifts from the code: line 463 names `serde` and `toml`, absent from `Cargo.toml`; line 46 lists `codex` and `cursor-agent`, the README lists `claude`, `opencode`, `omp`. (REVIEW.md, Design)
