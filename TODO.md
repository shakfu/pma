# TODO

## Critical

## High

- [x] `pma sync` checks an item's text before writing `gh:N` into its line #agent `link` in `src/sync.rs` checks only that the planned line still holds an unlinked item. If `TODO.md` gains a line above it while `gh issue create` runs, `gh:N` lands on the wrong item, and the next sync retitles that issue and opens a second one for the real item. Compare the item's `todo::normal_text` with the planned title, and refuse the write on a mismatch, as a changed file is refused now. Add a unit test that edits the file between plan and link.

- [x] `pma lint` reports an unclosed code fence #agent `src/todo.rs` toggles fence state on any line starting with three backticks or tildes. A four-backtick fence closes on three, a tilde block closes on backticks, and a fence never closed hides every later item from the parser while lint exits 0. Close a fence only on a line of the same character at least as long as the opening one, as CommonMark does, and report a fence still open at end of file as an error at its opening line. Unit tests for each case.

- [x] **Stable item ids**: an optional trailing id, `- [ ] text ^k3f9q`: 5 random lowercase Crockford base32 characters after a caret, unique within the file. Identity is the id, else `gh:N`, else the text, so rewording an item keeps its age and its open run. `pma lint --ids --apply` writes one into every open item, and the workflow `todo` sink gives one to an item it adds. Not minted at dispatch: publishing ticks the same line on origin, so an uncommitted id in the clone would conflict on every pull. The attempt counter stays keyed on the text.

- [ ] `pma pr` and `pma push` check the approved tree after `HEAD` has moved. `publish_one` in `src/publish.rs` compares the worktree with `approved_tree` only while `HEAD` equals `approved_head`. A publish that commits and then fails, on a rebase conflict, a verify failure or a timeout, leaves the run approved with `HEAD` moved, and an edit made in the worktree afterwards is committed and published on the retry without anyone reading it. The same holds for a commit made after approval, or an approval that could not read `HEAD`. Compare the tree of the approved content with what is about to be published whatever `HEAD` is, and test a failed publish followed by an edit and a retry. Found by the Opus workflow trial (docs/dev/workflow-pilot.md). #agent

- [ ] **`lint` and `scan` see one level only**: `discover` keeps directories holding `.git` directly under a root, and `lint` maps a directory to `<dir>/TODO.md`. A nested TODO.md inside a repository is never read, so `pma lint <root>/*` exited 0 while `hax/rxa`, `pktpy/docs` and `py/source/tests/matrix` all had errors. Decide between recursion and a one-level rule stated in the help text.

## Medium

- [x] `pma scan` bounds each git and gh call with a timeout #agent `scan_project` in `src/scan.rs` runs `git` and `gh` through `Command::output()` with no deadline, so one hung `gh run list` hangs the whole scan. `src/deps.rs` already bounds its tools with `agent::run_limited`. Give scan's calls a deadline, record a timed-out call in the project's detail rather than failing the scan, and test it with a fake `gh` that sleeps.

- [x] A `##Critical` heading without a space is a lint error #agent

- [x] **`pma status` sorts by explicit state and drops the health score**: worst first, failing CI, then lint or scan errors, then unpushed commits or leftover `pma/` branches, then open Critical items, then stale deps, then idle past the tier's horizon, tier breaking ties. The score prompts no action, its 5 weights are uncalibrated, it double-counts signals the matrix already lists as tasks, and `status` is its only reader. `--explain` names the state that placed each project; the `weights.*` settings are retired.

- [x] **`pma next` as the default view**: one ordered list in two sections, "for agents" (eligible tasks in dispatch order, with why each is urgent) and "for you" (runs awaiting review, open pull requests, Critical or due items that are not eligible). `pma tui` shows it instead of the 2x2. `pma matrix` stays, with Q4 relabelled "Later", since under the default weights every `Medium` and `Low` item lands in "Remove".

- [ ] **Workflow iteration: retry, then laps, then recursion**: all three are refused at `propose` today. Start only when the single-pass engine has run a real review, confirm, fix workflow end to end several times on real repositories with no refusal caused by a runtime bug. Retry first: same unit, same worktree, one attempt counter per lineage (W23). Parallelism across nodes waits until the serial engine is stable; `max_parallel 1` makes a pass fully serial meanwhile.

- [ ] `pma pr` and `pma push` refuse a run with no changes. With nothing staged and no agent commit, `publish_one` in `src/publish.rs` still commits the `TODO.md` tick, so the later `rev-list --count` sees one commit and `nothing to publish: no changes` never fires: the task is marked done and pushed or opened as a pull request with no work in it. Check for changes before the tick, and test approving and publishing a run whose changed paths are empty. Found by the Opus workflow trial (docs/dev/workflow-pilot.md). #agent

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

## Low

- [ ] **Identity by slug rather than by directory name**: `discover` skips a repository whose basename another root already supplied, so two roots cannot both hold a `py`. Worth doing when a second root exists, not before.
