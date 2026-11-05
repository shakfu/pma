use super::*;

/// What the run's change is read against: `start` for a clone run, else the
/// base.
pub fn from(run: &Run) -> &str {
    run.start.as_deref().unwrap_or(&run.base)
}

/// The side of the diff the reviewer sees. In a worktree, the working tree
/// itself, after `git add --intent-to-add` has made untracked files visible
/// to `git diff`. In a clone, a commit of the working tree built in a
/// temporary index, so the clone's own index is never touched: `git diff`
/// then takes it as any other revision. For a run whose tree is gone, the
/// tree the reviewer was last shown, which is also what a rejected or
/// published run shows.
fn to(run: &Run) -> Result<Option<String>> {
    let reviewable = matches!(
        run.state,
        RunState::Running | RunState::Ready | RunState::Approved | RunState::Failed
    );
    if !reviewable || !run.worktree.is_dir() {
        return Ok(run.reviewed_tree.clone());
    }
    match run.workspace {
        Workspace::Clone => Ok(Some(working_tree_commit(&run.repo)?)),
        Workspace::Worktree => {
            wt_git(run, &["add", "--all", "--intent-to-add"])?;
            Ok(None)
        }
    }
}

/// `git diff` arguments for the run's change: `from` against `to`, or
/// against the working tree when `to` is `None`.
fn range(run: &Run, to: Option<&str>) -> Vec<String> {
    let mut v = vec![from(run).to_string()];
    v.extend(to.map(str::to_string));
    v
}

/// The last line of `git diff --stat` against the base, untracked files
/// included.
pub(super) fn diffstat(run: &Run) -> Option<String> {
    let to = to(run).ok()?;
    let mut args = vec!["diff".to_string(), "--stat".into()];
    args.extend(range(run, to.as_deref()));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let stat = wt_git(run, &args).ok()?;
    Some(
        stat.lines()
            .last()
            .unwrap_or("no changes")
            .trim()
            .to_string(),
    )
}

/// Every path the run changed against its base, untracked files included and
/// both sides of a rename listed. NUL-delimited, because a path may contain a
/// newline, and `--name-status` rather than `--name-only`, because a rename
/// names two paths and only the status says so.
pub(super) fn changed_paths(run: &Run) -> Result<Vec<String>> {
    let to = to(run)?;
    let mut args = vec![
        "diff".to_string(),
        "--name-status".into(),
        "-z".into(),
        "--find-renames".into(),
    ];
    args.extend(range(run, to.as_deref()));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = wt_git(run, &args)?;
    let mut fields = out.split('\0').filter(|f| !f.is_empty());
    let mut paths = Vec::new();
    while let Some(status) = fields.next() {
        let wanted = match status.as_bytes().first() {
            // A rename or a copy names its source and its destination.
            Some(b'R' | b'C') => 2,
            Some(_) => 1,
            None => continue,
        };
        for _ in 0..wanted {
            match fields.next() {
                Some(p) => paths.push(p.to_string()),
                None => return Err(format!("`git diff` ended after status `{status}`").into()),
            }
        }
    }
    Ok(paths)
}

/// `git diff` of the run's change: the worktree against the base, or a
/// clone's working tree against `start`.
pub fn diff(run: &Run) -> Result<String> {
    // Not `start` against the tree: that would show every later commit as the
    // run's. While `HEAD` holds, the live tree stays right: a reviewer's tweak
    // belongs in what they read and publish.
    if clone_moved(run)
        && matches!(
            run.state,
            RunState::Ready | RunState::Approved | RunState::Failed
        )
    {
        let record = change_record(&crate::store::state_home()?, run);
        return Ok(std::fs::read_to_string(record).unwrap_or_else(|_| {
            "(none recorded: the clone has moved since this run, so its change \
             can no longer be told from later work)"
                .to_string()
        }));
    }
    let to = to(run)?;
    if to.is_none() && !run.worktree.is_dir() {
        return Err(format!(
            "{} no longer exists and no tree was recorded",
            run.worktree.display()
        )
        .into());
    }
    let mut args = vec!["diff".to_string()];
    args.extend(range(run, to.as_deref()));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // Two objects diff anywhere in the repository, so a run whose worktree
    // is gone still reads.
    unchanged_git(run)?;
    let dir = match run.worktree.is_dir() {
        true => &run.worktree,
        false => &run.repo,
    };
    git(dir, &args)
}

/// The worktree's content as one object id: `git add --all`, which stages
/// without committing, then `git write-tree`. The same call when publishing
/// says whether anything changed since. A clone's content is read through a
/// temporary index, so nothing is staged in the user's.
pub fn tree(run: &Run) -> Result<String> {
    if run.workspace == Workspace::Clone {
        unchanged_git(run)?;
        let commit = working_tree_commit(&run.repo)?;
        return git(&run.repo, &["rev-parse", &format!("{commit}^{{tree}}")]);
    }
    wt_git(run, &["add", "--all"])?;
    wt_git(run, &["write-tree"])
}

/// The most one reading adds to a run's review time. A run left open over
/// lunch is not half a day of review.
const REVIEW_CAP_SECONDS: i64 = 30 * 60;

/// A decision ends what its pane was kept open for.
fn settle_review(store: &Store, run: &Run) -> Result<()> {
    if let Some(pane) = &run.pane {
        crate::herdr::close(pane);
    }
    store.settle_review(run.id, crate::dates::now(), REVIEW_CAP_SECONDS)
}

pub fn approve(store: &Store, run: &mut Run, by: &str) -> Result<()> {
    let read = run.state;
    // Approving an already approved run re-takes the evidence, which is how
    // a reviewer says the tree is fine after a publish refused a stale one.
    if !matches!(run.state, RunState::Ready | RunState::Approved) {
        return Err(format!(
            "run #{} is {}; only a ready or approved run can be approved",
            run.id,
            run.state.name()
        )
        .into());
    }
    if run.approval == Some(crate::route::Approval::Propose) {
        return Err(format!(
            "run #{} took route `{}`, which is `propose`: it produces a patch and a \
             summary, and nothing is published. Change the route to publish it.",
            run.id,
            run.route.as_deref().unwrap_or("-")
        )
        .into());
    }
    // What is approved is what the reviewer was last shown. Anything written
    // since, by an agent that outlived its run or by anyone, is read first.
    let now = tree(run)?;
    if run
        .reviewed_tree
        .as_ref()
        .is_some_and(|shown| shown != &now)
    {
        return Err(format!(
            "run #{} changed after it was last shown; read `pma review {}` again, \
             then approve it",
            run.id, run.id
        )
        .into());
    }
    run.approved_tree = Some(now);
    run.approved_head = wt_git(run, &["rev-parse", "HEAD"]).ok();
    run.approved_by = Some(by.to_string());
    run.enter(RunState::Approved);
    settle_review(store, run)?;
    // No session lock is held: a publish may have run since `run` was read.
    store.update_run_from(run, read)
}

/// `keep` accepts hooks or settings the run found changed as the user's own.
/// Without it, such a run is rejected only once the clone matches its
/// snapshot again, so rejecting does not quietly lift what `clone_unchanged`
/// holds. Returns how many files the run's change held.
///
/// `leave_files` closes the run and touches nothing: for a change that has
/// already landed, or a clone that has moved on.
pub fn reject(
    store: &Store,
    run: &mut Run,
    keep: bool,
    infra: bool,
    leave_files: bool,
) -> Result<usize> {
    let fix = run.fixing_pr().is_some();
    let files = discard(store, run, keep, leave_files)?;
    settle_review(store, run)?;
    // Only the fix was refused. The pull request still holds the task.
    if fix {
        return Ok(files);
    }
    run.infra = infra;
    store.update_run(run)?;
    let attempts = store.attempts(Some(run.id))?;
    if infra {
        // The checks this run failed judged the infrastructure, not the task.
        let failed = attempts
            .iter()
            .filter(|a| a.verify_ok == Some(false))
            .count() as i64;
        store.refund_attempts(&run.project, &attempt_key(run), failed)?;
    } else if !attempts.is_empty() {
        // A reviewer who refuses the work has judged the task, whatever
        // verify made of it. A run that never started has no work to judge.
        store.consume_attempt(&run.project, &attempt_key(run))?;
    }
    Ok(files)
}

/// Removes a run's worktree and marks it rejected, consuming no attempt: what
/// `reject` does before a reviewer's judgement is counted. A local edit that
/// failed verify was counted by verify; one that did not apply was judged by
/// no one. Returns how many files the change held.
pub fn discard(store: &Store, run: &mut Run, keep: bool, leave_files: bool) -> Result<usize> {
    let read = run.state;
    if run.state == RunState::PrOpen {
        return Err(format!(
            "run #{} has an open pull request; merge or close it: {}",
            run.id,
            run.outcome.as_deref().unwrap_or_default()
        )
        .into());
    }
    if run.state.is_final() || matches!(run.state, RunState::Queued | RunState::Running) {
        return Err(format!(
            "run #{} is {}; it cannot be rejected",
            run.id,
            run.state.name()
        )
        .into());
    }
    if !keep
        && let Some(then) = &run.git_snapshot
        && run
            .error
            .as_deref()
            .is_some_and(|e| e.contains(GIT_CHANGED))
        && let Some(changes) = changes(then, &snapshot_in(&run.repo, &run.repo)?)
    {
        return Err(format!(
            "run #{} found the hooks or git settings of {} changed, and they still are:\n\
             {changes}\nRestore them first, or pass --keep-git-changes if you made them.",
            run.id,
            run.repo.display()
        )
        .into());
    }
    if run.workspace == Workspace::Clone {
        // Only the recorded change. A patch of the tree as it is now would hold
        // later work too, and reversing it would remove that without a word.
        let record = change_record(&crate::store::state_home()?, run);
        let files = patch_files(&record);
        if !leave_files {
            let close = format!(
                "`pma close {}`, or `X`, ends the run and leaves your files alone",
                run.id
            );
            if clone_moved(run) {
                return Err(format!(
                    "{} has moved past run #{}'s base, so taking its change out could \
                     remove later work. {close}",
                    run.repo.display(),
                    run.id
                )
                .into());
            }
            if !record.exists() {
                return Err(format!(
                    "run #{} has no record of its change, so it cannot be told from \
                     your own edits. Take it out yourself if you want it gone. {close}",
                    run.id
                )
                .into());
            }
            revert(run, Some(&record))?;
        }
        if leave_files {
            run.outcome = Some("closed; its files were left as they are".into());
        }
        run.enter(RunState::Rejected);
        store.update_run_from(run, read)?;
        return Ok(files);
    }
    // The patch stays in the run's log directory either way.
    let patch = keep_patch(run);
    let files = patch.as_deref().map_or(0, patch_files);
    // Written first, and only over the state that was read: a run another
    // front end approved meanwhile is refused with its worktree intact.
    if run.fixing_pr().is_some() {
        run.state = RunState::PrOpen;
        run.error = None;
        run.approved_tree = None;
        run.approved_head = None;
        run.approved_by = None;
    } else {
        run.enter(RunState::Rejected);
    }
    store.update_run_from(run, read)?;
    if run.state == RunState::PrOpen {
        git(&run.repo, &["update-ref", "-d", &pr_tip_ref(run)])?;
    }
    remove_worktree(&run.repo, &run.worktree, &run.branch)
        .map_err(|e| format!("run #{} is rejected, but its worktree stays: {e}", run.id))?;
    Ok(files)
}

/// How many files a patch from `git diff` touches; none when it cannot be
/// read.
fn patch_files(patch: &Path) -> usize {
    std::fs::read(patch).map_or(0, |b| {
        b.split(|&c| c == b'\n')
            .filter(|l| l.starts_with(b"diff --git "))
            .count()
    })
}

/// Writes the run's change to `runs/<id>/change.patch`, best effort: a run
/// that never ran has nothing to keep.
fn keep_patch(run: &Run) -> Option<PathBuf> {
    let home = crate::store::state_home().ok()?;
    // A worktree's `to` is the working tree itself; the patch needs an
    // object, so it is written from the tree.
    let to = match to(run).ok()? {
        Some(to) => to,
        None => tree(run).ok()?,
    };
    crate::publish::write_patch(&home, run, &to).ok()
}

/// Takes a clone run's change out of the working tree: the patch in reverse,
/// which refuses when the files moved on since. Only the working tree: the
/// index is the user's. Nothing to revert is fine.
fn revert(run: &Run, patch: Option<&Path>) -> Result<()> {
    let Some(patch) = patch else {
        return Ok(());
    };
    if std::fs::metadata(patch).map_or(0, |m| m.len()) == 0 {
        return Ok(());
    }
    git(
        &run.repo,
        &[
            "apply",
            "--reverse",
            "--binary",
            "--check",
            path_arg(patch)?,
        ],
    )
    .and_then(|_| {
        git(
            &run.repo,
            &["apply", "--reverse", "--binary", path_arg(patch)?],
        )
    })
    .map(|_| ())
    .map_err(|e| {
        format!(
            "run #{}'s change no longer reverts cleanly from {}: {e}\n\
             Take it out yourself, then reject again. The patch: {}",
            run.id,
            run.repo.display(),
            patch.display()
        )
        .into()
    })
}

/// Runs the agent again in the same worktree with the reviewer's feedback.
pub fn rework(
    store: &Store,
    home: &Path,
    cfg: &Config,
    run: &mut Run,
    feedback: &str,
) -> Result<()> {
    if !matches!(
        run.state,
        RunState::Ready | RunState::Failed | RunState::Approved
    ) {
        return Err(format!(
            "run #{} is {}; it cannot be reworked",
            run.id,
            run.state.name()
        )
        .into());
    }
    settle_review(store, run)?;
    if !run.worktree.is_dir() {
        return Err(format!(
            "{} no longer exists; reject the run",
            run.worktree.display()
        )
        .into());
    }
    run.feedback = Some(feedback.to_string());
    // The approver read a tree that is about to change.
    run.approved_tree = None;
    run.approved_head = None;
    run.approved_by = None;
    run.state = RunState::Running;
    store.update_run(run)?;
    let a = attempt(home, cfg, &store.agents()?, run, Some(feedback));
    store.update_run(run)?;
    store.insert_attempt(&a)?;
    if consumes(run) {
        store.consume_attempt(&run.project, &attempt_key(run))?;
    }
    Ok(())
}

/// Where a conflict fix records the pull request branch's tip, as the lease
/// its publish pushes over.
pub fn pr_tip_ref(run: &Run) -> String {
    format!("refs/pma/pr-tip/{}", run.id)
}

/// Sends a `pr-open` run back to review with its pull request's conflicts
/// resolved by the agent. The run's worktree is rebuilt from the branch, with
/// the default branch merged in and the markers left for the agent. Publishing
/// then updates the same pull request.
pub fn fix_conflicts(store: &Store, home: &Path, cfg: &Config, run: &mut Run) -> Result<()> {
    if run.state != RunState::PrOpen {
        return Err(format!(
            "run #{} is {}; it has no open pull request",
            run.id,
            run.state.name()
        )
        .into());
    }
    let url = run.outcome.clone().unwrap_or_default();
    if !crate::publish::conflicting(&url)? {
        return Err(format!("pull request has no conflicts: {url}").into());
    }
    clone_unchanged(store, &run.repo)?;
    git(&run.repo, &["fetch", "--quiet", "origin"])?;
    let onto = format!("refs/remotes/origin/{}", run.default_branch);
    let base = git(&run.repo, &["rev-parse", &onto])?;
    let tip = git(
        &run.repo,
        &["rev-parse", &format!("refs/remotes/origin/{}", run.branch)],
    )?;
    git(
        &run.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-B",
            &run.branch,
            path_arg(&run.worktree)?,
            &tip,
        ],
    )?;
    git(&run.repo, &["update-ref", &pr_tip_ref(run), &tip])?;
    // Fails on the conflicts it leaves; the list below is what matters.
    let _ = wt_git(run, &["merge", "--quiet", "--no-commit", "--no-ff", &onto]);
    let conflicted = wt_git(run, &["diff", "--name-only", "--diff-filter=U"])?;
    // The branch's tick is on a stale copy. Publishing ticks the new one.
    let _ = wt_git(run, &["checkout", &onto, "--", "TODO.md"]);
    let feedback = format!(
        "This change's pull request conflicts with `{}`, now merged into the working tree. \
         Resolve the conflict markers in these files, keeping both sides' intent, and run \
         the tests. Do not commit:\n{conflicted}",
        run.default_branch
    );
    run.base = base;
    run.feedback = Some(feedback.clone());
    run.approved_tree = None;
    run.approved_head = None;
    run.approved_by = None;
    run.state = RunState::Running;
    store.update_run(run)?;
    let mut a = attempt(home, cfg, &store.agents()?, run, Some(&feedback));
    let marked = conflicted.lines().find(|f| {
        std::fs::read_to_string(run.worktree.join(f))
            .is_ok_and(|text| text.lines().any(|l| l.starts_with("<<<<<<< ")))
    });
    if run.state == RunState::Ready
        && let Some(file) = marked
    {
        fail(run, &mut a, format!("conflict markers remain in {file}"));
    }
    store.update_run(run)?;
    store.insert_attempt(&a)?;
    if consumes(run) {
        store.consume_attempt(&run.project, &attempt_key(run))?;
    }
    Ok(())
}
