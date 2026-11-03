# Infinity: is it useful for pma?

Assessed 2026-11-03 from the [README](https://github.com/hydro-project/infinity) and the [`agent/` directory](https://github.com/hydro-project/infinity/tree/main/agent). The source of Infinity Code was not read.

## What it is

[Infinity](https://github.com/hydro-project/infinity) is "a Rust framework for building massively concurrent agentic systems". Apache-2.0, about 500 stars, no published release on the repository page.

- **Runtime.** Between turns an agent is its conversation history, so a waiting agent "hibernates for free and wakes on the next message". The same code runs in a local process or on AWS Lambda.

- **Reactive Agent Protocol (RAP).** A tool protocol that delivers results and subscription events "whenever they are ready". A compatibility layer runs existing MCP servers unchanged.

- **Infinity Code.** A coding agent on the runtime. It "runs builds and tests in the background", "edits in parallel threads with stacked sandboxes", and shows each result "as a diff to review".

`agent/` holds AWS CDK constructs (Lambda, SQS, Aurora DSQL, DynamoDB), installed as a library with `pnpm add`. No CLI binary, headless mode, JSON output or directory flag appears there.

## Fit

| Use | Fit | Reason |
|-|-|-|
| pma's runtime | No | pma runs agent CLIs as processes, at most `max_parallel` at a time. Its limits are review time and budget, not memory per waiting agent. |
| A pma worker | Unknown, probably no | A worker needs a CLI that takes a prompt and a directory and reports a result (`crates/pma-core/src/worker.rs`). None was found. |
| Design input | Maybe | Stacked sandboxes, below. |

## Stacked sandboxes

Parallel edits stacked as layers could give one diff per item without a worktree per item. Two open pma questions touch this:

- Bundles: the one-diff limit rules out one commit per item (`docs/dev/bundled-dispatch.md`).

- Clone runs: several runs share one clone (`docs/dev/clone-runs.md`).

The README does not say what a stacked sandbox is: an overlay filesystem, git worktrees or something else. The link to pma is inferred from the wording.

Background builds and tests during an edit are close to what `verify_base_alongside` already does.

## Recommendation

Do not adopt it. If parallel edits on one clone become a priority, read Infinity Code's sandbox source first. Recheck for a headless CLI if it publishes a release.
