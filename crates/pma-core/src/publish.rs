//! Publishing runs. `pma stage`, `pma commit` and `pma push` act in the
//! user's clone, each running the steps before it that the run has not done:
//! the change goes into the index, then a commit, then origin's default
//! branch. `pma pr` commits in the worktree and opens a pull request; a run
//! with one stays `pr-open`, holding its task, until it is merged or closed.
//! `land_local` writes a verified run into its clone as unstaged changes and
//! commits nothing. See docs/dev/stage-commit-push.md.

use std::process::Command;

use crate::config::{Attribution, Config, Workspace};
use crate::dispatch::{git, remove_worktree, wt_git};
use crate::store::{Result, Run, RunState, Store};
use crate::todo;

/// How far a run goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Into the clone's index.
    Stage,
    /// Staged, then committed in the clone.
    Commit,
    /// Committed, then pushed to the remote default branch.
    Push,
    /// Pushed as the run's own branch, with a pull request opened for it.
    Pr,
}

impl Target {
    pub fn name(self) -> &'static str {
        match self {
            Target::Stage => "stage",
            Target::Commit => "commit",
            Target::Push => "push",
            Target::Pr => "pr",
        }
    }

    /// The states a run may start from. Staging is the review, so only it
    /// takes a run no one approved.
    pub fn takes(self, state: RunState) -> bool {
        match self {
            Target::Stage => matches!(state, RunState::Ready | RunState::Approved),
            Target::Commit => matches!(state, RunState::Approved | RunState::Staged),
            Target::Push => matches!(
                state,
                RunState::Approved | RunState::Staged | RunState::Committed
            ),
            Target::Pr => state == RunState::Approved,
        }
    }
}

/// Publishes runs in id order, project by project. A failure stops the rest
/// of that project and leaves each run in the last state it reached; running
/// the command again resumes them. `done` sees each run with its outcome.
/// `subject` replaces the task text as the subject of a commit in the clone.
pub fn publish(
    store: &Store,
    home: &std::path::Path,
    cfg: &Config,
    runs: Vec<Run>,
    target: Target,
    subject: Option<&str>,
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
        if let Some(why) = crate::dispatch::needs_approval(&run) {
            done(&run, &Err(why));
            continue;
        }
        if let Some(why) = keeps_pr(&run, target) {
            done(&run, &Err(why));
            continue;
        }
        let result = match target {
            Target::Pr => open_pr(store, cfg, &mut run),
            _ => to_clone(store, home, cfg, &mut run, target, subject),
        };
        match result {
            Ok(report) => done(&run, &Ok(report)),
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

/// Refuses to publish a conflict fix anywhere but its open pull request.
pub fn keeps_pr(run: &Run, target: Target) -> Option<String> {
    let url = run.fixing_pr().filter(|_| target != Target::Pr)?;
    Some(format!(
        "run #{} fixes the conflicts of {url}; `pma pr {}` updates it, or reject the fix",
        run.id, run.id
    ))
}

fn open_pr(store: &Store, cfg: &Config, run: &mut Run) -> Result<String> {
    if run.workspace == Workspace::Clone {
        return Err(format!(
            "run #{} edited its clone on `{}`; `pma push {}` publishes it there. \
             A pull request needs a worktree run: dispatch with --worktree.",
            run.id, run.branch, run.id
        )
        .into());
    }
    let outcome = publish_one(cfg, run)?;
    run.enter(RunState::PrOpen);
    run.outcome = Some(outcome.clone());
    run.error = None;
    // Saved before cleanup, so a cleanup failure cannot hide what was
    // published.
    store.update_run(run)?;
    Ok(
        match remove_worktree(&run.repo, &run.worktree, &run.branch) {
            Ok(()) => outcome,
            Err(e) => {
                run.error = Some(format!("cleanup: {e}"));
                store.update_run(run)?;
                format!("{outcome}; warning: worktree not removed: {e}")
            }
        },
    )
}

/// Takes a run as far as `target`, saving each state it reaches. Returns the
/// outcome of each step taken, joined.
fn to_clone(
    store: &Store,
    home: &std::path::Path,
    cfg: &Config,
    run: &mut Run,
    target: Target,
    subject: Option<&str>,
) -> Result<String> {
    crate::dispatch::clone_unchanged(store, &run.repo)?;
    let mut said = Vec::new();
    if matches!(run.state, RunState::Staged | RunState::Committed)
        && run.workspace == Workspace::Worktree
        && run.worktree.exists()
    {
        // Staging removed it, or warned that it could not.
        remove_worktree(&run.repo, &run.worktree, &run.branch)?;
    }
    let mut step = |run: &mut Run, state: RunState, outcome: String| -> Result<()> {
        run.enter(state);
        run.outcome = Some(outcome.clone());
        run.error = None;
        store.update_run(run)?;
        said.push(outcome);
        Ok(())
    };
    if matches!(run.state, RunState::Ready | RunState::Approved) {
        let outcome = stage(home, run)?;
        step(run, RunState::Staged, outcome)?;
    }
    if target != Target::Stage && run.state == RunState::Staged {
        let outcome = commit(home, cfg, run, subject)?;
        step(run, RunState::Committed, outcome)?;
    }
    if target == Target::Push && run.state == RunState::Committed {
        let outcome = push(home, run)?;
        step(run, RunState::Pushed, outcome)?;
    }
    Ok(said.join("; "))
}

/// Where `stage` records the tree it staged, which `commit` and `push`
/// compare against.
fn staged_tree_file(home: &std::path::Path, id: i64) -> std::path::PathBuf {
    home.join("runs").join(id.to_string()).join("staged.tree")
}

fn staged_tree(home: &std::path::Path, run: &Run) -> Result<String> {
    let file = staged_tree_file(home, run.id);
    std::fs::read_to_string(&file)
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("{}: {e}; stage the run again", file.display()).into())
}

/// Refuses a clone whose HEAD is not the run's base: the run's check passed
/// against that base, and nothing checks the change against another.
fn at_base(run: &Run) -> Result<()> {
    let head = git(&run.repo, &["rev-parse", "HEAD"])?;
    if head == run.base {
        return Ok(());
    }
    let short = |c: &str| c.get(..7).unwrap_or(c).to_string();
    let behind = git(
        &run.repo,
        &["merge-base", "--is-ancestor", &head, &run.base],
    )
    .is_ok();
    let fix = match behind {
        true => "it is behind; `git pull --ff-only` first".to_string(),
        false => format!(
            "nothing checked the change against that. Publish it with `pma pr {}`, \
             or reject it and dispatch the task again",
            run.id
        ),
    };
    Err(format!(
        "{} is at {}, not run #{}'s base {}: {fix}",
        run.repo.display(),
        short(&head),
        run.id,
        short(&run.base),
    )
    .into())
}

/// Writes the run's change, its task ticked, into the clone's index and
/// working tree, then removes the worktree.
fn stage(home: &std::path::Path, run: &mut Run) -> Result<String> {
    if run.state == RunState::Ready && run.verify_ok == Some(false) {
        return Err(format!(
            "run #{} failed its check; `pma review {}` shows it",
            run.id, run.id
        )
        .into());
    }
    if !run.worktree.is_dir() {
        return Err(format!("{} no longer exists", run.worktree.display()).into());
    }
    let tree = crate::dispatch::tree(run)?;
    if run.state == RunState::Approved && run.approved_tree.as_ref() != Some(&tree) {
        return Err(format!(
            "the worktree changed after it was approved by {}; read `pma review {}` \
             and approve it again, or reject it",
            run.approved_by.as_deref().unwrap_or("?"),
            run.id
        )
        .into());
    }
    // Before the tick, which alone is not a change.
    let from = crate::dispatch::from(run).to_string();
    if tree == wt_git(run, &["rev-parse", &format!("{from}^{{tree}}")])? {
        return Err("nothing to stage: no changes".into());
    }
    if run.workspace == Workspace::Clone {
        return stage_in_clone(home, run, &tree);
    }
    let tree = tick(home, run, &tree)?;
    at_base(run)?;
    if git(&run.repo, &["diff", "--cached", "--quiet"]).is_err() {
        return Err(format!(
            "the index of {} holds staged changes; commit or unstage them first",
            run.repo.display()
        )
        .into());
    }
    let file = write_patch(home, run, &tree)?;
    git(
        &run.repo,
        &[
            "apply",
            "--index",
            "--binary",
            crate::dispatch::path_arg(&file)?,
        ],
    )
    .map_err(|e| format!("{NO_APPLY}{e}; patch: {}", file.display()))?;
    let record = staged_tree_file(home, run.id);
    std::fs::write(&record, &tree).map_err(|e| format!("{}: {e}", record.display()))?;
    let mut outcome = format!(
        "staged in {}; `git diff --cached` shows it",
        run.repo.display()
    );
    let draft = crate::dispatch::changelog_draft(home, run.id);
    if draft.is_file() {
        outcome.push_str(&format!("; changelog entries in {}", draft.display()));
    }
    if let Err(e) = remove_worktree(&run.repo, &run.worktree, &run.branch) {
        outcome.push_str(&format!("; warning: worktree not removed: {e}"));
    }
    Ok(outcome)
}

/// A clone run's change is already in the working tree, so staging is the
/// patch applied to the index alone, from `start` to the reviewed tree. The
/// tick goes into the working tree and the index together, since the file
/// the user sees is the one that will be committed.
fn stage_in_clone(home: &std::path::Path, run: &mut Run, tree: &str) -> Result<String> {
    at_base(run)?;
    if git(&run.repo, &["diff", "--cached", "--quiet"]).is_err() {
        return Err(format!(
            "the index of {} holds staged changes; commit or unstage them first",
            run.repo.display()
        )
        .into());
    }
    let file = write_patch(home, run, tree)?;
    git(
        &run.repo,
        &[
            "apply",
            "--cached",
            "--binary",
            crate::dispatch::path_arg(&file)?,
        ],
    )
    .map_err(|e| {
        format!(
            "{NO_APPLY}{e}; the index differs from the tree the run started from. \
             Stage it yourself with `git add`; the patch: {}",
            file.display()
        )
    })?;
    if !crate::dispatch::closes(run).is_empty() {
        let path = run.repo.join("TODO.md");
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("{}: is a symlink; pma edits only a file", path.display()).into());
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(edited) = ticked(run, &text) {
            todo::save(&path, &edited).map_err(|e| format!("{}: {e}", path.display()))?;
            git(&run.repo, &["add", "--", "TODO.md"])?;
        }
    }
    let staged = git(&run.repo, &["write-tree"])?;
    let record = staged_tree_file(home, run.id);
    std::fs::write(&record, &staged).map_err(|e| format!("{}: {e}", record.display()))?;
    let mut outcome = format!(
        "staged in {}; `git diff --cached` shows it",
        run.repo.display()
    );
    let draft = crate::dispatch::changelog_draft(home, run.id);
    if draft.is_file() {
        outcome.push_str(&format!("; changelog entries in {}", draft.display()));
    }
    Ok(outcome)
}

/// The run's tree with its item marked done in `TODO.md`, built in the
/// worktree's index. The file itself is left alone, so a refused stage leaves
/// the worktree as the reviewer saw it.
fn tick(home: &std::path::Path, run: &Run, tree: &str) -> Result<String> {
    if crate::dispatch::closes(run).is_empty() {
        return Ok(tree.to_string());
    }
    let path = run.worktree.join("TODO.md");
    // The agent controls the worktree, and a symlink would point the read
    // outside it.
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(format!("{}: is a symlink; pma edits only a file", path.display()).into());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let Some(edited) = ticked(run, &text) else {
        return Ok(tree.to_string());
    };
    let dir = home.join("runs").join(run.id.to_string());
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let ticked = dir.join("TODO.md");
    todo::save(&ticked, &edited).map_err(|e| format!("{}: {e}", ticked.display()))?;
    let blob = wt_git(
        run,
        &["hash-object", "-w", crate::dispatch::path_arg(&ticked)?],
    )?;
    wt_git(
        run,
        &[
            "update-index",
            "--cacheinfo",
            &format!("100644,{blob},TODO.md"),
        ],
    )?;
    wt_git(run, &["write-tree"])
}

/// Commits the staged change in the clone, with the user's hooks and
/// identity.
fn commit(
    home: &std::path::Path,
    cfg: &Config,
    run: &Run,
    subject: Option<&str>,
) -> Result<String> {
    crate::dispatch::unchanged_git(run)?;
    at_base(run)?;
    if git(&run.repo, &["write-tree"])? != staged_tree(home, run)? {
        return Err(format!(
            "the index of {} differs from what run #{} staged; commit it yourself \
             with `git commit`",
            run.repo.display(),
            run.id
        )
        .into());
    }
    git(
        &run.repo,
        &["commit", "--quiet", "-m", &message(cfg, run, subject)],
    )?;
    let sha = git(&run.repo, &["rev-parse", "--short", "HEAD"])?;
    Ok(format!("committed {sha} in {}", run.repo.display()))
}

/// Pushes the run's commit, and nothing else, to origin's default branch.
fn push(home: &std::path::Path, run: &Run) -> Result<String> {
    crate::dispatch::unchanged_git(run)?;
    let parent = git(&run.repo, &["rev-parse", "HEAD^"]).unwrap_or_default();
    if parent != run.base
        || git(&run.repo, &["rev-parse", "HEAD^{tree}"])? != staged_tree(home, run)?
    {
        return Err(format!(
            "HEAD in {} is not run #{}'s commit; push it yourself with `git push`",
            run.repo.display(),
            run.id
        )
        .into());
    }
    let sha = git(&run.repo, &["rev-parse", "HEAD"])?;
    git(
        &run.repo,
        &[
            "push",
            "--quiet",
            "origin",
            &format!("{sha}:refs/heads/{}", run.default_branch),
        ],
    )
    .map_err(|e| {
        format!(
            "{e}. origin's {} moved since the run's base; rebase and push yourself",
            run.default_branch
        )
    })?;
    Ok(format!(
        "pushed {} to {}",
        &sha[..7.min(sha.len())],
        run.default_branch
    ))
}

/// The diff from what the run started from to `tree`, in the run's log
/// directory.
pub fn write_patch(home: &std::path::Path, run: &Run, tree: &str) -> Result<std::path::PathBuf> {
    let dir = home.join("runs").join(run.id.to_string());
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = dir.join("change.patch");
    // Written by git rather than through `git`'s trimmed output, which
    // drops a final context line that is a single space. The flags undo
    // settings that make a diff `apply` cannot read.
    let output = format!("--output={}", crate::dispatch::path_arg(&file)?);
    wt_git(
        run,
        &[
            "diff",
            "--binary",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            &output,
            crate::dispatch::from(run),
            tree,
        ],
    )?;
    Ok(file)
}

/// Writes a run that passed verify into its clone as unstaged changes, then
/// removes its worktree; a run that did not pass is rejected. Nothing is
/// staged or committed: the developer stages, commits or discards. Returns
/// what became of it, with what to report. The patch stays in the run's log
/// directory either way.
pub fn land_local(
    store: &Store,
    home: &std::path::Path,
    run: &mut Run,
) -> Result<(Landed, String)> {
    if run.workspace == Workspace::Clone {
        return Err(format!(
            "run #{} edited its clone directly and waits there for review",
            run.id
        )
        .into());
    }
    let passed = run.state == RunState::Ready && run.verify_ok == Some(true) && run.error.is_none();
    let refused = if !passed {
        Some(match (&run.verify, run.verify_ok) {
            (None, _) => format!("`{}` states no verify command", run.project),
            (_, Some(false)) => "verify failed".to_string(),
            _ => run
                .error
                .clone()
                .unwrap_or_else(|| format!("run is {}", run.state.name())),
        })
    } else {
        let tree = match &run.reviewed_tree {
            Some(t) => t.clone(),
            None => crate::dispatch::tree(run)?,
        };
        let file = write_patch(home, run, &tree)?;
        if std::fs::metadata(&file).map_or(0, |m| m.len()) == 0 {
            Some("no changes".to_string())
        } else {
            crate::dispatch::clone_unchanged(store, &run.repo)?;
            // Without `--index`: the working tree only, all or nothing.
            match crate::dispatch::git(
                &run.repo,
                &["apply", "--binary", crate::dispatch::path_arg(&file)?],
            ) {
                Ok(_) => None,
                Err(e) => Some(format!("{NO_APPLY}{e}; patch: {}", file.display())),
            }
        }
    };
    match refused {
        None => {
            remove_worktree(&run.repo, &run.worktree, &run.branch)?;
            run.enter(RunState::Applied);
            let draft = crate::dispatch::changelog_draft(home, run.id);
            run.outcome = Some(match draft.is_file() {
                true => format!(
                    "applied to {} unstaged; changelog entries in {}",
                    run.repo.display(),
                    draft.display()
                ),
                false if crate::dispatch::wants_changelog(run) => format!(
                    "applied to {} unstaged; no changelog entry drafted",
                    run.repo.display()
                ),
                false => format!("applied to {} unstaged", run.repo.display()),
            });
            store.update_run(run)?;
            Ok((Landed::Applied, run.outcome.clone().unwrap_or_default()))
        }
        Some(why) => {
            run.outcome = Some(format!("discarded: {why}"));
            crate::dispatch::discard(store, run, false, false)?;
            let landed = match why.starts_with(NO_APPLY) {
                true => Landed::Conflict,
                false => Landed::Discarded,
            };
            // Another edit's landing refused it, not the agent's work, so it
            // is in no share.
            if landed == Landed::Conflict {
                run.infra = true;
                store.update_run(run)?;
            }
            Ok((landed, format!("discarded: {why}")))
        }
    }
}

const NO_APPLY: &str = "does not apply to the clone: ";

/// What became of a local edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landed {
    Applied,
    /// It failed verify, had no changes, or had no verify command.
    Discarded,
    /// It passed verify against a base that another edit has since changed,
    /// and no longer applies. Its unit is done again, from the new clone.
    Conflict,
}

/// Commits the approved tree in the worktree, pushes the run's branch and
/// opens a pull request for it.
fn publish_one(cfg: &Config, run: &Run) -> Result<String> {
    let wt = &run.worktree;
    if !wt.is_dir() {
        return Err(format!("{} no longer exists", wt.display()).into());
    }
    let (Some(approved), Some(approved_head)) = (&run.approved_tree, &run.approved_head) else {
        return Err(format!(
            "run #{} has no approved tree and head to publish; approve it again",
            run.id
        )
        .into());
    };
    let head = wt_git(run, &["rev-parse", "HEAD"])?;
    // Before any publish has committed, an edit since approval is someone's
    // work, so it is refused rather than discarded below.
    if &head == approved_head && &crate::dispatch::tree(run)? != approved {
        return Err(format!(
            "the worktree changed after it was approved by {}; read \
             `pma review {}` and approve it again, or reject it",
            run.approved_by.as_deref().unwrap_or("?"),
            run.id
        )
        .into());
    }
    if which("gh").is_none() {
        return Err("`pma pr` needs `gh` on PATH; `pma push` pushes without it".into());
    }
    if approved == &wt_git(run, &["rev-parse", &format!("{}^{{tree}}", run.base)])? {
        return Err("nothing to publish: no changes".into());
    }
    let message = message(cfg, run, None);

    // Every attempt builds one commit of the approved tree on the base. A
    // failed publish leaves its commits, and verify its output, in the
    // worktree; the agent's own commits hold whatever their history held,
    // and review showed only the net diff. `-B` also recovers an agent that
    // switched branches.
    wt_git(
        run,
        &[
            "checkout",
            "--quiet",
            "--force",
            "-B",
            &run.branch,
            &run.base,
        ],
    )?;
    wt_git(run, &["read-tree", "--reset", "-u", approved])?;
    wt_git(run, &["commit", "--quiet", "-m", &message])?;
    if !crate::dispatch::closes(run).is_empty() {
        let path = wt.join("TODO.md");
        // The agent controls the worktree, and a symlink would point the
        // read and the write outside it.
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("{}: is a symlink; pma edits only a file", path.display()).into());
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(edited) = ticked(run, &text) {
            todo::save(&path, &edited).map_err(|e| format!("{}: {e}", path.display()))?;
            wt_git(run, &["add", "TODO.md"])?;
            wt_git(run, &["commit", "--quiet", "--amend", "--no-edit"])?;
        }
    }
    // A conflict fix rebuilt the branch, so its push is forced, but only over
    // the tip the fix read: a commit pushed since is refused, not lost.
    let tip_ref = crate::dispatch::pr_tip_ref(run);
    let lease = wt_git(run, &["rev-parse", "--verify", "--quiet", &tip_ref]).ok();
    // A retry rebuilt its commits, so their ids differ from what an
    // earlier attempt pushed. The same tree there means it pushed, unless
    // that is still the tip a fix read: the same tree on the old base.
    let origin_branch = format!("origin/{}", run.branch);
    let pushed = wt_git(run, &["fetch", "--quiet", "origin", &run.branch]).is_ok()
        && wt_git(run, &["rev-parse", &format!("{origin_branch}^{{tree}}")])?
            == wt_git(run, &["rev-parse", "HEAD^{tree}"])?
        && lease.as_ref() != Some(&wt_git(run, &["rev-parse", &origin_branch])?);
    if !pushed {
        match &lease {
            Some(tip) => wt_git(
                run,
                &[
                    "push",
                    "--quiet",
                    "-u",
                    &format!("--force-with-lease={}:{tip}", run.branch),
                    "origin",
                    &run.branch,
                ],
            )?,
            None => wt_git(run, &["push", "--quiet", "-u", "origin", &run.branch])?,
        };
    }
    if lease.is_some() {
        wt_git(run, &["update-ref", "-d", &tip_ref])?;
    }
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
    Ok(if open.is_empty() {
        let (title, trailers) = message.split_once("\n\n").unwrap_or((&message, ""));
        let body = pr_body(run, trailers);
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
                &body,
            ],
        )?
    } else {
        open
    })
}

/// A pull request's body: the run, the agent's summary, the verify result,
/// then the commit's trailers.
fn pr_body(run: &Run, trailers: &str) -> String {
    let mut parts = vec![format!("pma run #{}: {}", run.id, run.text)];
    if let Some(s) = run
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("## Summary\n\n{s}"));
    }
    let verify = match (run.verify.as_deref(), run.verify_ok) {
        (None, _) => "no verify command".to_string(),
        (Some(c), Some(true)) => format!("`{c}` passed"),
        (Some(c), Some(false)) => format!("`{c}` failed"),
        (Some(c), None) => format!("`{c}` did not run"),
    };
    parts.push(format!("## Verify\n\n{verify}"));
    if !trailers.is_empty() {
        parts.push(trailers.to_string());
    }
    parts.join("\n\n")
}

/// Whether GitHub finds the pull request at `url` in conflict with its base.
pub fn conflicting(url: &str) -> Result<bool> {
    let read = crate::sync::gh(&[
        "pr",
        "view",
        url,
        "--json",
        "mergeable",
        "--jq",
        ".mergeable",
    ])?;
    Ok(read.trim() == "CONFLICTING")
}

/// Merges a `pr-open` run's pull request. Squashes, as a publish makes one
/// commit; a repository that forbids it refuses through `gh`.
pub fn merge(store: &Store, run: &mut Run) -> Result<String> {
    if run.state != RunState::PrOpen {
        return Err(format!(
            "run #{} is {}; it has no open pull request",
            run.id,
            run.state.name()
        )
        .into());
    }
    let url = run.outcome.clone().unwrap_or_default();
    if conflicting(&url)? {
        return Err(format!(
            "pull request has conflicts: {url}\nPress F, or run `pma merge {} --fix`, to have an agent fix them.",
            run.id
        )
        .into());
    }
    crate::sync::gh(&["pr", "merge", &url, "--squash"])?;
    run.enter(RunState::Merged);
    store.update_run(run)?;
    Ok(format!("pull request merged: {url}"))
}

/// Moves each `pr-open` run to `merged` when its pull request is merged, or
/// to `closed` when it is closed without merging. An open pull request keeps
/// the run `pr-open`; `done` still sees it when someone has reviewed or
/// commented on it, as GitHub shows it. `done` also sees each run whose pull
/// request `gh` cannot read, which stays `pr-open`. A `committed` run settles
/// too; see `settle_committed`.
pub fn settle(
    store: &Store,
    mut done: impl FnMut(&Run, &std::result::Result<String, String>),
) -> Result<()> {
    for run in settle_committed(store)? {
        done(&run, &Ok(run.outcome.clone().unwrap_or_default()));
    }
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
        // Closing judges the work, as a rejection does. Charged after the
        // update, so a failed save cannot charge it twice.
        if run.state == RunState::Closed {
            store.consume_attempt(&run.project, &crate::dispatch::attempt_key(&run))?;
        }
        done(&run, &Ok(outcome));
    }
    Ok(())
}

/// Moves each `committed` run to `pushed` once its remote-tracking default
/// branch holds the commit, as after a `git push` by hand. Nothing else would
/// end such a run: `pma push` refuses a `HEAD` that has moved past it. Returns
/// the runs moved. A commit rebased or squashed on the way is not recognised.
pub fn settle_committed(store: &Store) -> Result<Vec<Run>> {
    let runs = store.runs()?;
    let mut settled = Vec::new();
    for run in runs.iter().filter(|r| r.state == RunState::Committed) {
        let Some(sha) = committed_sha(run) else {
            continue;
        };
        let branch = format!("origin/{}", run.default_branch);
        if crate::dispatch::clone_held(&runs, &run.repo).is_err()
            || git(&run.repo, &["merge-base", "--is-ancestor", sha, &branch]).is_err()
        {
            continue;
        }
        let mut run = run.clone();
        run.outcome = Some(format!("{sha} found on {branch}"));
        run.enter(RunState::Pushed);
        store.update_run(&run)?;
        settled.push(run);
    }
    Ok(settled)
}

/// The commit a `committed` run's outcome names, as `commit` wrote it.
fn committed_sha(run: &Run) -> Option<&str> {
    run.outcome
        .as_deref()?
        .strip_prefix("committed ")?
        .split(' ')
        .next()
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
    let mut cmd = Command::new("gh");
    cmd.args(args).current_dir(dir);
    let what = args[..2].join(" ");
    let out = crate::scan::bounded(cmd, crate::sync::GH_TIMEOUT)
        .map_err(|e| format!("gh {what}: {e}"))?;
    if !out.success {
        return Err(format!("gh {what}: {}", out.stderr.trim()).into());
    }
    Ok(out.stdout.trim().to_string())
}

/// `text` with each item the run closes marked done; `None` when none was
/// open in it.
fn ticked(run: &Run, text: &str) -> Option<String> {
    let mut out: Option<String> = None;
    for (key, item, _) in crate::dispatch::closes(run) {
        if let Some(edited) = todo::mark_done(out.as_deref().unwrap_or(text), key, item) {
            out = Some(edited);
        }
    }
    out
}

/// The task text as subject, unless `subject` replaces it; `Closes #N` and
/// the co-author trailer as body. A bundle's subject counts its items, and
/// its body lists the ones it closes.
fn message(cfg: &Config, run: &Run, subject: Option<&str>) -> String {
    let closes = crate::dispatch::closes(run);
    let bundle = run.task_key == crate::dispatch::BUNDLE;
    let counted = format!("{} TODO items", closes.len());
    let subject = match subject {
        Some(s) => s,
        None if bundle => &counted,
        None => &run.text,
    };
    let mut body = Vec::new();
    if bundle {
        let listed: Vec<String> = closes
            .iter()
            .map(|(_, text, _)| format!("- {text}"))
            .collect();
        body.push(listed.join("\n"));
    }
    // A run with no item has no issue: every such pick is made without one.
    let issues: Vec<String> = closes
        .iter()
        .filter_map(|c| c.2)
        .map(|n| format!("Closes #{n}"))
        .collect();
    if !issues.is_empty() {
        body.push(issues.join("\n"));
    }
    // Only a worker whose identity `pma` knows gets a trailer. Inventing an
    // address for an unknown one would attribute the commit to nobody.
    if cfg.attribution == Attribution::CoAuthor && run.agent == "claude" {
        body.push("Co-Authored-By: Claude <noreply@anthropic.com>".into());
    }
    if body.is_empty() {
        subject.to_string()
    } else {
        format!("{subject}\n\n{}", body.join("\n\n"))
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

    /// A bundle's commit counts what it closes, lists it, and closes each
    /// issue; a dropped item is in none of the three.
    #[test]
    fn a_bundle_message_names_the_items_it_closes() {
        let item = |key: &str, gh, dropped| crate::store::RunItem {
            key: key.into(),
            text: format!("task {key}"),
            gh,
            dropped,
        };
        let run = Run {
            task_key: crate::dispatch::BUNDLE.into(),
            agent: "other".into(),
            items: vec![
                item("a", Some(7), false),
                item("b", Some(8), true),
                item("c", None, false),
            ],
            ..Run::blank()
        };
        let cfg = Config::default();
        assert_eq!(
            message(&cfg, &run, None),
            "2 TODO items\n\n- task a\n- task c\n\nCloses #7"
        );
        assert!(message(&cfg, &run, Some("Fix two things")).starts_with("Fix two things\n\n"));
    }

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
