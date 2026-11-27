//! The fix-CI loop. pma pushes a headless fix-CI run to a pull request, waits
//! for its checks, and sends the run back to its agent while they fail, up to
//! `ci_loop.max` rounds. The agent never pushes, and pma never merges.
//! Design: docs/dev/ci-loop.md.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::config::Config;
use crate::dispatch;
use crate::publish;
use crate::store::{self, Result, Run, RunState, Session, Store};

/// Who approves a round's change.
pub const BY: &str = "ci-loop";

/// How often a round reads its pull request.
const POLL: Duration = Duration::from_secs(60);

/// A pull request with no check by then gets none: CI does not run on it.
const NO_CHECKS: Duration = Duration::from_secs(600);

/// Failed reads in a row a wait tolerates, as a network blip.
const TRIES: u32 = 3;

/// How long a round waits for the project's lock, held by a scan or a job.
const LOCK_WAIT: Duration = Duration::from_secs(600);

/// Why the clone at `repo` cannot loop; `None` when it can.
pub fn refused(repo: &Path) -> Option<String> {
    let mut on_pr = false;
    let files = std::fs::read_dir(repo.join(".github").join("workflows"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"));
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let events = triggers(&text);
        on_pr |= events.iter().any(|e| e == "pull_request");
        // A round's branch push, and its pull request, start these.
        let reached: Vec<&str> = events
            .iter()
            .map(String::as_str)
            .filter(|e| REACHED.contains(e))
            .collect();
        if reached.is_empty() {
            continue;
        }
        if let Some(line) = exposure(&text) {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            return Some(format!(
                "{name} runs on {} with `{line}`, which a round's code could use",
                reached.join(", ")
            ));
        }
    }
    if !on_pr {
        return Some("no workflow runs on pull_request".into());
    }
    let slug = match slug_of(repo) {
        Ok(slug) => slug,
        Err(e) => return Some(e.to_string()),
    };
    let api = format!("repos/{slug}/actions/permissions/workflow");
    match crate::sync::gh(&["api", &api, "--jq", ".default_workflow_permissions"]) {
        Ok(p) if p.trim() == "read" => None,
        Ok(p) => Some(format!(
            "its workflows get a `{}` GITHUB_TOKEN, which a round's code could push with; \
             set Settings > Actions > General > Workflow permissions to read",
            p.trim()
        )),
        Err(e) => Some(format!("its GITHUB_TOKEN permission is unknown: {e}")),
    }
}

/// The events a round can start a workflow by: its branch push, the branch's
/// creation, its pull request, and a workflow run after any of them. Branch
/// filters are not read, so every `push` counts.
const REACHED: [&str; 4] = ["push", "create", "pull_request", "workflow_run"];

/// The events a workflow file's top-level `on:` names, by its text.
fn triggers(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.split('#').next().unwrap_or_default().trim_end())
        .collect();
    fn key(l: &str) -> Option<&str> {
        ["on:", "\"on\":", "'on':", "true:"]
            .iter()
            .find_map(|k| l.strip_prefix(k))
            .map(str::trim)
    }
    let Some(at) = lines.iter().position(|l| key(l).is_some()) else {
        return Vec::new();
    };
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|w| !w.is_empty())
            .map(String::from)
            .collect()
    };
    let inline = key(lines[at]).unwrap_or_default();
    if !inline.is_empty() {
        // `on: push`, `on: [push, pull_request]`, or a flow map; only its
        // keys name events, but every word is kept, which over-counts.
        return words(inline);
    }
    let block: Vec<&str> = lines[at + 1..]
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .take_while(|l| l.starts_with(' ') || l.starts_with('\t'))
        .collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let Some(first) = block.first().map(|l| indent(l)) else {
        return Vec::new();
    };
    block
        .iter()
        .filter(|l| indent(l) == first)
        .filter_map(|l| {
            let l = l.trim().trim_start_matches("- ");
            let name = l.split(':').next()?.trim().trim_matches(['"', '\'']);
            Some(name.to_string()).filter(|n| !n.is_empty())
        })
        .collect()
}

/// The first line of a workflow that gives its jobs a secret or a token that
/// can write, by its text with quotes dropped. The repository default does
/// not cap a workflow's own `permissions:`.
fn exposure(text: &str) -> Option<&str> {
    text.lines()
        .find(|raw| {
            let l: String = raw
                .split('#')
                .next()
                .unwrap_or_default()
                .chars()
                .filter(|c| !matches!(c, '"' | '\''))
                .collect();
            let l = l.trim();
            // Inside an expression, `secrets` names a secret unless it is
            // `secrets.GITHUB_TOKEN`: `secrets.X`, `secrets[X]`, `toJSON(secrets)`.
            let secret = l.contains("${{")
                && l.match_indices("secrets").any(|(i, m)| {
                    let rest = &l[i + m.len()..];
                    !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                        && !rest.starts_with(".GITHUB_TOKEN")
                });
            let write = l.contains("write-all")
                || l.match_indices(": write").any(|(i, m)| {
                    l[i + m.len()..]
                        .chars()
                        .next()
                        .is_none_or(|c| matches!(c, ' ' | ',' | '}'))
                });
            let inherit = l.starts_with("secrets:") && l.ends_with("inherit");
            secret || write || inherit
        })
        .map(str::trim)
}

fn slug_of(repo: &Path) -> Result<String> {
    let url = dispatch::git(repo, &["remote", "get-url", "origin"])?;
    Ok(crate::scan::github_slug(url.trim()).ok_or("origin is not on GitHub")?)
}

/// What a round's change does that the loop leaves to a person: it edits CI
/// itself, deletes a test, or skips one. A checks pass then proves nothing.
/// It reads the tree that would be published, new files included, with a
/// rename read as a deletion.
fn guards(run: &Run) -> Vec<String> {
    let tree = match dispatch::tree(run) {
        Ok(t) => t,
        Err(e) => return vec![format!("its tree is unreadable: {e}")],
    };
    let read = |args: &[&str]| {
        let mut all = vec!["diff", "--no-renames"];
        all.extend(args);
        all.extend([run.base.as_str(), tree.as_str()]);
        dispatch::wt_git(run, &all).map_err(|e| e.to_string())
    };
    let mut out = Vec::new();
    match read(&["--name-status"]) {
        Ok(names) => {
            let entries: Vec<(&str, &str)> =
                names.lines().filter_map(|l| l.split_once('\t')).collect();
            let ci: Vec<&str> = entries
                .iter()
                .map(|(_, p)| *p)
                .filter(|p| p.starts_with(".github/"))
                .collect();
            if !ci.is_empty() {
                out.push(format!("it edits CI: {}", ci.join(" ")));
            }
            let tests: Vec<&str> = entries
                .iter()
                .filter(|(s, p)| *s == "D" && is_test(p))
                .map(|(_, p)| *p)
                .collect();
            if !tests.is_empty() {
                out.push(format!("it deletes tests: {}", tests.join(" ")));
            }
        }
        Err(e) => out.push(format!("its changed files are unknown: {e}")),
    }
    match read(&["-U0"]) {
        Ok(diff) => out.extend(skip(&diff).map(|l| format!("it skips a test: {}", l.trim()))),
        Err(e) => out.push(format!("its diff is unreadable: {e}")),
    }
    out
}

/// Whether `path` names a test file, by the usual conventions.
fn is_test(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    let mut parts: Vec<&str> = path.split('/').collect();
    let file = parts.pop().unwrap_or_default();
    let stem = file.split('.').next().unwrap_or_default();
    parts
        .iter()
        .any(|d| matches!(*d, "test" | "tests" | "spec" | "__tests__"))
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || file.contains(".test.")
        || file.contains(".spec.")
}

/// The first added line that skips a test or lets a step fail.
fn skip(diff: &str) -> Option<&str> {
    const MARKS: [&str; 11] = [
        "#[ignore",
        "@pytest.mark.skip",
        "@pytest.mark.xfail",
        "pytest.skip(",
        "unittest.skip",
        "it.skip(",
        "test.skip(",
        "describe.skip(",
        "t.Skip(",
        "@Disabled",
        "continue-on-error",
    ];
    let mut file = "";
    for l in diff.lines() {
        if let Some(p) = l.strip_prefix("+++ ") {
            file = p.strip_prefix("b/").unwrap_or(p);
            continue;
        }
        let Some(added) = l.strip_prefix('+') else {
            continue;
        };
        // `xit(` only as a word: `exit(` is not a skipped test.
        let xit = added.match_indices("xit(").any(|(i, _)| {
            !added[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        });
        let masked = added.contains("|| true") && runs_tests(file);
        if xit || masked || MARKS.iter().any(|m| added.contains(m)) {
            return Some(l);
        }
    }
    None
}

/// Whether `path` names a file that runs the tests, where `|| true` hides a
/// failure. Elsewhere it is an ordinary shell idiom.
fn runs_tests(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or_default();
    matches!(
        file,
        "Makefile"
            | "makefile"
            | "GNUmakefile"
            | "justfile"
            | "Justfile"
            | "package.json"
            | "tox.ini"
            | "noxfile.py"
    ) || file.ends_with(".mk")
        || file.to_ascii_lowercase().contains("test")
}

/// What a round's wait ended on.
#[derive(Debug, PartialEq)]
enum Checked {
    Passed,
    /// Each failed check's workflow and link.
    Failed(Vec<(String, String)>),
    NoChecks,
    TimedOut,
    /// Someone else pushed to the branch: the head named.
    Moved(String),
    /// The run left `pr-open`: a person merged, closed or reviewed it.
    Ended(RunState),
}

enum Poll {
    /// Not decided; whether any check has reported.
    Waiting {
        any: bool,
    },
    Done(Checked),
}

/// Polls until `poll` decides, `limit` passes, or no check reports within
/// `NO_CHECKS`. The first poll is at once; `TRIES` failed polls in a row end
/// it. `elapsed` and `sleep` are the clock, apart so tests need not wait.
fn wait(
    limit: Duration,
    mut poll: impl FnMut() -> Result<Poll>,
    elapsed: impl Fn() -> Duration,
    mut sleep: impl FnMut(),
) -> Result<Checked> {
    let mut failed = 0;
    loop {
        let polled = match poll() {
            Ok(p) => {
                failed = 0;
                p
            }
            Err(e) => {
                failed += 1;
                if failed >= TRIES {
                    return Err(e);
                }
                Poll::Waiting { any: true }
            }
        };
        match polled {
            Poll::Done(c) => return Ok(c),
            Poll::Waiting { any: false } if elapsed() >= NO_CHECKS.min(limit) => {
                return Ok(Checked::NoChecks);
            }
            _ if elapsed() >= limit => return Ok(Checked::TimedOut),
            _ => sleep(),
        }
    }
}

/// What a pull request's head and checks say about the commit pma pushed.
/// A head still at `previous`, the round before's, is GitHub not yet updated.
fn judge(head: &str, pushed: &str, previous: Option<&str>, rollup: &[Value]) -> Poll {
    if previous == Some(head) {
        return Poll::Waiting { any: true };
    }
    if head != pushed {
        return Poll::Done(Checked::Moved(head.into()));
    }
    let c = publish::checks(rollup);
    match (c.total, c.pending.is_empty(), c.failed.is_empty()) {
        (0, ..) => Poll::Waiting { any: false },
        (_, false, _) => Poll::Waiting { any: true },
        (_, true, true) => Poll::Done(Checked::Passed),
        (_, true, false) => Poll::Done(Checked::Failed(publish::failed_checks(rollup))),
    }
}

/// Waits on `run`'s pull request until `deadline`, re-reading the run each time.
fn watch(
    store: &Store,
    run: &Run,
    url: &str,
    (pushed, previous): (&str, Option<&str>),
    deadline: Instant,
) -> Result<Checked> {
    let started = Instant::now();
    wait(
        deadline.saturating_duration_since(started),
        || {
            let now = store.run(run.id)?;
            if now.state != RunState::PrOpen {
                return Ok(Poll::Done(Checked::Ended(now.state)));
            }
            let read =
                crate::sync::gh(&["pr", "view", url, "--json", "headRefOid,statusCheckRollup"])?;
            let pr: Value = serde_json::from_str(&read).map_err(|e| format!("gh pr view: {e}"))?;
            let head = pr["headRefOid"].as_str().unwrap_or_default();
            let rollup = pr["statusCheckRollup"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            Ok(judge(head, pushed, previous, &rollup))
        },
        || started.elapsed(),
        || std::thread::sleep(POLL),
    )
}

/// The Actions run a check links to: `.../actions/runs/<id>/job/<job>`.
fn run_of(url: &str) -> Option<u64> {
    let (_, rest) = url.split_once("/actions/runs/")?;
    rest.split('/').next()?.parse().ok()
}

/// A run's attempt number and whether that attempt has finished.
fn run_attempt(slug: &str, id: u64) -> Result<(u64, bool)> {
    let read = crate::sync::gh(&[
        "run",
        "view",
        &id.to_string(),
        "-R",
        slug,
        "--json",
        "attempt,status",
    ])?;
    let v: Value = serde_json::from_str(&read).map_err(|e| format!("gh run view: {e}"))?;
    Ok((
        v["attempt"].as_u64().unwrap_or_default(),
        v["status"].as_str() == Some("completed"),
    ))
}

/// Re-runs each run's failed jobs once and waits until every new attempt has
/// finished. A new attempt number, not the status, says the re-run started,
/// so an old failure is never read as the new result.
fn rerun(slug: &str, ids: &[u64], deadline: Instant) -> Result<bool> {
    let mut before = Vec::new();
    for id in ids {
        before.push((*id, run_attempt(slug, *id)?.0));
        crate::sync::gh(&["run", "rerun", &id.to_string(), "--failed", "-R", slug])?;
    }
    let started = Instant::now();
    let finished = wait(
        deadline.saturating_duration_since(started),
        || {
            for (id, attempt) in &before {
                let (now, done) = run_attempt(slug, *id)?;
                if now <= *attempt || !done {
                    return Ok(Poll::Waiting { any: true });
                }
            }
            Ok(Poll::Done(Checked::Passed))
        },
        || started.elapsed(),
        || std::thread::sleep(POLL),
    )?;
    Ok(finished == Checked::Passed)
}

/// Takes back the loop's own approval, so a change no person reviewed is not
/// left approved when its publish fails.
fn unapprove(store: &Store, run: &mut Run) -> Result<()> {
    *run = store.run(run.id)?;
    if run.state == RunState::Approved && run.approved_by.as_deref() == Some(BY) {
        run.state = RunState::Ready;
        run.approved_tree = None;
        run.approved_head = None;
        run.approved_by = None;
        store.update_run(run)?;
    }
    Ok(())
}

/// Ends the loop with `why` on the run. `charge` consumes one attempt: the
/// loop ran out of rounds with CI still failing.
fn stop(store: &Store, run: &mut Run, why: String, charge: bool) -> Result<String> {
    *run = store.run(run.id)?;
    run.error = Some(why.clone());
    store.update_run(run)?;
    if charge {
        store.consume_attempt(&run.project, &dispatch::attempt_key(run))?;
    }
    Ok(why)
}

/// Takes a finished fix-CI run round by round until its pull request's checks
/// pass, a gate stops it, or `ci_loop.max` rounds are spent. Each step is
/// reported to `say`. An error is also recorded on the run.
pub fn drive(
    store: &Store,
    home: &Path,
    cfg: &Config,
    run: Run,
    say: &mut dyn FnMut(String),
) -> Result<()> {
    let id = run.id;
    let Err(e) = rounds(store, home, cfg, run, say) else {
        return Ok(());
    };
    let why = format!("CI loop stopped: {e}");
    if let Ok(mut run) = store.run(id) {
        let _ = unapprove(store, &mut run);
        let _ = stop(store, &mut run, why.clone(), false);
    }
    Err(why.into())
}

/// The project's lock is held while a round publishes or works, and released
/// while it waits on CI.
fn rounds(
    store: &Store,
    home: &Path,
    cfg: &Config,
    mut run: Run,
    say: &mut dyn FnMut(String),
) -> Result<()> {
    let max = cfg.ci_loop_max.max(1);
    let limit = Duration::from_secs(cfg.ci_loop_wait.max(1) as u64 * 60);
    let locks = store::home()?;
    let slug = slug_of(&run.repo)?;
    let project = [run.project.clone()];
    let lock = || {
        let started = Instant::now();
        loop {
            match Session::acquire_projects(&locks, &project) {
                Ok(held) => return Ok(held),
                Err(e) if started.elapsed() >= LOCK_WAIT => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_secs(5)),
            }
        }
    };
    for round in 1..=max {
        if run.state != RunState::Ready {
            say(format!(
                "round {round}: run #{} is {}; the loop stopped",
                run.id,
                run.state.name()
            ));
            return Ok(());
        }
        let mut reasons = crate::accept::loop_reasons(&run);
        if run.approval == Some(crate::route::Approval::Propose) {
            reasons.push(format!(
                "its route `{}` is `propose`",
                run.route.as_deref().unwrap_or("-")
            ));
        }
        reasons.extend(guards(&run));
        let tip = format!("{}^{{tree}}", dispatch::pr_tip_ref(&run));
        if round > 1 && dispatch::tree(&run)? == dispatch::wt_git(&run, &["rev-parse", &tip])? {
            reasons.push("the round changed nothing".into());
        }
        if !reasons.is_empty() {
            say(format!(
                "round {round}: waits in `pma review`: {}",
                reasons.join("; ")
            ));
            return Ok(());
        }
        let previous = run.pr_head.clone();
        {
            let _held = lock()?;
            dispatch::approve(store, &mut run, BY)?;
            let mut said = Err(String::new());
            let target = publish::Target::Pr;
            publish::publish(store, home, cfg, vec![run.clone()], target, None, |_, o| {
                said = o.clone();
            })?;
            match said {
                Ok(s) => say(format!("round {round}: {s}")),
                Err(e) => {
                    unapprove(store, &mut run)?;
                    say(stop(
                        store,
                        &mut run,
                        format!("round {round}: not published: {e}"),
                        false,
                    )?);
                    return Ok(());
                }
            }
        }
        run = store.run(run.id)?;
        let (Some(url), Some(pushed)) = (run.outcome.clone(), run.pr_head.clone()) else {
            return Err(format!("run #{} published with no pull request recorded", run.id).into());
        };
        // One deadline for the round: the watch, the re-run and its watch.
        let deadline = Instant::now() + limit;
        let heads = (pushed.as_str(), previous.as_deref());
        let mut checked = watch(store, &run, &url, heads, deadline)?;
        if let Checked::Failed(failed) = &checked {
            let mut ids: Vec<u64> = failed.iter().filter_map(|(_, u)| run_of(u)).collect();
            ids.sort_unstable();
            ids.dedup();
            if !ids.is_empty() {
                say(format!(
                    "round {round}: checks failed; re-running the failed jobs once"
                ));
                checked = match rerun(&slug, &ids, deadline)? {
                    true => watch(store, &run, &url, heads, deadline)?,
                    false => Checked::TimedOut,
                };
            }
        }
        let short = |s: &str| s[..s.len().min(7)].to_string();
        let why = match checked {
            Checked::Passed => {
                say(format!(
                    "CI passed on {url} after {round} round(s); review it, then `pma merge {}`",
                    run.id
                ));
                return Ok(());
            }
            Checked::Ended(state) => {
                say(format!(
                    "run #{} is {}; the loop stopped",
                    run.id,
                    state.name()
                ));
                return Ok(());
            }
            Checked::Failed(failed) => {
                let mut workflows: Vec<String> = failed.iter().map(|(w, _)| w.clone()).collect();
                workflows.sort();
                workflows.dedup();
                if round == max {
                    let why = format!(
                        "CI still failing after {max} round(s): {}: {url}",
                        workflows.join(", ")
                    );
                    say(stop(store, &mut run, why, true)?);
                    return Ok(());
                }
                let runs: Vec<(String, String)> = failed
                    .iter()
                    .filter_map(|(w, u)| {
                        let id = run_of(u)?;
                        Some((
                            w.clone(),
                            format!("https://github.com/{slug}/actions/runs/{id}"),
                        ))
                    })
                    .collect();
                let logs = dispatch::failed_ci_logs(&run.repo, &workflows, &runs)?;
                say(format!(
                    "round {round}: CI still fails: {}; round {} starts",
                    workflows.join(", "),
                    round + 1
                ));
                let _held = lock()?;
                let (outcome, head) = (run.outcome.clone(), run.pr_head.clone());
                dispatch::fix_checks(store, home, cfg, &mut run, &logs)?;
                if run.state != RunState::Ready {
                    // The pull request still holds this round's change; the
                    // run goes back to it, so `pma merge` and settling see it.
                    let why = format!(
                        "round {} ended {}: {}; the pull request keeps round {round}'s change",
                        round + 1,
                        run.state.name(),
                        run.error.clone().unwrap_or_default()
                    );
                    run.state = RunState::PrOpen;
                    (run.outcome, run.pr_head) = (outcome, head);
                    store.update_run(&run)?;
                    say(stop(store, &mut run, why, false)?);
                    return Ok(());
                }
                continue;
            }
            Checked::NoChecks => format!(
                "no check reported on {url} within {} minutes; CI may not run on pull requests",
                NO_CHECKS.min(limit).as_secs() / 60
            ),
            Checked::TimedOut => format!(
                "checks unfinished after {} minutes: {url}",
                cfg.ci_loop_wait
            ),
            Checked::Moved(head) => format!(
                "the pull request's head moved to {}, not pma's {}; the loop stopped",
                short(&head),
                short(&pushed)
            ),
        };
        say(stop(store, &mut run, why, false)?);
        return Ok(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_workflow_s_events_are_read_from_its_on_key() {
        let events = |t: &str| triggers(t);
        assert_eq!(
            events("name: ci\non:\n  push:\n    branches: [main]\n  pull_request:\njobs:\n  t:\n"),
            ["push", "pull_request"]
        );
        assert_eq!(
            events("on: [push, pull_request]\n"),
            ["push", "pull_request"]
        );
        assert_eq!(events("on: push\n"), ["push"]);
        assert_eq!(
            events("on:\n  - push\n  - workflow_dispatch\n"),
            ["push", "workflow_dispatch"]
        );
        assert_eq!(
            events("on:\n  pull_request_target:\n"),
            ["pull_request_target"]
        );
        assert_eq!(events("on:\n  push:\n  # pull_request:\n"), ["push"]);
        // An `if:` names an event without triggering on it.
        let only_push = "on: push\njobs:\n  t:\n    if: github.event_name == 'pull_request'\n";
        assert_eq!(events(only_push), ["push"]);
        assert!(events("jobs:\n  t:\n").is_empty());
    }

    #[test]
    fn a_workflow_that_exposes_a_secret_or_a_write_token_is_found() {
        let ci = "on: pull_request\njobs:\n  t:\n    steps:\n      - run: make test\n";
        assert_eq!(exposure(ci), None);
        let token = "        env:\n          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n";
        assert_eq!(exposure(&format!("{ci}{token}")), None);
        for (line, found) in [
            ("permissions:\n  contents: write\n", "contents: write"),
            (
                "permissions:\n  contents: \"write\"\n",
                "contents: \"write\"",
            ),
            (
                "permissions: {contents: write}\n",
                "permissions: {contents: write}",
            ),
            ("permissions: write-all\n", "permissions: write-all"),
            (
                "    env:\n      KEY: ${{ secrets.PYPI }}\n",
                "KEY: ${{ secrets.PYPI }}",
            ),
            (
                "    env:\n      KEY: ${{ secrets['PYPI'] }}\n",
                "KEY: ${{ secrets['PYPI'] }}",
            ),
            (
                "    env:\n      ALL: ${{ toJSON(secrets) }}\n",
                "ALL: ${{ toJSON(secrets) }}",
            ),
            ("    secrets: inherit\n", "secrets: inherit"),
        ] {
            assert_eq!(exposure(&format!("{ci}{line}")), Some(found), "{line}");
        }
        assert_eq!(
            exposure("permissions:\n  contents: read # not write\n"),
            None
        );
        assert_eq!(exposure("      - name: check no secrets leak\n"), None);
    }

    #[test]
    fn tests_and_skips_are_recognised() {
        for p in [
            "tests/test_a.py",
            "src/a_test.go",
            "web/a.test.ts",
            "test/x.c",
            "pkg/__tests__/b.js",
        ] {
            assert!(is_test(p), "{p}");
        }
        for p in ["src/lib.rs", "contest.py", "latest/notes.md"] {
            assert!(!is_test(p), "{p}");
        }
        let diff = "+++ b/t.py\n+x = 1\n+@pytest.mark.skip(reason='flaky')\n-#[ignore]\n";
        assert_eq!(skip(diff), Some("+@pytest.mark.skip(reason='flaky')"));
        assert_eq!(skip("+let n = v.iter().skip(1);\n-#[ignore]\n"), None);
        assert_eq!(skip("+    sys.exit(main())\n+  process.exit(1);\n"), None);
        assert_eq!(
            skip("+  xit('parses', () => {});\n"),
            Some("+  xit('parses', () => {});")
        );
        assert_eq!(skip("+++ b/run.sh\n+rm -f out || true\n"), None);
        assert_eq!(
            skip("+++ b/Makefile\n+\tcargo test || true\n"),
            Some("+\tcargo test || true")
        );
    }

    #[test]
    fn a_check_link_names_its_run() {
        assert_eq!(
            run_of("https://github.com/o/r/actions/runs/42/job/99"),
            Some(42)
        );
        assert_eq!(run_of("https://github.com/o/r/actions/runs/42"), Some(42));
        assert_eq!(run_of("https://ci.example.com/build/7"), None);
    }

    #[test]
    fn checks_on_the_pushed_head_decide_a_round() {
        let check = |c: &str| {
            json!({"name":"test","workflowName":"ci","conclusion":c,
                   "detailsUrl":"https://github.com/o/r/actions/runs/5/job/6"})
        };
        let pending = json!({"name":"lint","status":"IN_PROGRESS","conclusion":""});
        let decided = |p: Poll| match p {
            Poll::Done(c) => Some(c),
            Poll::Waiting { .. } => None,
        };
        assert_eq!(
            decided(judge("a", "a", None, &[check("SUCCESS")])),
            Some(Checked::Passed)
        );
        assert_eq!(
            decided(judge("a", "a", None, &[check("FAILURE")])),
            Some(Checked::Failed(vec![(
                "ci".into(),
                "https://github.com/o/r/actions/runs/5/job/6".into()
            )]))
        );
        assert_eq!(
            decided(judge("a", "a", None, &[check("FAILURE"), pending])),
            None
        );
        assert!(matches!(
            judge("a", "a", None, &[]),
            Poll::Waiting { any: false }
        ));
        assert_eq!(
            decided(judge("b", "a", None, &[check("SUCCESS")])),
            Some(Checked::Moved("b".into()))
        );
        // Just after a force-push GitHub may still name the old head.
        assert_eq!(
            decided(judge("b", "a", Some("b"), &[check("FAILURE")])),
            None
        );
    }

    /// The first poll is at once; with no check by `NO_CHECKS` the wait ends,
    /// and with checks still going it ends at `limit`.
    #[test]
    fn a_wait_ends_on_a_verdict_silence_or_its_limit() {
        use std::cell::Cell;
        let run = |polls: Vec<Poll>, limit: u64| {
            let clock = Cell::new(Duration::ZERO);
            let mut polls = polls.into_iter();
            let mut sleeps = 0;
            let got = wait(
                Duration::from_secs(limit),
                || Ok(polls.next().unwrap_or(Poll::Waiting { any: true })),
                || clock.get(),
                || {
                    sleeps += 1;
                    clock.set(clock.get() + POLL);
                },
            )
            .unwrap();
            (got, sleeps)
        };
        assert_eq!(
            run(vec![Poll::Done(Checked::Passed)], 1800),
            (Checked::Passed, 0)
        );
        let silent = (0..20).map(|_| Poll::Waiting { any: false }).collect();
        assert_eq!(run(silent, 1800), (Checked::NoChecks, 10));
        assert_eq!(run(vec![], 300), (Checked::TimedOut, 5));
    }

    /// A failed poll is retried; `TRIES` in a row end the wait.
    #[test]
    fn a_failed_read_is_retried() {
        let polls = std::cell::Cell::new(0);
        let flaky = |fail: u32| {
            polls.set(0);
            wait(
                Duration::from_secs(1800),
                || {
                    polls.set(polls.get() + 1);
                    match polls.get() <= fail {
                        true => Err("gh: network".into()),
                        false => Ok(Poll::Done(Checked::Passed)),
                    }
                },
                || Duration::ZERO,
                || {},
            )
        };
        assert_eq!(flaky(TRIES - 1).unwrap(), Checked::Passed);
        assert!(flaky(TRIES).is_err());
    }
}
