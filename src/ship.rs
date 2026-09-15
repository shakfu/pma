//! Ship: commit approved runs, publish them, and remove their worktrees.

use std::process::Command;

use crate::config::{Attribution, Config, Publish};
use crate::dispatch::{git, remove_worktree};
use crate::store::{Result, Run, RunState, Store};
use crate::todo;

/// Ships approved runs in id order, project by project. A failure stops the
/// rest of that project and leaves its runs approved. `done` sees each run
/// with its outcome.
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
                run.state = RunState::Shipped;
                run.outcome = Some(outcome.clone());
                run.error = None;
                store.update_run(&run)?;
                done(&run, &Ok(outcome));
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
        if let Err(e) = git(wt, &["rebase", "--quiet", &upstream]) {
            let _ = git(wt, &["rebase", "--abort"]);
            return Err(format!(
                "rebase onto {upstream} failed; resolve it in {}: {e}",
                wt.display()
            )
            .into());
        }
    }

    // After the rebase, so tasks shipped together do not conflict in `## Done`.
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
            let (title, body) = message.split_once("\n\n").unwrap_or((&message, ""));
            let out = Command::new("gh")
                .args([
                    "pr",
                    "create",
                    "--base",
                    &run.default_branch,
                    "--head",
                    &run.branch,
                ])
                .args(["--title", title, "--body", body])
                .current_dir(wt)
                .output()
                .map_err(|e| format!("gh: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "gh pr create: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )
                .into());
            }
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
    };
    remove_worktree(&run.repo, wt, &run.branch)?;
    Ok(outcome)
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
