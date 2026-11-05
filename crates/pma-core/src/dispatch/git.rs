use super::*;

/// Runs `git -C dir args`, returning trimmed stdout or an error naming the
/// command and its stderr. With no filesystem monitor: one names a program,
/// and these calls carry the user's credentials. Bounded by `GIT_TIMEOUT`, so
/// a stalled fetch or push cannot hold the session lock.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "core.fsmonitor=false"])
        .args(args);
    let what = args.join(" ");
    let out = crate::scan::bounded(cmd, GIT_TIMEOUT).map_err(|e| format!("git {what}: {e}"))?;
    if !out.success {
        return Err(format!("git {what}: {}", out.stderr.trim()).into());
    }
    Ok(out.stdout.trim().to_string())
}

/// How long one `git` call outside a scan may take. Longer than a scan's
/// bound: a push, a fetch or a worktree checkout of a large repository is slow.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(600);

/// `git` in the run's worktree, once its `.git` is still the one `git
/// worktree add` wrote and its hooks and settings are the ones taken at
/// dispatch. An agent that replaced either would hand these calls, which
/// carry the user's credentials, a program of its own to run.
pub fn wt_git(run: &Run, args: &[&str]) -> Result<String> {
    unchanged_git(run)?;
    git(&run.worktree, args)
}

/// How a snapshot change reads in an error, which is also how a run that
/// found one is recognised afterwards.
pub(super) const GIT_CHANGED: &str = "the repository's hooks or git settings changed since run #";

/// Refuses a run whose repository gained or changed a hook, or a setting
/// that runs a program or redirects a push, since dispatch. pma's git calls
/// carry the user's credentials and environment, and none of that is in the
/// diff the reviewer read. Also refuses a replaced `.git`, which is all a run
/// from before the snapshot existed can be checked for. A run whose
/// worktree is gone, once staged, is checked in its clone.
pub fn unchanged_git(run: &Run) -> Result<()> {
    let gone = !run.worktree.exists();
    let Some(then) = &run.git_snapshot else {
        return match gone || run.workspace == Workspace::Clone {
            true => Ok(()),
            false => own_gitdir(run),
        };
    };
    let now = match gone {
        true => snapshot_in(&run.repo, &run.repo)?,
        false => git_snapshot(run)?,
    };
    match changes(then, &now) {
        None => Ok(()),
        Some(changes) => Err(format!(
            "{GIT_CHANGED}{} was dispatched:\n{changes}\n\
             pma runs no git in {} until they are restored, since its calls carry your \
             credentials. If you made these changes, `pma review {} --reject \
             --keep-git-changes`.",
            run.id,
            run.repo.display(),
            run.id,
        )
        .into()),
    }
}

/// The lines each snapshot has and the other lacks, or `None` when equal.
pub(super) fn changes(then: &str, now: &str) -> Option<String> {
    if then == now {
        return None;
    }
    let (then, now): (Vec<&str>, Vec<&str>) = (then.lines().collect(), now.lines().collect());
    let lines: Vec<String> = then
        .iter()
        .filter(|l| !now.contains(l))
        .map(|l| format!("  - {l}"))
        .chain(
            now.iter()
                .filter(|l| !then.contains(l))
                .map(|l| format!("  + {l}")),
        )
        .collect();
    Some(lines.join("\n"))
}

/// Refuses to run git in `repo` while a run there that is not settled found
/// its snapshot changed, until the clone matches that snapshot again. A
/// setting an agent added stays in the clone after its run fails; the next
/// fetch or `worktree add` would run it, and the next snapshot would take it
/// as its baseline.
pub fn clone_unchanged(store: &Store, repo: &Path) -> Result<()> {
    clone_held(&store.runs()?, repo)
}

/// `clone_unchanged` over runs already read, for a caller checking many
/// repositories.
pub fn clone_held(runs: &[Run], repo: &Path) -> Result<()> {
    let found: Vec<&Run> = runs
        .iter()
        .filter(|r| {
            r.repo == repo
                && !r.state.is_final()
                && r.git_snapshot.is_some()
                && r.error.as_deref().is_some_and(|e| e.contains(GIT_CHANGED))
        })
        .collect();
    if found.is_empty() {
        return Ok(());
    }
    let now = snapshot_in(repo, repo)?;
    for run in found {
        if let Some(changes) = changes(run.git_snapshot.as_deref().unwrap_or_default(), &now) {
            return Err(format!(
                "{GIT_CHANGED}{} was dispatched, and they are still in {}:\n{changes}\n\
                 pma runs no git there until they are restored. If you made these changes, \
                 `pma review {} --reject --keep-git-changes`.",
                run.id,
                repo.display(),
                run.id,
            )
            .into());
        }
    }
    Ok(())
}

/// Refuses a worktree whose `.git` is not a file naming this worktree's
/// entry under the clone's `worktrees/`.
pub fn own_gitdir(run: &Run) -> Result<()> {
    let dot_git = run.worktree.join(".git");
    let replaced = || -> Box<dyn std::error::Error> {
        format!(
            "{} is not the file `git worktree add` wrote; the run's worktree was \
             tampered with, so pma runs no git in it. Reject the run.",
            dot_git.display()
        )
        .into()
    };
    let is_file = std::fs::symlink_metadata(&dot_git).is_ok_and(|m| m.is_file());
    let named = is_file
        .then(|| std::fs::read_to_string(&dot_git).ok())
        .flatten()
        .and_then(|t| Some(PathBuf::from(t.strip_prefix("gitdir: ")?.trim_end())))
        .and_then(|p| p.canonicalize().ok())
        .ok_or_else(replaced)?;
    let common = git(
        &run.repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let expected = Path::new(&common)
        .canonicalize()
        .map_err(|e| format!("{common}: {e}"))?
        .join("worktrees");
    // The entry names its worktree back, so `.git` cannot borrow another's.
    let back = std::fs::read_to_string(named.join("gitdir"))
        .ok()
        .and_then(|t| Path::new(t.trim_end()).canonicalize().ok());
    if named.parent() != Some(expected.as_path()) || back != dot_git.canonicalize().ok() {
        return Err(replaced());
    }
    Ok(())
}

/// What makes git run a program in the worktree, or push somewhere else,
/// that the diff does not show: each hook by content and executable bit, and
/// each repository setting that names a command or rewrites a URL, by a hash
/// of its value, since a URL may carry a token. Publishing compares it with the one
/// taken at dispatch. Hooks that run tracked files, such as husky's scripts or
/// a `.pre-commit-config.yaml`, run what the reviewer approved in the diff.
/// Global and system settings are the user's, so they are left out.
pub fn git_snapshot(run: &Run) -> Result<String> {
    if run.workspace == Workspace::Clone {
        return snapshot_in(&run.repo, &run.repo);
    }
    own_gitdir(run)?;
    snapshot_in(&run.repo, &run.worktree)
}

/// The snapshot as git reads it in `dir`, a worktree of `repo` or the clone
/// itself.
pub(super) fn snapshot_in(repo: &Path, dir: &Path) -> Result<String> {
    use std::os::unix::fs::PermissionsExt;

    let mut lines = Vec::new();
    let hooks = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
    )?;
    let mut files: Vec<(String, PathBuf, bool)> = std::fs::read_dir(&hooks)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let meta = std::fs::metadata(e.path()).ok()?;
            (meta.is_file() && !name.ends_with(".sample"))
                .then(|| (name, e.path(), meta.permissions().mode() & 0o111 != 0))
        })
        .collect();
    files.sort();
    if !files.is_empty() {
        let mut args = vec!["hash-object", "--no-filters", "--"];
        args.extend(files.iter().filter_map(|(_, p, _)| p.to_str()));
        let hashes = git(repo, &args)?;
        for ((name, _, executable), hash) in files.iter().zip(hashes.lines()) {
            let x = if *executable {
                "executable"
            } else {
                "not executable"
            };
            lines.push(format!("hook {name} {hash} {x}"));
        }
    }
    // NUL-separated scope and entry, each entry `key\nvalue`.
    let listing = git(dir, &["config", "--list", "--show-scope", "-z"])?;
    let mut settings: Vec<(String, String)> = Vec::new();
    let mut fields = listing.split('\0');
    while let (Some(scope), Some(entry)) = (fields.next(), fields.next()) {
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        if matches!(scope, "local" | "worktree") && runs_or_redirects(key) {
            settings.push((key.to_string(), value.to_string()));
        }
    }
    settings.sort();
    for (key, value) in settings {
        lines.push(format!("config {key} {}", hash_text(repo, &value)?));
    }
    Ok(lines.join("\n"))
}

/// A setting that names a program for git to run, or changes where a push
/// goes. Keys arrive with section and name lowercased.
fn runs_or_redirects(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "core.hookspath",
        "core.fsmonitor",
        "core.sshcommand",
        "core.editor",
        "core.pager",
        "core.askpass",
        "core.gitproxy",
        "diff.external",
        "sequence.editor",
        "remote.origin.url",
        "remote.origin.pushurl",
        "remote.pushdefault",
    ]
    .contains(&key.as_str())
        || [
            "filter.",
            "credential.",
            "url.",
            "include.",
            "includeif.",
            "gpg.",
        ]
        .iter()
        .any(|p| key.starts_with(p))
        || [".textconv", ".command", ".driver"]
            .iter()
            .any(|s| key.ends_with(s))
}

/// Git's object id for `text`, so a value is compared without being stored.
fn hash_text(dir: &Path, text: &str) -> Result<String> {
    use std::io::Write;

    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["hash-object", "--no-filters", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("git: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("git hash-object: no stdin")?
        .write_all(text.as_bytes())
        .map_err(|e| format!("git hash-object: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git hash-object: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git hash-object: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A commit of the clone's working tree, untracked files included and ignored
/// ones not, on top of `HEAD`. A local edit starts from it, so it sees the
/// developer's uncommitted work and the edits applied before it, and its patch
/// applies to the tree it was made from. Built in a temporary index, so the
/// clone's index, refs and files are untouched; `HEAD` when nothing differs.
pub fn working_tree_commit(repo: &Path) -> Result<String> {
    let head = git(repo, &["rev-parse", "HEAD"])?;
    let index = std::env::temp_dir().join(format!(
        "pma-index-{}-{}",
        std::process::id(),
        crate::dates::now()
    ));
    let run = |args: &[&str]| -> Result<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["-c", "core.fsmonitor=false"])
            .args(args)
            .env("GIT_INDEX_FILE", &index)
            .env("GIT_AUTHOR_NAME", "pma")
            .env("GIT_AUTHOR_EMAIL", "pma@localhost")
            .env("GIT_COMMITTER_NAME", "pma")
            .env("GIT_COMMITTER_EMAIL", "pma@localhost")
            .output()
            .map_err(|e| format!("git: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )
            .into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let built = (|| {
        run(&["read-tree", &head])?;
        run(&["add", "--all"])?;
        let tree = run(&["write-tree"])?;
        if tree == run(&["rev-parse", &format!("{head}^{{tree}}")])? {
            return Ok(head.clone());
        }
        run(&["commit-tree", &tree, "-p", &head, "-m", "pma: working tree"])
    })();
    let _ = std::fs::remove_file(&index);
    built
}

/// Lists `.pma/` in the repository's `info/exclude`, which the clone and
/// every worktree share. Not `.gitignore`: that is a tracked file, and an
/// edit to it would wait in `git status` for a commit.
pub fn exclude_pma(repo: &Path) -> Result<()> {
    let common = git(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let file = Path::new(&common).join("info").join("exclude");
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", file.display()).into()),
    };
    if text
        .lines()
        .any(|l| matches!(l.trim(), ".pma" | ".pma/" | "/.pma" | "/.pma/"))
    {
        return Ok(());
    }
    let sep = if text.is_empty() || text.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let dir = file.parent().expect("info/exclude has a parent");
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(&file, format!("{text}{sep}.pma/\n"))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(())
}

/// Removes the worktree and its branch. Either may already be gone.
pub fn remove_worktree(repo: &Path, worktree: &Path, branch: &str) -> Result<()> {
    // A clone run's `worktree` is the repository itself.
    let same = || match (worktree.canonicalize(), repo.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => worktree == repo,
    };
    if same() {
        return Err(format!("{} is the repository, not a worktree", worktree.display()).into());
    }
    let gitfile = std::fs::symlink_metadata(worktree.join(".git")).is_ok_and(|m| m.is_file());
    if worktree.exists() && gitfile {
        git(
            repo,
            &["worktree", "remove", "--force", path_arg(worktree)?],
        )?;
    } else if worktree.exists() {
        // Git refuses a worktree whose `.git` is not its file. The directory
        // is pma's, so it goes directly, and `prune` drops the entry.
        std::fs::remove_dir_all(worktree).map_err(|e| format!("{}: {e}", worktree.display()))?;
    }
    let _ = git(repo, &["worktree", "prune"]);
    if git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
    {
        git(repo, &["branch", "-D", branch])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `.pma/` is excluded once, in the file the clone and its worktrees
    /// share, after whatever the file already held.
    #[test]
    fn pma_is_excluded_once() {
        let dir = std::env::temp_dir().join(format!("pma-exclude-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]).unwrap();
        let file = dir.join(".git/info/exclude");
        std::fs::write(&file, "*.swp").unwrap();
        exclude_pma(&dir).unwrap();
        exclude_pma(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "*.swp\n.pma/\n");
        std::fs::create_dir_all(dir.join(".pma")).unwrap();
        std::fs::write(dir.join(".pma/changelog.md"), "x").unwrap();
        let status = git(&dir, &["status", "--porcelain", "--untracked-files=all"]).unwrap();
        assert!(!status.contains(".pma"), "{status}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
