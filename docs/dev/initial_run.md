# Initial run

2026-09-24. The first pilot dispatch ([pilot.md](pilot.md), item 4): run #1, `pma dispatch pma:23`, "A `##Critical` heading without a space is a lint error". Worker `claude` on the host, no container, `publish = pr`. The run reached `ready`; it has not shipped.

## Numbers

| Measure | Value |
|-|-|
| Base verify (`make check`) | 27 s, FAILED |
| Agent | 85 s, 20 turns, $0.39 |
| Head verify | about 11 s, FAILED |
| Dispatch to ready | 97 s |
| Changed paths | `src/todo.rs`, `CHANGELOG.md`, both in scope |
| Commits by the agent | 0 |
| Complexity (pma's estimate) | 3 of 5 |
| Difficulty (human estimate) | 1 |
| Model | not recorded: the worker ran at its own default |
| Permission denials | 3: one narrower `cargo test`, two `git config` reads |

## What worked

- **The pipeline, dispatch to ready.** Worktree from `origin/main`, base verify, agent, head verify, changed paths, scope check, cost, the git snapshot, and the review reasons all ran and were recorded. No step needed a manual fix.

- **The change is correct.** Two or three `#` followed by text is an error; a bare `##` and `####`-or-deeper are left alone, as `design.md` specifies. It came with two test cases and a CHANGELOG entry in the file's own style. It also flags `###parser`, one step beyond the task, and the summary says so.

- **The summary was honest.** It named the verify failure and its likely cause, and marked that cause as inferred because it could not read the git config. It listed what it left undone.

- **The bounds held.** The push block stopped the tests' pushes. The allowlist denied three commands outside the verify command. The agent made no commit and did not touch `TODO.md`.

- **Review by exception flagged the run**: "verify failed at the head".

## What did not work

- **Verify cannot pass for `pma` itself.** Eight CLI tests push to local bare repositories in their fixtures. Verify runs under `agent::restrict`, and the step 1 `pushInsteadOf` block now rewrites those pushes too: `ssh: Could not resolve hostname pma-agents-cannot-push`. The tests inherit `GIT_CONFIG_*` from the environment instead of isolating themselves. `make check` passed during step 1 only because it ran outside that environment.

- **Base and head both failed, so verify gave no signal.** Class B acceptance needs a check that fails at the base and passes at the head. A base that fails for a reason unrelated to the task makes that comparison meaningless, and nothing told the difference before the agent ran.

- **The run cannot ship.** Ship re-runs verify on the integrated tree, which fails the same way. The run has to be rejected, although the agent did the task.

- **The money was spent before the fault was visible.** Dispatch ran the agent after the base had failed. Class B expects a failing base, so this is correct behaviour, but it cost $0.39 to find a problem a preflight would have found for nothing.

- **The pilot preparation measured the wrong thing.** The verify times and results in `pilot.md` came from fresh clones in a plain shell, not from the agent's environment. The environment is the variable that broke.

- **The model was not recorded.** No preset names a model, so `runs.model` is null. The runs cannot be compared by model later.

- **The allowlist slowed diagnosis.** The agent could run only `make check` whole, so it could not rerun one failing test or read the git config. That is the intended bound. The cost is that each check took a full `make check`.

## Recommendations

In order.

1. **Make `pma`'s tests hermetic.** The `git` helpers and `Env::command` in `tests/cli.rs`, and the git calls in `agent.rs` tests, should remove `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_*`, `GIT_CONFIG_VALUE_*` and `GIT_CONFIG_PARAMETERS`. A test suite should not depend on ambient git configuration. Tests only.

2. **Add a verify preflight: `pma verify <project>`.** Run the project's verify in a fresh worktree of `origin`, under the agent's environment, and record the result in the base cache. Run it once per cohort project before any dispatch. It would have caught this fault at no cost. It also replaces the hand measurements in `pilot.md` with ones taken where it matters.

3. **Reject run #1 as an infrastructure failure, then redispatch.** `pma review 1 --reject --minutes N`, logged in `pilot.md`. Rejection consumes one of the task's two attempts (REVIEW.md, D4), so redispatch with `--retry`. Report the run as infrastructure, not as an agent result.

4. **Name the model.** `pma preset set pilot claude <model>`, then `pma preset use pilot`, so every pilot run records the model it ran.

5. **Decide the push block for local paths.** The block also stops pushes to local paths. Any project whose tests push to fixture repositories will fail verify the same way. Two options:
   - Keep it strict and make such test suites hermetic. This is the recommended option.
   - Narrow the block to network URLs, which lets an agent push into local clones.

6. **Leave the allowlist as it is for the pilot.** Record denials per run. If they cluster on narrower forms of the verify command, such as a single test, consider an allow rule for that prefix.

## Open

- Does detail predict success? One run cannot say. The difficulty estimate (1) and pma's complexity estimate (3) disagree; the pilot will show which tracks outcome.
- Would the run have passed verify on a working base? Probably: the 205 unit tests passed at the head, including the new cases. It is still unverified until the redispatch.
