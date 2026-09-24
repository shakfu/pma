//! Ship: commit approved runs, publish them, and remove their worktrees. A
//! run published as a pull request stays `pr-open`, holding its task, until
//! the pull request is merged or closed.

use std::process::Command;

use crate::config::{Attribution, Config, Publish};
use crate::dispatch::{remove_worktree, wt_git};
use crate::store::{Result, Run, RunState, Store};
use crate::todo;

/// Ships approved runs in id order, project by project. A failure stops the
/// rest of that project and leaves its runs approved; running `ship` again
/// resumes them. `done` sees each run with its outcome.
pub fn ship(
    store: &Store,
    home: &std::path::Path,
    cfg: &Config,
    runs: Vec<Run>,
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
        match ship_one(home, cfg, &run) {
            Ok(outcome) => {
                run.enter(match cfg.publish_for(&run.project) {
                    Publish::Push => RunState::Shipped,
                    Publish::Pr => RunState::PrOpen,
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
                run.error = Some(format!("ship: {e}"));
                store.update_run(&run)?;
                blocked.push(run.project.clone());
                done(&run, &Err(e.to_string()));
            }
        }
    }
    Ok(())
}

fn ship_one(home: &std::path::Path, cfg: &Config, run: &Run) -> Result<String> {
    let wt = &run.worktree;
    if !wt.is_dir() {
        return Err(format!("{} no longer exists", wt.display()).into());
    }
    unchanged_git(run)?;
    // What was approved is what gets published, or nothing is. Once ship has
    // committed, the head has moved and the content is its own; a resumed
    // ship is recognised further down by its pushed commits.
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
    let publish = cfg.publish_for(&run.project);
    if publish == Publish::Pr && which("gh").is_none() {
        return Err("publish = pr needs `gh` on PATH; or `pma config publish push`".into());
    }
    let message = message(cfg, run);

    wt_git(run, &["add", "--all"])?;
    let staged = wt_git(run, &["diff", "--cached", "--quiet"]).is_err();
    if staged {
        wt_git(run, &["commit", "--quiet", "-m", &message])?;
    }
    let upstream = format!("origin/{}", run.default_branch);
    if publish == Publish::Push {
        wt_git(run, &["fetch", "--quiet", "origin"])?;
        // An earlier ship pushed these commits and stopped before recording
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

    // After the rebase, so ticks on nearby lines by tasks shipped together do
    // not conflict.
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
        return Err("nothing to ship: no changes".into());
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
            .join("verify-ship.log");
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
    let outcome = match publish {
        Publish::Push => {
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
        Publish::Pr => {
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
/// that runs a program or redirects a push, since dispatch. Ship commits and
/// pushes with the user's credentials, and none of that is in the diff the
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
         ship would run them with your credentials. Restore them, or reject the run.",
        run.id,
        changes.join("\n")
    )
    .into())
}

/// Moves each `pr-open` run to `shipped` when its pull request is merged, or
/// to `rejected` when it is closed unmerged. `done` sees each settled run, and
/// each run whose pull request `gh` cannot read, which stays `pr-open`.
pub fn settle(
    store: &Store,
    mut done: impl FnMut(&Run, &std::result::Result<String, String>),
) -> Result<()> {
    for mut run in store.runs()? {
        if run.state != RunState::PrOpen {
            continue;
        }
        let url = run.outcome.clone().unwrap_or_default();
        let state =
            match crate::sync::gh(&["pr", "view", &url, "--json", "state", "--jq", ".state"]) {
                Ok(s) => s,
                Err(e) => {
                    done(&run, &Err(e));
                    continue;
                }
            };
        let outcome = match state.trim() {
            "MERGED" => {
                // `published_at` keeps the time the pull request was opened.
                run.enter(RunState::Shipped);
                format!("pull request merged: {url}")
            }
            "CLOSED" => {
                run.enter(RunState::Rejected);
                run.error = Some("pull request closed without merging".into());
                format!("pull request closed without merging: {url}")
            }
            _ => continue,
        };
        store.update_run(&run)?;
        done(&run, &Ok(outcome));
    }
    Ok(())
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
