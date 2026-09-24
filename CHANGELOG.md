# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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

