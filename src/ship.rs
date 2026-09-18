//! Ship: commit approved runs, publish them, and remove their worktrees. A
//! run published as a pull request stays `pr-open`, holding its task, until
//! the pull request is merged or closed.

use std::process::Command;

use crate::config::{Attribution, Config, Publish};
use crate::dispatch::{git, remove_worktree};
use crate::store::{Result, Run, RunState, Store};
use crate::todo;

/// Ships approved runs in id order, project by project. A failure stops the
/// rest of that project and leaves its runs approved; running `ship` again
/// resumes them. `done` sees each run with its outcome.
pub fn ship(
    store: &Store,
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
        match ship_one(cfg, &run) {
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

fn ship_one(cfg: &Config, run: &Run) -> Result<String> {
    let wt = &run.worktree;
    if !wt.is_dir() {
        return Err(format!("{} no longer exists", wt.display()).into());
    }
    let publish = cfg.publish_for(&run.project);
    if publish == Publish::Pr && which("gh").is_none() {
        return Err("publish = pr needs `gh` on PATH; or `pma config publish push`".into());
    }
    let message = message(cfg, run);

    git(wt, &["add", "--all"])?;
    let staged = git(wt, &["diff", "--cached", "--quiet"]).is_err();
    if staged {
        git(wt, &["commit", "--quiet", "-m", &message])?;
    }
    let upstream = format!("origin/{}", run.default_branch);
    if publish == Publish::Push {
        git(wt, &["fetch", "--quiet", "origin"])?;
        // An earlier ship pushed these commits and stopped before recording
        // it. Pushed commits keep their ids, so HEAD is in the upstream.
        let head = git(wt, &["rev-parse", "HEAD"])?;
        if head != run.base && git(wt, &["merge-base", "--is-ancestor", "HEAD", &upstream]).is_ok()
        {
            let sha = git(wt, &["rev-parse", "--short", "HEAD"])?;
            return Ok(format!("already pushed {sha} to {}", run.default_branch));
        }
        if let Err(e) = git(wt, &["rebase", "--quiet", &upstream]) {
            let _ = git(wt, &["rebase", "--abort"]);
            return Err(format!(
                "rebase onto {upstream} failed; resolve it in {}: {e}",
                wt.display()
            )
            .into());
        }
    }

    // After the rebase, so ticks on nearby lines by tasks shipped together do
    // not conflict.
    if !crate::dispatch::is_signal(&run.task_key) {
        let path = wt.join("TODO.md");
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(edited) = todo::mark_done(&text, &run.task_key, &run.text) {
            std::fs::write(&path, edited).map_err(|e| format!("{}: {e}", path.display()))?;
            git(wt, &["add", "TODO.md"])?;
            if staged {
                git(wt, &["commit", "--quiet", "--amend", "--no-edit"])?;
            } else {
                git(wt, &["commit", "--quiet", "-m", &message])?;
            }
        }
    }
    if git(wt, &["rev-list", "--count", &format!("{upstream}..HEAD")])? == "0" {
        return Err("nothing to ship: no changes".into());
    }

    let outcome = match publish {
        Publish::Push => {
            git(
                wt,
                &[
                    "push",
                    "--quiet",
                    "origin",
                    &format!("HEAD:refs/heads/{}", run.default_branch),
                ],
            )?;
            let sha = git(wt, &["rev-parse", "--short", "HEAD"])?;
            format!("pushed {sha} to {}", run.default_branch)
        }
        Publish::Pr => {
            git(wt, &["push", "--quiet", "-u", "origin", &run.branch])?;
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
    if cfg.attribution == Attribution::CoAuthor && run.agent == crate::agent::AGENT {
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
