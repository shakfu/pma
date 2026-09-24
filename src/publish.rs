//! Publishing approved runs, one of two ways, each its own command:
//! `pma push` commits a run and pushes it to the default branch, and `pma pr`
//! commits it and opens a pull request. Both remove the worktree afterwards. A
//! run with a pull request stays `pr-open`, holding its task, until the pull
//! request is merged or closed.

use std::process::Command;

use crate::config::{Attribution, Config};
use crate::dispatch::{remove_worktree, wt_git};
use crate::store::{Result, Run, RunState, Store};
use crate::todo;

/// Where a run's commit goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Rebased onto the remote default branch and pushed to it.
    Push,
    /// Pushed as the run's own branch, with a pull request opened for it.
    Pr,
}

/// Publishes approved runs in id order, project by project. A failure stops
/// the rest of that project and leaves its runs approved; running the command
/// again resumes them. `done` sees each run with its outcome.
pub fn publish(
    store: &Store,
    home: &std::path::Path,
    cfg: &Config,
    runs: Vec<Run>,
    target: Target,
    mut done: impl FnMut(&Run, &std::result::Result<String, String>),
) -> Result<()> {
    let mut blocked: Vec<String> = Vec::new();
    for mut run in runs {
        if blocked.contains(&run.project) {
            done(
                &run,
                &Err("skipped: an earlier run in this project failed".into()),
            );
            continue;
        }
        match publish_one(home, cfg, &run, target) {
            Ok(outcome) => {
                run.enter(match target {
                    Target::Push => RunState::Pushed,
                    Target::Pr => RunState::PrOpen,
                });
                run.outcome = Some(outcome.clone());
                run.error = None;
                // Saved before cleanup, so a cleanup failure cannot hide
                // what was published.
                store.update_run(&run)?;
                let report = match remove_worktree(&run.repo, &run.worktree, &run.branch) {
                    Ok(()) => outcome,
                    Err(e) => {
                        run.error = Some(format!("cleanup: {e}"));
                        store.update_run(&run)?;
                        format!("{outcome}; warning: worktree not removed: {e}")
                    }
                };
                done(&run, &Ok(report));
            }
            Err(e) => {
                run.error = Some(format!("publish: {e}"));
                store.update_run(&run)?;
                blocked.push(run.project.clone());
                done(&run, &Err(e.to_string()));
            }
        }
    }
    Ok(())
}

fn publish_one(home: &std::path::Path, cfg: &Config, run: &Run, target: Target) -> Result<String> {
    let wt = &run.worktree;
    if !wt.is_dir() {
        return Err(format!("{} no longer exists", wt.display()).into());
    }
    unchanged_git(run)?;
    // What was approved is what gets published, or nothing is. Once a publish
    // has committed, the head has moved and the content is its own; a resumed
    // publish is recognised further down by its pushed commits.
    let head = wt_git(run, &["rev-parse", "HEAD"]).ok();
    if let Some(approved) = &run.approved_tree
        && head == run.approved_head
    {
        let now = crate::dispatch::tree(run)?;
        if &now != approved {
            return Err(format!(
                "the worktree changed after it was approved by {}; read \
                 `pma review {}` and approve it again, or reject it",
                run.approved_by.as_deref().unwrap_or("?"),
                run.id
            )
            .into());
        }
    }
    if target == Target::Pr && which("gh").is_none() {
        return Err("`pma pr` needs `gh` on PATH; `pma push` pushes without it".into());
    }
    let message = message(cfg, run);

    wt_git(run, &["add", "--all"])?;
    let staged = wt_git(run, &["diff", "--cached", "--quiet"]).is_err();
    if staged {
        wt_git(run, &["commit", "--quiet", "-m", &message])?;
    }
    let upstream = format!("origin/{}", run.default_branch);
    if target == Target::Push {
        wt_git(run, &["fetch", "--quiet", "origin"])?;
        // An earlier push pushed these commits and stopped before recording
        // it. Pushed commits keep their ids, so HEAD is in the upstream.
        let head = wt_git(run, &["rev-parse", "HEAD"])?;
        if head != run.base
            && wt_git(run, &["merge-base", "--is-ancestor", "HEAD", &upstream]).is_ok()
        {
            let sha = wt_git(run, &["rev-parse", "--short", "HEAD"])?;
            return Ok(format!("already pushed {sha} to {}", run.default_branch));
        }
        if let Err(e) = wt_git(run, &["rebase", "--quiet", &upstream]) {
            let _ = wt_git(run, &["rebase", "--abort"]);
            return Err(format!(
                "rebase onto {upstream} failed; resolve it in {}: {e}",
                wt.display()
            )
            .into());
        }
    }

    // After the rebase, so ticks on nearby lines by tasks published together
    // do not conflict.
    if !crate::dispatch::without_item(&run.task_key) {
        let path = wt.join("TODO.md");
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(edited) = todo::mark_done(&text, &run.task_key, &run.text) {
            std::fs::write(&path, edited).map_err(|e| format!("{}: {e}", path.display()))?;
            wt_git(run, &["add", "TODO.md"])?;
            if staged {
                wt_git(run, &["commit", "--quiet", "--amend", "--no-edit"])?;
            } else {
                wt_git(run, &["commit", "--quiet", "-m", &message])?;
            }
        }
    }
    if wt_git(run, &["rev-list", "--count", &format!("{upstream}..HEAD")])? == "0" {
        return Err("nothing to publish: no changes".into());
    }
    // The rebase merged other work into this tree, and the tick above added
    // a line. Two changes that each pass against the same base can fail
    // together, and a clean rebase is not a semantic one.
    if let Some(command) = &run.verify {
        let timeout = std::time::Duration::from_secs(cfg.timeout as u64 * 60);
        let agent_env = home.join("agent-env");
        let log = home
            .join("runs")
            .join(run.id.to_string())
            .join("verify-publish.log");
        let _ = std::fs::create_dir_all(log.parent().unwrap_or(&agent_env));
        match crate::dispatch::verify_once(command, wt, &agent_env, &log, timeout) {
            Ok((Some(true), _)) => {}
            Ok((Some(false), _)) => {
                return Err(format!(
                    "`{command}` failed on the integrated tree; see {}",
                    log.display()
                )
                .into());
            }
            Ok((None, _)) => {
                return Err(format!("`{command}` timed out on the integrated tree").into());
            }
            Err(e) => return Err(format!("`{command}` could not run: {e}").into()),
        }
    }

    // Again, because verify ran code from the tree and could have installed
    // a `pre-push` hook.
    unchanged_git(run)?;
    let outcome = match target {
        Target::Push => {
            wt_git(
                run,
                &[
                    "push",
                    "--quiet",
                    "origin",
                    &format!("HEAD:refs/heads/{}", run.default_branch),
                ],
            )?;
            let sha = wt_git(run, &["rev-parse", "--short", "HEAD"])?;
            format!("pushed {sha} to {}", run.default_branch)
        }
        Target::Pr => {
            wt_git(run, &["push", "--quiet", "-u", "origin", &run.branch])?;
            // A retry after `gh pr create` failed may find the pull request
            // made anyway.
            let open = gh_in(
                wt,
                &[
                    "pr",
                    "list",
                    "--head",
                    &run.branch,
                    "--state",
                    "open",
                    "--json",
                    "url",
                    "--jq",
                    ".[0].url // empty",
                ],
            )?;
            if open.is_empty() {
                let (title, body) = message.split_once("\n\n").unwrap_or((&message, ""));
                gh_in(
                    wt,
                    &[
                        "pr",
                        "create",
                        "--base",
                        &run.default_branch,
                        "--head",
                        &run.branch,
                        "--title",
                        title,
                        "--body",
                        body,
                    ],
                )?
            } else {
                open
            }
        }
    };
    Ok(outcome)
}

/// Refuses a run whose repository gained or changed a hook, or a setting
/// that runs a program or redirects a push, since dispatch. Publishing commits
/// and pushes with the user's credentials, and none of that is in the diff the
/// reviewer read. A run from before the snapshot existed has nothing to
/// compare.
fn unchanged_git(run: &Run) -> Result<()> {
    let Some(then) = &run.git_snapshot else {
        return Ok(());
    };
    let now = crate::dispatch::git_snapshot(run)?;
    if &now == then {
        return Ok(());
    }
    let (then, now): (Vec<&str>, Vec<&str>) = (then.lines().collect(), now.lines().collect());
    let changes: Vec<String> = then
        .iter()
        .filter(|l| !now.contains(l))
        .map(|l| format!("  - {l}"))
        .chain(
            now.iter()
                .filter(|l| !then.contains(l))
                .map(|l| format!("  + {l}")),
        )
        .collect();
    Err(format!(
        "the repository's hooks or git settings changed since run #{} was dispatched:\n{}\n\
         publishing would run them with your credentials. Restore them, or reject the run.",
        run.id,
        changes.join("\n")
    )
    .into())
}

/// Moves each `pr-open` run to `merged` when its pull request is merged, or
/// to `closed` when it is closed without merging. An open pull request keeps
/// the run `pr-open`; `done` still sees it when someone has reviewed or
/// commented on it, as GitHub shows it. `done` also sees each run whose pull
/// request `gh` cannot read, which stays `pr-open`.
pub fn settle(
    store: &Store,
    mut done: impl FnMut(&Run, &std::result::Result<String, String>),
) -> Result<()> {
    for mut run in store.runs()? {
        if run.state != RunState::PrOpen {
            continue;
        }
        let url = run.outcome.clone().unwrap_or_default();
        let read = crate::sync::gh(&[
            "pr",
            "view",
            &url,
            "--json",
            "state,reviewDecision,comments,reviews",
            "--jq",
            "[.state, .reviewDecision, (.comments | length), (.reviews | length)] | @tsv",
        ]);
        let read = match read {
            Ok(s) => s,
            Err(e) => {
                done(&run, &Err(e));
                continue;
            }
        };
        let mut fields = read.trim().split('\t');
        let state = fields.next().unwrap_or_default();
        let decision = fields.next().unwrap_or_default();
        let comments: usize = fields.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let reviews: usize = fields.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let outcome = match state {
            "MERGED" => {
                // `published_at` keeps the time the pull request was opened.
                run.enter(RunState::Merged);
                format!("pull request merged: {url}")
            }
            "CLOSED" => {
                run.enter(RunState::Closed);
                run.error = Some("pull request closed without merging".into());
                format!("pull request closed without merging: {url}")
            }
            _ => {
                if let Some(activity) = activity(decision, comments, reviews) {
                    done(&run, &Ok(format!("pull request open: {activity}: {url}")));
                }
                continue;
            }
        };
        store.update_run(&run)?;
        done(&run, &Ok(outcome));
    }
    Ok(())
}

/// What has happened on an open pull request, in GitHub's terms: its review
/// decision, and how many comments and reviews it has. `None` when nothing
/// has.
fn activity(decision: &str, comments: usize, reviews: usize) -> Option<String> {
    let plural = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    match decision {
        "APPROVED" => parts.push("approved".to_string()),
        "CHANGES_REQUESTED" => parts.push("changes requested".to_string()),
        _ => {}
    }
    if comments > 0 {
        parts.push(plural(comments, "comment"));
    }
    if reviews > 0 {
        parts.push(plural(reviews, "review"));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Runs `gh` in `dir`, so it finds the repository from the git remote.
fn gh_in(dir: &std::path::Path, args: &[&str]) -> Result<String> {
    let out = Command::new("gh")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "gh {}: {}",
            args[..2].join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The task text as subject; `Closes #N` and the co-author trailer as body.
fn message(cfg: &Config, run: &Run) -> String {
    let mut body = Vec::new();
    if let Some(n) = run.gh {
        body.push(format!("Closes #{n}"));
    }
    // Only a worker whose identity `pma` knows gets a trailer. Inventing an
    // address for an unknown one would attribute the commit to nobody.
    if cfg.attribution == Attribution::CoAuthor && run.agent == "claude" {
        body.push("Co-Authored-By: Claude <noreply@anthropic.com>".into());
    }
    if body.is_empty() {
        run.text.clone()
    } else {
        format!("{}\n\n{}", run.text, body.join("\n\n"))
    }
}

fn which(program: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(program))
            .find(|p| p.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_on_an_open_pull_request_reads_as_github_shows_it() {
        assert_eq!(activity("", 0, 0), None);
        assert_eq!(activity("REVIEW_REQUIRED", 0, 0), None);
        assert_eq!(
            activity("CHANGES_REQUESTED", 2, 1).as_deref(),
            Some("changes requested, 2 comments, 1 review")
        );
        assert_eq!(
            activity("APPROVED", 0, 1).as_deref(),
            Some("approved, 1 review")
        );
        assert_eq!(activity("", 1, 0).as_deref(), Some("1 comment"));
    }
}
