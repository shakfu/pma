# Workflow pilot

The first runs of the workflow engine on real repositories and real agents, 2026-09-24 and 2026-09-25. Four instances of one review-then-confirm workflow, over `pma` and `argdec`. No step edits a repository.

## Setup

- Database: a copy of `~/.config/pma/projects.db` in `PMA_HOME=~/.config/pma-trial`, so the real database stayed at schema 26. State (worktrees, artifacts, logs) went to `~/.config/pma-trial/state`.
- Worker: `claude` (Claude Code 2.1.282) on the host, signed in with the user's own login. No API key was set in the environment. The `total_cost_usd` figures below are what Claude Code reports, which for a subscription login is a usage figure, not a charge.
- Projects: `pma` (47 tracked files, Rust) and `argdec` (16 tracked files, Python). A workflow that edits nothing reads the clone's `HEAD`, so `pma` was reviewed at `647c850`, without the day's uncommitted fixes.
- Workflow: `review` (agent `map`, `0..n`, emits `finding`), then `confirm` (agent `map`, `0..1`, may write only `reason`), then a note per kept finding. Each agent step runs once per unit in a scratch worktree.
- Approval: each plan printed by a pass was approved by its id. `batch_budget` sized plans to 5 runs.

## Found before the first run

The `claude` worker runs with `--permission-mode acceptEdits`, which approves edits inside the working directory only (see [pilot.md](pilot.md), "Setup"). A read step's `in.json` and `out.json` were in `state/artifacts/`, outside its worktree, and `read_out` read a missing `out.json` as an empty result. A review that could not write would have reported no findings, and a confirm would have dropped every claim, all without an error.

Fixed before spending: a step's files sit in `.pma/` inside its worktree and are copied to `artifacts/` when the tree is removed, and a missing `out.json` fails the run. Every run in the four instances wrote its output.

## Runs

| Instance | Review | Confirm | Findings | Kept | True | Cost |
|-|-|-|-|-|-|-|
| 1 | haiku | haiku | 7 | 6 | 0, one partly | $1.52 |
| 2 | haiku | sonnet | 3 | 0 | 0 | $1.31 |
| 3 | Opus 5.5 | Opus 5.5 | 7 | 5 | 7 | $2.73 |
| 4 | Opus 5.5 | Opus 5.5 | 17 | 17 | 17 | $6.79 |

"True" is what a reproduction outside the workflow showed: each `argdec` trigger run in Python 3.14, and each `pma` claim read against the code at `HEAD` or run against the binary. Instance 2's review made different claims from instance 1's, since every instance reviews afresh; the confirm steps were not judged on the same claims.

Per run:

| Step | Model | Runs | Cost per run | Seconds per run |
|-|-|-|-|-|
| review | haiku | 4 | $0.21-0.57 | 190-340 |
| review | Opus 5.5 | 4 | $0.42-1.83 | 75-243 |
| confirm | haiku | 7 | $0.06-0.12 | 30-71 |
| confirm | sonnet | 3 | $0.16-0.26 | 44-57 |
| confirm | Opus 5.5 | 24 | $0.16-0.49 | 21-97 |

Instances 2 to 4 routed each step by a policy (`{"match": {"node": "review"}, "model": ...}`), with no `-m` flag. Each run recorded its route and model.

## What the confirm step did

- **haiku** kept 6 of 7 claims. Four `pma` claims shared one false premise, that Rust's `str::lines()` splits on a bare `\r`; it does not ([`str::lines`](https://doc.rust-lang.org/std/primitive.str.html#method.lines)), and `pma lint` and `prune` handle `\r`-only and mixed endings without a panic. The `argdec` claim that `add_subparsers()` defaults to `required=True` is also false. The `reason` each confirm wrote restated the reviewer's premise.
- **sonnet** rejected all 3 of instance 2's claims, which also shared one false premise: that rusqlite's `unchecked_transaction()` does not roll back on drop. rusqlite 0.40.2 builds it with `DropBehavior::Rollback`. The rejections named that.
- **Opus 5.5** kept every true claim it saw. Its reviews and confirms ran code to reproduce each bug. `pma` grants a read step no shell rule, so the execution came from the user's own Claude Code settings; two confirms also wrote scripts to `/tmp`.

## What the caps did

Instance 3 used `reason: line(300)` and `breadth` 5, both chosen without evidence.

- Two confirms wrote a 351- and a 407-character `reason`. The type check refused both, and two true findings were lost. The prompt had not stated the limit.
- The `argdec` review returned exactly 5 findings, `breadth`'s maximum. At 15, instance 4 found 11.

Instance 4 changed `reason` to `lines(30)` and `breadth` to 15, and `pma` now appends each step's output contract to its prompt: the array's shape, `@id` where it applies, the fields a step may set, and each field's limits, generated from the declared type. Nothing was refused.

## Bugs found

In `argdec`, 11, each reproduced by hand and filed in its `TODO.md`: reversed positional order, diamond and mixin inheritance, `%` in a docstring or version, child commands shadowed by a parent's positional or its option defaults, a `--func` dest, `staticmethod` and `classmethod` commands, an empty prefix, and digit segments in flat mode.

In `pma` at `647c850`, 8 true claims across instances 3 and 4:

- Already fixed in the working tree that day: the `todo` sink's `remove` pruning every finished item, multi-line text written into `TODO.md`, a missing `out.json` read as empty, a scan deleting tasks on a read error, and `sync` linking by a stale line number.
- New, filed in `TODO.md`: ship skips the approved-tree check once `HEAD` has moved, and ship commits the `TODO.md` tick for a run with no changes.
- Arguable: class A- may edit `LICENSE`, `COPYING` and `.netrc`. The code and the comment on `PRIVILEGED` allow it; A-'s scope is `.github/**`.

## Conclusions

- The model decides the result. A cheap confirm step does not filter a cheap review; it agrees with it.
- A field limit that does not fit its content discards paid, correct work. State it in the prompt, which `pma` now does, and size it to the content.
- The engine held over 46 runs: plan approval by id, routing per step, the type check, unit routing and artifact keeping worked, and no worktree was left behind.
- Why a confirm dropped a claim is stored only in its run's summary. Recording a reason per drop against the unit is the next change the trial points to.

## Reproducing

Run against a copy of the database, so the real one is not migrated:

```sh
export PMA_HOME=~/.config/pma-trial
mkdir -p $PMA_HOME
sqlite3 ~/.config/pma/projects.db ".backup '$PMA_HOME/projects.db'"
```

### Instances 1 and 2: `pilot.rhai`

Worst case 12 agent runs over two projects, $12.00 at `agent_budget` $1. `reason` is one line of at most 300 characters and `breadth` is at most 5, the two caps instance 3 showed were too tight. The prompts state the output format by hand; `pma` did not yet append the contract.

```rhai
// Pilot: review, then confirm, then a note per confirmed finding. No edits.
let review = "Review the code of `{name}` in the current directory for correctness bugs: " +
    "code that gives a wrong result, crashes, or loses data. Ignore style. " +
    "Write at most {$breadth} findings to {out} as a JSON array " +
    "of objects with the fields `severity` (one of critical, high, medium, low), `title` (one line " +
    "naming the bug), `detail` (an array of short lines: where, why, and how to trigger it) and " +
    "`paths` (an array of the files involved). Write [] if you find none.";
let confirm = "The file {in} holds one claimed bug in the code in the current directory, as a JSON " +
    "array of one object. Check the claim against the code; run nothing that changes a file. " +
    "If the bug is real, write to {out} a JSON array holding that object with every field unchanged, " +
    "`@id` included, plus `reason`: one line saying how you confirmed it. If you cannot confirm it, write [].";
let graph = source("project")
    .expand("review", "finding", "{$breadth}", review)
    .filter("confirm", ["reason"], confirm)
    .emit_note("record", #{ text: "{@project}: {severity}: {title} -- {reason}" })
    .output();
document(#{ finding: #{ fields: #{
    severity: req(choice(["critical", "high", "medium", "low"])),
    title:    req(unique(line(200))),
    detail:   lines(20),
    paths:    list(),
    reason:   line(300),
}}}, [
    workflow("pilot", graph, #{
        params: #{ breadth: bounded("int", 5, 5) },
        caps: #{ max_units: 30, max_edits: 0 },
    }),
])
```

Instance 1 ran with `-m haiku` on every step. Instance 2 routed the steps apart, with no `-m`, since a flag overrides a route:

```json
{"route": [
  {"name": "reviewer", "match": {"node": "review"}, "agent": "claude", "model": "haiku", "approval": "each"},
  {"name": "confirmer", "match": {"node": "confirm"}, "agent": "claude", "model": "sonnet", "approval": "each"},
  {"name": "tasks", "approval": "each"}
]}
```

### Instances 3 and 4: Opus 5.5

Both routed every step to Opus 5.5 by its full id, since the `opus` alias may name another version:

```json
{"route": [
  {"name": "reviewer", "match": {"node": "review"}, "agent": "claude", "model": "claude-opus-5-5", "approval": "each"},
  {"name": "confirmer", "match": {"node": "confirm"}, "agent": "claude", "model": "claude-opus-5-5", "approval": "each"},
  {"name": "tasks", "approval": "each"}
]}
```

Settings: `agent_budget` 3, so an Opus run is not stopped at $1, `batch_budget` 15, five runs a plan, and for instance 4 `workflow_budget` 60. Instance 3 ran `pilot.rhai`.

### Instance 4: `pilot2.rhai`

Worst case 32 agent runs over two projects, $96.00 at `agent_budget` $3. `reason` is a list of up to 30 lines and `breadth` is 15. The prompts say what to do; `pma` appends the output format from the declared type.

```rhai
// Pilot 2: review, then confirm, then a note per confirmed finding. No edits.
// The output format comes from the declared type, which pma appends to each
// prompt; the prompts say only what to do.
let review = "Review the code of `{name}` in the current directory for correctness bugs: " +
    "code that gives a wrong result, crashes, or loses data. Ignore style. " +
    "Report each bug with `severity`, a one-line `title`, `detail` saying where, why and how " +
    "to trigger it, and the `paths` involved.";
let confirm = "{in} holds one claimed bug in the code in the current directory. Check the claim " +
    "against the code, and reproduce it where you can without changing any tracked file. " +
    "Keep it only if it is real, and set `reason` to the evidence: what you ran or read, and what it showed.";
let graph = source("project")
    .expand("review", "finding", "{$breadth}", review)
    .filter("confirm", ["reason"], confirm)
    .emit_note("record", #{ text: "{@project}: {severity}: {title}\n{reason}" })
    .output();
document(#{ finding: #{ fields: #{
    severity: req(choice(["critical", "high", "medium", "low"])),
    title:    req(unique(line(200))),
    detail:   lines(20),
    paths:    list(),
    reason:   lines(30),
}}}, [
    workflow("pilot", graph, #{
        params: #{ breadth: bounded("int", 15, 15) },
        caps: #{ max_units: 50, max_edits: 0 },
    }),
])
```

### Commands

```sh
pma route propose routes.json && pma route activate 1
pma workflow propose pilot.rhai && pma workflow activate 1
pma workflow run pilot pma argdec            # prints the review plan and its id
pma workflow run pilot --instance 1 --approve <id>
# repeat with each plan id printed until "nothing left to run"
pma note                                     # one note per confirmed finding
```

Each run's `in.json` and `out.json` are kept under `$PMA_HOME/state/artifacts/<instance>/<node>/<n>/`, and the confirm step's reasoning for a dropped claim is in the run's summary: `sqlite3 $PMA_HOME/projects.db "select summary from runs where node = 'confirm'"`.

