# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- **`pma integrate herdr`** runs every agent in a herdr pane: a tab per run in a `pma: <project>` workspace, closed when the run is decided. Headless by default, which keeps cost, budget and verdict; `I` or `--interactive` gives a live `claude` that reports none of them. `H` opens a run's pane and `T` opens an interactive agent on its directory. A single `integration` setting, since integrations exclude each other. Over the socket API, with an `events.subscribe` connection per run: a blocked agent shows on the run and raises a toast, and closing a pane ends its run at once. Design: `docs/dev/herdr-integration.md`.

- `D` in the TUI and web dispatches a task and pushes the run if its gates are clean, as `pma dispatch --push` does.

- On the projects tab, Space marks projects and `z` runs one name over them. `pma run --tag` was the only way to run over several.

- A decision from a run's detail opens the next ready run in the view, so reviewing M runs takes no navigation between them. Stage and commit stay on their run: it has a next step, and their notice offers to open it.

- `pma report` measures review time: from a run's first reading to the decision on it, at most 30 minutes a reading. It stayed at 0 when `t` and `--minutes` were the only way in; a stated figure still wins, over adding the two, which would count one reading twice. A run staged and never approved is not timed.

- `U` in the TUI and web pushes every approved, staged or committed run in the list. Publishing several runs was CLI-only, so M runs took 2M keys.

- `pma push <id> --approve [--drop N,...]` approves a ready run and pushes it in one command, as `u` does.

- **`pma merge <id>`** merges a run's open pull request; `M` does so in the TUI and web. A conflicting one is refused, and `pma merge <id> --fix` or `F` has an agent resolve the conflicts on the default branch. The run returns to review, so its diff is read before anything is pushed, and `pma pr` updates the same pull request. That push is forced only over the tip the fix read, so a commit pushed since is refused rather than lost. `stage`, `commit` and `push` refuse a fix, which would leave the pull request open and untracked, and the TUI and web do not offer them on one. Rejecting a fix returns the run to `pr-open` and charges no attempt, and retrying a failed one runs the fix again. Rebuilding on the branch was chosen over closing and redispatching, which charges an attempt and discards the pull request's discussion.

- `Shift+Tab` moves to the previous list in the TUI and the web. The TUI ignored it; the web treated it as `Tab` and moved forward.

- **Bundles.** The `TODO.md` items `pma dispatch` names in one project are one run: one agent session, one diff, one review, one commit. The prompt numbers the tasks. `pma review <id> --approve --drop 2,3` leaves those items open; publishing ticks the rest, lists them in the commit body and closes each one's issue. A dropped item's changes stay in the diff. A bundle is never clean, so `--push` and batch approval pass over it, and a ready one must be approved before it is staged or published. In the clone, which holds one run, several items could not run at all. Design and open steps: `docs/dev/bundled-dispatch.md`.

- Space marks tasks in the TUI, and a checkbox does in the web; `d` then dispatches every marked task, one bundle per project. Marks clear on a dispatch, on Escape and on another view, and a mark on a row a refresh removed is dropped, so nothing unseen is dispatched.

- **`pma dispatch <project> --prompt <text>`** runs an agent over the project with a prompt of your own, for work that is no `TODO.md` item; `P` in a project view does the same. The run is named by the prompt's first line, is reviewed and published as any run, and ticks nothing. `pma review <id> --retry` dispatches the same prompt again. The TUI and web prompt is one line.

- Approving a bundle in the TUI or the web lists its items, checked; the ones unchecked are left open, as `--drop` leaves them. A run that is not a bundle is approved without a question, as before.

- `pma task <project:line>` prints a task in full, as Enter on it does in the TUI and the web; `<project:ci>` prints the signal.

- **`pma lint --fix`** lists the mechanical errors a rewrite settles, and `--apply` writes them: a byte-order mark, a missing or wrong `# TODO` title, a section heading not written `## Name`, a plain bullet where an item belongs, and a checkbox not written `- [ ]` or `- [x]`. Dates, issue numbers, duplicates and fences stay for a person. The parser stays strict: one broken file hid every item it held, and 46 of one project's errors were the same bullet. In the TUI and the web, `f` on a project now offers prune or fix.

- Task rows in the inbox, the matrix and a project's view carry the issue a task tracks, as `#N`, linked to its page in the web when the scan recorded the repository. A run's detail shows it too, and `pma review <id>` prints the number.

- The web page keeps its view in the address: `?tab=project&project=alpha&open=run:52`. A reload returns to it, and Back and Forward move between views. The query string rather than the fragment, which carries the launch token. No client router: the page has no build step.

- `m` asks for the commit's subject, starting from the task text, in the TUI and the web; `pma commit <id> --subject=<text>` sets it from the CLI. The trailers stay.

- A pull request's body names the run, then the agent's summary, the verify result and the trailers. It was the commit message less its subject, so empty for most runs.

- `pma review <id> --reject` and both front ends say what a rejection did: `Run #52 rejected; reverted 2 files in ~/projects/alpha.`

- `pma review <id> --retry`, and `R` on a failed run in both front ends, rejects the run and dispatches its task again with the agent and model its last attempt ran, its used attempts reset. The task is found before the rejection, so one no longer in `TODO.md` leaves the run as it was.

- The activity tab lists finished runs (rejected, pushed, merged, closed, applied), newest first, up to 100. `b` there cycles a filter by state; the web also has a button per state.

- A run that is staged or committed is a notice in both front ends, naming the next step: `#4 alpha: staged in its clone, commit next`. `pma stage` and `pma commit` end their line with the command that comes next.

- **`instance_budget`** caps what one workflow instance may spend over every pass, in dollars; `off` by default. An unattended pass approves no plan that would take the instance's reported spend, plus the plan's worst case, past it, and prints why; the plan waits for approval by its id, which the cap does not hold. It admits plans rather than capping spend, since no built-in worker stops a run exactly at `agent_budget`. Before it, an unattended pass was bounded only by its worst case, $63 for `find-issues` over three projects. `docs/dev/scheduling-work.md`.

```sh
pma config instance_budget 10
pma workflow run find-issues --tag nightly --unattended
```

- `/` in the TUI's run list filters it by name; the web's list has a filter box, and the arrows move among what it keeps. The web's names and descriptions line up in columns.

### Changed

- A run whose agent was denied an action is not clean, and `pma review` lists what was denied. Auto mode skips a blocked action and carries on, so such a run could otherwise pass `b` and `--push` with work undone. Reads the result's `permission_denials` and classifier tool results, since no run has yet shown the classifier filling that list.

- **`claude` runs with `--permission-mode auto`** instead of `acceptEdits`. Headless runs have no one to ask, so any command outside the verify allowlist was denied: 10 denials across 5 recorded runs, 2 of them the changelog draft pma's own prompt asks for. Auto over a wider allowlist because the denied commands were compound. Schema 44 rewrites that flag in every row running `claude`, and leaves another mode alone. A model without auto mode starts in Manual, where `-p` denies what needs approval.

- `u` on a ready bundle shows the checklist `a` shows, then pushes. It used to refuse the bundle, so a push took `a`, then `u`.

- `pma dispatch` with several items of one project starts one bundle, not one run per item; `--separate` restores one run each.

- A command already queued or running from the same directory is refused as a job: `job 12 is queued already: pma verify alpha`. A second click on verify queued a second verify. It applies to `--detach` too.

- The web toolbar holds a view's own acts and the selected row's primary and danger acts, and the act that moves the row on from its state: commit for a staged run, push for a committed one. A row's other acts are under More; the TUI footer lists the next act first. Scan, sync and add note, which apply on every view, moved to the sidebar. At 1100 px the inbox toolbar went from 9 buttons over two lines to 6 on one, before a row's acts.

- A run offers only the acts its state takes, in both front ends: a pushed or rejected run no longer offers approve, reject or push, which were refused only once tried.

- One click on a web row with a detail (a run, a job, a scan, a route, a campaign, a task in its project) opens it. Other rows are selected, as before. It took a double-click.

- The TUI footer queues transitions: three runs reaching review within a second name the first with "2 more", and each Enter opens the next. Before, only the last was named.

### Fixed

- **Rejecting a clone run could remove later work.** The revert was built from the working tree at the moment of rejecting, not from what the agent changed, so once the clone had moved on it reversed every later commit and all uncommitted work, and reported success. Each attempt now records its change, and a reject reverses only that. A clone past the run's base refuses; `pma close <id>`, or `X`, ends the run, leaves the files alone and stays out of the accepted share, over a flag on `--reject`: rejecting means taking the change out. A run from before this has no record and can only be closed that way.

- An agent could still reach herdr, and through it type into panes that hold credentials: with `HERDR_SOCKET_PATH` removed, herdr falls back to its default socket. The variable now names a path that does not exist, which outranks the default.

- `pma prune --apply` exits 1 when it skips a file for lint errors. It printed a message and exited 0, so a script read the file as pruned. The dry run still exits 0, because it changes nothing.

- **`pma project export` refuses a CSV row that would not read back.** A comma in a tag split it into two tags, and a project named `#x` read back as a comment and was lost. A refused row points to `.json`. Refusal over quoting keeps rows hand-editable, as the format already did for a comma in a name.

- **One failed dependency tool leaves the outdated count unmeasured.** The count summed the tools that succeeded, so a failed tool beside a clean one stored 0 outdated. Any failure now stores no count, because a partial count understates the total. The other tools' packages stay in the detail.

- **A code fence indented 1-3 spaces hides its content from the `TODO.md` parser.** Indented lines never reached the fence check, so such a fence opened nothing, and an indented closer left a fence open. The parser and item insertion now follow CommonMark's 0-3 spaces. A backtick line with a backtick in its info string no longer opens a fence, so inline code in a description stays inline.

- **`pma review 3 4 --approve --minutes 5` records the minutes.** The batch path approved the runs without reading `--minutes`, so the time was lost with no message. Each run named gets the full N minutes, matching the per-run total `--minutes` adds to. Splitting N across the list would need sub-minute shares and a new action. A refused list records nothing, so rerunning it does not count the time twice.

- `make test` passes when run from a pma job. Each `pma` the CLI tests started inherited `PMA_JOB`, so it printed `pma job approve <id>` where the tests expect `--approve <plan>`.

- `pma prune` closes the gap a removed item leaves. Items separated by blank lines kept both neighbours of each removal, so a run of removed items left a run of blank lines. Blank lines no removal touched stay as written.

- **A commit pushed by hand no longer holds its clone.** A `committed` run stayed so after `git push`, because `pma push` refuses a `HEAD` that has moved past it, so every later clone dispatch was refused. `pma review` and a clone dispatch now move it to `pushed` once `origin/<default branch>` holds the commit. Matching the commit's ancestry over a new column settles runs already stuck; a rebased or squashed commit is not recognised.

- The clone-busy refusal names the step that ends the hold, by the run's state. It said "approve or reject" to a run already committed.

- `pma commit` and `pma push` refresh the project's tasks, so a ticked item leaves the task lists without a `pma scan`. The refresh does not ask GitHub, and keeps the last CI result. `pma pr` and `pma merge` are unchanged: the tick is on the branch until the clone pulls it.

- **A project whose name contains a dot can take `projects.<name>.verify` and `projects.<name>.sandbox`.** The key was refused as unknown, so a project such as `socket.io` could not set either. The field is the last component of the key, so the name is unambiguous.

- **Terminal control sockets are removed from an agent's environment**: herdr's `HERDR_*`, `TMUX`, `TMUX_PANE`, `STY`, `ZELLIJ`, `ZELLIJ_SESSION_NAME`, `KITTY_LISTEN_ON`, `WEZTERM_UNIX_SOCKET` and `NVIM`. An agent inherited them and could type a command such as `git push` into another pane holding the user's credentials. Under herdr, `HERDR_ENV` also enabled its agent skill and state hooks for headless runs. A socket at a default path, such as tmux's or herdr's, stays reachable by a process that looks for it. Analysis: `docs/dev/herdr-integration.md`.

- The web page opened blank, with its footer on "Connecting" and no projects in the sidebar. `app.js` still named the tabs `next` and `status` from before the tabs were merged; the first draw threw before the event stream was opened, so no live update arrived either.

- Enter in a web prompt cancelled it. The dialog's first button, Cancel, was the form's default button.

## [0.9.0]

Upgrading from 0.8:

- The database moves from schema 40 to 42 on first open. 0.8 refuses it afterwards; copy `~/.config/pma/projects.db` first if you may go back.

- The TUI and the web have eight tabs on `1` to `8`, not ten on `0` to `9`: inbox, projects, activity, matrix, library, report, notes, settings. `0` and `9` do nothing.

- Runs edit your clone by default. `pma config workspace worktree` restores the previous behaviour for every run, `--worktree` for one.

- Verify runs unconfined unless `pma config projects.<name>.sandbox on` says otherwise.

### Added

**Runs that edit the clone.** `workspace`, `clone` by default, puts the agent in the user's clone, from its working tree as it is. The change waits there for review: `git diff` shows it, `pma review` approves it, `pma push` commits and pushes it, and a rejection reverts it from the working tree. Verify runs in the clone, so the project's build is reused rather than rebuilt per run. One clone run per project at a time; a second is refused until the first is decided. `--worktree` on `pma dispatch`, `pma run` and `pma workflow run` gives one run the worktree of before. Chosen over keeping the worktree as the default because reviewing in place, with the project's own tools and caches, was what the worktree got in the way of. `docs/dev/clone-runs.md`.

```sh
pma run fix-ci spore          # edits ~/projects/spore; `pma review 1` shows the diff
pma review 1 --approve && pma push 1
pma review 1 --reject         # reverts it instead
pma run fix-ci spore --push   # approve and push when the gates are clean
```

**`--push`** on `pma dispatch` and `pma run`: approve each run whose recorded gates are clean and push it; the rest wait in `pma review`.

**Front ends notice changes.** Both read the store's data version once a second and reload within a second of any other process committing; the web shows a toast when a run reaches review, fails or is pushed, and when a job ends or waits for approval. Before, a view refreshed on a 30 s timer when idle, not at all behind an open detail, and said nothing about what changed.

**`run` from the TUI and the web.** `z` on a project, or on a row naming one, lists the `pma run` names with what each does; `p` and `w` (checkboxes in the web) add `--push` and `--worktree`. The job it queues shows in the project's view as it runs.

**Seven tabs, and one place per thing.** Inbox: what waits on you (runs in review, plans awaiting approval), what is under way (running agents with what their log last showed, queued jobs), and the tasks, marked for an agent or for you. Projects: the status view. Activity: every job, running agent and workflow instance. Library: workflow revisions and campaigns. Matrix, report and notes as before. A project's view lists its runs and jobs in one Activity pane, under way first. Chosen over keeping ten tabs because a run in review showed on three of them, a running agent on three, and one CI fix took six views to follow from `z` to pushed. `docs/dev/frontends.md`.

**Configuring from the TUI and the web.** A settings tab lists every `pma config` key, the roots, the agents, the presets and the route revisions, and edits them in place: `e` edits, `x` resets or removes, `a` makes a default or activates a route, `r` adds a root, `p` a preset, `f` proposes a route from a file. On a project, `e` sets its tier, a tag, its verify command or its sandbox from one typed line, `f` prunes its finished items, and a TODO.md pane shows what lint and prune would say. `r` on a project lists a plain, dependency or offline rescan and `pma deps refresh`. On the library, `a` adds a campaign and `x` removes one. Enter opens a scan's changes, a campaign's members and a route's policy. Chosen over leaving configuring to `settings.toml` because the stated goal is a front end that needs no terminal; `project export`, `import`, `forget` and `clone` stay CLI-only as file and bulk operations. `docs/dev/frontends.md`.

```
8          settings: keys, roots, agents, presets, routes
e          on a project: tier 2 | tag +oss | verify make test | sandbox on
```

**One confirm from review to the branch.** `u` on a ready run approves it and pushes; `m` and `p` likewise commit or open a pull request. `a` still approves alone. In the TUI the acts apply inside a run's detail, after reading the diff. A run that reaches review, fails or is pushed, and a job that ends or waits for approval, is named in the TUI's footer with Enter to open it, and in a web toast with an Open button.

### Changed

- The verify sandbox is off unless `projects.<name>.sandbox` is `on`. A test suite that names `/tmp` itself, which the policy did not grant, failed every verify of a project under it; `/tmp` is granted now too.

- A discarded or rejected run keeps its patch in `runs/<id>/change.patch`, as the `--local` help said and the code did not; `pma review <id>` shows a run's diff after its worktree is gone, from the tree it recorded.

- The project view's `for` column says `named` for a task an agent takes only when dispatched by name, such as a signal of an untiered project, rather than `you`.

- The web page has no drawer. The selected row's actions join the toolbar, and a run or a job opens in place of the tables, full width. The drawer repeated the row's cells beside the row and squeezed the diff into an overlay.

- A key means one thing for a row rather than one thing per tab: on the inbox, `a` approves a run or a plan and `s` stages a run or stops a job, by the row it is pressed on. The footer, the toolbar and `?` show only the acts that fit the row.

- Enter on a task opens its project, as `o` does; in the project's own view it opens the task in full: its line, group, tags, due date, issue, placement, who may take it, the runs holding it, the item's indented description, and for a signal the failing workflows or the outdated packages. `d` dispatches from there. On a run or a job Enter opens the diff or the log, as before.

- A job queued with `--detach` from the CLI records the project its one target names, so it shows in that project's view as jobs queued from the TUI and the web do.

- `pma run --review` is documented as what it is since runs edit the clone: it applies to a worktree run, which otherwise writes a verified edit into the clone unstaged. A clone run waits in `pma review` with or without it.

- The TUI and the web say `job 7 queued: pma run fix-ci spore` when they queue a job, rather than the CLI's hint to run `pma job`.

## [0.8.0]

Upgrading from 0.7:

- The database moves from schema 35 to 40 on first open. 0.7 refuses it afterwards. Schema 40 drops the `scan_error` columns, so the move cannot be undone; copy `~/.config/pma/projects.db` first if you may go back.

- `broken` is gone from `pma status`, the history and the views. Seven faults replace it; see Changed.

- The first `pma run` of a workflow puts the bundled library in effect, unless you activated a revision of your own. Its worst case is $63 per project, over the default `workflow_budget` of $25, so it asks for `pma config workflow_budget 63` once.

### Added

**Five more library workflows.** `security-review` and `test-gaps` are `find-issues` aimed at one class of fault, each with a confirm that must find the path or the missing test. `project-brief` writes one note per project: what it does, for whom, in what. `fix-findings` is `apply-fixes` without the lap and retry the runtime does not take, so it runs; `review-and-fix` composes it with `find-issues`, fixing the critical and high findings and printing the rest. All five are in `pma run`. A `brief` type joins the document's types. `docs/dev/workflows.md` 15.16 to 15.20.

**`pma run`: a review, a CI fix or an upgrade, by name.** One command over `pma workflow run` and `pma dispatch`. A target is a project's name or its path. Reviews run to the end and print what they found and where the prose is. `fix-ci` and `deps-upgrade` write each edit that passes verify into the clone, unstaged; `--review` leaves it in `pma review`. With no revision of your own in effect, the bundled library is put in effect on first use, within `workflow_budget`. `docs/dev/run.md`.

```sh
pma run                                   # the names
pma run review ~/projects/argdec          # claude, opus unless you configured otherwise
pma run review -w claude:haiku a b        # another worker and model
pma run fix-ci --all                      # the plan; `--yes` runs it
```

`pma dispatch --local` does the same for any target: it starts from the clone's working tree and lands each verified edit there unstaged, as `pma workflow run --local` does. A local `ci` pick reads its logs from the default branch rather than the branch checked out.

**`pma stage` and `pma commit`.** Each runs the steps before it that the run has not done, in your clone:

```sh
pma stage 4     # the change, item ticked, into your clone's index; no approval needed
pma commit 4    # then a commit there, with pma's message
pma push 4      # then that commit, and nothing else, to origin's default branch
```

Runs gain the states `staged` and `committed`, which hold their task. The TUI and web bind `s` and `m` on runs. `docs/dev/stage-commit-push.md`.

**CI and release workflows.** `ci.yml` runs `make check` on Linux and macOS on every push. `release.yml` builds pma for x86_64 and aarch64 Linux and macOS when a version tag such as `0.8.0` is pushed. It publishes the archives and `SHA256SUMS` to a GitHub release, with that version's CHANGELOG section as its notes. Run by hand without a tag, it keeps the archives as run artifacts and publishes nothing. Windows is not built: pma-core uses Unix-only APIs, and a Windows build now stops with "pma supports Linux and macOS only".

**Scan history: what changed, `pma history`, and `pma status --html`.** A scan reported only counts, such as "CI failing in 6", without naming the projects, and kept no record of earlier scans. It now prints what changed since the scan before and lists the projects with failing CI or broken. Each scan records every project's state and its changes: items opened and closed, CI failing or passing again, a project broken, mended, new, returned or absent. The last 100 scans are kept. The status view in the TUI, the web and the HTML page gains three panes: failing CI, one row per failing workflow with its latest failed run; the last scan's changes; and the recent scans. In the web and the HTML page, a project links to its GitHub repository, its CI to its Actions page, and a failing workflow to its run. The scan records each failing run's URL from `gh run list`. Schemas 36 and 37.

```sh
pma history                          # the recorded scans, newest first
pma history last                     # what the latest one changed; or a scan's number
pma status --all --html status.html  # one self-contained page, no server
```

A CI change is judged against the last state a scan knew, so an offline scan's `unknown` reports neither a failure nor a fix. Failing CI shows when it began; with no earlier passing record, that time is a lower bound. An item is matched across scans as the store already matches it: by key, then by text. Rewording an item without an id still reads as one item closed and one opened. Rendering HTML from `pma_core::screen` keeps the page's tones and columns in step with the web view; an export of the web app's JavaScript was the alternative, and needs a server to load its data.

**`docs/dev/pma-minos-integration.md`, the state of running a worker under minos's `minosb`.** No code depends on minos yet. Measured on Claude Code 2.1.299 with several turns in one session: `total_cost_usd` is cumulative for the session, so `parse_claude`'s last `result` record gives the right cost. The same choice makes the summary the last turn's reply, not the task's outcome, and makes a run whose last turn was interrupted read as failed (`error_during_execution`). Both matter only for a worker that takes corrections mid-run; see issues 2-4 there. `using-containers.md` and `implementation-plan.md` 2b.6-2b.8 now follow minos's broker design.

**A workflow library, two new workflows, and the `doc` sink.** `pma workflow library` prints the nine workflows `propose` takes, with their types, as one document. It comes from `docs/dev/workflows.md` section 15, and a test holds the two to each other. A revision is a whole document, so the library is printed for you to edit or merge, then proposed, rather than installed beside your own.

```sh
pma workflow library | pma workflow propose -
pma workflow run todo-audit myproject
```

- `todo-audit`: a reviewer claims which open `TODO.md` items the code already does, citing `file:line`; a confirm checks each claim; each confirmed one becomes a note. It ticks nothing: a wrong "done" would close open work.

- `doc-drift`: find-issues aimed at the README and docs, for statements the code contradicts.

- The `doc` sink appends one block per unit to a file in the project's clone, uncommitted: `{"sink": "doc", "map": {"path": "REVIEW.md", "text": "..."}}`, or `.emit_doc` in a script. Section 9 put it under the instance's artifacts; a report is read and committed with the code, and artifacts are never collected. A path that leaves the repository, `.git`, `TODO.md` or a symlink is refused.

The library's worst case is $63 per unit of input at the default `agent_budget` (from `ensemble-review`), over the default `workflow_budget` of $25, so activating it needs a higher budget.

**`pma deps refresh` and the `refresh` worker.** `pma deps refresh <dir>` moves each lock within its constraints: `cargo update`, `uv lock --upgrade`, or `go get -u ./...` with `go mod tidy`. The seeded `refresh` worker runs it in a dispatch's worktree, so the project's check decides what is kept, with no agent and no model. When the check refuses a refresh, the deps prompt carries its output, and the agent is asked to find the update that broke it. A refresh counts attempts under its own key; counted against the task, one refused refresh and its rejection left the agent none. It is not charged against `batch_budget`: a worker that reports no cost is charged its whole `agent_budget`, which held a default batch to 5 refreshes that cost nothing. No route is seeded, since a policy without a catch-all refuses every other dispatch. Schema 39 seeds the worker through `settings.toml`.

```sh
pma dispatch a:deps b:deps -a refresh
pma dispatch a:deps -a claude    # after a refused refresh: carries its check output
```

### Changed

**`broken` is split into seven faults.** One state covered a git failure, an unreadable `TODO.md`, a timed-out git call, lint errors and a git hold. Each now has its own state: `git settings changed`, `git unreadable`, `CI unknown`, `TODO unreadable`, `TODO lint`, `unpublished unknown`, `scan partial`. Each fault ranks just below the state it could hide, so a known state outranks a possible one. A git call that fails or times out names the fact it lost; before, a failure read as an empty fact. A gone upstream or unlisted `pma/` branches is `unpublished unknown`. A `gh` failure or an unknown default branch is `CI unknown`; offline scans and origins off GitHub are not faults. The unpushed count now comes from `git status --porcelain=v2 --branch`, whose headers tell no commit, no upstream and a gone upstream apart, in place of a `rev-list` whose failure could mean any of them. Recent scans and `pma history` show a column only for a fault some listed scan found. `TODO lint` names the first error and counts the rest. The scan records each fault separately: the old `scan_error` text was free-form and could not be sorted reliably. Recent scans count projects per fault. Schema 40 classifies stored errors by the prefix the scan wrote. It drops a `mended` change when no earlier change names the fault it mends.

**The web Health page shows one table at a time.** Four tables side by side each scrolled both ways. Tabs above the table switch between them, as `Tab` does.

**Untiered projects are ranked at tier 5.** `default_tier` now defaults to 5, so a new install's `pma next`, `matrix` and `status` cover every project, below the tiered ones. An untiered project's CI and deps signals are shown but not taken by `dispatch --auto`; name them, as `pma dispatch <project>:ci`, to dispatch one. Its `#agent` items stay eligible. Headers count untiered projects whatever the default, and `status` still shows their tier as `-`. `pma config default_tier never` restores the old behaviour.

**`pma push` works in your clone, and publishing runs no check.** `push` committed, rebased, re-ran the check and pushed from the run's worktree, so the clone never showed the commit before it was public, and the name covered only the last step. It now commits in the clone if `pma commit` has not, and pushes that commit. The clone must be at the run's base, and `stage` refuses a non-empty index. The publish check is gone from `push` and `pr`: with no rebase, it re-tested the tree the run's check passed plus the tick, and git refuses a push onto a moved origin. A second run on the same base is refused once the first lands; publish it with `pma pr`.

**`pma status` fits 80 columns.** The `local` column is now `dirty`, holding the count of changed files or `-`, and the todo column reads `N lint` and `error`.

**Stale deps means a lock that can move and has not changed for 30 days.** Any count above zero placed a project in the state, so 60 of 60 measured projects were in it on 2026-10-21, and a measurement 36 days old was shown as current. A project is now in it when a measurement within `deps.days` (default 30) finds the lock can move, and the lock has not changed for `deps.days`; 25 of the 60 were. uv is measured with `uv lock --upgrade --dry-run` instead of `uv tree --outdated`: the old count included releases the constraints exclude, which no lock refresh can reach. The count now includes transitive packages, as cargo's does, and cargo's additions and removals count too. Every scan reads lock age from `git log`, with no network. Schema 38. The decision record is `docs/dev/deps-refresh.md`.

**Docs.** The README's route example states its `approval`, without which the parser refuses it. `docs/dev/design.md` names the agents that are seeded and the crates in use (`toml_edit`, `serde_json`).

### Fixed

**A guard that names a parameter now reads the argument.** `{"severity": ["{$severity}"]}` compared the field with the literal text `{$severity}`, so the edge was never taken and a default edge, or settling, took the unit instead; a comparison with a parameter failed outright. Section 6.4 states both forms. A pass now writes the instance's arguments into its guards before any unit enters the graph.

**Front-end fixes from an accessibility pass.** The TUI restores the terminal on a panic; `ratatui::try_init` installs no hook, so a panic left it raw on the alternate screen. `Home` and `End` move to the first and last row in the TUI and the web; only `g` and `G` were bound, and the TUI sent `End` as `G` but `Home` as itself. In the web, `Tab` switches lists only while nothing has focus, so a keyboard user can tab from a button onward; and the light theme's `good` and `warn` pills were 4.49:1 against their backgrounds, under the 4.5:1 that 12px text needs. The Health tabs name their panel, not its heading.

**`pma report` counts local edits left unstaged.** A `--local` workflow edit applied to its clone was counted nowhere: not accepted, not landed, not open. It now has an `applied` column, and the headline names the count. It stays out of the accepted share: no one has judged it, and it has not reached the default branch. A local edit discarded because another edit's landing left it unable to apply now counts as an infrastructure rejection, in no share; it was counted against the agent.

**A repository with no workflows reads as CI `none`, not unknown.** `gh` prints nothing for it, which was parsed as JSON and failed with "EOF while parsing".

**A map node is held to the width the instance was given, not the parameter's maximum.** `review-fix` declares `breadth` with a maximum of 15. `--set breadth=2` was recorded, but the review was told "at most 15" and accepted 4 findings, so the fix plan was twice the size asked for. The prompt and the check now read the given value, capped at the maximum; pricing still uses the maximum, which bounds any value given.

**A local edit that another edit overtook is done again, not lost.** With `max_parallel` above 1, `--local` edits in one group start from the same clone. When an earlier one changed the lines a later one also changed, the later edit passed verify but no longer applied, and its finding was marked done. Its unit now goes back in the queue, and the next plan does it again from the clone as the edits before it left it; the discarded run still uses no attempt, and `max_edits` bounds the retries. Running local edits one at a time was the alternative; it gives up the parallelism for every edit to save the few that collide.

**A task's attempts are charged when its work is judged, and a retry resets them only once its agent starts.** A pull request closed without merging used no attempt, so a task whose pull requests kept being closed was dispatched without limit; closing now uses one, as `--reject` does. `dispatch --retry` cleared the count while choosing tasks, so a run then refused at `batch_budget` still left its task with none used.

**An agent log with invalid UTF-8 keeps its result.** One stray byte made the whole log read as empty, so the run failed and recorded no cost; bad bytes are now replaced. `parse_claude` also took the last JSON line before checking its type, so a later non-result line hid the result.

**A finished item with an open checkbox under it is neither pruned nor removed.** `pma prune` and a workflow's `todo` sink `remove` deleted the item with its description, open sub-items included; the prune dry run listed only the parent. `prune` now keeps the item and names the open line. `remove` fails the step with the same reason.

**`TODO.md` writes cannot lose or misdirect the file.**

- The tick made when publishing wrote the file in place, so a crash could leave it truncated. It now uses the same atomic save as every other writer.

- All writers shared one temporary file, so two at once could fail, or leave a mix of both texts. Each now has its own, created new, so a file or symlink planted at the name is never written through.

- Publishing refuses a `TODO.md` that the agent replaced with a symlink. The tick followed the link and wrote outside the worktree.

**`pma project forget` removes all of the project's settings.** It kept `verify` and `sandbox`, and settings are matched by project name. A later, unrelated repository with the same name ran the old `verify` command, and with `sandbox = false` its agent ran without a sandbox no one chose for it. The dry run lists the settings it will remove.

**A stalled `git` or `gh` call outside a scan is killed.** Dispatch, publish and `pma sync` had no bound, so a hung fetch, push or API call held the session lock until someone killed it, and every later unattended run waited behind it. `git` calls stop after 10 minutes and `gh` calls after 2, with the process group killed as a scan already does. The scan's own 60-second bound was the alternative; it is too short for a push or a large checkout. `pma project clone` stays unbounded, since a clone's length depends on the repository and someone is watching it.

**`pma config <retired key>` says what replaced the key**, as setting one already did, instead of "unknown setting".

**Malformed input is read, or refused with its cause.**

- `pma project import` reads a file that starts with a byte-order mark. It read the CSV header as a project.

- A `TODO.md` that starts with one is an error that names the mark, not "must start with `# TODO`".

- An empty `PMA_HOME` counts as unset. A relative `PMA_HOME` or `PMA_STATE` is refused, since the database and session lock followed the working directory. A relative `XDG_STATE_HOME` is ignored, as the XDG specification says.

- `timeout` is at most 240 minutes. A larger value could overflow when converted to seconds; the cap also refuses a value meant as seconds.

- `gh:N` above `i64::MAX` is a lint error. The store wrapped it negative.

## [0.7.0]

Upgrading from 0.6:

- The database moves from schema 31 to 35 on first open. 0.6 refuses it afterwards.

- The first command writes `~/.config/pma/settings.toml` from the database. From then on the file is the configuration; edit it, or use `pma config` as before.

- The `claude` worker switches to `--output-format stream-json --verbose`, unless its record was edited.

- `pma tui` changed keys: `0`-`9` switch tabs, `m` no longer opens the matrix (`2` does), and `?` lists the rest.

- From a clone, the binary is in `crates/pma`: `make install` as before, or `cargo install --path crates/pma`.

### Added

**A view of each project.** `pma project show <name>`, the TUI's `o` on any row that names a project, and the web's PROJECTS list in the sidebar each show one project: its tier, states with their reasons, CI, dependencies, local changes, verify command and tags, then its open tasks and every run it has had, newest first. Tasks dispatch and runs are approved or rejected from there. The view says when a scan last read the project, in yellow once that is over a day old, since a stale "failing CI" reads like a current one; `r` rescans that project alone. The status tab now lists every project, untiered ones too, since it is where a project is opened from. Each front end offers only the actions that fit the selected row, from `pma_core::screen::fits`.

**`pma web`, and a TUI with every view and action.** `pma tui` now has ten tabs (next, matrix, runs, jobs, agents, workflows, campaigns, report, notes, status) and acts on the selected row: dispatch, approve, reject, rework, publish, approve or stop a job, resume or start a workflow, notes, scan. It draws tables with column headers, colours states and priorities by meaning from the terminal's palette (`NO_COLOR` turns it off), previews the selected row, and colours a run's diff. The web is designed for a browser rather than after the terminal: a sidebar, tables with status pills, a drawer with the selected row's actions, toasts, and light and dark themes. `pma web` serves the same in a browser: it listens on 127.0.0.1 only, behind a token made for each launch, and refuses another `Host`, a POST from another origin or not in JSON, and data requests without the session cookie. Both read one key table (`pma_core::keys`) and one model of rows and actions (`pma_core::screen`), so neither can show or do what the other cannot; tests hold them to it. Long actions run as jobs. A build without the `web` feature has no HTTP server. Design: `docs/dev/frontends.md`.

```sh
pma tui
pma web            # opens the page; --no-open prints the address
pma web --app      # in a Chrome or Chromium window with no browser controls
pma web --no-open --port 9000   # then, elsewhere: ssh -L 9000:127.0.0.1:9000 host
```

**A shared API for front ends, and a cargo workspace.** `pma_core::api` defines what a front end may read (`Query`, returning a `View` of plain data) and do (`Action`, returning an `Outcome`). A long action, such as a dispatch or an approved plan, is queued as a job running the same CLI command. The CLI now renders each `View`; its output is unchanged. The code moves into `crates/pma-core` (no printing, no UI dependency), `crates/pma-tui` and `crates/pma`, so the TUI and the web view each depend on the core alone. Design: `docs/dev/frontends.md`.

**Configuration moves to `~/.config/pma/settings.toml`.** It holds the `config` keys, roots, workers, presets, and each project's tier and tags. Edit it by hand, or with `pma config`, `pma root`, `pma project`, `pma agent` and `pma preset`, which keep its comments. The first command writes it from the database; schema 35. An invalid file stops every command, naming the key. The tables of the same names stay as a mirror the file refills when its text changes, so queries that join a project's tier keep working; the file wins. Workflow and route revisions stay in the database: activating one checks its cost and records who did, which a hand edit would skip. Design: `docs/dev/frontends.md`.

```toml
roots = ["/home/me/projects"]

[config]
agent_budget = 3
tiers.2 = 0.7

[projects.argdec]
tier = 2
tags = ["py"]
verify = "make test"
```

**`pma workflow run --unattended` and `--local`.** `--unattended` approves each plan as it is reached and runs until the instance finishes or waits. Each plan is still sized by `batch_budget`, and the caps and the activated worst case still bound the instance. `--local` starts each edit from a commit of the clone's working tree, built in a temporary index, with no fetch and no remote. An edit that passes verify is written into the clone as unstaged changes, and one that fails is discarded; the run becomes `applied` or `rejected`, and its patch stays in `runs/<id>/change.patch`. Nothing is staged or committed. Edits run in groups of `max_parallel`, so at 1 each sees the ones applied before it; edits in one group share a base, and one that no longer applies is discarded with that reason. A discard consumes no attempt beyond what verify consumed.

```sh
pma workflow run review-fix myproject --local --unattended
```

**`--detach` on `pma dispatch` and `pma workflow run`, and `pma job`.** `--detach` queues the command and returns at once. A runner process, started on demand and holding `runner.lock`, runs queued jobs one at a time and exits when none is left; it outlives the terminal. `pma job` lists jobs, `pma job log <id>` prints one's output, and `pma job stop <id>` cancels a queued job or kills a running one. Each job runs as a child `pma` under the agent watchdog, so a killed job kills its agents and a dead runner kills its job. Opt-in rather than the default, so approval mode and scripts that read a command's output keep working. Schema 32. Design: `docs/dev/async_dispatch.md`.

```sh
pma workflow run review-fix myproject --local --unattended --detach
pma job log 1
```

**`pma watch`, `pma job approve` and `notify`.** `pma watch` shows each running agent's tool calls, last tool and last words, read from its log, and redraws until nothing runs. The `claude` worker now writes `stream-json` so its log can be read mid-run; schema 33 updates a row still on the shipped template and leaves an edited one alone. A detached workflow without `--unattended` stops at its plan as `awaiting-approval`; `pma job approve <id>` queues the approval with the flags the plan was priced at. `pma config notify '<command>'` runs once per finished job with `PMA_JOB_ID`, `PMA_JOB_STATE` and `PMA_JOB_SUMMARY` set. Earlier binaries refuse a schema 33 database. Known gaps are listed under Remaining work in `docs/dev/async_dispatch.md`.

```sh
pma config notify 'notify-send pma "$PMA_JOB_SUMMARY"'
pma watch
```

**Agents draft changelog entries; pma keeps them out of the change.** In a project with a `CHANGELOG.md`, the agent is told not to edit it and to write one entry per separable change to `.pma/changelog.md`. pma moves the file to `runs/<id>/changelog.md` before the diff, and `pma review <id>` and a local apply show the entries or say none was drafted. A run without entries is flagged, not discarded. Drafting rather than editing `CHANGELOG.md` avoids conflicts between edits that run together.

### Fixed

**Rejecting a run no longer overwrites an approval made meanwhile.** Reject read the run, removed its worktree, then wrote `rejected` whatever the row held by then, so a run approved from elsewhere in between lost its tree and its approval. It now writes `rejected` only over the state it read, before it removes anything. A front end's approve or reject also carries the state its row showed, and is refused once the run has left it.

**A workflow review run is `done`, not left `ready`.** A run of a node that is not an `edit` changes no files and has its tree removed once its output is read, yet it stayed `ready` in `pma review`. Showing it then reported the removed tree as "tampered with" and advised rejecting the run. Such a run now ends `done`, which is final, and `pma review <id>` shows no diff for it. Schema 34 moves existing ones.

**A plan approved in the foreground settles the job waiting on it.** A detached job that stopped at a plan stayed `awaiting-approval` if the plan was approved with `pma workflow run --approve`. A later `pma job approve` then queued a spent plan, which failed. A job's log now names `pma job approve <id>` rather than the foreground command, and either route marks the job `approved`.

**A workflow edit's attempts are counted per unit, not per root.** The counter was keyed on `workflow:<instance>:<root>`, and every finding a review yields shares the project unit as its root. Two failed or rejected fixes in one project then refused every other finding from it. The key is now the unit itself, or for a lap's unit, the unit the lap started from.

**A workflow plan id names its target and arguments.** It hashed each node's units by id, and root unit ids are `u1..uN` in every instance. So a plan printed for one project could be approved to run on another, under another instance, or with another `--set`. It now also hashes the arguments, each node's definition, each unit's data and resolved worker, the routing policy revision and `agent_budget`. A plan whose units resolve to different workers shows `several` instead of the first unit's.

**`make check` passes on macOS.** The clone test compared against the temp directory as given. `root add` canonicalizes it, and macOS's `/var` is a symlink to `/private/var`.

### Security

**`pma pr` and `pma push` publish one commit of the approved tree, and none of the agent's commits.** Publishing kept every commit the agent made since the base, and review shows only the net diff. A file committed and then deleted, such as a key read from `~/.ssh`, was pushed in history without anyone reading it. The commit is still made with `git commit` on the base, so the user's hooks run.

## [0.6.0]

Upgrading from 0.5:

- The database moves from schema 29 to 31 on first open. 0.5 refuses it afterwards. Cached base verify results are dropped and measured again.

- Verify runs confined. A check that writes outside the worktree, the temp directory and the toolchain caches now fails; `pma config projects.<name>.sandbox off` restores the old behaviour for that project. On Linux the sandbox needs kernel 6.2.

### Added

**`pma review <id> --reject --infra`: a rejection for a fault of the check or the machine, not the agent.** Every rejection counted as the agent's: `pma report` put the pilot's first run, rejected because its tests inherited the verify push block, in the decided runs, and the complexity estimate read it as a refused change in that project. Such a run is now in no share, `report` names it, and the attempts its failed checks used are given back. A free-text reason was the alternative; nothing in `pma` would read it.

**`pma project clone`: check out a project whose record exists but whose working tree does not.** It clones the GitHub `owner/name` the last scan recorded, with `gh repo clone` so a private repository authenticates as `gh` does. A dry run unless `--apply`; `--root` picks the root when several are registered, and a directory of the project's name is never cloned over. It is under `project` with `forget`, which it reverses in part.

### Changed

**An absent project is not ranked.** `status`, `matrix`, `next` and `tui` scored a project no scan can find, so its tasks competed for attention with nothing to dispatch against them. `pma project` still lists it.

**`pma review <id> --minutes` writes only the review time.** It wrote the whole run from a copy read without the session lock.

### Fixed

**A stale approval no longer overwrites a publish.** `review --approve` holds no session lock, so a `pma push` that ran between its read and its write was undone: the run went from `pushed` back to `approved`. The write now requires the state it read, and otherwise names the state the run is in. Taking the session lock was the alternative; it would refuse every approval while a dispatch runs.

**A workflow pass's transactions wait for another writer.** They read and then wrote inside a deferred transaction. SQLite fails such an upgrade at once, without waiting, when another connection holds the write lock, after the agent had run. They now begin immediate, so the busy timeout applies.

**The batch budget charges a run that reports no cost its whole `agent_budget`.** It counted as free, so `omp`, `opencode` and a run killed at its timeout never stopped a batch.

**Rejecting a run that never started uses no attempt.** A run refused at the batch budget had to be rejected to clear it, and two such rejections reached the attempt limit.

**An untiered project is routed at `default_tier`.** It was ranked there but routed with no tier, so a route with a tier condition never matched it, in a dispatch or a workflow node.

**`pma route replay` compares approval and scope.** It compared the route, agent and model, so a candidate that changed only how a run is approved, or the paths it may touch, replayed as "0 routed differently".

**An `#agent #manual` item is listed for you, not for agents.** `next` treated `#agent` alone as eligible, though dispatch refuses `#manual`. The label "not #agent" is now "not for agents".

**The complexity estimate no longer reads `and/or`, `e.g.` or `Node.js` as a path.** Each lowered the estimate by one, which can choose a weaker model.

**A workflow `todo` sink no longer writes under a heading inside a code fence.** The parser does not see an item there, so each resumed pass added another copy.

### Security

**Verify can write only to the worktree, the temp directory and the toolchain caches.** It ran the project's command unconfined on the host, so a `Makefile`, `build.rs` or `conftest.py` the agent edited could write anywhere the user can, including with the `sanduk` worker, whose agent is contained. [sanduk-sandbox](https://github.com/shakfu/sanduk-rs/tree/main/crates/sanduk-sandbox) applies Landlock on Linux and Seatbelt on macOS to the whole process group. A sandbox that cannot start fails the verify rather than running it unconfined. Reads and the network stay open, so this bounds what a bad build file can damage, not what it can read or send. Nothing under the repository's `.git` is writable: granting `objects/` would let a check delete history, and a verify that runs `git add` fails. Whether verify ran confined is part of the base cache's key, so a confined head is never compared with an unconfined base.

**`GH_ENTERPRISE_TOKEN`, `GITHUB_ENTERPRISE_TOKEN`, `GIT_ASKPASS` and `SSH_ASKPASS` are removed from an agent's environment**, as `GH_TOKEN`, `GITHUB_TOKEN` and `SSH_AUTH_SOCK` were. The README said agents run with no push credentials; it now says none are in the environment, and that credentials on disk stay readable.

## [0.5.0]

Upgrading from 0.4:

- The database moves from schema 28 to 29 on first open. 0.4 refuses it afterwards.

- A routing policy with an unknown key or an out-of-range condition is refused. An active revision like that fails at dispatch; propose a corrected one.

- `pma review <id> --approve` requires the tree last shown. After editing a run's worktree yourself, read it with `pma review <id>` first.

- A repository where a run found its hooks or git settings changed is held until they are restored, or until `pma review <id> --reject --keep-git-changes`.

### Security

**A hook or git setting an agent adds fails its run before `pma` runs git after it.** The snapshot taken at dispatch was compared only at publishing. An agent could write `filter.x.clean` to the clone's `.git/config` and a `.gitattributes` beside it, and the `git diff` that measures the run ran the filter under `pma`'s environment, `GH_TOKEN` and `SSH_AUTH_SOCK` included. Every git call in a run's worktree now compares the snapshot first, and a workflow read node, which took none, now takes one.

The setting stays in the clone after the run fails, so `dispatch`, `verify` and workflow nodes refuse that repository until it matches the run's snapshot again. `pma scan` leaves it unread, since `git status` runs an added `core.fsmonitor`; the project keeps its last scan and shows the reason as its scan error. `pma review <id> --reject` waits for the same, unless `--keep-git-changes` accepts the change as the user's; if any rejection lifted the hold, the next dispatch would take the setting as its baseline. Storing the values so `reject` could restore them was the alternative; the snapshot keeps hashes because a URL may carry a token.

**A retried publish publishes the approved tree and nothing written since.** The approved tree was compared only while `HEAD` was still the approved head. A publish that committed and then failed, on a rebase conflict, a verify failure or a timeout, moved `HEAD`, and the retry committed whatever the worktree held: an edit made after approval, or a file the publish-time verify wrote, such as `uv.lock`. Each attempt now starts from the approved head and tree. The commit is still made by `git commit`, so the user's hooks run; `git commit-tree` would have skipped them. A rebase conflict is no longer resolved in the worktree and published as found: rebase there, then approve again.

**An approval must match the tree the reviewer was last shown**: what the run left when it became ready, or what `pma review <id>` printed since. An agent that outlives its kill, by calling `setsid`, could write to the worktree between the diff being read and approved, and the approval took whatever was there. A tree that changed is refused with the command to read it again. A batch approval, which shows no diff, now takes only what the agent left. Binding to what `pma review` printed, rather than requiring the tree id on the command line, keeps `--approve` as it was.

### Changed

**The help for `lint`, `prune` and `scan` says only a repository's root `TODO.md` is read.** A nested `TODO.md` is not a task file, which the README already stated. Recursing was the alternative; it would read files no other command treats as tasks.

### Fixed

**A task that mentions `{model}`, `{dir}`, `{budget}` or `{timeout}` reaches the agent as written.** Placeholders were filled one after another over the whole argument, the prompt included. Text such as `{model}` was replaced, and with no model chosen the prompt argument and the flag before it were dropped. Placeholders are now filled in one pass, so an inserted value is not read again.

**A run with no changes is not published.** The `TODO.md` tick counted as a commit, so the task was marked done and published with no work in it.

**An applied route outranks the default preset**, as documented. The default preset was read before the route, so once one was set, as migration 24 does, a route's agent and model never applied. The default preset's model now applies only to its own worker.

**A policy in shadow refuses, scopes and approves nothing.** It refused a task no route matched, replaced the class scope with a route's `scope`, and stored the route's approval, so a `propose` route blocked `pma review --approve`. Workflow read nodes had the same fault. A shadow run records the revision and the route it would take, and `pma review` shows it `in shadow`.

**A policy with an unknown key or a mistyped value is refused.** A missing key read as no condition: `"clas"` in `match`, or `"match": "A"`, matched everything, `"model": 4` was dropped and `"attempts": 3` became 1. `complexity` and `tier` must lie in 1 to 5 and `lap` be at least 0. An active revision outside those bounds now fails at dispatch; propose a corrected one.

## [0.4.0]

Upgrading from 0.3:

- The database moves from schema 24 to 28 on first open. 0.3 refuses it afterwards.

- `pma ship` is now `pma pr` or `pma push`; `publish` settings are retired. See Changed.

- `weights.*` settings are retired, with the health score.

- `pma workflow run --yes` is now `--approve <plan>`.

- New worktrees, logs and artifacts go to `~/.local/state/pma`.

### Added

**Each agent step's prompt ends with the output it is held to**, generated from the type it declares and the op it applies: the array's shape, `@id` and `@from` where they apply, the fields it may set, and each field's limits. A prompt author restated these by hand, and in a trial a confirm wrote a `reason` longer than the type allowed and two true findings were refused.

**`pma next`, and `pma` alone runs it: what should happen next, and who does it.** One list of tasks for agents, in the order `pma dispatch --auto` takes them, and one of what waits on you: runs to review, then to publish, then pull requests to merge, then critical or urgent tasks no agent may take or may take no more. The order is the matrix's and what an agent may take is dispatch's, so the view and `--auto` cannot disagree. `pma tui` opens on it; `m` switches to the 2x2. Each list shows `quadrant_limit` rows; `--all` shows every row.

**Stable item ids: a trailing `^k3f9q`.** An item's identity was its text, so rewording one reset its age and hid a run it had. An id is 5 random characters of lowercase Crockford base32 (no `i`, `l`, `o`, `u`) after a caret, which is how Obsidian marks a block id and which GitHub renders as text; `<<id>>` was rejected because CommonMark reads `<k3f9q>` as an HTML tag, which GitHub strips. Identity is now the id, else `gh:N`, else the text. `pma lint --ids` lists the open items without one and `--apply` writes them, moving any open run onto the id; the workflow `todo` sink gives one to each item it adds. A scan carries an item's age by key, including the date git gave its old text. Not minted at dispatch: publishing ticks that line on origin, and an uncommitted id in the clone would then conflict on every pull.

```markdown
- [ ] accept a trailing comma #parser ^7hq2m
```

**`pma status --all` includes untiered projects**, ranked as tier 5 and shown with tier `-`. Without it, a portfolio with no tiers had no rows, so the CI state `pma scan` records could not be read from `pma`. Scoped to `status` rather than set through `default_tier`, which also moves untiered tasks into `matrix` and `dispatch`.

**`pma verify <project>`: the check a dispatch would run, before one does.** It checks out the head of the remote default branch in a fresh worktree and runs the verify command there under the agent's environment, then records the result as that commit's base. The first pilot dispatch paid an agent to discover that `pma`'s own check could not pass in that environment; a check run in a plain shell had passed. Exits 1 when any named project's check does not pass.

**A `sanduk` worker, so a dispatch can run in a container.** Seeded like the other templates and selected the same way -- `pma config agent sanduk`, or `agent` on a route -- it runs the same `claude` inside a disposable container with the API key held on the host. Four of its flags are why it is a template rather than a line in the README: `--work-at-host-path` mounts the worktree where the host has it, so a path in a diff or a stack trace resolves; `--stream-json` passes the agent's own records through, which is what `claude-json` reads the cost from; `--no-report-instruction` keeps `REPORT.md` out of the tree the diff is taken from; and `--provider anthropic` is named beside `--agent claude`, because sanduk's default provider is openai and the mismatch fails the run before the container starts. It carries no allowlist: a container that denies egress does not also need a `Bash()` rule.

`key-safe` rather than `sealed`, because sealed blocks the fetch `cargo`, `go` and `pip` do mid-build and a run that cannot fetch fails for a reason unrelated to its task. Verify still runs on the host: containing the agent and not the build is stage (a) of [docs/dev/using-containers.md](docs/dev/using-containers.md), not the end of it. Proved as far as the `docker run` argv sanduk prints for it; not yet run in a container.

**`{timeout}` in a worker's arguments**, filled with the run's own deadline less 30 seconds. A worker that bounds itself has to stop first: given the same deadline it is killed by `pma` instead, and one that runs a container leaves the container for sanduk's sweep to find.

**`pma workflow run` executes all five primitives.** Agent `map` and `reduce` nodes run one model per unit, reading `in.json` and writing `out.json` under `<data>/artifacts/<instance>/<node>/<n>/`, numbered per run; what comes back is checked against the declared type, the node's `max_units`, and, for `out: 1` and `out: 0..1`, that the id is one it was given and no field outside `writes` changed. A node that is not an `edit` works in a detached worktree, so a model that writes cannot reach the clone. An `edit` node goes through `dispatch::prepare` and the phase 1 gates and leaves a run to review, as a dispatch does. The four remaining `check` rules -- `verify`, `scope-clean`, `ci-green`, `pr-merged` -- read the run an `edit` recorded against the unit; with no run they are `unknown`, which takes the default edge rather than passing or failing.

Iteration is not included. `retry`, a lap edge and a self-edge parse, bound and cost as they always did, and the runtime takes none of the three: a document using them runs its forward path once. A node's `publish` is parsed and ignored for the same kind of reason -- an agent node's worktree is discarded after the run. `docs/dev/workflows.md` marks both at the point it specifies them.

**The rules of section 8 are all built.** `open-issues` through `gh issue list`, `open-runs` from the runs not in a final state, `outdated-deps` from the last `--deps` measurement, and `rank` and `limit:<n>` on a reduce. A rule that names something unbuilt is an error, never a verdict of `unknown` or a quiet fallback to another rule's behaviour.

**Calls are flattened at propose time (W2).** A `call` node is replaced by its callee's nodes, named `<call site>/<node>`, before anything is estimated or run, so the runtime holds one graph, one set of caps and one frontier. An argument the call site passed becomes the default of a parameter the flat graph declares, under the same qualified name, while the callee's `max` still bounds it. Guards on both sides of a call compose by union; a field both guard differently is refused, since picking one would silently drop the other.

**A pass runs `max_parallel` agents at once.** An agent `map` or `reduce` node stages every run in its bag -- the artifact files, the detached worktree, the `runs` row -- then hands the agents to a pool of threads and records what comes back on the thread that owns the store, which is one `rusqlite` connection and stays where it is. An `edit` node prepares its whole bag before calling `dispatch::execute` once, rather than once per unit, which is what `max_parallel` and `batch_budget` already meant to `pma dispatch`.

**`pma workflow run` takes `dispatch`'s targets and `--set`.** `cynn:31` is one item, `cynn:critical` one per open item under the heading, `cynn:ci` a signal; a target whose element type is not the one the workflow reads is refused by name, and `cynn:q1` is refused because it holds items and signals at once. `--set name=value` binds a declared parameter, checked against its type and its maximum where it is given, and recorded on the instance so a replay reads the same instantiation.

**`pma project export` and `pma project import`: every project's tier and tags as one file.** Tiering a portfolio meant one command per project. `export` writes a row per project, the extension picking the format -- `.csv` as `name,tier,tag,tag`, `.json` as a list of objects -- and `import` reads an edited file back, listing what would change until `--apply`. A file naming something that is not a project is refused whole, so a typo does not leave half the portfolio retiered. A project the file leaves out keeps what it has; a project it names ends with exactly the tags in its row.

**`pma scan` draws a progress bar.** `[########------------] 38/95 alpha` on stderr, redrawn as each project lands, cleared before the summary. A scan of 95 repositories takes about 15 seconds and printed nothing until it was over. Silent when stderr is not a terminal, so a pipe or a CI log holds the bytes it held before.

### Security

**`pma`'s git calls refuse a worktree whose `.git` was replaced.** `pma` runs `git add`, `diff`, `commit` and `push` in the agent's worktree with the user's credentials. An agent could swap the worktree's `.git` file for a repository of its own whose `core.fsmonitor` named a program, and the next `pma` git call ran it. Each call now checks the `.git` file first, and runs with `core.fsmonitor=false`. A run caught this way fails, and `--reject` removes its directory directly, since git refuses to.

**An agent's push is refused on every remote.** `remote.origin.pushurl` was added to a remote's push URLs, not substituted for them: a remote with its own `pushurl`, another remote, or a URL on the command line still took the push. An empty-prefix `pushInsteadOf` now rewrites every push URL, and a `pre-push` hook covers a remote with an explicit `pushurl`, which git exempts from `pushInsteadOf`. `restrict` also appends its settings after any `GIT_CONFIG_*` the user set, which a fixed count overwrote.

**Publishing refuses a repository whose hooks or git settings changed since dispatch.** A hook, or a setting that runs a program or redirects a push, added during a run would run at publishing with the user's credentials, and neither is in the diff. The run records its hooks and those settings at dispatch, before base verify, and `pma pr` and `pma push` compare them before they commit and again before they push. The user's own hooks keep running. The alternative, disabling hooks when publishing, would have dropped them too. Schema 26.

### Fixed

**An agent step's files sit inside its own scratch tree, and a step that writes no output failed.** `in.json` and `out.json` were outside the tree, where Claude Code in `-p` mode may not write, and a missing `out.json` read as an empty result: a review that could not write reported no findings, and a confirm dropped every claim. The files are copied to `artifacts/` before the tree is removed. Found before the first trial on real repositories, which then wrote its output.

**A value a call site pins bounds the estimate, and one over the callee's maximum is refused.** The estimate read the callee's declared maximum even for a constant the caller passed, which `--set` can no longer change: `portfolio-sweep` priced 400 confirms where 288 can run. A constant above the callee's maximum was accepted.

**`open-issues` treats an empty field as absent.** An issue with no labels projected `tags: ""`, which a declared `line` refuses, so every unlabelled issue was refused.

**The library in `docs/dev/workflows.md` reads as it is written, and a test holds it there.** Five of its twelve examples did not parse: `task` was never declared, `apply-fixes` wrote an undeclared `verdict`, and `settle` guarded on `@merged` where the check writes `@pr-merged`. `triage-issues` parsed and would have refused every issue at run time. Prompts that said "each unit in {in}" now say one, since an agent `map` runs once per unit. The section names which examples `propose` accepts today.

**A count of leftover `pma/` branches reads "branches"**, not "branchs".

**A failed read of the attempt counter is an error, not zero attempts.** Any database error read as "no attempts", which lifted the limit it guards.

**A callee's parameter is renamed once when calls are inlined.** Renames were applied in sequence, so with `a -> b` and `b -> x` a prompt's `{$a}` became `{$x}` while the bound read `b`.

**`--set` sets only the workflow's own parameters.** A value a call site pinned became a flat parameter such as `issues/breadth`, which `--set` could override. A parameter name may no longer contain `/`.

**A GitHub slug is read only from the host `github.com`.** Any URL containing the text matched, so `notgithub.com/a/b` read as `a/b`. `user@` and an explicit port are accepted.

**Help and error text names the current commands**: `pma project forget`, `pma project tag add`, absent rather than forgotten projects, and `pma preset set` for the retired `model` setting.

**A workflow node runs once every node before it has finished.** A node ran as soon as any unit reached it, so a `reduce` joining several branches ran once per branch: two readings of the same three items deduplicated to six.

**A unit a node failed to handle no longer travels on as if it had succeeded.** A failed or refused agent run, an output the type check refused, an `edit` that `prepare` refused and a sink that could not write each routed the node's input along every unguarded edge. An unconfirmed finding reached the output, and a node that changes type passed a unit of the wrong type downstream. Such a unit now takes a default edge that carries its type, or settles, and is named on stderr.

**Routes by `node` and `lap` apply at run time.** A read node never consulted the policy, and an `edit` was dispatched with no node, so the node-less catch-all served it. Under a policy, a read node that no route names is refused rather than sent to the settings.

**A workflow `edit` counts against the attempt limit, keyed on its lineage.** `run_edit` never checked the limit, and the counter was keyed on the prompt text rather than on `workflow:<instance>:<root>` (W23).

**An agent node's results are recorded in one transaction per batch**, after the run's own row. A pass killed part way, or a cap hit part way, left some of a batch's units minted and the batch waiting, so the next pass minted them again. `workflow run` also fails runs an ended session left running, as `dispatch` does. A crash between the agent's exit and the record still runs that agent again.

**A cap is recorded on the instance whichever node hits it.** The mark was written inside the transaction the cap rolled back, so rule and reduce nodes hit the same cap on every pass.

**`ci-green` and `pr-merged` wait while the run is on its way**, and read the pull request by the URL `pma pr` recorded. They read the worktree, which publishing removes, so both returned `unknown` after publishing, and a check never waited. A `run` unit from `open-runs` now reaches the run it names.

**The `todo` sink refuses one unit rather than aborting the pass**, and no longer loses data silently. A missing section wrote nothing and routed the unit on as written; an item already open was added again, which the linter refuses; `remove` pruned every finished item in the file; a list field reached `TODO.md` as JSON text.

**An annotation or a filter can no longer drop a field it was not asked to write.** The minted unit is the one it was given with only its `writes` fields taken from the model. An agent `reduce` names its inputs in `@from` and writes at most one unit per group. `unique: "normalised"` drops a repeat within the bag or of an open item, as section 16 states. A rule node must emit the type its rule writes, and `open-issues` is checked against the type it projects onto.

**`workflow check` prices recursion over every level.** A self-edge was refused as a cycle, so the decompose example in `docs/dev/workflows.md` could not be read, and the cost bound counted one level of runs.

**A scan that cannot read `TODO.md` keeps the project's tasks.** A read error other than a missing file deleted every task row, so the next good scan gave each item `first_seen = now`.

**A task keeps its age when `sync` writes `gh:N` into it.** `first_seen` was looked up by key, and `gh:N` changes an item's key from its text to the issue number. It is now matched by key or by text.

**`TODO.md` is rewritten atomically** by `prune`, `sync` and the workflow `todo` sink: written to a temporary file, flushed, then renamed. `std::fs::write` truncated first, so a crash could leave the uncommitted file empty. A symlinked `TODO.md` is written through, and keeps its mode.

**A code fence closes only on a line of its own character, at least as long as its opening**, as CommonMark has it. A fence never closed is a lint error at its opening line. Before, three backticks closed four, backticks closed tildes, and an unclosed fence hid every later item while `lint` exited 0.

**`sync` checks an item's text before writing `gh:N`.** It checked only that the planned line held an unlinked item, so a line added above it while `gh issue create` ran put the number on the wrong item.

**`scan` bounds each `git` and `gh` call at 60 seconds** and kills its process group. One hung `gh run list` held the whole scan. A timeout is recorded in the project's scan error.

**The CI signal judges only enabled workflows.** A deleted workflow's runs stay in `gh run list`, so its last failure read as failing CI until 50 newer runs pushed it out. A workflow with no decisive run among the latest 100 is queried on its own rather than dropped.

**`##Critical` is a lint error.** Without the space it is not a Markdown heading, so the parser skipped it silently, and its items were filed under the section above. It still opens the section it names. `##Notes` and other names stay ignored.

**`pma`'s own tests pass under its verify.** Their fixtures push to local repositories, and inherited the `GIT_CONFIG_*` settings with which `agent::restrict` blocks every push, so `make check` failed at the base and at the head of every `pma` dispatch. The tests now start git and `pma` without them.

**An agent no longer outlives its session.** Agents ran in their own process group, so Ctrl-C killed `pma` and not the agent, and freed the session lock. The next `pma review` then failed the run as interrupted while the agent still wrote to it, and `--reject` could delete the worktree under it. Each agent and verify now runs under a `sh` watchdog that kills the group when `pma`'s end of a pipe closes. That covers every way `pma` can exit, with no signal handler, on Linux and macOS alike. The group is also killed after a normal exit, so a background process the agent started does not keep writing to the worktree.

**A dispatch refused after its worktree existed left the worktree behind.** A route refusal, or an error from base verify or worker choice, came after `git worktree add`, with no run row to release the worktree and branch. They are now removed.

**An item named like a signal is dispatched as an item.** An item whose text was `CI` or `deps` had the signal's key, and one starting `Workflow:` or `campaign:` read as a unit or campaign task, which skipped the origin check and was never ticked when published. Such keys are now prefixed with `item:`.

**`default_tier` accepts 1 to 5.** Any number from 0 was accepted, and every ranking command then panicked: `9` indexed past the tier weights and `0` underflowed.

**Two integration tests shared a scratch directory.** `Scratch::new` keyed the path on the test's label and the process id, and `publishing_resumes_after_a_partial_failure` and `an_instance_resumes_under_its_own_revision` both passed `"resume"`. Tests in one binary share a pid and run in parallel, so each wiped the directory on the way in and deleted it on the way out, under the other: whichever lost the race failed with `git init: cannot change to .../root/alpha`. The path now carries a counter, so a repeated label cannot collide.

**An instance now resumes under the revision it started with.** `--instance` read whichever revision was active, so activating another between passes walked old units with a different graph: moves are keyed by edge index, so the units could route through unrelated edges or reach a changed sink. The instance is loaded first, the name on the command line must be the one it runs, and its own recorded revision is what the pass walks. An instance that stopped short records why and is not resumed into the same wall; a finished one still re-derives to nothing, because readiness is evidence rather than a cursor (W9).

**`caps.max_units` and `caps.max_edits` are enforced where units and runs are written.** The estimator capped a node's output but the pass did not, so a project with more `TODO.md` items than the declared `max_units` minted more units than the figure `activate` weighed against the budget. Both caps are now checked at the write boundary. Exhaustion stops the pass, names the node and the cap, and records the instance as `capped`: a bag silently short of its input cannot be told from a complete one by anything downstream.

**`--dry-run` starts no instance.** It created one, inserted the root units and recorded their entry moves before testing the flag, so repeating a dry run left open instances the listing then showed. The root bag is built in memory and priced there.

**A node's bookkeeping commits per unit.** Each unit's children, its routing and the marker saying its node is done with it were separate writes, so a failure part way through a bag left a unit routed by a node that had not finished with it. They now commit together. The boundary is one unit rather than one node because a sink writes a file: a unit whose file was written and whose move was rolled back would have its sink run twice.

**A `reduce` applies the rule it names.** Every reduce ran `dedupe`'s "one per group, keep the first" whatever its `rule` said, so `rank` and `limit:<n>` were accepted, costed and then silently wrong. `limit:<n>` now truncates each group and `rank` orders it by `priority`, which is why a type declaring no `priority` is refused at `rank`; `limit:` with a count below 1 is refused where the document is read.

**A `reduce` marks its kept input handled.** Only the units it dropped got a settled move, so the unit it kept was still waiting at the node and the frontier offered it again on the next turn, node after node until the instance hit its cap. No test reached a reduce in a pass, which is why it held.

**`batch_budget` bounds a pass, not a node's turn.** The accumulator was local to one call, and `advance` re-enters a node as its frontier refills, so a pass could commit the budget once per node and then again. It is now held across the pass. A node the budget admits nothing for keeps its place in the frontier and is priced in the plan, so the pass ends where a pass should and the next one resumes there.

**A declaration is checked where it is written.** `parse_params` checked an enum's members and nothing else, and a field's `max`, `min` and `unique` were read with `as_i64` and `as_str`, so `"max": "ten"` silently meant 200 and `"unique": true` silently meant not unique. A default is now checked against its parameter's own type and maximum, `max` is refused on a parameter that is not an int, and a field option that does not apply to its type is refused by name.

### Changed

**`pma pr <id>...` and `pma push <id>...` replace `pma ship`.** `ship` published every approved run to wherever the `publish` setting said, so one command did two different things and where a run went was not visible at the call. Each command now does one: `pr` opens a pull request, `push` pushes to the default branch. Both take the runs to publish, or `--all-approved`, and refuse one that is not approved. The `publish` and `projects.<name>.publish` settings are retired; setting one names the commands.

The `shipped` state is gone. A pushed run is `pushed`. A pull request run is `pr-open`, then `merged` or `closed`, as on GitHub; `closed` was `rejected`, which also meant a run the reviewer refused. While a pull request is open, `pma review` shows its review decision, comment count and review count. `pma report`'s `merged` column is now `landed`: pushed or merged. Migration 28 moves `shipped` runs with a pull request URL to `merged` and the rest to `pushed`, moves `rejected` runs with one to `closed`, and drops the stored `publish` settings.

**Q4 is labelled "Later", not "Remove".** Under the default weights no `Medium` or `Low` item is ever important, so every such item that is not urgent lands in Q4, and "Remove" was the wrong advice for most of a backlog.

**`pma status` sorts projects by the worst state each is in, and the health score is gone.** Worst first: failing CI, broken `TODO.md` or scan, unpublished work, critical items, stale deps, idle; tier breaks ties. The score prompted no action, its five weights were never calibrated, it counted again signals the matrix already lists as tasks, and `status` was its only reader. The `health` column is now `state`, naming the worst state and counting the others; `--explain` lists each with what put the project there. The `weights.*` settings are retired, and migration 27 drops stored ones.

**`workflow check`, `workflow activate` and `workflow run --dry-run` name each agent node the routing policy would refuse**, since no route names it. A policy stating no node routes refuses every agent node, and that surfaced only when a pass ran.

**`pma workflow run --approve <plan>` replaces `--yes`.** A pass prints a plan -- each agent node, its units, worker, model and ceiling -- with an id, and an approval runs that plan and stops. `--yes` approved a budget: the pass went on to nodes that became ready after the printed ones and were never shown. `--dry-run` prints the id a first pass would print.

**A workflow document that states a construct the runtime does not take is refused at `propose`**, naming it: `retry`, lap edges, self-edges, `@children`, `publish`, the `doc` sink and `nonempty`. They parsed and priced, and a pass ran as if they were absent. `check` still prices them. A sink's mapping must name fields the sink writes, and `todo add` must map `priority`.

**`pma workflow stop <instance>`, and `--cap max_units=N` to resume a capped instance.** A capped instance could not be resumed, so the agent output it had paid for was abandoned. A raised cap is recorded on the instance and may only go up.

**`workflow_budget` is weighed at activation only, per unit of input.** A pass also compared the total over its argument bag against the same setting, so a revision that activated could not run over two projects. `batch_budget` and the approved plan bound what a pass spends.

**A read node of a workflow that edits works from the fetched default branch**, where its `edit` nodes start, so a finding names code the fix will see. A workflow that edits nothing still reads the clone's `HEAD`.

**Worktrees, logs and artifacts moved to `~/.local/state/pma`** (`$XDG_STATE_HOME/pma`, or `$PMA_HOME/state` when `PMA_HOME` is set; `PMA_STATE` overrides both). The database directory holds only `projects.db` and `session.lock`. The README and design no longer say the database can be shared between machines through git: nothing detected two copies diverging, and git cannot merge them. Worktrees of open runs stay where they were made, since each run records its path.

**`lint` and `prune` skip a directory argument that holds no `TODO.md`**, printing a note. A glob over a root names every repository, and one without a list failed the command. A file named outright must still exist.

**`pma project tier` takes the tier first and any number of projects:** `pma project tier 1 alpha beta gamma`. The tier is the argument shared across a run of projects, so it goes first and the list follows, as `pma project tag add` already reads. Setting a tier no longer resolves one project at a time: every name is checked before any is written. The form that printed one project's tier is gone, since `pma project` lists every name beside its tier.

`make install` now builds the release binary and copies it to `~/.local/bin`, overridable with `PREFIX`. It ran `cargo install --path .` before, which rebuilds from scratch into `~/.cargo/bin` and ignores any release binary already in `target/`.

## [0.3.1]

### Changed

**The command list is grouped, and three project commands moved under one noun.** 24 commands listed alphabetically told a reader nothing about where to start. The listing now has six sections -- the loop, tasks, reading, setup, agents, many at once -- generated from clap's own metadata against a table, so a command missing from the table fails a test rather than vanishing from the help. A second test keeps every line inside 72 characters, which is why the one-liners are short and the long form stays in each command's own help.

`pma tier`, `pma tag` and `pma forget` are now `pma project tier`, `pma project tag` and `pma project forget`: all three act on a project's record, none is in the common loop. `pma project` on its own lists the projects with their tier, tags and last scan. `scan`, `dispatch`, `review`, `ship` and `lint` did not move.

## [0.3.0]

### Added

**`pma workflow check`, and the workflow document behind it.** A workflow is a typed, parameterised function over bags of units: a graph whose nodes apply one of five primitives and whose edges decide, by rule, where each unit goes next. `check` reads a document, refuses what it cannot apply exactly -- a type disagreement across an edge, a `0..n` node with no unit cap, a cycle that is not a declared lap edge, a lap edge with no terminal path, declared effects that differ from the graph's -- and prints the worst-case runs and cost per node. The bound is computed at every parameter's declared maximum, so it does not depend on what an invocation passes; `activate` will refuse a document above it. Nothing is stored and nothing runs yet. Design: `docs/dev/workflows.md`.

A document may be written as JSON or built by a Rhai script, which is a second way to write the same thing: the script's value is converted to JSON and read by the same validator, and `--emit-json` prints it. A script applies one combinator per primitive to a graph value that carries its own output port, so a stage is wired by application and no node's name is repeated in an edge. A reusable piece of a graph is then an ordinary function, and a fan-out is a list of them:

```rhai
fn reviewer(node, what) {
    |g| g.expand(node, "finding", "{$breadth}",
                 "Review `{name}` for " + what + ". Write findings to {out}.")
}

let graph = source("project")
    .fan([reviewer("bugs", "correctness bugs"), reviewer("tests", "missing tests")])
    .join("merge", ["title"])
    .filter("confirm", ["reason"], "Confirm each unit in {in}.")
    .output();
```

**A model is named by a preset, and both older places for it are retired.** The `model` setting sat beside the choice of worker, so `-a opencode` had to drop it and a pairing could not be configured at all; migration 22 moved it onto the worker record. A worker's own model then said what a preset says and less, so migration 24 turns each one into a preset of that worker's name, makes it the default where that worker was the configured one, and drops the column. Both old commands explain where the setting went rather than failing as unknown. A worker is now how to run a program; which model it runs at, and with what configuration, is a preset.

**`pma preset` names a worker, a model and the configuration that goes with them, and `-p` selects one.** `pma preset set omp-luna-high omp gpt-5.6-luna --thinking high`, then `pma dispatch -p omp-luna-high` or `pma workflow run … -p omp-luna-high`; `pma preset use <name>` makes it the default. Effort is arguments rather than a field, because what expresses it differs per agent -- `--thinking` for omp, `--variant` for opencode, nothing on claude's command line -- so a worker's `args` template says where they go with a new `{extra}` placeholder, which stands for however many a preset adds. A preset takes part in no matching, so it competes with no route, and a flag beside `-p` wins. A preset naming a worker that does not exist is refused where it is set; one naming no model leaves the worker its own. The preset and its arguments are recorded on each run, so a replay reads back what ran. Schema 23.

`pma agent` is now that pair view: one line per worker, the model it would use, and any pair an active route names beyond them. The full record, args and environment included, moved to `pma agent show <name>`.

**Two more workers, and providers as configuration rather than code.** `opencode` and `omp` are seeded as templates beside `claude`, checked against `opencode` 1.18.27 and `omp` 18.1.18. A seed is written only when no row has that name, so an edited worker survives an upgrade. Schema 21.

`pma` runs agents and is not an API client: `{model}` reaches the worker verbatim, so `-a opencode -m openai/gpt-5.2` or `-m openrouter/anthropic/claude-sonnet-4.5` resolves in the agent's own provider configuration. Provider keys are inherited from the session, since `agent::restrict` strips only what lets a child push; a worker that needs a base URL for an OpenAI-compatible endpoint, or its own config path, carries it in a new `env` field, applied after `restrict` so a record cannot restore a stripped credential.

`parse` gains `json:<summary>:<cost>[:<error>]`: dotted paths into the last JSON value a run printed. A worker whose output shape nothing else reads is then a record rather than another variant in the code, and `opencode` and `omp` ship as `text-tail` -- verdict from the exit status, cost unknown rather than guessed -- until their shapes are confirmed.

**`pma workflow run` advances one pass and spends nothing without approval.** A node a rule decides -- `check`, `emit`, and `map` or `reduce` backed by a rule -- runs as soon as its units arrive, because it is free and deterministic. At the first node an agent would decide the pass stops and prints what it would run, with the worker, the model and a ceiling; `--yes` is what approves it, and `--dry-run` prices the pass without running even the free nodes. `-m haiku` runs a graph at a cheap model. A pass holds no state: units are immutable and every move a unit makes is recorded against the edge it took, or the guard that refused it, so the frontier is re-derived on each invocation and a killed pass resumes by recomputing. A test asserts the gate against the filesystem rather than against a claim: the fake worker writes a log line when invoked, and that file must not exist.

`pma workflow propose` stores a document as a draft revision, normalised as `pma` read it, with the script that built it kept beside it for provenance: a revision reads back as JSON whichever form wrote it. `pma workflow activate` refuses a revision whose worst case per unit of input exceeds the new `workflow_budget` setting, naming both figures. A route may now match on `node` and `lap`, where a route stating no node serves task dispatch alone, so an existing policy's catch-all cannot absorb a workflow's nodes; `*/fix` matches that node wherever it was called from. Schema 20 adds the `workflows`, `workflow_instances`, `workflow_units`, `workflow_moves` and `workflow_verdicts` tables, and `workflow_instance`, `node`, `unit` and `lap` on `runs`.

`effects` is inferred from the graph unless stated, since the builder knows whether an `edit` or an `emit` is present; a workflow that invokes another states its own, because a callee's effects are not visible from the caller.

A script builds a document and nothing else: it never runs during a pass, sees no unit and decides no guard, because a graph whose shape a script chose could not be costed before it was activated. `rhai` is compiled without a clock and without modules, `eval` is disabled, and the operation, depth and size limits are set, so one script yields one document on every machine.

## [0.2.0]

### Added

The agent-loop entries below turn one prompt per task into a measured, gated pipeline. `docs/dev/implementation-plan.md` sequences it; `docs/dev/plan-review.md` is the review it answers.

**Per-attempt records.** Each agent invocation and the verification after it is an append-only `attempts` row with its own summary, cost, duration, verify result and model. `runs` kept one mutable row per task, so a rework overwrote the previous attempt and a task that failed twice before succeeding recorded only the success -- the case calibration most needs. `pma review <id>` lists the attempts when there is more than one. Schema 6.

**A timestamp per run transition:** `dispatched_at`, `ready_at`, `decided_at`, `published_at`, and reported review time from `pma review <id> --minutes N`. `runs` held only the current attempt's start time, which a rework moved, so nothing could count changes published per day or bound a report to one pass. `published_at` is when `pma` pushed or opened the pull request; a merge days later does not move it.

**Task class and the decision behind a dispatch.** `deps` is class A, `ci` and any unclassified item are B, and an item tagged `#manual` is D and is refused before a worktree exists. The run records the class, its allowed globs, the tier, the description, `agent_budget` and `timeout` as they were at dispatch, and never updates them: a rescan, a reworded item or `pma config` would otherwise leave no way to say what a past dispatch decided. Schema 7.

**Verification at the base commit**, in the fresh worktree before the agent starts. `pma review <id>` reports both ends: ``make test`: base FAILED, head passed`. One run at the head proves the tree is green now; it cannot tell a regression from a repository that was already broken, which is what class A auto-approval requires. Results are cached per project, base commit, command and timeout, so a batch pays once and changing the command or the timeout misses rather than reuses. A base check that could not start is unknown, not failing. The verify command is now chosen once, at dispatch: it was re-detected on every attempt, so a rework could check the head with a command the base was never checked with. Schema 8.

**Path scope, checked against the whole change.** Every path a run touched is enumerated against its base and recorded, and `pma review <id>` reports what its class does not permit: `scope  1 of 5 files OUTSIDE: .github/workflows/ci.yml`. Untracked files are staged with `--intent-to-add` so a new file counts, the listing is NUL-delimited because a path may contain a newline, and `--name-status` names both sides of a rename. Paths that could not be enumerated are unknown with the reason, never an empty list, which would read as a run that changed nothing. Schema 9.

`.github/**`, `LICENSE`, `COPYING` and `.netrc` are refused to every class but A-, and `TODO.md` to all of them, judged on the paths actually changed rather than the class predicted at dispatch. A workflow runs with repository tokens and CI validates the changed workflow rather than checking it, so a task misread as a dependency bump must not reach one.

**Review by exception.** `pma review` marks each run `clean` or `N to read`, and `pma review <id>` lists why:

```
read this run because:
  - outside class B: .github/workflows/ci.yml

  - the base already passed, so no check discriminates this change
```

What the two verify results must show depends on the class. A must leave a green tree green. B is accepted by a check that fails at the base and passes at the head; green to green shows no regression but no fix either. A missing command, a check that did not run and an unknown base are each distinct reasons. So is a run that cost more than `agent_budget`, which admits runs rather than capping spend: `claude` checks its own cap between turns and has exceeded it, and a worker with no cap overshoots without limit. A run that edited the files implementing its own verify command is read whatever else passed. The reasons are computed from the run, so changing a rule re-reads the evidence.

**A task an agent cannot close is not chosen again.** Attempts are counted per project and task revision; at two, `--auto` passes the task over and a named dispatch refuses, until `pma dispatch <target> --retry`. The count survives the run that raised it, so rejecting a task and dispatching it again no longer starts from zero. The revision is the task's normalised text, so rewording starts fresh -- a revised specification has not been tried -- and `fix CI: build` and `fix CI: build, test` are different incidents. A red check and a rejection each consume one; a budget refusal, a failed spawn and a timeout consume none. Schema 10.

**`pma report`** shows what dispatching produced, grouped by class, project or agent:

```
67% of 3 decided runs accepted

by class  runs  1st pass  accepted  merged  open  attempts  cost      agent  review
B         4     1         2         2       0     4         $0.40+1?  0s     10m00s
```

First-attempt passes, acceptances and merges are three columns rather than one number. A queued or running run is left out rather than counted against the share, an open pull request is in no share, and a worker that reported no cost appears as `+1?` rather than summed as free.

**Agents are records, not a code path.** `pma agent set <name> <field> <value>` defines a worker: how the prompt and directory are passed, how a model is named, whether a budget argument exists, how success and cost are read back, and whether there is a sandbox or an allowlist. `{prompt}`, `{dir}`, `{model}` and `{budget}` are filled in at dispatch, and an argument whose placeholder has no value is dropped with the flag before it, so `--model {model}` disappears whole. `claude` is seeded with the behaviour it had compiled in. Two parsers: `claude-json`, and `text-tail` for a worker with no structured output, which leaves cost unknown rather than zero. New settings `agent` and `model`. Schema 11.

What a record cannot carry, the pipeline carries: the worktree, the stripped push credentials, `pma` running `verify` itself and the scope check do not depend on the worker.

**A complexity estimate per run**, 1 to 5, from features measured at dispatch: whether the task names a path or a symbol, its words, its description lines, the repository's tracked files, the base verify duration, and the project's accepted share. `pma review <id>` shows `complexity  2 of 5 (v1)`. The rule is a sum of named adjustments, and both it and the features are stored with the rule's version, so replaying a past decision re-runs the rule named on the run. No model is called. Schema 13.

**Routing policy as a versioned artifact.**

```json
{"route": [
  {"name": "chores", "match": {"class": "A", "complexity": "1-2"},
   "model": "haiku", "escalate": {"model": "sonnet", "attempts": 1},
   "approval": "batch"},
  {"name": "rest", "match": {}, "approval": "each"}
]}
```

First match wins; a task matching no route refuses the dispatch by name rather than falling through to a default nobody wrote. One judgment per revision, not one per task. `pma route propose` stores a draft that routes nothing; `pma route activate <rev>` puts it in effect and records who did it, and `--shadow` records the computed route without applying it. Each run keeps the revision it was dispatched under, so a later activation cannot rewrite a past decision. `unattended` is refused where the document is read if it covers class A-, C or D, or names no class at all. Schema 14.

The document is JSON, not the TOML the design named: this crate parses JSON already, and adding a TOML parser and `serde` derive for one file is the larger change.

**`pma route replay <file>`** applies a candidate to the recorded runs and reports what it would route differently, reading each run's own snapshot so a task edited, retiered, rescanned or reworked since cannot change the answer. No cost is projected: what a different model would spend, or whether it would succeed, is not in this data, and the report says so instead of printing a number.

**Escalation.** A route may retry once at a stronger model where the check refused the work, with the first attempt still in the worktree. Both attempts are reserved against `batch_budget` before the first starts, so the second is not refused after the first spent the room, and each is recorded with the model it ran.

**An approval is evidence about a tree, not a state.** Approving records the tree it was given for, the commit it was taken at, and who gave it; ship publishes that tree or nothing. Ship rebased, ticked the item, amended and pushed without rerunning anything, so an edited worktree could publish content no check had seen. `verify` now runs again on the integrated tree after the rebase and the tick: two changes that each pass against the same base can fail together, and a clean rebase is not a semantic one. A rework withdraws the approval, and approving an approved run re-takes the evidence, which is how a reviewer says a refused tree is fine after looking again. Schema 15.

**Approval modes are acted on.** A route with `propose` refuses approval by name. `pma review --approve 1 2 3` approves a batch, checking every named run first, so a list with one run to read in it approves nothing.

**Campaigns: one task definition across many repositories.**

```
pma campaign add workflows "add a workflow" --projects a,b,c --class A- \
  --describe "Create .github/workflows/ci.yml that runs on push."
pma campaign run workflows
```

Each repository gets its own worktree, its own base and head verification and its own pull request, and the reviewer holds one context across them. Membership is fixed when the campaign is defined, so a rescan cannot move work under one in flight. A member whose run is not final is named -- `beta: #2 is failed; reject or rework it to dispatch again` -- rather than dispatched over, which would leave its worktree behind and could open a second pull request for the same work. Schema 16.

**Deps signal.** `pma scan --deps` counts outdated dependencies with `cargo update --dry-run`, `uv tree --outdated` and `go list -u -m all`, adds an "update dependencies" task, and scores deps in health. It is opt-in because it took 44s for 52 repos against 1s for a plain scan. Plain scans keep the last measurement. Counts differ in kind between tools: cargo's include transitive crates.

**`pma prune`** removes finished items and their descriptions from TODO.md files, and each v1 `## Done` section whole. A dry run unless `--apply`. Files with lint errors are skipped.

**Leftover worktrees in the hygiene signal.** A scan counts local `pma/` branches that no open run owns, and "resolve local changes" lists them. Each `pma` worktree has such a branch, so the count also finds branches whose worktree is gone. Hygiene scores 0.5 per condition, capped at 1, which keeps the existing values. The database schema moves to version 4.

**`pma note`** adds, edits, removes and lists portfolio notes.

**`pma tui`** shows the matrix as a 2x2 layout with the selected task's details. `ratatui` is built with only its crossterm backend, which adds 69 crates instead of 152.

**`pma sync`.** Opens an issue labelled `pma:critical` for each `## Critical` item and writes `gh:N` into its line, marks items done when their issue is closed, retitles and relabels linked issues to match TODO.md, and lists open issues by other people. A dry run unless `--apply`. Write-backs stay uncommitted in the clone rather than going through `pma ship`, which commits worktrees only.

Each `gh:N` is saved as soon as its issue exists, and an open labelled issue with an unlinked item's title is linked rather than duplicated, so an interrupted sync does not open a second issue. Adding `gh:N` changes an item's key, so dispatch and ship now match a task by key or by text.

**`pma dispatch`, `pma review`, `pma ship`.** Run `claude` on tasks in worktrees of the remote default branch, verify the result with the project's own tests, review the diff, then commit, push or open a PR, and mark the item done. Settings: `max_parallel`, `batch_budget`, `agent_budget`, `timeout`, `publish`, `attribution`, and per project `projects.<name>.verify` and `projects.<name>.publish`. The database schema moves to version 2; version 1 databases are upgraded on open.

The item is marked done after the rebase, not before. Git treats changes to adjacent lines as a conflict, so ticking first could conflict for two tasks from one project. An item must be open in the remote `TODO.md` to be dispatched; otherwise ship would have no line to mark.

`claude --max-budget-usd` is checked between turns and was exceeded in use ($0.09 under a $0.05 cap). `batch_budget` limits which runs start, not their total spend.

**Each project's GitHub `owner/name`**, read from the origin URL by the scan and stored on the project row. `pma sync` derived it per invocation and no other command could reach it, so an item's `gh:N` was an issue number with no repository to resolve it against. The local path remains the project's identity. Every scan re-reads the slug, so an origin that moves off GitHub clears it. Schema 17.

**`pma forget <project>`** deletes an absent project's record -- its tasks, tags, attempt counters, cached base checks and campaign memberships -- and is a dry run until `--apply`. Nothing else deletes a project row now that a full scan only marks, so a repository that is gone for good would otherwise stay indefinitely. It refuses a project still under a root, which the next scan would restore anyway, and one with runs that are not shipped or rejected, since each may still own a worktree; finished runs are kept.

**Private project tags.** `pma tag add ai cyllama inferna` groups projects, `pma tag` lists the tags with their counts, and `pma tag show <tag>` names the members. Every command that takes project names also takes `--tag`, repeatable, selecting the union: `pma status --tag ai --tag audio` covers both groups and lists a project carrying both of them once. Tags are local to the database, never read from or written to GitHub, whose topics describe a repository for search rather than group one's own work.

A `--tag` that matches no project is an error, because an empty selection means the whole portfolio everywhere else, and `pma sync --tag typo --apply` would then act on every project. A tag selection also makes `pma scan` partial: as a full scan it would mark every project it did not name absent. Schema 19.

**One target can name more than one task, and the agent is chosen per dispatch.**

```sh
pma dispatch -a pi -m sonnet cynn:critical   # every open item under ## Critical
pma dispatch cynn:q1                         # every task the last matrix placed in Q1
pma dispatch cynn                            # the project's tasks, in a list with checkboxes
```

`-a` and `-m` outrank `config agent`, `config model` and an applied route: a flag is the last word on the run in front of you. Naming another agent drops the configured model, which names a model of the configured agent, and a named model pins it -- escalation exists to reach for a stronger model, and the caller has just reached. A target that names one task still fails the batch when that task cannot run; one that names many passes each over with its reason on stderr, since a heading held up by a single task with a live run would be useless. A bare project name opens the list rather than guessing, and nothing in it starts checked: dispatch spends money.

### Changed

**A full scan marks a project absent instead of deleting it.** The row keeps its tier, its tasks and their `first_seen`, and records when it stopped being found; `pma scan` reports `absent: <name> is no longer under a root`. Deleting dropped the tier and every task's `first_seen`, and a later scan can rebuild neither, so a project moved between roots came back with all of its work aged from the day it returned. `pma dispatch`, `pma campaign` and `pma sync` refuse an absent project and name where it was last seen, since its recorded path no longer holds a working tree. Schema 18.

**Eligibility replaces quadrant gating.** `pma dispatch --auto` draws from the tasks an agent may take -- the `ci` and `deps` signals at any tier, and items tagged `#agent` -- in matrix order. Importance orders the queue; eligibility decides what is taken from it. `Medium` and `Low` are never important at any tier, so quadrant gating hid exactly the mechanical maintenance agents are best at, and a `deps` task landed in Q4 where `--auto` never reached it. `dispatch_quadrants` and `overflow_quadrants` are retired; the 2x2 remains a view.

**Urgency is sequencing, not decay.** A task is urgent for a deadline within `urgent_within`, a signal that blocks other work in its repository, or `#urgent`. `stale_after` and its five settings are retired: with no due dates in a portfolio the rule reduced to "older than 30 days", which within a month admitted most tier-1 tasks and told the queue nothing it did not already know from sorting by age. Age now breaks ties and fills `pma stale`.

**`default_tier`** ranks projects that have no tier of their own instead of leaving them out of the matrix. Unset by default.

**Retiring a setting is a migration, not only a code change.** A stored row for an unknown key made every command fail. The upgrade deletes the rows, `pma config` explains a retired name and what replaced it, and a row written by another binary is skipped rather than fatal. Schema 12.

**The agent may run the verify command.** `claude` got `acceptEdits` only, so in `-p` mode it could not run the project's tests, while `pma` ran code the agent had edited anyway. It now gets one exact `Bash(...)` rule per subcommand of verify. Other shell commands stay denied. The design states that the permission mode is not an isolation boundary.

**TODO.md format v2: finished items stay in their priority section.** `## Done` is no longer part of the format. Ship and sync tick `- [ ]` to `- [x]` in place, and lint accepts `- [x]` in any priority section. A `## Done` section grows without limit; `pma prune` removes finished items when wanted. Each item in an existing `## Done` raises a warning that names `pma prune`.

**`pma lint` accepts bullets outside the priority sections in a migrated file.** The "no items" warning now fires only when a file also has no priority section. A file with `## Critical` to `## Low` and no open items is valid, and its declined or deferred work can stay as plain bullets instead of becoming tasks.

### Fixed

**A task shipped as a pull request was dispatched again before the merge.** The run became `shipped`, which frees its task, while the item stayed open on the default branch. The second run then failed to push over the first run's remote branch. Such a run is now `pr-open` until `gh pr view` reports it merged (`shipped`) or closed (`rejected`); `pma review` and `pma dispatch` check. A new run's slug also avoids remote `pma/` branches. The database schema moves to version 5, so an older `pma` refuses the file instead of reading `pr-open` as failed. The upgrade returns runs already shipped as pull requests to `pr-open`, so their merge state is checked once.

**`pma review` during a dispatch marked the running runs failed.** It assumed no other session was live. A failed run could then be rejected or reworked while its agent still ran. `dispatch`, `ship`, `review --reject` and `review --rework` now hold an `flock` on `session.lock`, and interrupted runs are failed only when that lock is free. A lock was chosen over a pid column in `runs`: the kernel releases it on exit, and pids are reused. A command that needs the lock waits up to 2s, since `pma review` holds it for milliseconds. `File::try_lock` raises the minimum Rust version to 1.89.

**`pma ship` could not resume after a partial success.** If removing the worktree failed after a push, the run stayed approved, and the retry reported "nothing to ship" for work already on the default branch. If `gh pr create` failed after the branch was pushed, a retry could fail on the existing pull request. The outcome is now saved before cleanup, and a cleanup failure is a warning. A retry records pushed commits found in the default branch, and reuses an open pull request for the branch.

**`pma dispatch --auto` could dispatch nothing when its top task was refused.** A refused pick kept its place, so `-n 1` with an unpushed item on top started no run. The next candidate now takes the place. The refusal says whether the item is missing from the remote `TODO.md` or already done there; it always said "commit and push it first".

**A fix-CI prompt could carry the wrong log.** It took the latest failed run of any workflow on the branch, which may be an old failure of a workflow that passes now. With no failed run, it called `gh run view null`. It now takes each failing workflow's latest decisive run, as scan does, and refuses the task when that workflow passes.

**CI detail names the cause of a `gh` failure.** It kept the last line of `gh`'s stderr, which is an alternative or an update notice. Without authentication every project read "Alternatively, populate the GH_TOKEN environment variable..." instead of "please run: gh auth login".

## [0.1.0]

### Added

**Item groups.** An item's group is the nearest `###` heading above it within its section, shown as a column in `pma matrix`. A group comes from the item's position in the file rather than a tag. Migrated files can keep their original sub-headings without a long per-item tag. The trade-off: an item moved to another place changes group.

**`pma scan`, `pma matrix`, `pma status`.** Scan the git repos under the roots into SQLite, place the tasks of tiered projects in an Eisenhower matrix, and rank projects by health. `status --explain` prints each signal's share. Tiers, roots and settings are managed with `pma tier`, `pma root` and `pma config`.

An item's age is taken from the commit where its text first appeared in `TODO.md`, not from `git blame`. On real repos, blame dated every migrated item to the migration commit, because migration added a tag to each line. Every age restarted at 0, and no item could become urgent by age for a month.

**`pma lint`.** Checks TODO.md files against format v1, defined in `docs/dev/design.md`. The parser works line by line, not through a markdown AST, so later stages can edit one line without re-rendering the file. A file with no items but with plain bullets elsewhere is flagged. Otherwise such a file lints clean while `pma` sees none of its tasks.

