use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

/// A scratch directory unique to one test, removed when dropped.
///
/// The name is a label, not the identity: tests in one binary share a pid and
/// run in parallel, so two that picked the same label shared a directory --
/// each wiped it on the way in and deleted it on the way out, under the other.
/// The counter is what makes the path unique.
struct Scratch(PathBuf);

static SCRATCH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

impl Scratch {
    fn new(name: &str) -> Self {
        let n = SCRATCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pma-test-{}-{name}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // `pma root add` canonicalizes, and macOS's temp dir is a symlink.
        Scratch(dir.canonicalize().unwrap())
    }

    fn project(&self, name: &str, todo: &str) -> PathBuf {
        let dir = self.0.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("TODO.md"), todo).unwrap();
        dir
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A command that ignores git settings passed through the environment. Under
/// `pma`'s own verify, `agent::restrict` sets them to block every push, and
/// the fixtures here push to local repositories. It also ignores `PMA_JOB`: under
/// a `pma` job, a plan would print `pma job approve` instead of `--approve <id>`.
fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("PMA_JOB");
    cmd
}

fn pma(args: &[&std::ffi::OsStr]) -> Output {
    command(env!("CARGO_BIN_EXE_pma"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn clean_file_exits_zero_silently() {
    let s = Scratch::new("clean");
    let dir = s.project("a", "# TODO\n\n## High\n\n- [ ] thing\n");
    let out = pma(&["lint".as_ref(), dir.as_os_str()]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "");
    assert!(out.stderr.is_empty());
}

#[test]
fn warnings_alone_exit_zero() {
    let s = Scratch::new("warn");
    let dir = s.project("a", "# TODO\n\n## Low\n\n- [x] finished #42\n");
    let out = pma(&["lint".as_ref(), dir.as_os_str()]);
    assert!(out.status.success());
    let todo = dir.join("TODO.md");
    assert_eq!(
        stdout(&out),
        format!(
            "{}:5: warning: `#42` is text; write `gh:42` to link the issue\n",
            todo.display()
        )
    );
}

#[test]
fn prune_lists_then_removes_finished_items() {
    let s = Scratch::new("prune");
    let text = "# TODO\n\n## High\n\n- [x] finished\n  notes\n- [ ] open\n- [x] parent\n  - [ ] child\n\n## Done\n\n- [x] old\n- [ ] stray\n";
    let a = s.project("a", text);
    let bad = s.project("bad", "## High\n\n- [x] kept\n");
    let todo = a.join("TODO.md");

    let out = pma(&["prune".as_ref(), a.as_os_str(), bad.as_os_str()]);
    assert!(out.status.success());
    assert_eq!(
        stdout(&out),
        format!(
            "{0}:11: remove `## Done`, lines 11-14\n\
             {0}:14: remove open item with `## Done`: `- [ ] stray`\n\
             {0}:5: remove `finished`\n\
             {0}:8: kept `parent`: line 9 under it is open\n\
             {1}: skipped: lint errors; see `pma lint`\n\
             2 removals; run `pma prune --apply` to make them\n",
            todo.display(),
            bad.join("TODO.md").display()
        )
    );
    assert_eq!(fs::read_to_string(&todo).unwrap(), text);

    let out = pma(&["prune".as_ref(), "--apply".as_ref(), a.as_os_str()]);
    assert!(out.status.success());
    assert!(
        stdout(&out).ends_with("2 removals made; TODO.md edits are uncommitted\n"),
        "{}",
        stdout(&out)
    );
    assert_eq!(
        fs::read_to_string(&todo).unwrap(),
        "# TODO\n\n## High\n\n- [ ] open\n- [x] parent\n  - [ ] child\n"
    );
    let out = pma(&[
        "prune".as_ref(),
        a.as_os_str(),
        s.0.join("none").as_os_str(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout(&out).ends_with("nothing to prune\n"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn errors_in_any_file_exit_one_and_every_file_is_checked() {
    let s = Scratch::new("errors");
    let bad = s.project("bad", "## High\n");
    let good = s.project("good", "# TODO\n");
    let missing = s.0.join("missing");
    fs::create_dir_all(&missing).unwrap();
    let named = s.0.join("gone.md");

    let out = pma(&[
        "lint".as_ref(),
        bad.as_os_str(),
        good.as_os_str(),
        missing.as_os_str(),
        named.as_os_str(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(
        text.contains(&format!("{}:1: error:", bad.join("TODO.md").display())),
        "{text}"
    );
    assert!(
        text.contains(&format!("{}: error:", named.display())),
        "a file named outright must exist: {text}"
    );
    assert!(!text.contains(&good.display().to_string()), "{text}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "2 errors, 0 warnings\n"
    );
}

/// `pma lint <root>/*/` names every repository, and not every one keeps a
/// list, so a directory without TODO.md is passed over rather than failed.
#[test]
fn a_directory_without_a_todo_is_skipped() {
    let s = Scratch::new("no-todo");
    let good = s.project("good", "# TODO\n");
    let missing = s.0.join("missing");
    fs::create_dir_all(&missing).unwrap();
    for command in ["lint", "prune"] {
        let out = pma(&[command.as_ref(), good.as_os_str(), missing.as_os_str()]);
        assert_eq!(out.status.code(), Some(0), "{command}");
        assert!(
            stdout(&out).contains(&format!(
                "{}: skipped: no TODO.md",
                missing.join("TODO.md").display()
            )),
            "{command}: {}",
            stdout(&out)
        );
    }
}

const DAY: i64 = 86_400;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn git(dir: &std::path::Path, args: &[&str], date: Option<i64>) {
    let mut cmd = command("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args);
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    if let Some(d) = date {
        cmd.env("GIT_AUTHOR_DATE", format!("@{d} +0000"))
            .env("GIT_COMMITTER_DATE", format!("@{d} +0000"));
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn pma_in(home: &std::path::Path, args: &[&str]) -> (String, String, bool) {
    let out = command(env!("CARGO_BIN_EXE_pma"))
        .env("PMA_HOME", home)
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// An empty `PMA_HOME` is unset; a relative one is refused, since the
/// database would follow the working directory.
#[test]
fn pma_home_must_be_absolute_when_set() {
    let s = Scratch::new("pma-home");
    let run = |value: &str| {
        command(env!("CARGO_BIN_EXE_pma"))
            .env("PMA_HOME", value)
            .env("HOME", &s.0)
            .env_remove("PMA_STATE")
            .current_dir(&s.0)
            .args(["config", "timeout"])
            .output()
            .unwrap()
    };
    let out = run("rel");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("PMA_HOME must be an absolute path"),
        "{err}"
    );
    assert!(!s.0.join("rel").exists());
    assert!(run("").status.success());
    assert!(s.0.join(".config/pma").exists());
}

fn ok(home: &std::path::Path, args: &[&str]) -> String {
    let (out, err, success) = pma_in(home, args);
    assert!(success, "pma {args:?} failed\nstdout: {out}\nstderr: {err}");
    out
}

/// The id in `--approve <id>`, as a pass or a dry run prints it.
fn plan_id(out: &str) -> String {
    let at = out
        .find("--approve ")
        .unwrap_or_else(|| panic!("no plan to approve: {out}"));
    out[at + "--approve ".len()..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect()
}

/// A scan names what changed since the one before and what needs attention;
/// `history` lists the scans and what each changed; `status --html` writes
/// them as a page.
#[test]
fn a_scan_reports_what_changed_and_history_keeps_it() {
    let s = Scratch::new("history");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = root.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    git(&alpha, &["init", "-q"], None);
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] keep\n- [ ] finish\n",
    )
    .unwrap();
    ok(&home, &["root", "add", root.to_str().unwrap()]);
    assert!(ok(&home, &["history"]).contains("no scan recorded yet"));
    let first = ok(&home, &["scan", "--offline"]);
    assert!(!first.contains("since the scan before"), "{first}");
    assert!(first.contains("`pma history`"), "{first}");

    // One item done, one added, a second `#` heading, and a new project.
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] keep\n- [ ] new one\n\n# Other\n",
    )
    .unwrap();
    let beta = root.join("beta");
    fs::create_dir_all(&beta).unwrap();
    git(&beta, &["init", "-q"], None);
    let second = ok(&home, &["scan", "--offline"]);
    assert!(second.contains("since the scan before:"), "{second}");
    assert!(
        second.contains("alpha  TODO lint  line 8: a second `#` heading"),
        "the fault is named, with its first error: {second}"
    );
    assert!(
        second.contains("alpha  items      1 opened, 1 closed"),
        "{second}"
    );
    assert!(second.contains("beta   new"), "{second}");
    assert!(second.contains("needs attention:"), "{second}");

    let listed = ok(&home, &["history"]);
    let lines: Vec<&str> = listed.lines().collect();
    assert_eq!(lines.len(), 3, "{listed}");
    assert!(lines[1].starts_with("2 "), "newest first: {listed}");
    let changed = ok(&home, &["history", "last"]);
    assert!(changed.starts_with("scan 2, "), "{changed}");
    assert!(changed.contains("1 TODO lint, 2 open items"), "{changed}");
    assert!(changed.contains("opened     high: new one"), "{changed}");
    assert!(changed.contains("closed     high: finish"), "{changed}");
    assert!(ok(&home, &["history", "1"]).contains("no change since the scan before"));
    let (_, err, success) = pma_in(&home, &["history", "9"]);
    assert!(!success && err.contains("no scan 9"), "{err}");

    let page = s.0.join("status.html");
    ok(
        &home,
        &["status", "--all", "--html", page.to_str().unwrap()],
    );
    let html = fs::read_to_string(&page).unwrap();
    assert!(html.starts_with("<!doctype html>"), "{html}");
    for text in [
        "Changed at the last scan",
        "Recent scans",
        "new one",
        "alpha",
    ] {
        assert!(html.contains(text), "{text} missing");
    }
}

#[test]
fn scan_rank_and_explain_a_real_repo() {
    let s = Scratch::new("stage2");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = root.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    fs::create_dir_all(root.join("beta")).unwrap();
    git(&root.join("beta"), &["init", "-q"], None);

    // Committed 100 days ago: two items and a source file.
    git(&alpha, &["init", "-q"], None);
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n### core\n\n- [ ] old high\n\n## Low\n\n- [ ] old low\n",
    )
    .unwrap();
    fs::write(alpha.join("lib.rs"), "\n").unwrap();
    git(&alpha, &["add", "."], None);
    git(&alpha, &["commit", "-qm", "init"], Some(now() - 100 * DAY));
    // 50 days ago the low item was retagged. Its text is unchanged, so it keeps
    // its age; the commit only touched TODO.md, so it is not activity.
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n### core\n\n- [ ] old high\n\n## Low\n\n- [ ] old low #later\n",
    )
    .unwrap();
    git(&alpha, &["commit", "-qam", "retag"], Some(now() - 50 * DAY));
    // A branch left by a run the database no longer knows.
    git(&alpha, &["branch", "pma/stray"], None);
    // Not committed: a new item, which also leaves the tree dirty.
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n### core\n\n- [ ] old high\n- [ ] new high\n\n## Low\n\n- [ ] old low #later\n",
    )
    .unwrap();

    let (_, err, success) = pma_in(&home, &["scan"]);
    assert!(!success && err.contains("pma root add"), "{err}");
    let (_, err, success) = pma_in(&home, &["matrix"]);
    assert!(!success && err.contains("pma scan"), "{err}");

    ok(&home, &["root", "add", root.to_str().unwrap()]);
    ok(&home, &["project", "tier", "1", "alpha"]);
    assert!(ok(&home, &["project"]).contains("alpha  tier 1"));
    let (_, err, success) = pma_in(&home, &["project", "tier", "6", "alpha"]);
    assert!(!success && err.contains("1 to 5"), "{err}");

    let scanned = ok(&home, &["scan", "--offline"]);
    assert_eq!(
        scanned.split(" in ").next(),
        Some("scanned 2 projects"),
        "{scanned}"
    );
    assert!(
        scanned
            .lines()
            .next()
            .unwrap()
            .ends_with(": 1 with TODO.md, 3 open items"),
        "{scanned}"
    );
    assert!(!scanned.contains("since the scan before"), "{scanned}");

    // `never` leaves untiered projects out of the ranking.
    ok(&home, &["config", "default_tier", "never"]);
    let matrix = ok(&home, &["matrix", "--all"]);
    let expected = "\
Q1 Do right away: 0

Q2 Schedule for later: 2
  alpha:7  T1  high  open 100d  core  old high
  alpha:8  T1  high  open 0d    core  new high

Q3 Delegate or avoid: 0

Q4 Later: 3
  alpha     T1  medium  open 0d    resolve local changes: 1 changed file, 1 leftover pma branch
  alpha:12  T1  low     open 100d  old low
  alpha     T1  low     open 70d   review project: no code commits in 100 days
";
    let (head, body) = matrix.split_once("\n\n").unwrap();
    assert_eq!(
        head,
        "last scan just now; 1 tiered projects, 1 untiered \
         (pma project tier <1-5> <project>..., or pma config default_tier <1-5>)"
    );
    assert_eq!(body, expected);

    // Placed by its worst state, a leftover `pma/` branch, with its idleness
    // counted beside it. CI was not measured, which places it nowhere.
    let status = ok(&home, &["status", "--explain"]);
    assert!(
        status.contains("\nalpha    1     unpublished +1"),
        "{status}"
    );
    assert!(
        status.contains("\nalpha  tier 1\n  unpublished  1 leftover pma branch\n"),
        "{status}"
    );
    assert!(
        status.contains("  idle         no code commit in 100 days, horizon 30\n"),
        "{status}"
    );
    assert!(status.contains("  CI not measured: offline"), "{status}");
    assert!(!status.contains("beta"), "{status}");

    // `--all` ranks the untiered project as tier 5 and marks it so. It has no
    // commit at all, so it is idle, which sorts after unpublished work.
    let status = ok(&home, &["status", "--all", "--explain"]);
    assert!(
        status.starts_with("last scan just now; 2 projects, 1 untiered ranked as tier 5\n"),
        "{status}"
    );
    let rows: Vec<&str> = status.lines().skip(3).take(2).collect();
    assert!(
        rows[0].starts_with("alpha    1     unpublished +1")
            && rows[1].starts_with("beta     -     idle"),
        "{status}"
    );
    assert!(
        status.contains("\nbeta  tier 5, untiered\n  idle  no code commits found\n"),
        "{status}"
    );

    // Lowering tier 1's multiplier makes its high items unimportant.
    ok(&home, &["config", "tiers.1", "0.3"]);
    assert_eq!(ok(&home, &["config", "tiers.1"]), "0.3\n");
    assert!(ok(&home, &["config"]).contains("tiers.1 = 0.3  (set)\n"));
    assert!(ok(&home, &["matrix", "-q", "q1"]).ends_with("Q1 Do right away: 0\n"));
    let (_, err, success) = pma_in(&home, &["config", "tiers.1", "much"]);
    assert!(
        !success && err.contains("tiers.1: expected a number"),
        "{err}"
    );
    ok(&home, &["config", "tiers.1", "--reset"]);
    assert!(ok(&home, &["matrix", "-q", "q2"]).contains("Q2 Schedule for later: 2\n"));

    // By default an untiered project is ranked at tier 5, still marked untiered.
    ok(&home, &["config", "default_tier", "--reset"]);
    let matrix = ok(&home, &["matrix"]);
    assert!(
        matrix.starts_with("last scan just now; 2 projects, 1 untiered ranked as tier 5\n"),
        "{matrix}"
    );
    let status = ok(&home, &["status"]);
    assert!(status.contains("\nbeta     -     idle"), "{status}");

    // Age orders `pma stale`; it no longer moves anything into Q1.
    let stale = ok(&home, &["stale", "-n", "2"]);
    assert!(
        stale.contains("alpha:7   T1  high  open 100d  old high"),
        "{stale}"
    );
    assert!(stale.trim_end().ends_with("and 1 more"), "{stale}");

    // A rescan keeps the tier and each task's first sighting.
    ok(&home, &["scan", "--offline", "alpha"]);
    assert!(ok(&home, &["project"]).contains("alpha  tier 1"));
}

/// A stand-in for `claude -p`: writes a file named by the task, tries to push,
/// and prints a result costing $0.10.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
case "$2" in
  *"reviewer's feedback"*) echo hi > hello.txt ;;
  *"add greeting"*) echo hi > hello.txt ;;
  *"second task"*) echo two > second.txt ;;
  *"third task"*) echo three > third.txt ;;
esac
if git push -q origin HEAD:refs/heads/agent 2>/dev/null; then echo pushed; else echo blocked; fi >> "$PUSH_LOG"
echo '{"type":"result","subtype":"success","is_error":false,"result":"did it","total_cost_usd":0.1}'
"#;

/// A worker with no structured output and no allowlist: the exit status is
/// its only verdict and its cost is unknown. It also proves `{dir}` reaches a
/// worker that takes its directory as an argument.
const PLAIN_WORKER: &str = r#"#!/bin/sh
cd "$3" || exit 1
echo hi > hello.txt
echo "wrote hello.txt in $3"
"#;

/// A stand-in that strays: an untracked workflow, a rename out of scope, and
/// a deletion. Its own summary claims it only touched the manifest.
const STRAYING_CLAUDE: &str = r#"#!/bin/sh
mkdir -p .github/workflows
echo "on: push" > .github/workflows/ci.yml
echo bump >> Cargo.lock
git mv Makefile build.mk
git rm -q notes.txt
echo '{"type":"result","subtype":"success","is_error":false,"result":"updated Cargo.lock","total_cost_usd":0.1}'
"#;

struct Env {
    home: PathBuf,
    bin: PathBuf,
    push_log: PathBuf,
}

impl Env {
    fn command(&self, args: &[&str]) -> Command {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = command(env!("CARGO_BIN_EXE_pma"));
        cmd.env("PMA_HOME", &self.home)
            .env("PATH", path)
            .env("PUSH_LOG", &self.push_log)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "u")
            .env("GIT_AUTHOR_EMAIL", "u@example.com")
            .env("GIT_COMMITTER_NAME", "u")
            .env("GIT_COMMITTER_EMAIL", "u@example.com")
            .args(args);
        cmd
    }

    fn run(&self, args: &[&str]) -> (String, String, bool) {
        let out = self.command(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.success(),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (out, err, success) = self.run(args);
        assert!(success, "pma {args:?} failed\nstdout: {out}\nstderr: {err}");
        out
    }
}

fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
    let out = command("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Installs `scripts` as executables on the `PATH` of a new `Env`, and a
/// tiered project `alpha` cloned from `origin.git`. Returns the env, origin
/// and the clone.
fn dispatch_env(s: &Scratch, scripts: &[(&str, &str)]) -> (Env, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let bin = s.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for (name, script) in scripts {
        fs::write(bin.join(name), script).unwrap();
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let env = Env {
        home: s.0.join("home"),
        bin,
        push_log: s.0.join("push.log"),
    };

    // origin holds three items and a test that needs hello.txt.
    let seed = s.0.join("seed");
    fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"], None);
    fs::write(
        seed.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] add greeting #agent\n  say hello in hello.txt\n- [ ] second task #agent gh:7\n- [ ] third task #agent\n\n## Low\n\n- [ ] guarded task #manual\n",
    )
    .unwrap();
    fs::write(seed.join("Makefile"), "test:\n\ttest -f hello.txt\n").unwrap();
    git(&seed, &["add", "."], None);
    git(&seed, &["commit", "-qm", "init"], None);
    let origin = s.0.join("origin.git");
    git(&s.0, &["clone", "-q", "--bare", "seed", "origin.git"], None);
    let root = s.0.join("root");
    fs::create_dir_all(&root).unwrap();
    git(&root, &["clone", "-q", "../origin.git", "alpha"], None);
    let alpha = root.join("alpha");

    env.ok(&["root", "add", root.to_str().unwrap()]);
    env.ok(&["project", "tier", "1", "alpha"]);
    // The tests on this env were written for worktree runs, several of
    // which share one clone; clone runs have their own tests.
    env.ok(&["config", "workspace", "worktree"]);
    env.ok(&["scan", "--offline"]);
    (env, origin, alpha)
}

/// `--local` starts from the clone and writes an edit that passes verify
/// into it, unstaged; one that fails verify is discarded and the clone keeps
/// none of it.
#[test]
fn a_local_dispatch_lands_verified_edits_unstaged() {
    let s = Scratch::new("dispatchlocal");
    let (env, _, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    // Both start from the same tree, which lacks hello.txt, so the second
    // task's verify fails.
    let out = env.ok(&["dispatch", "alpha:5", "alpha:7", "--local", "--separate"]);
    assert!(out.contains("#1 alpha: applied to"), "{out}");
    assert!(out.contains("#2 alpha: discarded: verify failed"), "{out}");
    assert!(out.contains("1 applied unstaged, 1 not"), "{out}");
    assert_eq!(fs::read_to_string(alpha.join("hello.txt")).unwrap(), "hi\n");
    assert!(!alpha.join("second.txt").exists());
    let staged = command("git")
        .args([
            "-C",
            alpha.to_str().unwrap(),
            "diff",
            "--cached",
            "--name-only",
        ])
        .output()
        .unwrap();
    assert!(staged.stdout.is_empty(), "nothing is staged");
    assert_eq!(
        fs::read_to_string(&env.push_log).unwrap(),
        "blocked\nblocked\n"
    );
    // The discarded edit keeps its patch, and its review still shows it.
    let patch = env.home.join("state/runs/2/change.patch");
    assert!(
        fs::read_to_string(&patch).unwrap().contains("+two"),
        "{patch:?}"
    );
    let detail = env.ok(&["review", "2"]);
    assert!(
        detail.contains("rejected") && detail.contains("+two"),
        "{detail}"
    );
}

/// A clone run edits the clone itself, from its working tree as it is, and
/// waits there for review: nothing staged, the user's own files untouched. A
/// second run on the clone is refused until the first is decided. Approval
/// stages exactly the reviewed change, task tick included, and `pma push`
/// commits and pushes it; a rejection reverts the change from the working
/// tree, keeps its patch, and the diff still reads.
#[test]
fn a_clone_run_edits_the_clone_and_waits_for_review() {
    let s = Scratch::new("clonerun");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    env.ok(&["config", "workspace", "clone"]);
    fs::write(alpha.join("mine.txt"), "my own work\n").unwrap();

    let out = env.ok(&["dispatch", "alpha:5"]);
    assert!(
        out.contains("#1 alpha: edited") && out.contains("in place"),
        "{out}"
    );
    assert_eq!(fs::read_to_string(alpha.join("hello.txt")).unwrap(), "hi\n");
    assert!(git_out(&alpha, &["diff", "--cached", "--name-only"]).is_empty());
    let status = git_out(&alpha, &["status", "--short"]);
    assert_eq!(status, "?? hello.txt\n?? mine.txt\n", "{status}");
    assert_eq!(fs::read_to_string(&env.push_log).unwrap(), "blocked\n");

    let detail = env.ok(&["review", "1"]);
    assert!(detail.contains("+hi"), "{detail}");
    assert!(!detail.contains("my own work"), "{detail}");
    assert!(
        detail.contains("`make test`: base FAILED, head passed"),
        "{detail}"
    );
    assert!(detail.contains("in clone"), "{detail}");

    let (_, err, success) = env.run(&["dispatch", "alpha:7"]);
    assert!(!success && err.contains("holds run #1"), "{err}");

    env.ok(&["review", "1", "--approve"]);
    let (out, err, success) = env.run(&["pr", "1"]);
    assert!(!success && out.contains("--worktree"), "{out}{err}");
    let out = env.ok(&["push", "1"]);
    assert!(
        out.contains("staged in") && out.contains("committed") && out.contains("pushed"),
        "{out}"
    );
    let pushed = git_out(&origin, &["show", "main:hello.txt"]);
    assert_eq!(pushed, "hi\n");
    assert!(
        git_out(&origin, &["show", "main:TODO.md"]).contains("- [x] add greeting"),
        "the task is ticked in the commit"
    );
    assert_eq!(
        git_out(&alpha, &["rev-parse", "HEAD"]),
        git_out(&origin, &["rev-parse", "main"])
    );
    let status = git_out(&alpha, &["status", "--short"]);
    assert_eq!(status, "?? mine.txt\n", "{status}");

    // The clone is free again. This run passes verify, since hello.txt is
    // there now, and is rejected anyway.
    env.ok(&["scan", "--offline"]);
    let out = env.ok(&["dispatch", "alpha:7"]);
    assert!(out.contains("#2 alpha: edited"), "{out}");
    assert_eq!(
        fs::read_to_string(alpha.join("second.txt")).unwrap(),
        "two\n"
    );
    let out = env.ok(&["review", "2", "--reject"]);
    assert!(
        out.starts_with("Run #2 rejected; reverted 1 file in "),
        "{out}"
    );
    assert!(
        !alpha.join("second.txt").exists(),
        "the rejection reverts it"
    );
    let status = git_out(&alpha, &["status", "--short"]);
    assert_eq!(status, "?? mine.txt\n", "{status}");
    let patch = env.home.join("state/runs/2/change.patch");
    assert!(
        fs::read_to_string(&patch).unwrap().contains("+two"),
        "the patch stays"
    );
    let detail = env.ok(&["review", "2"]);
    assert!(
        detail.contains("rejected") && detail.contains("+two"),
        "{detail}"
    );
}

/// `--push` approves and pushes a run whose gates are clean, from the clone,
/// and leaves one that is not in review.
#[test]
fn push_approves_and_pushes_clean_runs() {
    let s = Scratch::new("clonepush");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    env.ok(&["config", "workspace", "clone"]);

    let out = env.ok(&["dispatch", "alpha:5", "--push"]);
    assert!(out.contains("pushed"), "{out}");
    assert_eq!(git_out(&origin, &["show", "main:hello.txt"]), "hi\n");
    assert!(git_out(&alpha, &["status", "--short"]).is_empty());

    // Green at the base and green at the head proves nothing for specified
    // work, so this one waits.
    env.ok(&["scan", "--offline"]);
    let out = env.ok(&["dispatch", "alpha:7", "--push"]);
    assert!(
        out.contains("waits in `pma review`") && out.contains("base already passed"),
        "{out}"
    );
    assert_eq!(
        fs::read_to_string(alpha.join("second.txt")).unwrap(),
        "two\n"
    );
    assert!(env.ok(&["review"]).contains("#2  ready"));
}

#[test]
fn dispatch_review_rework_and_push() {
    let s = Scratch::new("stage3");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);

    // Both runs start from origin; the second fails its verify.
    let out = env.ok(&["dispatch", "--auto", "-n", "2"]);
    assert!(
        out.contains("#1 alpha: add greeting\n#2 alpha: second task\n"),
        "{out}"
    );
    assert!(
        out.ends_with("2 ready, 0 failed, $0.20 spent; see `pma review`\n"),
        "{out}"
    );
    assert_eq!(
        fs::read_to_string(&env.push_log).unwrap(),
        "blocked\nblocked\n"
    );
    let (_, err, success) = env.run(&["dispatch", "alpha:5"]);
    assert!(!success && err.contains("already has a run"), "{err}");

    let list = env.ok(&["review"]);
    assert!(
        list.contains("#1  ready  alpha  verify ok      $0.10"),
        "{list}"
    );
    assert!(
        list.contains("#2  ready  alpha  verify FAILED  $0.10"),
        "{list}"
    );
    let detail = env.ok(&["review", "1"]);
    assert!(detail.starts_with("#1 ready  alpha  Q2\n"), "{detail}");
    // Class B, a base that failed and a head that passes: the check
    // discriminates, so nothing objects.
    assert!(
        detail.contains("accept: every recorded gate is clean"),
        "{detail}"
    );
    assert!(list.contains("clean"), "{list}");
    // #2 failed its verify, so it is not.
    let failed = env.ok(&["review", "2"]);
    assert!(
        failed.contains("read this run because:\n  - verify failed at the head"),
        "{failed}"
    );
    // The test needs hello.txt, which the base does not have: the agent
    // fixed a failing tree rather than leaving a green one green.
    assert!(
        detail.contains("`make test`: base FAILED, head passed"),
        "{detail}"
    );
    assert!(detail.contains("+hi"), "{detail}");
    // #1 names hello.txt and carries a description, in a small repository
    // with a working check. #2 is two words with neither.
    assert!(detail.contains("2 of 5 (v1)"), "{detail}");
    assert!(failed.contains("4 of 5 (v1)"), "{failed}");
    let (out, _, _) = env.run(&["review", "2", "--approve"]);
    assert_eq!(out, "");

    // Rework keeps the worktree and adds the reviewer's fix.
    let out = env.ok(&["review", "2", "--rework", "tests need hello.txt"]);
    assert!(out.contains("verify ok  $0.20"), "{out}");

    // The rework does not overwrite the failed attempt that preceded it.
    let detail = env.ok(&["review", "2", "--minutes", "7"]);
    let attempts = detail
        .split_once("attempts:\n")
        .unwrap_or_default()
        .1
        .lines()
        .take(2)
        .collect::<Vec<_>>();
    assert!(attempts[0].contains("verify FAILED"), "{detail}");
    assert!(attempts[1].contains("verify passed"), "{detail}");
    assert!(
        detail.contains("reviewed") && detail.contains("7m00s"),
        "{detail}"
    );
    // A run seen twice sums its review time.
    let detail = env.ok(&["review", "2", "--minutes", "3"]);
    assert!(detail.contains("10m00s"), "{detail}");
    // One attempt says nothing the rows above it do not.
    assert!(!env.ok(&["review", "1"]).contains("attempts:"));

    env.ok(&["review", "1", "--approve"]);
    // Approving an approved run re-takes its evidence rather than refusing.
    env.ok(&["review", "2", "--approve"]);
    env.ok(&["review", "2", "--approve"]);
    let (_, err, success) = env.run(&["review", "9", "--approve"]);
    assert!(!success && err.contains("no run #9"), "{err}");

    // An item that exists only in the local file cannot be dispatched.
    let local = fs::read_to_string(alpha.join("TODO.md")).unwrap().replace(
        "- [ ] third task #agent\n",
        "- [ ] third task #agent\n- [ ] local only #agent\n",
    );
    fs::write(alpha.join("TODO.md"), local).unwrap();
    env.ok(&["scan", "--offline"]);
    let (_, err, success) = env.run(&["dispatch", "alpha:9"]);
    assert!(
        !success && err.contains("commit and push it first"),
        "{err}"
    );
    assert!(!env.home.join("state/worktrees/alpha/local-only").exists());

    // A rejected run leaves no worktree or branch.
    env.ok(&["dispatch", "alpha:8"]);
    env.ok(&["review", "3", "--reject"]);
    assert!(!env.home.join("state/worktrees/alpha/third-task").exists());
    assert_eq!(git_out(&alpha, &["branch", "--list", "pma/third-task"]), "");

    // `git apply --index` refuses a file with unstaged edits, and the run
    // stays approved.
    let (out, _, success) = env.run(&["push", "1"]);
    assert!(
        !success && out.contains("TODO.md: does not match index"),
        "{out}"
    );
    git(&alpha, &["checkout", "-q", "--", "TODO.md"], None);
    let (out, _, success) = env.run(&["push", "1", "2"]);
    assert!(!success, "{out}");
    assert!(out.contains("#1 alpha: staged in "), "{out}");
    assert!(out.contains("; committed "), "{out}");
    assert!(out.contains("; pushed "), "{out}");
    // #2 shares #1's base, which the clone has moved past. Nothing checked
    // it against #1, so it is refused rather than rebased.
    assert!(out.contains("not run #2's base"), "{out}");
    assert!(out.contains("Publish it with `pma pr 2`"), "{out}");
    let log = git_out(&origin, &["log", "--format=%B|", "main"]);
    assert_eq!(log, "add greeting\n|\ninit\n|\n");
    // The clone holds what was pushed.
    assert_eq!(
        git_out(&alpha, &["rev-parse", "HEAD"]),
        git_out(&origin, &["rev-parse", "main"])
    );
    assert_eq!(
        git_out(&origin, &["show", "main:TODO.md"]),
        "# TODO\n\n## High\n\n- [x] add greeting #agent\n  say hello in hello.txt\n- [ ] second task #agent gh:7\n- [ ] third task #agent\n\n## Low\n\n- [ ] guarded task #manual\n"
    );
    assert_eq!(git_out(&origin, &["branch", "--list", "agent"]), "");
    assert!(!env.home.join("state/worktrees/alpha/add-greeting").exists());
    assert_eq!(
        git_out(&alpha, &["branch", "--list", "pma/add-greeting"]),
        ""
    );
    let (_, err, success) = env.run(&["push", "1"]);
    assert!(
        !success
            && err.contains(
                "run #1 is pushed; `pma push` takes an approved, staged or committed run"
            ),
        "{err}"
    );

    // `third task` failed its verify and was then rejected, so it is not
    // dispatched again without an explicit reset.
    let (_, err, success) = env.run(&["dispatch", "alpha:8"]);
    assert!(
        !success && err.contains("2 attempts on `third task` were used without an accepted result"),
        "{err}"
    );

    // A run whose budget would exceed the batch's does not start, and a run
    // that never reached the agent consumes no attempt.
    env.ok(&["config", "batch_budget", "0.05"]);
    let out = env.ok(&["dispatch", "alpha:8", "--retry"]);
    assert!(
        out.contains("not started: batch budget $0.05 reached"),
        "{out}"
    );
    assert!(
        out.ends_with("0 ready, 1 failed, $0.00 spent; see `pma review`\n"),
        "{out}"
    );
    // Four runs: one pushed, one approved, `third task` rejected, and one refused at the
    // batch budget, which is not decided because no one judged it. One passed
    // on its first attempt and one after a rework, so four attempts in all,
    // and the refused run reported no cost.
    let report = env.ok(&["report"]);
    assert!(
        report.contains("67% of 3 decided runs accepted"),
        "{report}"
    );
    let b: Vec<&str> = report
        .lines()
        .find(|l| l.starts_with("B "))
        .unwrap()
        .split_whitespace()
        .collect();
    assert_eq!(
        &b[..9],
        ["B", "4", "1", "2", "1", "0", "0", "4", "$0.40+1?"],
        "{report}"
    );
    assert!(report.contains("10m00s"), "review time: {report}");
    assert!(
        env.ok(&["report", "--by", "project"])
            .contains("by project"),
        "{report}"
    );
    let (_, err, ok) = env.run(&["report", "--by", "model"]);
    assert!(!ok && err.contains("unknown dimension `model`"), "{err}");

    // The budget refusal never reached the agent, so `--retry` reset nothing,
    // and neither it nor its rejection uses an attempt.
    env.ok(&["config", "batch_budget", "5.0"]);
    env.ok(&["review", "4", "--reject"]);
    let (_, err, success) = env.run(&["dispatch", "alpha:8"]);
    assert!(!success && err.contains("2 attempts on"), "{err}");
    // A retry that starts resets the count: the two runs after it each use one.
    env.ok(&["dispatch", "alpha:8", "--retry"]);
    env.ok(&["review", "5", "--reject"]);
    env.ok(&["dispatch", "alpha:8"]);
    env.ok(&["review", "6", "--reject"]);
    let (_, err, success) = env.run(&["dispatch", "alpha:8"]);
    assert!(!success && err.contains("2 attempts on"), "{err}");
}

/// A rejection put down to infrastructure judges neither the agent nor the
/// task: it is in no share, and gives back the attempt its failed check used.
#[test]
fn an_infrastructure_rejection_uses_no_attempt_and_is_in_no_share() {
    let s = Scratch::new("infra");
    let (env, _, _) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);

    // `third task` fails its check, which uses one of its two attempts.
    env.ok(&["dispatch", "alpha:8"]);
    env.ok(&["review", "1", "--reject", "--infra"]);
    let report = env.ok(&["report"]);
    assert!(report.starts_with("nothing decided yet"), "{report}");
    assert!(
        report.contains("rejected for infrastructure, in no share: #1"),
        "{report}"
    );

    // Both attempts are still there: the check and a plain rejection.
    env.ok(&["dispatch", "alpha:8"]);
    env.ok(&["review", "2", "--reject"]);
    let (_, err, success) = env.run(&["dispatch", "alpha:8"]);
    assert!(!success && err.contains("2 attempts"), "{err}");
    assert!(
        env.ok(&["report"])
            .starts_with("0% of 1 decided run accepted"),
        "the plain rejection counts"
    );

    let (_, err, success) = env.run(&["review", "2", "--infra"]);
    assert!(!success && err.contains("--reject"), "{err}");
}

/// A stand-in for `gh pr`, driven by files in `$PMA_HOME`: `pr-state` for
/// `pr view`, `pr-list` for an open pull request, `pr-create-fails`.
const FAKE_GH_PR: &str = r#"#!/bin/sh
case "$1 $2" in
  "pr create")
    if [ -e "$PMA_HOME/pr-create-fails" ]; then echo "HTTP 502" >&2; exit 1; fi
    printf '%s\n' "$@" > "$PMA_HOME/pr-create-args"
    echo "https://github.com/me/alpha/pull/1" ;;
  "pr list") if [ -e "$PMA_HOME/pr-list" ]; then cat "$PMA_HOME/pr-list"; fi ;;
  "pr view") cat "$PMA_HOME/pr-state" ;;
  "pr merge") printf '%s\n' "$@" > "$PMA_HOME/pr-merge-args" ;;
  *) echo "fake gh: unexpected $*" >&2; exit 1 ;;
esac
"#;

/// Puts a commit on origin's default branch that conflicts with `hello.txt`
/// on a run's branch.
fn conflict_on_origin(s: &Scratch, origin: &std::path::Path) {
    let up = s.0.join("up");
    git(&s.0, &["clone", "-q", origin.to_str().unwrap(), "up"], None);
    fs::write(up.join("hello.txt"), "theirs\n").unwrap();
    git(&up, &["add", "."], None);
    git(&up, &["commit", "-qm", "theirs"], None);
    git(&up, &["push", "-q", "origin", "main"], None);
}

#[test]
fn a_pull_request_merges_or_has_its_conflicts_fixed() {
    let s = Scratch::new("pr-merge");
    let (env, origin, _alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE), ("gh", FAKE_GH_PR)]);
    let pr_state = env.home.join("pr-state");

    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    env.ok(&["pr", "1"]);
    let tip = git_out(&origin, &["rev-parse", "pma/add-greeting"]);
    conflict_on_origin(&s, &origin);

    // A conflict refuses the merge and names the fix.
    fs::write(&pr_state, "CONFLICTING\n").unwrap();
    let (_, err, success) = env.run(&["merge", "1"]);
    assert!(
        !success && err.contains("has conflicts") && err.contains("--fix"),
        "{err}"
    );
    assert!(!env.home.join("pr-merge-args").exists());

    // The fix returns the run to review, on the new base.
    env.ok(&["merge", "1", "--fix"]);
    let out = env.ok(&["review", "1"]);
    assert!(out.starts_with("#1 ready"), "{out}");

    // Publishing moves the same branch to one commit on the new base.
    env.ok(&["review", "1", "--approve"]);
    env.ok(&["pr", "1"]);
    let log = git_out(&origin, &["log", "--format=%s", "main..pma/add-greeting"]);
    assert_eq!(log.lines().count(), 1, "{log}");
    assert_ne!(git_out(&origin, &["rev-parse", "pma/add-greeting"]), tip);
    assert_eq!(
        git_out(&origin, &["merge-base", "main", "pma/add-greeting"]),
        git_out(&origin, &["rev-parse", "main"])
    );
    let todo = git_out(&origin, &["show", "pma/add-greeting:TODO.md"]);
    assert!(todo.contains("- [x] add greeting"), "{todo}");

    fs::write(&pr_state, "MERGEABLE\n").unwrap();
    assert!(env.ok(&["merge", "1"]).contains("pull request merged"));
    let args = fs::read_to_string(env.home.join("pr-merge-args")).unwrap();
    assert!(args.contains("--squash"), "{args}");
    assert!(env.ok(&["review", "1"]).starts_with("#1 merged"));
    let (_, err, success) = env.run(&["merge", "1"]);
    assert!(!success && err.contains("no open pull request"), "{err}");
}

/// A conflict fix publishes only to its pull request, and rejecting it keeps
/// the pull request tracked.
#[test]
fn a_conflict_fix_stays_with_its_pull_request() {
    let s = Scratch::new("pr-keep");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE), ("gh", FAKE_GH_PR)]);
    let pr_state = env.home.join("pr-state");
    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    env.ok(&["pr", "1"]);
    let tip = git_out(&origin, &["rev-parse", "pma/add-greeting"]);
    conflict_on_origin(&s, &origin);
    let main = git_out(&origin, &["rev-parse", "main"]);
    fs::write(&pr_state, "CONFLICTING\n").unwrap();

    env.ok(&["merge", "1", "--fix"]);
    env.ok(&["review", "1", "--approve"]);
    assert!(env.ok(&["next"]).contains("pma pr 1"));
    assert!(!env.ok(&["next"]).contains("pma push 1"));
    for verb in ["stage", "commit", "push"] {
        let (out, err, success) = env.run(&[verb, "1"]);
        assert!(!success, "{verb}: {out}");
        assert!(
            format!("{out}{err}").contains("`pma pr 1` updates it"),
            "{verb}: {out}{err}"
        );
    }
    assert_eq!(git_out(&origin, &["rev-parse", "main"]), main);
    assert_eq!(git_out(&alpha, &["diff", "--cached"]), "");

    // Rejecting drops the fix alone: the task is held and no attempt charged.
    let out = env.ok(&["review", "1", "--reject"]);
    assert!(
        out.contains("its pull request stays open: https://"),
        "{out}"
    );
    assert!(env.ok(&["review", "1"]).starts_with("#1 pr-open"));
    assert_eq!(git_out(&origin, &["rev-parse", "pma/add-greeting"]), tip);
    assert_eq!(git_out(&alpha, &["for-each-ref", "refs/pma"]), "");
    let (_, err, success) = env.run(&["dispatch", "alpha:5"]);
    assert!(!success && err.contains("already has a run"), "{err}");

    // It stays settled from GitHub, and can be fixed again.
    env.ok(&["merge", "1", "--fix"]);
    assert!(env.ok(&["review", "1"]).starts_with("#1 ready"));
    env.ok(&["review", "1", "--reject"]);
    fs::write(&pr_state, "CLOSED\n").unwrap();
    env.ok(&["review"]);
    assert!(env.ok(&["review", "1"]).starts_with("#1 closed"));
}

/// A commit pushed to the pull request after the fix read it is not lost.
#[test]
fn a_conflict_fix_does_not_push_over_a_newer_commit() {
    let s = Scratch::new("pr-lease");
    let (env, origin, _alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE), ("gh", FAKE_GH_PR)]);
    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    env.ok(&["pr", "1"]);
    conflict_on_origin(&s, &origin);
    fs::write(env.home.join("pr-state"), "CONFLICTING\n").unwrap();
    env.ok(&["merge", "1", "--fix"]);

    let up = s.0.join("up");
    git(&up, &["fetch", "-q", "origin"], None);
    git(
        &up,
        &[
            "checkout",
            "-q",
            "-b",
            "pma/add-greeting",
            "origin/pma/add-greeting",
        ],
        None,
    );
    fs::write(up.join("human.txt"), "mine\n").unwrap();
    git(&up, &["add", "."], None);
    git(&up, &["commit", "-qm", "human"], None);
    git(&up, &["push", "-q", "origin", "pma/add-greeting"], None);
    let human = git_out(&origin, &["rev-parse", "pma/add-greeting"]);

    env.ok(&["review", "1", "--approve"]);
    let (_, _, success) = env.run(&["pr", "1"]);
    assert!(!success);
    assert_eq!(git_out(&origin, &["rev-parse", "pma/add-greeting"]), human);
}

/// An agent that leaves conflict markers fails the run rather than reaching
/// review.
#[test]
fn conflict_markers_left_behind_fail_the_run() {
    const IDLE_ON_FIX: &str = r#"#!/bin/sh
case "$2" in
  *"reviewer's feedback"*) ;;
  *) echo hi > hello.txt ;;
esac
echo '{"type":"result","subtype":"success","is_error":false,"result":"did it","total_cost_usd":0.1}'
"#;
    let s = Scratch::new("pr-markers");
    let (env, origin, _alpha) = dispatch_env(&s, &[("claude", IDLE_ON_FIX), ("gh", FAKE_GH_PR)]);
    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    env.ok(&["pr", "1"]);
    conflict_on_origin(&s, &origin);
    fs::write(env.home.join("pr-state"), "CONFLICTING\n").unwrap();
    env.ok(&["merge", "1", "--fix"]);
    let out = env.ok(&["review", "1"]);
    assert!(out.starts_with("#1 failed"), "{out}");
    assert!(
        out.contains("conflict markers remain in hello.txt"),
        "{out}"
    );

    // Retrying runs the fix again rather than dispatching a second run.
    env.ok(&["review", "1", "--retry"]);
    let out = env.ok(&["review", "1"]);
    assert!(out.starts_with("#1 failed"), "{out}");
    let (_, err, success) = env.run(&["review", "2"]);
    assert!(!success, "no second run: {err}");
    env.ok(&["review", "1", "--reject"]);
    assert!(env.ok(&["review", "1"]).starts_with("#1 pr-open"));
}

#[test]
fn a_pull_request_holds_its_task_until_merged_or_closed() {
    let s = Scratch::new("pr");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE), ("gh", FAKE_GH_PR)]);
    let pr_state = env.home.join("pr-state");

    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    let out = env.ok(&["pr", "1"]);
    assert_eq!(out, "#1 alpha: https://github.com/me/alpha/pull/1\n");
    // The body names the run and says what verify made of it.
    let args = fs::read_to_string(env.home.join("pr-create-args")).unwrap();
    let body = args
        .split_once("--body\n")
        .map(|(_, b)| b)
        .unwrap_or_default();
    assert!(
        body.starts_with("pma run #1: add greeting") && body.contains("## Verify"),
        "{args}"
    );
    assert_eq!(
        git_out(&origin, &["branch", "--list", "pma/*"]),
        "  pma/add-greeting\n"
    );

    fs::write(&pr_state, "OPEN\n").unwrap();
    assert!(env.ok(&["review"]).contains("#1  pr-open  alpha"));
    let out = env.ok(&["dispatch", "--auto", "-n", "1"]);
    assert!(out.contains("#2 alpha: second task"), "{out}");
    env.ok(&["review", "2", "--reject"]);
    let (_, err, success) = env.run(&["dispatch", "alpha:5"]);
    assert!(!success && err.contains("already has a run"), "{err}");
    let (_, err, success) = env.run(&["review", "1", "--reject"]);
    assert!(
        !success && err.contains("open pull request; merge or close it"),
        "{err}"
    );

    // A closed pull request frees the task. Its remote branch stays, so the
    // new run takes another name.
    fs::write(&pr_state, "CLOSED\n").unwrap();
    let out = env.ok(&["review"]);
    assert!(
        out.starts_with("#1 alpha: pull request closed without merging: https://"),
        "{out}"
    );
    assert!(
        env.ok(&["review", "1"]).starts_with("#1 closed  alpha"),
        "a closed pull request is `closed`, not rejected"
    );
    let out = env.ok(&["dispatch", "alpha:5"]);
    assert!(out.contains("#3 alpha: add greeting"), "{out}");
    assert!(env.ok(&["review", "3"]).contains("pma/add-greeting-2 in"));

    env.ok(&["review", "3", "--approve"]);
    env.ok(&["pr", "3"]);
    // An open pull request someone has reviewed says so, as GitHub does.
    fs::write(&pr_state, "OPEN\tCHANGES_REQUESTED\t2\t1\n").unwrap();
    let out = env.ok(&["review"]);
    assert!(
        out.starts_with(
            "#3 alpha: pull request open: changes requested, 2 comments, 1 review: https://"
        ),
        "{out}"
    );
    assert!(out.contains("#3  pr-open"), "{out}");
    fs::write(&pr_state, "MERGED\n").unwrap();
    let out = env.ok(&["review"]);
    assert_eq!(
        out,
        "#3 alpha: pull request merged: https://github.com/me/alpha/pull/1\nno runs to review\n"
    );
    assert!(!alpha.join("hello.txt").exists(), "the clone is untouched");
    assert!(env.ok(&["review", "3"]).starts_with("#3 merged  alpha"));
}

/// Gives up the first time it is run, then writes hello.txt.
const GIVING_UP_CLAUDE: &str = r#"#!/bin/sh
if [ ! -e "$PMA_HOME/tried" ]; then
  touch "$PMA_HOME/tried"
  printf '{"type":"result","subtype":"error","is_error":true,"result":"gave up","total_cost_usd":0.1}\n'
  exit 0
fi
echo hi > hello.txt
printf '{"type":"result","subtype":"success","is_error":false,"result":"did it","total_cost_usd":0.1}\n'
"#;

/// `--retry` rejects a failed run and dispatches its task again, with the
/// agent the run had; a run that has not failed is refused.
#[test]
fn a_failed_run_is_retried_as_it_was() {
    let s = Scratch::new("retry");
    let (env, _origin, _alpha) = dispatch_env(&s, &[("claude", GIVING_UP_CLAUDE)]);
    env.ok(&["dispatch", "alpha:5"]);
    let shown = env.ok(&["review", "1"]);
    assert!(shown.starts_with("#1 failed"), "{shown}");
    let out = env.ok(&["review", "1", "--retry"]);
    assert!(out.starts_with("Run #1 rejected;"), "{out}");
    assert!(out.contains("#2 alpha: add greeting"), "{out}");
    assert!(env.ok(&["review", "2"]).starts_with("#2 ready"));
    assert!(env.ok(&["review", "1"]).starts_with("#1 rejected"));
    let (_, err, success) = env.run(&["review", "2", "--retry", "--approve"]);
    assert!(!success && err.contains("cannot be used with"), "{err}");
    let (_, err, success) = env.run(&["review", "2", "--retry"]);
    assert!(
        !success && err.contains("--retry takes a failed run"),
        "{err}"
    );
}

/// Does every task its prompt names, as one agent session would.
const BUNDLE_CLAUDE: &str = r#"#!/bin/sh
case "$2" in *"add greeting"*) echo hi > hello.txt ;; esac
case "$2" in *"second task"*) echo two > second.txt ;; esac
case "$2" in *"third task"*) echo three > third.txt ;; esac
echo '{"type":"result","subtype":"success","is_error":false,"result":"1 and 2 done; 3 half done","total_cost_usd":0.1}'
"#;

/// Items of one project named together are one run, a task named twice once.
/// An item origin does not hold open is left out. The approver drops the
/// items not done: only the rest are ticked and closed, though the whole diff
/// is published. Staging is no way around the approval.
#[test]
fn a_bundle_is_one_run_and_its_approver_says_what_it_closes() {
    let s = Scratch::new("bundle");
    let (env, origin, _) = dispatch_env(&s, &[("claude", BUNDLE_CLAUDE)]);
    let (out, err, success) = env.run(&["dispatch", "alpha:5", "alpha:high", "alpha:12"]);
    assert!(success, "{out}{err}");
    assert!(
        out.starts_with("#1 alpha: add greeting; second task; third task\n"),
        "{out}"
    );
    assert!(
        err.contains(
            "warning: alpha: `guarded task` is tagged #manual and is not dispatched; left out"
        ),
        "{err}"
    );

    let detail = env.ok(&["review", "1"]);
    assert!(
        detail.contains("items:\n  1. add greeting\n  2. second task\n  3. third task\n"),
        "{detail}"
    );
    assert!(detail.contains("a bundle of 3 items"), "{detail}");
    for added in ["+hi", "+two", "+three"] {
        assert!(detail.contains(added), "{detail}");
    }
    let (_, err, success) = env.run(&["dispatch", "alpha:7"]);
    assert!(!success && err.contains("already has a run"), "{err}");

    let (out, _, success) = env.run(&["stage", "1"]);
    assert!(
        !success && out.contains("run #1 is a bundle; approve it first"),
        "{out}"
    );
    let (_, err, success) = env.run(&["review", "1", "--approve", "--drop", "4"]);
    assert!(
        !success && err.contains("run #1 has items 1 to 3; there is no item 4"),
        "{err}"
    );
    let (_, err, success) = env.run(&["review", "1", "--approve", "--drop", "1,2,3"]);
    assert!(!success && err.contains("reject the run instead"), "{err}");
    env.ok(&["review", "1", "--approve", "--drop", "3"]);
    let detail = env.ok(&["review", "1"]);
    assert!(
        detail.contains("  3. third task  (left open)\n"),
        "{detail}"
    );

    env.ok(&["push", "1"]);
    let todo = git_out(&origin, &["show", "main:TODO.md"]);
    assert!(
        todo.contains("- [x] add greeting")
            && todo.contains("- [x] second task")
            && todo.contains("- [ ] third task"),
        "{todo}"
    );
    let message = git_out(&origin, &["log", "-1", "--format=%B", "main"]);
    assert!(
        message.starts_with("2 TODO items\n\n- add greeting\n- second task\n\nCloses #7"),
        "{message}"
    );
    // A dropped item's changes are published with the rest.
    assert_eq!(git_out(&origin, &["show", "main:third.txt"]), "three\n");
}

/// A bundle's failures count against the bundle, which stops at the attempt
/// limit as one task does, until --retry.
#[test]
fn a_bundle_stops_at_its_attempt_limit() {
    let s = Scratch::new("bundle-attempts");
    let (env, _, _) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    // The check fails, which uses one attempt; the rejection uses the other.
    env.ok(&["dispatch", "alpha:7", "alpha:8"]);
    env.ok(&["review", "1", "--reject"]);
    let (_, err, success) = env.run(&["dispatch", "alpha:7", "alpha:8"]);
    assert!(
        !success && err.contains("2 attempts on the bundle `second task; third task`"),
        "{err}"
    );
    let out = env.ok(&["dispatch", "alpha:7", "alpha:8", "--retry"]);
    assert!(out.contains("#2 alpha: second task; third task"), "{out}");
}

/// The clone holds one run, so a bundle is how several items run there at
/// all; one run each is refused after the first.
#[test]
fn a_bundle_runs_in_the_clone() {
    let s = Scratch::new("bundle-clone");
    let (env, _, alpha) = dispatch_env(&s, &[("claude", BUNDLE_CLAUDE)]);
    env.ok(&["config", "workspace", "clone"]);
    let (_, err, _) = env.run(&["dispatch", "alpha:5", "alpha:7", "--separate"]);
    assert!(err.contains("its clone holds run #1"), "{err}");
    env.ok(&["review", "1", "--reject"]);

    let (out, err, success) = env.run(&["dispatch", "alpha:5", "alpha:7"]);
    assert!(success && !err.contains("holds run"), "{out}{err}");
    assert!(
        out.starts_with("#2 alpha: add greeting; second task\n"),
        "{out}"
    );
    assert_eq!(fs::read_to_string(alpha.join("hello.txt")).unwrap(), "hi\n");
    assert_eq!(
        fs::read_to_string(alpha.join("second.txt")).unwrap(),
        "two\n"
    );
}

/// A failed bundle is retried as the same bundle.
#[test]
fn a_failed_bundle_is_retried_as_a_bundle() {
    let s = Scratch::new("bundle-retry");
    let (env, _, _) = dispatch_env(&s, &[("claude", GIVING_UP_CLAUDE)]);
    env.ok(&["dispatch", "alpha:5", "alpha:7"]);
    assert!(env.ok(&["review", "1"]).starts_with("#1 failed"));
    let out = env.ok(&["review", "1", "--retry"]);
    assert!(out.contains("#2 alpha: add greeting; second task"), "{out}");
    assert!(env.ok(&["review", "2"]).contains("  2. second task\n"));
}

/// A prompt runs over a whole project in place of a task: reviewed and
/// published as any run, with nothing ticked. A failed one is retried with
/// the prompt it kept.
#[test]
fn a_prompt_is_a_run_over_the_project() {
    let s = Scratch::new("prompt");
    let (env, origin, _) = dispatch_env(&s, &[("claude", GIVING_UP_CLAUDE)]);
    for (args, why) in [
        (
            vec!["alpha:5"],
            "runs over a whole project; name `alpha` alone",
        ),
        (vec!["alpha", "beta"], "runs over one project"),
        (vec!["alpha", "--auto"], "cannot be used with"),
    ] {
        let mut argv = vec!["dispatch", "--prompt", "add greeting"];
        argv.extend(args);
        let (_, err, success) = env.run(&argv);
        assert!(!success && err.contains(why), "{argv:?}: {err}");
    }
    let (_, err, success) = env.run(&["dispatch", "alpha", "--prompt", "  "]);
    assert!(!success && err.contains("--prompt needs text"), "{err}");

    // The first attempt gives up; the retry dispatches the same prompt.
    env.ok(&[
        "dispatch",
        "alpha",
        "--prompt",
        "add greeting\nwith a newline at the end",
    ]);
    assert!(env.ok(&["review", "1"]).starts_with("#1 failed"));
    let out = env.ok(&["review", "1", "--retry"]);
    assert!(out.contains("#2 alpha: add greeting"), "{out}");

    env.ok(&["review", "2", "--approve"]);
    env.ok(&["push", "2"]);
    assert_eq!(git_out(&origin, &["show", "main:hello.txt"]), "hi\n");
    assert!(
        git_out(&origin, &["show", "main:TODO.md"]).contains("- [ ] add greeting"),
        "a prompt ticks no item"
    );
    assert_eq!(
        git_out(&origin, &["log", "-1", "--format=%s", "main"]),
        "add greeting\n"
    );
}

/// A pull request closed without merging is a reviewer refusing the work, so
/// it uses an attempt as a rejection does.
#[test]
fn a_closed_pull_request_uses_an_attempt() {
    let s = Scratch::new("pr-closed");
    let (env, _origin, _alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE), ("gh", FAKE_GH_PR)]);
    fs::write(env.home.join("pr-state"), "CLOSED\n").unwrap();
    for id in ["1", "2"] {
        env.ok(&["dispatch", "alpha:5"]);
        env.ok(&["review", id, "--approve"]);
        env.ok(&["pr", id]);
        env.ok(&["review"]);
    }
    let (_, err, success) = env.run(&["dispatch", "alpha:5"]);
    assert!(
        !success && err.contains("2 attempts on `add greeting`"),
        "{err}"
    );
}

/// The agent controls the worktree, so a `TODO.md` it turned into a symlink is
/// refused rather than ticked through.
#[test]
fn publishing_refuses_a_todo_md_the_agent_made_a_symlink() {
    let s = Scratch::new("pr-symlink");
    let outside = s.0.join("outside.md");
    let text = "# TODO\n\n## High\n\n- [ ] add greeting #agent\n";
    fs::write(&outside, text).unwrap();
    let agent = format!(
        "#!/bin/sh\necho hi > hello.txt\nrm TODO.md\nln -s {} TODO.md\n\
         echo '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"ok\",\"total_cost_usd\":0.1}}'\n",
        outside.display()
    );
    let (env, _origin, _alpha) = dispatch_env(&s, &[("claude", &agent), ("gh", FAKE_GH_PR)]);
    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    let (out, err, _) = env.run(&["pr", "1"]);
    assert!(
        format!("{out}{err}").contains("TODO.md: is a symlink"),
        "{out}{err}"
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), text);
}

#[test]
fn publishing_resumes_after_a_partial_failure() {
    let s = Scratch::new("resume");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE), ("gh", FAKE_GH_PR)]);

    // The push succeeds and removing the worktree fails.
    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    let worktree = env.home.join("state/worktrees/alpha/add-greeting");
    git(
        &alpha,
        &["worktree", "lock", worktree.to_str().unwrap()],
        None,
    );
    let out = env.ok(&["push", "1"]);
    assert!(
        out.contains("; warning: worktree not removed: ") && out.contains("; pushed "),
        "{out}"
    );
    assert_eq!(
        env.ok(&["review"]),
        "no runs to review\n",
        "published all the same"
    );

    // As if pma had stopped between the push and recording it. Pushing the
    // same commit again changes nothing, and the worktree goes this time.
    rusqlite::Connection::open(env.home.join("projects.db"))
        .unwrap()
        .execute("UPDATE runs SET state = 'committed' WHERE id = 1", [])
        .unwrap();
    git(
        &alpha,
        &["worktree", "unlock", worktree.to_str().unwrap()],
        None,
    );
    let out = env.ok(&["push", "1"]);
    assert!(out.starts_with("#1 alpha: pushed "), "{out}");
    assert!(!worktree.exists());
    assert_eq!(
        git_out(&origin, &["log", "--format=%s", "main"]),
        "add greeting\ninit\n",
        "pushed once"
    );

    // `gh pr create` fails after the push; the retry finds the pull request.
    env.ok(&["dispatch", "alpha:7"]);
    env.ok(&["review", "2", "--approve"]);
    fs::write(env.home.join("pr-create-fails"), "").unwrap();
    let (out, _, success) = env.run(&["pr", "2"]);
    assert!(!success && out.contains("gh pr create: HTTP 502"), "{out}");
    assert!(env.ok(&["review"]).contains("#2  approved"));
    fs::remove_file(env.home.join("pr-create-fails")).unwrap();
    fs::write(
        env.home.join("pr-list"),
        "https://github.com/me/alpha/pull/9\n",
    )
    .unwrap();
    // Run 1 is pushed, so only run 2 is approved.
    let (_, err, success) = env.run(&["pr", "2", "--all-approved"]);
    assert!(!success && err.contains("cannot be used with"), "{err}");
    assert_eq!(
        env.ok(&["pr", "--all-approved"]),
        "#2 alpha: https://github.com/me/alpha/pull/9\n"
    );
    assert_eq!(env.ok(&["push", "--all-approved"]), "no approved runs\n");
}

/// A stage refused after it ticked the item leaves the worktree as it was
/// approved, so a retry stages the approved tree and the tick, and an edit
/// since approval is still refused.
#[test]
fn a_retried_publish_publishes_only_what_was_approved() {
    let s = Scratch::new("retry-approved");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE), ("gh", FAKE_GH_PR)]);

    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    // Something the user staged would go into the run's commit.
    fs::write(alpha.join("mine.txt"), "mine\n").unwrap();
    git(&alpha, &["add", "mine.txt"], None);
    let (out, _, success) = env.run(&["push", "1"]);
    assert!(!success && out.contains("holds staged changes"), "{out}");
    assert!(env.ok(&["review"]).contains("#1  approved"));
    git(&alpha, &["rm", "-q", "--cached", "mine.txt"], None);

    let worktree = env.home.join("state/worktrees/alpha/add-greeting");
    fs::write(worktree.join("extra.txt"), "extra\n").unwrap();
    let (out, _, success) = env.run(&["push", "1"]);
    assert!(
        !success && out.contains("the worktree changed after it was approved"),
        "{out}"
    );
    fs::remove_file(worktree.join("extra.txt")).unwrap();

    assert!(env.ok(&["push", "1"]).contains("; pushed "));
    assert_eq!(git_out(&origin, &["show", "main:hello.txt"]), "hi\n");
    assert_eq!(
        git_out(&origin, &["ls-tree", "--name-only", "main"]),
        "Makefile\nTODO.md\nhello.txt\n"
    );
    assert!(git_out(&origin, &["show", "main:TODO.md"]).contains("- [x] add greeting"));
    assert_eq!(
        git_out(&origin, &["log", "--format=%s", "main"]),
        "add greeting\ninit\n"
    );
    // The user's untracked file is left alone.
    assert_eq!(
        fs::read_to_string(alpha.join("mine.txt")).unwrap(),
        "mine\n"
    );

    // `pma pr` fails after its push; the retry rebuilds its commit with
    // another date, and finds the same tree already pushed.
    env.ok(&["dispatch", "alpha:7"]);
    env.ok(&["review", "2", "--approve"]);
    fs::write(env.home.join("pr-create-fails"), "").unwrap();
    let (out, _, success) = env.run(&["pr", "2"]);
    assert!(!success && out.contains("gh pr create: HTTP 502"), "{out}");
    let pushed = git_out(&origin, &["rev-parse", "pma/second-task"]);
    fs::remove_file(env.home.join("pr-create-fails")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert_eq!(
        env.ok(&["pr", "2"]),
        "#2 alpha: https://github.com/me/alpha/pull/1\n"
    );
    assert_eq!(
        git_out(&origin, &["rev-parse", "pma/second-task"]),
        pushed,
        "the branch is not pushed again"
    );
}

/// Each command runs the steps before it that the run has not done. Staging
/// is the review, so a ready run stages without approval, and commits only
/// what was staged.
#[test]
fn stage_commit_and_push_follow_git() {
    let s = Scratch::new("stage-commit-push");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    env.ok(&["dispatch", "alpha:5"]);

    let (_, err, success) = env.run(&["commit", "1"]);
    assert!(
        !success && err.contains("run #1 is ready; `pma commit` takes an approved or staged run"),
        "{err}"
    );
    let out = env.ok(&["stage", "1"]);
    assert!(out.starts_with("#1 alpha: staged in "), "{out}");
    assert!(out.ends_with("; next: `pma commit 1`\n"), "{out}");
    assert!(!env.home.join("state/worktrees/alpha/add-greeting").exists());
    assert_eq!(
        git_out(&alpha, &["diff", "--cached", "--name-only"]),
        "TODO.md\nhello.txt\n"
    );
    assert!(
        fs::read_to_string(alpha.join("TODO.md"))
            .unwrap()
            .contains("- [x] add greeting")
    );
    // A staged run holds its task.
    let (_, err, success) = env.run(&["dispatch", "alpha:5"]);
    assert!(!success && err.contains("already has a run"), "{err}");

    // An index that is no longer what was staged is the user's to commit.
    fs::write(alpha.join("hello.txt"), "edited\n").unwrap();
    git(&alpha, &["add", "hello.txt"], None);
    let (out, _, success) = env.run(&["commit", "1"]);
    assert!(
        !success && out.contains("differs from what run #1 staged"),
        "{out}"
    );
    fs::write(alpha.join("hello.txt"), "hi\n").unwrap();
    git(&alpha, &["add", "hello.txt"], None);

    let (_, err, success) = env.run(&["commit", "1", "--subject= "]);
    assert!(!success && err.contains("one line of text"), "{err}");
    let out = env.ok(&["commit", "1", "--subject=Add the greeting"]);
    assert!(out.starts_with("#1 alpha: committed "), "{out}");
    assert!(out.ends_with("; next: `pma push 1`\n"), "{out}");
    assert_eq!(
        git_out(&alpha, &["log", "-1", "--format=%s"]),
        "Add the greeting\n"
    );
    assert_eq!(git_out(&origin, &["log", "--format=%s", "main"]), "init\n");

    // The user's own commit after it is not pushed.
    fs::write(alpha.join("mine.txt"), "mine\n").unwrap();
    git(&alpha, &["add", "mine.txt"], None);
    git(&alpha, &["commit", "-qm", "mine"], None);
    let (out, _, success) = env.run(&["push", "1"]);
    assert!(!success && out.contains("is not run #1's commit"), "{out}");
    git(&alpha, &["reset", "-q", "--hard", "HEAD^"], None);
    let out = env.ok(&["push", "1"]);
    assert!(out.starts_with("#1 alpha: pushed "), "{out}");
    assert_eq!(
        git_out(&origin, &["log", "--format=%s", "main"]),
        "Add the greeting\ninit\n"
    );
}

/// Commits a secret, then deletes it: the net diff is clean.
const COMMITTING_CLAUDE: &str = r#"#!/bin/sh
echo key > secret.txt
git add secret.txt && git commit -qm "wip: secret"
case "$2" in *"second task"*) f=second.txt ;; *) f=hello.txt ;; esac
git rm -q secret.txt && echo hi > $f && git add $f
git commit -qm "tidy up"
echo '{"type":"result","subtype":"success","is_error":false,"result":"did it","total_cost_usd":0.1}'
"#;

/// Review shows the net diff, so only the approved tree is published: one
/// commit on the base, none of the agent's history.
#[test]
fn the_agents_own_commits_are_not_published() {
    let s = Scratch::new("agent-commits");
    let (env, origin, _) = dispatch_env(&s, &[("claude", COMMITTING_CLAUDE), ("gh", FAKE_GH_PR)]);
    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    assert!(env.ok(&["push", "1"]).contains("; pushed "));
    assert_eq!(
        git_out(&origin, &["log", "--format=%s", "main"]),
        "add greeting\ninit\n"
    );
    assert_eq!(
        git_out(
            &origin,
            &["log", "--all", "--format=%s", "--", "secret.txt"]
        ),
        ""
    );

    env.ok(&["dispatch", "alpha:7"]);
    env.ok(&["review", "2", "--approve"]);
    env.ok(&["pr", "2"]);
    assert_eq!(
        git_out(&origin, &["log", "--format=%s", "main..pma/second-task"]),
        "second task\n"
    );
}

/// An approval is of what the reviewer was shown. A write after that, by an
/// agent that outlived its run or by anyone, is read before it is approved.
#[test]
fn an_approval_requires_the_tree_last_shown() {
    let s = Scratch::new("shown");
    let (env, origin, _) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    env.ok(&["dispatch", "alpha:5"]);
    let worktree = env.home.join("state/worktrees/alpha/add-greeting");
    fs::write(worktree.join("late.txt"), "late\n").unwrap();

    // Never shown since the agent finished: the list's diffstat did not
    // include it.
    let (_, err, ok) = env.run(&["review", "1", "--approve"]);
    assert!(
        !ok && err.contains("changed after it was last shown"),
        "{err}"
    );

    // Shown, it is approved and published.
    assert!(env.ok(&["review", "1"]).contains("late.txt"));
    env.ok(&["review", "1", "--approve"]);
    assert!(env.ok(&["push", "1"]).contains("; pushed "));
    assert_eq!(git_out(&origin, &["show", "main:late.txt"]), "late\n");

    // Shown, then written to: refused again.
    env.ok(&["dispatch", "alpha:7"]);
    env.ok(&["review", "2"]);
    let worktree = env.home.join("state/worktrees/alpha/second-task");
    fs::write(worktree.join("second.txt"), "changed\n").unwrap();
    let (_, err, ok) = env.run(&["review", "2", "--approve"]);
    assert!(
        !ok && err.contains("changed after it was last shown"),
        "{err}"
    );
}

/// A worker that changes nothing.
const IDLE_CLAUDE: &str = r#"#!/bin/sh
echo '{"type":"result","subtype":"success","is_error":false,"result":"nothing to do","total_cost_usd":0.1}'
"#;

/// The tick alone is not a change: publishing it would mark the task done
/// with no work in it.
#[test]
fn a_run_with_no_changes_is_not_published() {
    let s = Scratch::new("no-changes");
    let (env, origin, _) = dispatch_env(&s, &[("claude", IDLE_CLAUDE)]);
    env.ok(&["config", "projects.alpha.verify", "none"]);
    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    let (out, _, success) = env.run(&["push", "1"]);
    assert!(
        !success && out.contains("nothing to stage: no changes"),
        "{out}"
    );
    assert_eq!(git_out(&origin, &["log", "--format=%s", "main"]), "init\n");
}

#[test]
fn auto_dispatch_passes_over_refused_tasks_and_names_the_cause() {
    let s = Scratch::new("refused");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);

    // The clone has an unpushed item first, and is behind a tick on origin.
    let todo = fs::read_to_string(alpha.join("TODO.md")).unwrap();
    fs::write(
        alpha.join("TODO.md"),
        todo.replace(
            "- [ ] add greeting",
            "- [ ] local only #agent\n- [ ] add greeting",
        ),
    )
    .unwrap();
    let other = s.0.join("other");
    git(
        &s.0,
        &["clone", "-q", origin.to_str().unwrap(), "other"],
        None,
    );
    let ticked = fs::read_to_string(other.join("TODO.md"))
        .unwrap()
        .replace("- [ ] third task", "- [x] third task");
    fs::write(other.join("TODO.md"), ticked).unwrap();
    git(&other, &["commit", "-qam", "tick"], None);
    git(&other, &["push", "-q"], None);
    env.ok(&["scan", "--offline"]);

    let (out, err, success) = env.run(&["dispatch", "--auto", "-n", "1"]);
    assert!(success, "{err}");
    assert!(
        err.contains("warning: alpha: `local only` is not in TODO.md on origin/main; commit and push it first"),
        "{err}"
    );
    assert!(out.starts_with("#1 alpha: add greeting\n"), "{out}");

    let (_, err, success) = env.run(&["dispatch", "alpha:9"]);
    assert!(
        !success && err.contains("`third task` is already done on origin/main; pull the clone"),
        "{err}"
    );

    // Class D: refused before any worktree exists.
    let (_, err, success) = env.run(&["dispatch", "alpha:13"]);
    assert!(
        !success && err.contains("`guarded task` is class D and is not dispatched"),
        "{err}"
    );
    assert!(!env.home.join("state/worktrees/alpha/guarded-task").exists());
    assert_eq!(
        git_out(&alpha, &["branch", "--list", "pma/guarded-task"]),
        ""
    );
}

/// Every path a run changed is checked, whatever the agent says it did and
/// whatever class the task was predicted to be. A deps task is class A, whose
/// scope is manifests alone.
#[test]
fn the_scope_check_reads_the_whole_change_not_the_agents_report() {
    let s = Scratch::new("scope");
    let (env, _, alpha) = dispatch_env(&s, &[("claude", STRAYING_CLAUDE)]);
    // The moved Makefile takes the test target with it; the scope check is
    // what this test is about.
    env.ok(&["config", "projects.alpha.verify", "none"]);
    fs::write(alpha.join("Cargo.lock"), "lock\n").unwrap();
    fs::write(alpha.join("notes.txt"), "notes\n").unwrap();
    git(&alpha, &["add", "Cargo.lock", "notes.txt"], None);
    git(&alpha, &["commit", "-qm", "lock"], None);
    git(&alpha, &["push", "-q"], None);
    env.ok(&["scan", "--offline"]);
    env.ok(&["dispatch", "alpha:5"]);

    // Five paths from four edits: the untracked workflow, the edited lock
    // file, both sides of the rename, and the deletion. The agent reported
    // only the lock file.
    let detail = env.ok(&["review", "1"]);
    assert!(detail.contains("B, any path"), "{detail}");
    assert!(
        detail.contains("1 of 5 files OUTSIDE: .github/workflows/ci.yml"),
        "{detail}"
    );
    assert!(detail.contains("updated Cargo.lock"), "{detail}");
    // Without a verify command there is no acceptance, whatever the scope.
    assert!(
        detail.contains("  - outside class B: .github/workflows/ci.yml"),
        "{detail}"
    );
    assert!(detail.contains("  - no verify command"), "{detail}");
    assert!(env.ok(&["review"]).contains("2 to read"), "{detail}");
}

/// A second worker runs through the same dispatch, review and push path as
/// A worker that reports no cost is charged its whole budget, so the batch
/// budget still stops it.
#[test]
fn unknown_cost_counts_against_the_batch_budget() {
    let s = Scratch::new("unknown-cost");
    let (env, _, _) = dispatch_env(&s, &[("plain", PLAIN_WORKER)]);
    env.ok(&["agent", "set", "plain", "command", "plain"]);
    env.ok(&[
        "agent",
        "set",
        "plain",
        "args",
        r#"["--task","{prompt}","{dir}"]"#,
    ]);
    env.ok(&["config", "agent", "plain"]);
    env.ok(&["config", "max_parallel", "1"]);
    env.ok(&["config", "agent_budget", "1"]);
    env.ok(&["config", "batch_budget", "1.5"]);
    let out = env.ok(&["dispatch", "--auto", "-n", "2"]);
    assert!(
        out.contains("not started: batch budget $1.5 reached"),
        "{out}"
    );
}

/// `claude`, with no code that knows its name.
#[test]
fn a_worker_without_json_output_or_an_allowlist_runs_the_same_path() {
    let s = Scratch::new("adapter");
    let (env, origin, _) = dispatch_env(&s, &[("plain", PLAIN_WORKER)]);

    let listed = env.ok(&["agent"]);
    assert!(listed.contains("* claude"), "{listed}");
    assert!(listed.contains("cost,allowlist"), "{listed}");

    env.ok(&["agent", "set", "plain", "command", "plain"]);
    env.ok(&[
        "agent",
        "set",
        "plain",
        "args",
        r#"["--task","{prompt}","{dir}"]"#,
    ]);
    env.ok(&["config", "agent", "plain"]);
    let listed = env.ok(&["agent"]);
    assert!(listed.contains("* plain"), "{listed}");
    assert!(listed.contains("text-tail"), "{listed}");

    env.ok(&["dispatch", "alpha:5"]);
    let detail = env.ok(&["review", "1"]);
    // The exit status decided, the tail is the summary, and the cost is
    // unknown rather than zero.
    assert!(detail.contains("plain, cost not reported"), "{detail}");
    assert!(detail.contains("wrote hello.txt in"), "{detail}");
    assert!(
        detail.contains("`make test`: base FAILED, head passed"),
        "{detail}"
    );
    assert!(
        detail.contains("accept: every recorded gate is clean"),
        "{detail}"
    );

    env.ok(&["review", "1", "--approve"]);
    assert!(env.ok(&["push", "1"]).contains("; pushed "), "pushed");
    assert_eq!(git_out(&origin, &["show", "main:hello.txt"]), "hi\n");
    // No cost to sum, and `--by agent` names the worker that did the work.
    let report = env.ok(&["report", "--by", "agent"]);
    assert!(report.contains("plain"), "{report}");
    assert!(report.contains("$0.00+1?"), "{report}");

    // A run whose worker was removed says so rather than running nothing.
    env.ok(&["agent", "rm", "plain"]);
    env.ok(&["dispatch", "alpha:7"]);
    let detail = env.ok(&["review", "2"]);
    assert!(detail.contains("unknown agent `plain`"), "{detail}");

    let (_, err, ok) = env.run(&["agent", "set", "x", "parse", "codex-json"]);
    assert!(!ok && err.contains("unknown parser `codex-json`"), "{err}");
    let (_, err, ok) = env.run(&["agent", "set", "x", "args", "not json"]);
    assert!(!ok && err.contains("JSON array"), "{err}");
}

/// A worker that reports which model it was given, so a route's choice can
/// be seen from outside.
const MODEL_CLAUDE: &str = r#"#!/bin/sh
model=unset
while [ $# -gt 0 ]; do
  case "$1" in --model) model="$2" ;; esac
  shift
done
echo hi > hello.txt
printf '{"type":"result","subtype":"success","is_error":false,"result":"ran as %s","total_cost_usd":0.1}\n' "$model"
"#;

/// A worker that adds the file a campaign asks for, and fails in one named
/// repository so a partial campaign can be restarted.
const CAMPAIGN_WORKER: &str = r#"#!/bin/sh
if [ "$(basename "$(dirname "$PWD")")" = "beta" ] && [ ! -e "$PMA_HOME/fixed" ]; then
  printf '{"type":"result","subtype":"error","is_error":true,"result":"gave up","total_cost_usd":0.1}
'
  exit 0
fi
mkdir -p .github/workflows
echo "on: push" > .github/workflows/ci.yml
printf '{"type":"result","subtype":"success","is_error":false,"result":"added the workflow","total_cost_usd":0.1}
'
"#;

/// One definition, three independent repositories, one worktree each,
/// verified separately, reviewed as a batch. A restart dispatches only the
/// members that have no live run.
#[test]
fn a_campaign_applies_one_definition_across_repositories() {
    let s = Scratch::new("campaign");
    let (env, _, _) = dispatch_env(&s, &[("claude", CAMPAIGN_WORKER)]);
    // Two more repositories from the same seed, independent of each other.
    let root = s.0.join("root");
    for name in ["beta", "gamma"] {
        git(&root, &["clone", "-q", "../origin.git", name], None);
        env.ok(&["project", "tier", "3", name]);
    }
    env.ok(&["scan", "--offline"]);
    // The seed's `make test` needs hello.txt, which a workflow campaign has
    // no business creating; a check that passes at both ends is what class
    // A- work is accepted against.
    for name in ["alpha", "beta", "gamma"] {
        env.ok(&["config", &format!("projects.{name}.verify"), "true"]);
    }

    assert!(env.ok(&["campaign"]).contains("no campaigns"), "empty");
    env.ok(&[
        "campaign",
        "add",
        "workflows",
        "add a workflow",
        "--projects",
        "alpha,beta,gamma",
        "--class",
        "A-",
        "--describe",
        "Create .github/workflows/ci.yml that runs on push.",
    ]);
    let listed = env.ok(&["campaign"]);
    assert!(
        listed.contains("class A-") && listed.contains("0/3 dispatched"),
        "{listed}"
    );

    // beta's agent gives up, so two of three produce a run to review.
    let out = env.ok(&["campaign", "run", "workflows"]);
    assert!(out.contains("2 ready, 1 failed"), "{out}");
    let show = env.ok(&["campaign", "show", "workflows"]);
    assert!(
        show.starts_with(
            "workflows: add a workflow
"
        ),
        "{show}"
    );
    assert!(show.contains("failed"), "{show}");

    // A- may edit `.github/**` and nothing else, so both are clean.
    let detail = env.ok(&["review", "1"]);
    assert!(detail.contains("A-, within .github/**"), "{detail}");
    assert!(detail.contains("1 files, all permitted"), "{detail}");
    assert!(detail.contains("base passed, head passed"), "{detail}");
    assert!(detail.contains("every recorded gate is clean"), "{detail}");

    // Batched review: one context across the repositories.
    let ready: Vec<String> = env
        .ok(&["review"])
        .lines()
        .filter(|l| l.contains(" ready "))
        .map(|l| l.split_whitespace().next().unwrap()[1..].to_string())
        .collect();
    assert_eq!(ready.len(), 2, "two runs to approve");
    let mut args = vec!["review"];
    args.extend(ready.iter().map(String::as_str));
    args.push("--approve");
    let approved = env.ok(&args);
    assert_eq!(approved.lines().count(), 2, "{approved}");

    // A member whose run failed is named, not dispatched over: its worktree
    // is still there and the reviewer decides what happens to it.
    let out = env.ok(&["campaign", "run", "workflows"]);
    assert!(
        out.contains("beta: #2 is failed; reject or rework it"),
        "{out}"
    );
    assert!(
        out.contains("every member of `workflows` has a run"),
        "{out}"
    );

    // Once it is rejected, a restart dispatches that member and no other.
    fs::write(env.home.join("fixed"), "").unwrap();
    env.ok(&["review", "2", "--reject"]);
    let out = env.ok(&["campaign", "run", "workflows"]);
    assert!(out.contains("1 ready, 0 failed"), "{out}");
    assert_eq!(out.matches(": add a workflow").count(), 1, "{out}");
    let show = env.ok(&["campaign", "show", "workflows"]);
    assert_eq!(show.matches("approved").count(), 2, "{show}");
    assert_eq!(show.matches("ready").count(), 1, "{show}");

    // Every member has a run, so there is nothing left to start.
    env.ok(&["review", "3", "--approve"]);
    let out = env.ok(&["campaign", "run", "workflows"]);
    assert!(
        out.contains("every member of `workflows` has a run"),
        "{out}"
    );
}

/// Approval names a tree. A publish sends that tree or nothing, and only onto
/// the base its check passed against.
#[test]
fn an_approval_is_evidence_about_a_tree_not_a_state() {
    let s = Scratch::new("evidence");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);

    // A worktree edited after approval is not published.
    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    let worktree = env.home.join("state/worktrees/alpha/add-greeting");
    fs::write(worktree.join("sneaked.txt"), "later\n").unwrap();
    let (out, err, success) = env.run(&["push", "1"]);
    assert!(!success, "{out}{err}");
    assert!(
        out.contains("the worktree changed after it was approved"),
        "{out}"
    );
    assert_eq!(git_out(&origin, &["log", "--format=%s", "main"]), "init\n");

    // Reviewing it again approves the tree that is actually there.
    fs::remove_file(worktree.join("sneaked.txt")).unwrap();
    env.ok(&["review", "1", "--approve"]);
    assert!(env.ok(&["push", "1"]).contains("; pushed "), "pushed");

    // A rework withdraws an approval: the approver read another tree.
    env.ok(&["dispatch", "alpha:7"]);
    env.ok(&["review", "2", "--approve"]);
    env.ok(&["review", "2", "--rework", "add hello.txt too"]);
    let detail = env.ok(&["review", "2"]);
    assert!(!detail.contains("approved by"), "{detail}");

    // Nothing checks the change against a moved origin, so git's refusal of
    // a push that does not fast-forward stands: another commit lands
    // upstream that the approved change breaks.
    env.ok(&["review", "2", "--approve"]);
    let other = s.0.join("other");
    git(
        &s.0,
        &["clone", "-q", origin.to_str().unwrap(), "other"],
        None,
    );
    fs::write(other.join("Makefile"), "test:\n\ttest -f never.txt\n").unwrap();
    git(&other, &["commit", "-qam", "stricter test"], None);
    git(&other, &["push", "-q"], None);
    let (out, err, success) = env.run(&["push", "2"]);
    assert!(!success, "{out}{err}");
    assert!(
        out.contains("origin's main moved since the run's base"),
        "{out}"
    );
    assert_eq!(
        git_out(&origin, &["log", "--format=%s", "main"]),
        "stricter test\nadd greeting\ninit\n",
        "nothing of the run reached the branch"
    );
    // The commit stays in the clone, for the user to rebase or drop.
    assert!(env.ok(&["review", "2"]).starts_with("#2 committed"));
    assert_eq!(
        git_out(&alpha, &["log", "-1", "--format=%s"]),
        "second task\n"
    );
}

/// A batch approval is all or nothing, and only for runs nothing objects to.
#[test]
fn a_batch_approval_refuses_a_list_with_anything_to_read_in_it() {
    let s = Scratch::new("batch");
    let (env, _, _) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    env.ok(&["dispatch", "--auto", "-n", "3"]);
    let list = env.ok(&["review"]);
    assert!(list.contains("clean"), "{list}");

    // #2 and #3 fail their checks, so the list is refused whole.
    let (_, err, success) = env.run(&["review", "1", "2", "--approve", "--minutes", "5"]);
    assert!(!success && err.contains("#2 is not clean"), "{err}");
    assert!(err.contains("verify failed at the head"), "{err}");
    assert!(!env.ok(&["review", "1"]).contains("5m00s"));
    assert_eq!(
        env.ok(&["review"]).matches("ready").count(),
        3,
        "nothing was approved"
    );

    // A propose route publishes nothing, whatever the gates say.
    let doc = s.0.join("policy.json");
    fs::write(
        &doc,
        r#"{"route":[{"name":"draft-only","match":{},"approval":"propose"}]}"#,
    )
    .unwrap();
    env.ok(&["route", "propose", doc.to_str().unwrap()]);
    env.ok(&["route", "activate", "1"]);
    env.ok(&["review", "1", "--reject"]);
    env.ok(&["dispatch", "alpha:5", "--retry"]);
    let (_, err, success) = env.run(&["review", "4", "--approve"]);
    assert!(!success && err.contains("which is `propose`"), "{err}");
}

/// `--minutes` on a batch approval reaches every run named.
#[test]
fn a_batch_approval_records_review_time_on_each_run() {
    const PASSING_CLAUDE: &str = r#"#!/bin/sh
echo hi > hello.txt
echo '{"type":"result","subtype":"success","is_error":false,"result":"did it","total_cost_usd":0.1}'
"#;
    let s = Scratch::new("batch-minutes");
    let (env, _, _) = dispatch_env(&s, &[("claude", PASSING_CLAUDE)]);
    env.ok(&["dispatch", "--auto", "-n", "2"]);

    env.ok(&["review", "1", "2", "--approve", "--minutes", "5"]);
    for id in ["1", "2"] {
        let detail = env.ok(&["review", id]);
        assert!(detail.starts_with(&format!("#{id} approved")), "{detail}");
        assert!(detail.contains("5m00s"), "{detail}");
    }
}

/// Fails its check at the first model and passes at the second, so a route's
/// escalation can be seen end to end.
const ESCALATING_CLAUDE: &str = r#"#!/bin/sh
model=unset
while [ $# -gt 0 ]; do
  case "$1" in --model) model="$2" ;; esac
  shift
done
if [ "$model" = "opus" ]; then echo hi > hello.txt; fi
printf '{"type":"result","subtype":"success","is_error":false,"result":"ran as %s","total_cost_usd":0.1}
' "$model"
"#;

/// A route may retry once at a stronger model where the check itself refused
/// the work. Each attempt is recorded with the model it ran.
#[test]
fn a_failed_check_escalates_once_and_records_both_attempts() {
    let s = Scratch::new("escalate");
    let (env, _, _) = dispatch_env(&s, &[("claude", ESCALATING_CLAUDE)]);
    let doc = s.0.join("policy.json");
    fs::write(
        &doc,
        r#"{"route":[{"name":"try-twice","match":{},"model":"haiku",
             "escalate":{"model":"opus","attempts":1},"approval":"each"}]}"#,
    )
    .unwrap();
    env.ok(&["route", "propose", doc.to_str().unwrap()]);
    env.ok(&["route", "activate", "1"]);

    env.ok(&["dispatch", "alpha:5"]);
    let detail = env.ok(&["review", "1"]);
    assert!(detail.contains("base FAILED, head passed"), "{detail}");
    let attempts = detail.split_once("attempts:\n").expect("two attempts").1;
    let lines: Vec<&str> = attempts.lines().take(2).collect();
    assert!(
        lines[0].contains("haiku") && lines[0].contains("verify FAILED"),
        "{detail}"
    );
    assert!(
        lines[1].contains("opus") && lines[1].contains("verify passed"),
        "{detail}"
    );
    // The refused check consumed one attempt; the one that passed did not.
    let (_, err, success) = env.run(&["dispatch", "alpha:5"]);
    assert!(!success && err.contains("already has a run"), "{err}");

    // The run's cost is both attempts. Its `model` stays what the route
    // chose at dispatch: the snapshot records the decision, and the attempt
    // records what actually ran.
    assert!(detail.contains("claude haiku, $0.20"), "{detail}");
}

/// Shadow records which route a policy would choose and applies none of it:
/// no refusal, no scope, no approval mode.
#[test]
fn a_shadow_policy_refuses_scopes_and_approves_nothing() {
    let s = Scratch::new("route-shadow");
    let (env, _, _) = dispatch_env(&s, &[("claude", MODEL_CLAUDE)]);
    let propose = |name: &str, doc: &str| {
        let path = s.0.join(name);
        fs::write(&path, doc).unwrap();
        env.ok(&["route", "propose", path.to_str().unwrap(), "--by", "me"]);
    };

    // Applied, `propose` would block approval and the scope would flag
    // hello.txt.
    propose(
        "all.json",
        r#"{"route":[{"name":"all","match":{},"model":"opus",
            "scope":["docs/**"],"approval":"propose"}]}"#,
    );
    env.ok(&["route", "activate", "1", "--shadow", "--by", "me"]);
    env.ok(&["dispatch", "alpha:5"]);
    let detail = env.ok(&["review", "1"]);
    assert!(detail.contains("all in revision 1, in shadow"), "{detail}");
    assert!(detail.contains("ran as unset"), "{detail}");
    assert!(
        !detail.contains("docs/**"),
        "the class scope holds: {detail}"
    );
    env.ok(&["review", "1", "--approve"]);

    // Applied, a task no route matches would be refused.
    propose(
        "narrow.json",
        r#"{"route":[{"name":"narrow","match":{"class":"A"},"approval":"each"}]}"#,
    );
    env.ok(&["route", "activate", "2", "--shadow", "--by", "me"]);
    env.ok(&["dispatch", "alpha:7"]);
    let detail = env.ok(&["review", "2"]);
    assert!(
        detail.contains("no route in revision 2, in shadow"),
        "{detail}"
    );
}

/// Stated order: flags, `-p`, an applied route, then the default preset. The
/// default preset is a setting, and a route is policy about the work.
#[test]
fn an_applied_route_outranks_the_default_preset() {
    let s = Scratch::new("route-preset");
    let (env, _, _) = dispatch_env(&s, &[("claude", MODEL_CLAUDE)]);
    env.ok(&["preset", "set", "cheap", "claude", "haiku"]);
    env.ok(&["preset", "use", "cheap"]);
    let doc = s.0.join("policy.json");
    fs::write(
        &doc,
        r#"{"route":[{"name":"all","match":{},"model":"opus","approval":"each"}]}"#,
    )
    .unwrap();
    env.ok(&["route", "propose", doc.to_str().unwrap(), "--by", "me"]);
    env.ok(&["route", "activate", "1", "--by", "me"]);

    env.ok(&["dispatch", "alpha:5"]);
    let detail = env.ok(&["review", "1"]);
    assert!(detail.contains("ran as opus"), "{detail}");

    // A preset named on the command line still outranks the route.
    env.ok(&["dispatch", "-p", "cheap", "alpha:7"]);
    let detail = env.ok(&["review", "2"]);
    assert!(detail.contains("ran as haiku"), "{detail}");
}

/// A project no scan can find is not ranked: nothing can be dispatched
/// against its tasks.
#[test]
fn an_absent_project_is_not_ranked() {
    let s = Scratch::new("absent");
    let (env, _, alpha) = dispatch_env(&s, &[]);
    let root = alpha.parent().unwrap();
    git(root, &["clone", "-q", "../origin.git", "beta"], None);
    env.ok(&["project", "tier", "1", "beta"]);
    env.ok(&["scan", "--offline"]);
    assert!(
        env.ok(&["matrix"]).contains("beta:"),
        "ranked while present"
    );

    fs::rename(root.join("beta"), s.0.join("beta-moved")).unwrap();
    env.ok(&["scan", "--offline"]);
    for view in [&["matrix"][..], &["status"], &["next"]] {
        let out = env.ok(view);
        assert!(!out.contains("beta"), "{view:?}: {out}");
        assert!(out.contains("alpha"), "{view:?}: {out}");
    }
}

/// `gh repo clone` from the local origin, so a clone needs no network.
const CLONING_GH: &str = r#"#!/bin/sh
[ "$1 $2" = "repo clone" ] || { echo "fake gh: unexpected $*" >&2; exit 1; }
git clone -q "$PMA_HOME/../origin.git" "$4"
"#;

/// A project whose checkout is gone is cloned back from the `owner/name` the
/// scan recorded: a dry run first, and never over a directory that exists.
#[test]
fn a_missing_checkout_is_cloned_from_its_recorded_slug() {
    let s = Scratch::new("clone");
    let (env, _, alpha) = dispatch_env(&s, &[("gh", CLONING_GH)]);
    let root = alpha.parent().unwrap().to_path_buf();
    git(&root, &["clone", "-q", "../origin.git", "beta"], None);
    git(
        &root.join("beta"),
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/me/beta.git",
        ],
        None,
    );
    env.ok(&["scan", "--offline"]);
    env.ok(&["project", "tag", "add", "work", "alpha", "beta"]);
    fs::rename(root.join("beta"), s.0.join("beta-moved")).unwrap();
    env.ok(&["scan", "--offline"]);

    let out = env.ok(&["project", "clone", "--tag", "work"]);
    assert!(out.contains("alpha: already checked out"), "{out}");
    assert!(
        out.contains(&format!(
            "beta: would clone me/beta into {}",
            root.join("beta").display()
        )),
        "{out}"
    );
    assert!(!root.join("beta").exists(), "a dry run clones nothing");

    let out = env.ok(&["project", "clone", "--tag", "work", "--apply"]);
    assert!(out.contains("beta: cloned me/beta"), "{out}");
    assert!(root.join("beta/TODO.md").exists());
    git(
        &root.join("beta"),
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/me/beta.git",
        ],
        None,
    );
    env.ok(&["scan", "--offline"]);
    assert!(
        env.ok(&["status", "--all"]).contains("beta"),
        "present again"
    );

    // A directory of that name is never cloned over.
    fs::rename(root.join("beta"), s.0.join("beta-again")).unwrap();
    env.ok(&["scan", "--offline"]);
    fs::create_dir(root.join("beta")).unwrap();
    let out = env.ok(&["project", "clone", "beta", "--apply"]);
    assert!(out.contains("already exists; not cloned"), "{out}");
}

/// An untiered project is ranked at `default_tier`, so a route conditioned on
/// tier must see that tier too.
#[test]
fn an_untiered_project_is_routed_at_the_default_tier() {
    let s = Scratch::new("default-tier");
    let (env, _, _) = dispatch_env(&s, &[("claude", MODEL_CLAUDE)]);
    env.ok(&["project", "tier", "none", "alpha"]);
    env.ok(&["config", "default_tier", "2"]);
    let doc = s.0.join("policy.json");
    fs::write(
        &doc,
        r#"{"route":[
             {"name":"tier-two","match":{"tier":"2-2"},"model":"haiku","approval":"each"},
             {"name":"rest","match":{},"model":"opus","approval":"each"}
           ]}"#,
    )
    .unwrap();
    env.ok(&["route", "propose", doc.to_str().unwrap()]);
    env.ok(&["route", "activate", "1"]);
    env.ok(&["dispatch", "alpha:5"]);
    let detail = env.ok(&["review", "1"]);
    assert!(detail.contains("ran as haiku"), "{detail}");
}

/// A policy is an artifact: proposed, reviewed, activated, and applied
/// deterministically. Replaying it over recorded runs must reproduce what
/// they were routed to.
#[test]
fn a_policy_is_a_draft_until_activated_and_replays_exactly() {
    let s = Scratch::new("route");
    let (env, _, _) = dispatch_env(&s, &[("claude", MODEL_CLAUDE)]);
    assert!(env.ok(&["route"]).contains("no routing policy"), "empty");

    let doc = s.0.join("policy.json");
    fs::write(
        &doc,
        r#"{"route":[
             {"name":"specified","match":{"class":"B","complexity":"1-3"},
              "model":"haiku","approval":"batch"},
             {"name":"rest","match":{},"model":"opus","approval":"each"}
           ]}"#,
    )
    .unwrap();
    let out = env.ok(&["route", "propose", doc.to_str().unwrap(), "--by", "me"]);
    assert!(out.contains("revision 1 stored as a draft"), "{out}");
    assert!(env.ok(&["route"]).contains("draft"), "still a draft");

    // One task throughout: `add greeting` names hello.txt and carries a
    // description, so it estimates 2 and matches the first route.
    // A draft does not route: the run takes the settings, as before.
    env.ok(&["dispatch", "alpha:5"]);
    let detail = env.ok(&["review", "1"]);
    assert!(detail.contains("ran as unset"), "{detail}");
    assert!(!detail.contains("revision 1"), "{detail}");
    env.ok(&["review", "1", "--reject"]);

    // Shadow records the route without applying it.
    env.ok(&["route", "activate", "1", "--shadow", "--by", "me"]);
    env.ok(&["dispatch", "alpha:5", "--retry"]);
    let detail = env.ok(&["review", "2"]);
    assert!(
        detail.contains("specified in revision 1, in shadow"),
        "{detail}"
    );
    assert!(detail.contains("ran as unset"), "shadow applies nothing");
    env.ok(&["review", "2", "--reject"]);

    // Activated, the same task takes the route's model.
    env.ok(&["route", "activate", "1", "--by", "me"]);
    assert!(env.ok(&["route"]).contains("active, by me"), "activated");
    env.ok(&["dispatch", "alpha:5", "--retry"]);
    let detail = env.ok(&["review", "3"]);
    assert!(detail.contains("ran as haiku"), "{detail}");

    // The gate: replaying the active policy changes nothing it decided.
    // #1 predates it and #2 was shadowed, so neither took the route's model;
    // both are differences worth seeing rather than matches.
    let replay = env.ok(&["route", "replay", "1"]);
    assert!(
        replay.contains("3 runs replayed, 2 routed differently"),
        "{replay}"
    );
    assert!(replay.contains("#1"), "dispatched before it: {replay}");
    assert!(replay.contains("#2"), "shadowed, so not applied: {replay}");
    assert!(!replay.contains("#3"), "applied, so unchanged: {replay}");
    assert!(replay.contains("Cost is not projected"), "{replay}");

    // A candidate that narrows the first route moves runs onto the second.
    let other = s.0.join("other.json");
    fs::write(
        &other,
        r#"{"route":[
             {"name":"specified","match":{"class":"B","complexity":"1-1"},
              "model":"haiku","approval":"batch"},
             {"name":"rest","match":{},"model":"opus","approval":"each"}
           ]}"#,
    )
    .unwrap();
    let replay = env.ok(&["route", "replay", other.to_str().unwrap()]);
    assert!(
        replay.contains("3 runs replayed, 3 routed differently"),
        "{replay}"
    );
    assert!(replay.contains("rest claude/opus"), "{replay}");

    // A policy that matches nothing refuses the dispatch by name.
    let narrow = s.0.join("narrow.json");
    fs::write(
        &narrow,
        r#"{"route":[{"name":"only-a","match":{"class":"A"},"approval":"each"}]}"#,
    )
    .unwrap();
    env.ok(&["route", "propose", narrow.to_str().unwrap()]);
    env.ok(&["route", "activate", "2"]);
    env.ok(&["review", "3", "--reject"]);
    let (_, err, success) = env.run(&["dispatch", "alpha:5", "--retry"]);
    assert!(
        !success && err.contains("matches no route in policy revision 2"),
        "{err}"
    );

    // An unreadable document never becomes a revision.
    let bad = s.0.join("bad.json");
    fs::write(&bad, r#"{"route":[{"match":{},"approval":"whenever"}]}"#).unwrap();
    let (_, err, success) = env.run(&["route", "propose", bad.to_str().unwrap()]);
    assert!(
        !success && err.contains("unknown approval `whenever`"),
        "{err}"
    );
    assert_eq!(env.ok(&["route"]).lines().count(), 2, "still two revisions");
}

/// A stand-in for `claude -p` that finishes once `$PMA_HOME/release` exists,
/// or after about 60s.
const SLOW_CLAUDE: &str = r#"#!/bin/sh
n=0
while [ ! -e "$PMA_HOME/release" ] && [ $n -lt 1200 ]; do sleep 0.05; n=$((n + 1)); done
echo hi > hello.txt
echo '{"type":"result","subtype":"success","is_error":false,"result":"did it","total_cost_usd":0.1}'
"#;

#[test]
fn a_second_session_leaves_running_runs_alone() {
    let s = Scratch::new("session");
    let (env, _, _) = dispatch_env(&s, &[("claude", SLOW_CLAUDE)]);

    /// Releases the agent when the test ends, passing or not.
    struct Release(PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = fs::write(&self.0, "");
        }
    }
    let release = Release(env.home.join("release"));

    // Piped, so a failed test does not wait on the child holding its output.
    let mut dispatch = env
        .command(&["dispatch", "alpha:5"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    let mut last = String::new();
    while !last.contains("#1  running") {
        if dispatch.try_wait().unwrap().is_some() || started.elapsed().as_secs() >= 30 {
            let _ = dispatch.kill();
            let out = dispatch.wait_with_output().unwrap();
            panic!(
                "the run never started; review: {last}\ndispatch: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        last = env.ok(&["review"]);
    }

    assert!(
        env.ok(&["review"]).contains("#1  running"),
        "review does not fail a live run"
    );
    for args in [
        &["dispatch", "alpha:6"][..],
        &["push", "1"],
        &["review", "1", "--reject"],
    ] {
        let (_, err, success) = env.run(args);
        assert!(
            !success && err.contains("another pma session (pid "),
            "{args:?}: {err}"
        );
    }

    drop(release);
    let out = dispatch.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 ready, 0 failed"),
        "{stdout}"
    );
    assert!(env.ok(&["review"]).contains("#1  ready"));
    env.ok(&["review", "1", "--reject"]);
}

/// A stand-in for `gh` backed by `issues.json`; mutating calls are logged.
const FAKE_GH: &str = r#"#!/usr/bin/env python3
import json, os, sys
state = os.environ["GH_STATE"]
issues = json.load(open(state))
args = sys.argv[1:]
def opt(name):
    return args[args.index(name) + 1] if name in args else None
def log(line):
    with open(state + ".log", "a") as f:
        f.write(line + "\n")
if args[:2] == ["api", "user"]:
    print("me")
elif args[:2] == ["issue", "list"]:
    print(json.dumps([dict(i, author={"login": i["author"]}, labels=[{"name": l} for l in i["labels"]]) for i in issues]))
elif args[:2] == ["label", "create"]:
    log("label create " + args[2])
elif args[:2] == ["issue", "create"]:
    n = max([i["number"] for i in issues] + [0]) + 1
    issues.append({"number": n, "title": opt("--title"), "state": "OPEN", "author": "me", "labels": [opt("--label")]})
    log("issue create %d %s | %s" % (n, opt("--title"), opt("--body")))
    print("https://github.com/me/alpha/issues/%d" % n)
elif args[:2] == ["issue", "edit"]:
    issue = next(i for i in issues if i["number"] == int(args[2]))
    if opt("--title"):
        issue["title"] = opt("--title")
    if opt("--add-label"):
        issue["labels"].append(opt("--add-label"))
    if opt("--remove-label"):
        issue["labels"].remove(opt("--remove-label"))
    log("issue edit " + " ".join(args[2:]))
else:
    sys.exit("fake gh: unexpected " + " ".join(args))
json.dump(issues, open(state, "w"))
"#;

#[test]
fn sync_plans_then_applies_and_settles() {
    use std::os::unix::fs::PermissionsExt;

    let s = Scratch::new("stage4");
    let bin = s.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("gh"), FAKE_GH).unwrap();
    fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
    let state = s.0.join("issues.json");
    fs::write(
        &state,
        r#"[{"number": 1, "title": "fixed elsewhere", "state": "CLOSED", "author": "me", "labels": []},
            {"number": 2, "title": "old crash title", "state": "OPEN", "author": "me", "labels": ["pma:critical"]},
            {"number": 3, "title": "please add X", "state": "OPEN", "author": "alice", "labels": []}]"#,
    )
    .unwrap();
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = s.project(
        "root/alpha",
        "# TODO\n\n## Critical\n\n- [ ] segfault on empty input\n  when stdin is empty\n- [ ] renamed crash gh:2\n\n## High\n\n- [ ] fixed elsewhere gh:1\n",
    );
    git(&alpha, &["init", "-q"], None);
    git(
        &alpha,
        &["remote", "add", "origin", "https://github.com/me/alpha.git"],
        None,
    );
    let beta = s.project("root/beta", "## High\n");
    git(&beta, &["init", "-q"], None);

    let sync = |args: &[&str]| {
        let out = command(env!("CARGO_BIN_EXE_pma"))
            .env("PMA_HOME", &home)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("GH_STATE", &state)
            .args(args)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    sync(&["root", "add", root.to_str().unwrap()]);
    sync(&["scan", "--offline"]);

    let original = fs::read_to_string(alpha.join("TODO.md")).unwrap();
    let (out, err) = sync(&["sync"]);
    assert_eq!(
        out,
        "alpha:5: open an issue for `segfault on empty input`\n\
         alpha: retitle #2 to `renamed crash`\n\
         alpha:11: mark done, #1 is closed\n\
         alpha: untriaged #3 by alice: please add X\n\
         beta: skipped: TODO.md has lint errors; see `pma lint`\n\
         3 changes; run `pma sync --apply` to make them\n",
        "{err}"
    );
    assert_eq!(fs::read_to_string(alpha.join("TODO.md")).unwrap(), original);
    assert!(
        !s.0.join("issues.json.log").exists(),
        "a dry run changes nothing"
    );

    let (out, err) = sync(&["sync", "--apply", "alpha"]);
    assert!(
        out.ends_with(
            "3 changes made\nTODO.md changed, uncommitted, in: alpha; commit, then `pma scan`\n"
        ),
        "{out}{err}"
    );
    assert_eq!(
        fs::read_to_string(s.0.join("issues.json.log")).unwrap(),
        "label create pma:critical\n\
         issue create 4 segfault on empty input | when stdin is empty\n\n\
         Opened by pma from a `## Critical` item in TODO.md.\n\
         issue edit 2 -R me/alpha --title renamed crash\n"
    );
    assert_eq!(
        fs::read_to_string(alpha.join("TODO.md")).unwrap(),
        "# TODO\n\n## Critical\n\n- [ ] segfault on empty input gh:4\n  when stdin is empty\n- [ ] renamed crash gh:2\n\n## High\n\n- [x] fixed elsewhere gh:1\n"
    );

    let (out, _) = sync(&["sync", "alpha"]);
    assert_eq!(out, "alpha: untriaged #3 by alice: please add X\nin sync\n");
}

#[test]
fn notes_and_measured_dependencies() {
    use std::os::unix::fs::PermissionsExt;

    let s = Scratch::new("stage5");
    let home = s.0.join("home");
    let bin = s.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("uv"),
        "#!/bin/sh\nprintf 'Resolved 3 packages in 1ms\\nUpdate ruff v0.14.10 -> v0.16.7\\nUpdate ty v0.0.8 -> v0.0.81\\n'\n",
    )
    .unwrap();
    fs::set_permissions(bin.join("uv"), fs::Permissions::from_mode(0o755)).unwrap();
    let run = |args: &[&str]| {
        let out = command(env!("CARGO_BIN_EXE_pma"))
            .env("PMA_HOME", &home)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .args(args)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.success(),
        )
    };
    let ok = |args: &[&str]| {
        let (out, err, success) = run(args);
        assert!(success, "pma {args:?}: {err}");
        out
    };

    assert_eq!(
        ok(&["note"]),
        "no notes; add one with `pma note add <text>`\n"
    );
    assert_eq!(
        ok(&["note", "add", "move", "CI", "to", "one", "workflow"]),
        "#1\n"
    );
    assert_eq!(ok(&["note", "add", "second"]), "#2\n");
    ok(&["note", "edit", "1", "move CI to reusable workflows"]);
    ok(&["note", "rm", "2"]);
    let (_, err, success) = run(&["note", "rm", "2"]);
    assert!(!success && err.contains("no note #2"), "{err}");
    let list = ok(&["note"]);
    assert!(list.starts_with("#1  20"), "{list}");
    assert!(
        list.ends_with("  move CI to reusable workflows\n"),
        "{list}"
    );

    let root = s.0.join("root");
    let alpha = s.project("root/alpha", "# TODO\n\n## High\n");
    git(&alpha, &["init", "-q"], None);
    fs::write(alpha.join("uv.lock"), "").unwrap();
    ok(&["root", "add", root.to_str().unwrap()]);
    ok(&["project", "tier", "1", "alpha"]);
    ok(&["scan", "--offline"]);
    assert!(ok(&["status", "--explain"]).contains("not measured; `pma scan --deps`"));
    let (_, err, success) = run(&["dispatch", "alpha:deps"]);
    assert!(
        !success && err.contains("no outdated dependencies"),
        "{err}"
    );

    let scanned = ok(&["scan", "--deps"]);
    assert!(
        scanned
            .lines()
            .next()
            .unwrap()
            .ends_with(", outdated dependencies in 1"),
        "{scanned}"
    );
    ok(&["scan", "--offline"]);
    let status = ok(&["status", "--explain"]);
    assert!(
        !status.contains("stale deps"),
        "a lock never committed has no age: {status}"
    );

    git(&alpha, &["add", "uv.lock"], None);
    git(
        &alpha,
        &["commit", "-qm", "lock"],
        Some(now() - 40 * 86_400),
    );
    ok(&["scan", "--offline"]);
    let status = ok(&["status", "--explain"]);
    // 40 days of seconds is 39 to 41 calendar days across a DST change.
    assert!(
        ["39", "40", "41"]
            .iter()
            .any(|d| status.contains(&format!("2 to move, lock {d}d old, measured today"))),
        "kept, and the lock is past deps.days: {status}"
    );
    assert!(
        ok(&["matrix", "--all"])
            .contains("alpha  T1  medium  open 0d  update dependencies: 2 outdated"),
        "the deps signal task"
    );
    assert!(ok(&["next"]).contains("\n  alpha:deps "), "for agents");

    // Ranked only at the default tier, the signal is shown but not offered.
    ok(&["project", "tier", "none", "alpha"]);
    assert!(
        ok(&["matrix", "--all"]).contains("alpha  T5  medium  open 0d  update dependencies"),
        "still ranked"
    );
    let next = ok(&["next"]);
    assert!(!next.contains("alpha:deps"), "{next}");
    ok(&["project", "tier", "1", "alpha"]);

    fs::write(alpha.join("uv.lock"), "refreshing").unwrap();
    ok(&["scan", "--offline"]);
    assert!(
        !ok(&["status", "--explain"]).contains("stale deps"),
        "an edited lock is being refreshed"
    );
}

/// A worker that reports the model it was handed, so a test can see which one
/// dispatch chose. With no model the placeholder is dropped and `$4` is empty.
const MODEL_WORKER: &str = r#"#!/bin/sh
cd "$3" || exit 1
echo hi > hello.txt
echo "model=$4"
# Whatever a preset added arrives after the model, so the test can see it.
shift 4 2>/dev/null || shift $#
while [ $# -gt 0 ]; do
  case "$1" in --thinking) echo "thinking=$2"; shift ;; esac
  shift
done
"#;

/// One target can name a heading or a quadrant. A task that cannot run holds
/// up only itself, where a target naming one task is an error.
#[test]
fn a_target_can_name_a_heading_or_a_quadrant() {
    let s = Scratch::new("select");
    let (env, _, _) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);

    let (_, err, success) = env.run(&["dispatch", "-a", "pi", "alpha:5"]);
    assert!(
        !success && err.contains("unknown agent `pi`; `pma agent` lists claude"),
        "{err}"
    );
    let (_, err, success) = env.run(&["dispatch", "alpha:soon"]);
    assert!(
        !success && err.contains("expected a TODO.md line, ci, deps, critical"),
        "{err}"
    );
    let (_, err, success) = env.run(&["dispatch", "alpha:critical"]);
    assert!(
        !success && err.contains("alpha: no open critical items at the last scan"),
        "{err}"
    );
    // A list needs a terminal; the test harness has none.
    let (_, err, success) = env.run(&["dispatch", "alpha"]);
    assert!(
        !success && err.contains("opens a list, which needs a terminal"),
        "{err}"
    );

    // Tier 1 high items are important and not urgent: the whole of Q2.
    let out = env.ok(&["dispatch", "alpha:q2", "--separate"]);
    assert!(
        out.starts_with("#1 alpha: add greeting\n#2 alpha: second task\n#3 alpha: third task\n"),
        "{out}"
    );

    // The same three by heading now: each is passed over with its reason,
    // rather than the first one ending the batch.
    let (_, err, success) = env.run(&["dispatch", "alpha:high"]);
    assert!(
        !success && err.ends_with("pma: nothing to dispatch: every task named was passed over\n"),
        "{err}"
    );
    assert_eq!(err.matches("already has a run").count(), 3, "{err}");
    assert!(err.contains("warning: alpha:5: already has a run"), "{err}");

    // The one Low item is #manual, so the heading selects nothing runnable.
    let (_, err, success) = env.run(&["dispatch", "alpha:low"]);
    assert!(
        !success && err.contains("`guarded task` is class D and is not dispatched"),
        "{err}"
    );
}
/// A worker is how to run a program; which model it runs at is a preset. The
/// retired field says so rather than failing as unknown.
#[test]
fn a_worker_no_longer_names_a_model() {
    let s = Scratch::new("flags");
    let (env, _, _) = dispatch_env(&s, &[("mw", MODEL_WORKER)]);
    env.ok(&["agent", "set", "mw", "command", "mw"]);
    env.ok(&[
        "agent",
        "set",
        "mw",
        "args",
        r#"["--task","{prompt}","{dir}","{model}","{extra}"]"#,
    ]);
    let (_, err, ok) = env.run(&["agent", "set", "mw", "model", "haiku"]);
    assert!(!ok, "the field is gone");
    assert!(
        err.contains("pma preset set"),
        "it says where it went: {err}"
    );
    // The retired setting points at the place a model now lives.
    let (_, err, ok) = env.run(&["config", "model", "haiku"]);
    assert!(!ok && err.contains("pma preset set"), "{err}");

    // With no preset and no flag, the worker is run without a model at all and
    // uses its own.
    env.ok(&["config", "agent", "mw"]);
    env.ok(&["dispatch", "alpha:5"]);
    assert!(
        env.ok(&["review", "1"]).contains("model=\n"),
        "no model named"
    );

    // A flag names one for the invocation.
    env.ok(&["dispatch", "-m", "sonnet", "alpha:7"]);
    assert!(
        env.ok(&["review", "2"]).contains("model=sonnet"),
        "the flag"
    );

    // The listing is about the program, not the model.
    let listed = env.ok(&["agent"]);
    assert!(listed.contains("* mw"), "{listed}");
    assert!(!listed.contains("sonnet"), "no model column: {listed}");
    assert!(
        env.ok(&["agent", "show", "mw"]).contains("{extra}"),
        "args shown"
    );
}

/// A preset names a worker, a model and the configuration that goes with them,
/// so a combination worth returning to has a name. Effort is arguments rather
/// than a field, because what expresses it differs per agent.
#[test]
fn a_preset_names_a_worker_a_model_and_its_configuration() {
    let s = Scratch::new("presets");
    let (env, _, _) = dispatch_env(&s, &[("mw", MODEL_WORKER)]);
    env.ok(&["agent", "set", "mw", "command", "mw"]);
    // `{extra}` is where this worker wants a preset's arguments.
    env.ok(&[
        "agent",
        "set",
        "mw",
        "args",
        r#"["--task","{prompt}","{dir}","{model}","{extra}"]"#,
    ]);

    assert!(env.ok(&["preset"]).contains("no presets"), "empty");
    // A preset may not name a worker that does not exist: it would name a
    // combination nothing could run.
    let (_, err, ok) = env.run(&["preset", "set", "bad", "nosuch", "haiku"]);
    assert!(!ok && err.contains("no worker `nosuch`"), "{err}");

    env.ok(&["preset", "set", "plain", "mw"]);
    env.ok(&["preset", "set", "cheap", "mw", "haiku"]);
    env.ok(&[
        "preset",
        "set",
        "cheap-high",
        "mw",
        "haiku",
        "--thinking",
        "high",
    ]);
    let listed = env.ok(&["preset"]);
    assert!(
        listed.contains("its own default"),
        "a preset may name no model"
    );
    assert!(listed.contains("--thinking high"), "{listed}");

    // `-p` runs one command with that preset, and its arguments reach the
    // worker where its record puts them.
    env.ok(&["dispatch", "-p", "cheap-high", "alpha:5"]);
    let detail = env.ok(&["review", "1"]);
    assert!(detail.contains("model=haiku"), "{detail}");
    assert!(
        detail.contains("thinking=high"),
        "the effort reached it: {detail}"
    );

    // A flag beside it is the more specific statement, so it wins.
    env.ok(&["dispatch", "-p", "cheap-high", "-m", "sonnet", "alpha:7"]);
    let detail = env.ok(&["review", "2"]);
    assert!(detail.contains("model=sonnet"), "{detail}");
    assert!(
        detail.contains("thinking=high"),
        "the rest of the preset stands"
    );

    // `use` makes it the default, and then no flag is needed.
    env.ok(&["preset", "use", "cheap"]);
    assert!(env.ok(&["preset"]).contains("* cheap"), "marked default");
    env.ok(&["dispatch", "alpha:8"]);
    let detail = env.ok(&["review", "3"]);
    assert!(detail.contains("model=haiku"), "{detail}");
    assert!(
        !detail.contains("thinking=high"),
        "a different preset: {detail}"
    );

    // Forgetting the default clears the setting rather than leaving a name
    // nothing resolves.
    env.ok(&["preset", "rm", "cheap"]);
    let listed = env.ok(&["preset"]);
    assert!(!listed.contains("* cheap"), "the default is gone: {listed}");
    let settings = env.ok(&["config"]);
    let preset_line = settings
        .lines()
        .find(|l| l.starts_with("preset ="))
        .unwrap_or_default();
    assert_eq!(
        preset_line.trim_end_matches("  (set)").trim(),
        "preset =",
        "the setting is cleared with it"
    );
}

/// A workflow document, in both its forms, through the store: a script and the
/// JSON it generates are one revision, and the worst case is what `activate`
/// weighs against the budget.
#[test]
fn a_workflow_document_is_a_draft_until_its_cost_is_accepted() {
    let s = Scratch::new("workflow");
    let home = s.0.join("home");
    fs::create_dir_all(&home).unwrap();
    let run = |args: &[&str]| {
        let out = command(env!("CARGO_BIN_EXE_pma"))
            .env("PMA_HOME", &home)
            .args(args)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.success(),
        )
    };

    let script = s.0.join("lib.rhai");
    fs::write(
        &script,
        r#"
        fn reviewer(node, what) {
            |g| g.expand(node, "finding", "{$breadth}",
                         "Review `{name}` for " + what + ". Write findings to {out}.")
        }
        let graph = source("project")
            .fan([reviewer("bugs", "correctness"), reviewer("tests", "coverage")])
            .join("merge", ["title"])
            .filter("confirm", ["reason"], "Confirm each unit in {in}.")
            .output();
        document(#{ finding: #{ fields: #{
            title: req(unique(line(200))),
            reason: line(200),
        }}}, [
            workflow("look", graph, #{
                params: #{ breadth: bounded("int", 4, 6) },
                caps: #{ max_units: 40, max_edits: 0 },
            }),
        ])
        "#,
    )
    .unwrap();

    assert!(run(&["workflow"]).0.contains("no workflow revisions"));

    // The script's document is checked and costed like any other: two
    // reviewers at the parameter's maximum, then one confirm per finding.
    let (out, err, ok) = run(&["workflow", "check", script.to_str().unwrap()]);
    assert!(ok, "{err}");
    assert!(
        out.contains("look(in: [project], breadth = 4) -> [finding]  pure"),
        "{out}"
    );
    assert!(
        out.contains("worst case: 14 agent runs, 0 edits, $14.00"),
        "{out}"
    );

    // The same document, emitted as JSON, is the same document.
    let (json, err, ok) = run(&["workflow", "check", script.to_str().unwrap(), "--emit-json"]);
    assert!(ok, "{err}");
    let from_json = s.0.join("lib.json");
    fs::write(&from_json, &json).unwrap();
    let (a, _, _) = run(&["workflow", "check", from_json.to_str().unwrap()]);
    assert_eq!(a, out, "the generated document estimates the same");

    // Proposing stores it as a draft, with its cost per unit of input.
    let (out, err, ok) = run(&[
        "workflow",
        "propose",
        script.to_str().unwrap(),
        "--by",
        "me",
    ]);
    assert!(ok, "{err}");
    assert!(
        out.contains("revision 1 stored as a draft: at most $14.00"),
        "{out}"
    );
    let listed = run(&["workflow"]).0;
    assert!(
        listed.contains("draft") && listed.contains("look"),
        "{listed}"
    );

    // A budget below the worst case refuses activation, naming both numbers.
    run(&["config", "workflow_budget", "10"]);
    let (_, err, ok) = run(&["workflow", "activate", "1", "--by", "me"]);
    assert!(!ok, "a revision over budget must not activate");
    assert!(err.contains("$14.00") && err.contains("$10.00"), "{err}");
    assert!(run(&["workflow"]).0.contains("draft"), "still a draft");

    run(&["config", "workflow_budget", "20"]);
    let (out, err, ok) = run(&["workflow", "activate", "1", "--by", "me"]);
    assert!(ok, "{err}");
    assert!(out.contains("in effect"), "{out}");
    assert!(run(&["workflow"]).0.contains("active, by me"));

    // The stored form is the document, whichever form wrote it, so a script's
    // revision reads back without the script.
    let shown = run(&["workflow", "show"]).0;
    assert!(shown.contains("\"name\": \"look\""), "{shown}");
    assert!(shown.contains("\"max_units\": \"{$breadth}\""), "{shown}");
    assert!(!shown.contains("fan("), "the script is not the document");
}

/// A pass over a workflow whose every node is a rule: it runs, it writes what
/// `pma` owns, and it spends nothing. A second pass advances nothing, because
/// what has run is derived from the units rather than from a cursor.
#[test]
fn a_pass_runs_the_free_nodes_and_never_spends_without_approval() {
    let s = Scratch::new("pass");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = root.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    git(&alpha, &["init", "-q"], None);
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## Critical\n\n## High\n\n- [ ] first thing\n- [x] done thing\n\n## Medium\n\n## Low\n",
    )
    .unwrap();
    git(&alpha, &["add", "."], None);
    git(&alpha, &["commit", "-qm", "init"], None);

    ok(&home, &["root", "add", root.to_str().unwrap()]);
    ok(&home, &["scan", "--offline"]);

    // Every node here is a rule or a check, so the worst case is $0.00.
    let free = s.0.join("free.rhai");
    fs::write(
        &free,
        r#"
        let graph = source("project")
            .check("lint", "lint-todo")
            .rule_expand("items", "item", "todo-items", 100)
            .rule_filter("open", "where:done=false")
            .emit_note("record", #{ text: "open: {text}" })
            .output();
        document(#{}, [
            workflow("stocktake", graph, #{ caps: #{ max_units: 200, max_edits: 0 }}),
        ])
        "#,
    )
    .unwrap();
    let out = ok(&home, &["workflow", "propose", free.to_str().unwrap()]);
    assert!(
        out.contains("at most $0.00"),
        "a graph of rules is free: {out}"
    );
    ok(&home, &["workflow", "activate", "1"]);

    // A dry run plans and prices, and starts nothing: repeating it leaves no
    // instance behind and writes no note.
    for _ in 0..2 {
        let out = ok(
            &home,
            &["workflow", "run", "stocktake", "alpha", "--dry-run"],
        );
        assert!(out.contains("`stocktake` over 1 unit(s)"), "{out}");
        assert!(
            out.contains("would run: lint") && out.contains("a rule, free"),
            "{out}"
        );
    }
    assert!(
        ok(&home, &["note"]).contains("no notes"),
        "a dry run writes nothing"
    );
    assert!(
        !ok(&home, &["workflow"]).contains("instance"),
        "a dry run starts no instance"
    );

    // The pass runs every rule node to exhaustion, in one invocation.
    ok(&home, &["workflow", "run", "stocktake", "alpha"]);
    let out = ok(&home, &["workflow", "run", "stocktake", "--instance", "1"]);
    assert!(out.contains("nothing left to run"), "{out}");
    let notes = ok(&home, &["note"]);
    assert!(notes.contains("open: first thing"), "{notes}");
    assert!(
        !notes.contains("done thing"),
        "the filter dropped it: {notes}"
    );

    // Again: the frontier is re-derived, and everything has been consumed.
    let out = ok(&home, &["workflow", "run", "stocktake", "--instance", "1"]);
    assert!(out.contains("nothing left to run"), "{out}");
    assert_eq!(
        ok(&home, &["note"]).lines().count(),
        notes.lines().count(),
        "a second pass writes no second note"
    );
}

/// A pass that reaches a node an agent decides stops, prices it, and spends
/// nothing until the developer says so. Approved, it runs the node, admits
/// what the model returned through the declared type, and carries it on.
#[test]
fn an_agent_node_is_priced_then_run_on_approval() {
    let s = Scratch::new("gate");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = root.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    git(&alpha, &["init", "-q"], None);
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] a thing\n",
    )
    .unwrap();
    git(&alpha, &["add", "."], None);
    git(&alpha, &["commit", "-qm", "init"], None);

    // A worker that records every invocation, so "nothing was spent" is a
    // fact about the filesystem rather than a claim. It answers by writing
    // the file the prompt named, which is the protocol a node uses.
    let bin = s.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let log = s.0.join("spend.log");
    fs::write(
        bin.join("claude"),
        format!(
            "#!/bin/sh\n\
             echo invoked >> {}\n\
             out=$(printf '%s' \"$2\" | sed -n 's/.*findings to //p')\n\
             printf '[{{\"title\":\"a finding\",\"@id\":\"u1\"}}]' > \"$out\"\n\
             printf '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"ok\",\"total_cost_usd\":0.1}}\\n'\n",
            log.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let run = |args: &[&str]| {
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = command(env!("CARGO_BIN_EXE_pma"))
            .env("PMA_HOME", &home)
            .env("PATH", path)
            .args(args)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.success(),
        )
    };
    run(&["root", "add", root.to_str().unwrap()]);
    run(&["scan", "--offline"]);

    let wf = s.0.join("review.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .expand("review", "finding", 3, "Review `{name}`. Write findings to {out}")
            .emit_note("record", #{ text: "found: {title}" })
            .output();
        document(#{ finding: #{ fields: #{ title: req(line(200)) }}}, [
            workflow("review", graph, #{ caps: #{ max_units: 10, max_edits: 0 }}),
        ])
        "#,
    )
    .unwrap();
    let (out, err, success) = run(&["workflow", "propose", wf.to_str().unwrap()]);
    assert!(success, "{err}");
    // One review run over one project; `max_units` bounds the findings it may
    // produce, not the runs it takes.
    assert!(out.contains("at most $1.00"), "{out}");
    run(&["workflow", "activate", "1"]);

    // The pass names the node, its worker and model, and its ceiling, and
    // stops without running it.
    let (out, err, success) = run(&["workflow", "run", "review", "alpha", "-m", "haiku"]);
    assert!(success, "{err}");
    assert!(out.contains("next: review"), "{out}");
    assert!(
        out.contains("claude/haiku"),
        "the cheap model it would use: {out}"
    );
    assert!(out.contains("Nothing was spent"), "{out}");
    assert!(out.contains("--approve"), "it says how to approve: {out}");
    assert!(!log.exists(), "the worker must not have run");
    let id = plan_id(&out);

    // An approval names the plan it was shown. Without `-m haiku` the plan
    // would run another model, so it is another plan and nothing runs.
    let (_, err, success) = run(&[
        "workflow",
        "run",
        "review",
        "--instance",
        "1",
        "--approve",
        &id,
    ]);
    assert!(!success && err.contains("not the plan"), "{err}");
    assert!(!log.exists(), "a refused approval spends nothing");

    // The plan it was shown is what unlocks it. The finding the model wrote
    // enters the graph as a unit and reaches the sink downstream of it.
    let (out, err, success) = run(&[
        "workflow",
        "run",
        "review",
        "--instance",
        "1",
        "-m",
        "haiku",
        "--approve",
        &id,
    ]);
    assert!(success, "{err}");
    assert!(out.contains("nothing left to run"), "{out}");
    assert_eq!(
        fs::read_to_string(&log).unwrap().lines().count(),
        1,
        "one run per unit"
    );
    assert!(
        run(&["note"]).0.contains("found: a finding"),
        "the unit carried on"
    );

    // A review changes no files, so its run is done and waits on no one.
    assert_eq!(run(&["review"]).0, "no runs to review\n");
    let detail = run(&["review", "1"]).0;
    assert!(detail.starts_with("#1 done"), "{detail}");
    assert!(
        detail.contains("(none: this node changes no files)"),
        "{detail}"
    );

    // The spend is recorded against the workflow, not loose in the run table.
    let report = run(&["report"]).0;
    assert!(
        report.contains("0.10"),
        "the run's cost is recorded: {report}"
    );
}

/// `todo-audit` from the shipped library, end to end: a claim that an open
/// item is done, confirmed, becomes a note, and `TODO.md` is left alone.
#[test]
fn the_library_todo_audit_notes_a_confirmed_claim() {
    let s = Scratch::new("audit");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = root.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    git(&alpha, &["init", "-q"], None);
    let todo = "# TODO\n\n## High\n\n- [ ] add greeting\n";
    fs::write(alpha.join("TODO.md"), todo).unwrap();
    git(&alpha, &["add", "."], None);
    git(&alpha, &["commit", "-qm", "init"], None);

    // The claim step writes one unit; the confirm step keeps what it reads.
    let bin = s.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("claude"),
        r#"#!/usr/bin/env python3
import json, re, sys
prompt = sys.argv[2]
out = re.search(r"(\S+/out\.json)", prompt).group(1)
if "claims the TODO.md item" in prompt:
    units = json.load(open(out.replace("out.json", "in.json")))
    for u in units:
        u["reason"] = ["hello.txt exists"]
else:
    units = [{"text": "add greeting", "verdict": "done", "evidence": "hello.txt:1"}]
json.dump(units, open(out, "w"))
print(json.dumps({"type": "result", "subtype": "success", "is_error": False,
                  "result": "ok", "total_cost_usd": 0.1}))
"#,
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let run = |args: &[&str]| {
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = command(env!("CARGO_BIN_EXE_pma"))
            .env("PMA_HOME", &home)
            .env("PATH", path)
            .args(args)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "pma {args:?}: {text}{err}");
        text
    };
    run(&["root", "add", root.to_str().unwrap()]);
    run(&["scan", "--offline"]);
    let library = s.0.join("library.json");
    fs::write(&library, run(&["workflow", "library"])).unwrap();
    run(&["workflow", "propose", library.to_str().unwrap()]);
    run(&["config", "workflow_budget", "100"]);
    run(&["workflow", "activate", "1"]);

    // Two agent steps, each approved by the plan it was shown.
    let out = run(&["workflow", "run", "todo-audit", "alpha"]);
    let out = run(&[
        "workflow",
        "run",
        "todo-audit",
        "--instance",
        "1",
        "--approve",
        &plan_id(&out),
    ]);
    let out = run(&[
        "workflow",
        "run",
        "todo-audit",
        "--instance",
        "1",
        "--approve",
        &plan_id(&out),
    ]);
    assert!(out.contains("nothing left to run"), "{out}");
    let notes = run(&["note"]);
    assert!(
        notes.contains("TODO.md item looks done: add greeting. hello.txt:1"),
        "{notes}"
    );
    assert_eq!(fs::read_to_string(alpha.join("TODO.md")).unwrap(), todo);
}

/// The `doc` sink appends one block per unit to a file in the clone, left
/// uncommitted, and a second pass adds nothing it already wrote.
#[test]
fn the_doc_sink_appends_a_block_per_unit() {
    let s = Scratch::new("doc-sink");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = root.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    git(&alpha, &["init", "-q"], None);
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] first thing\n- [ ] second thing\n",
    )
    .unwrap();
    git(&alpha, &["add", "."], None);
    git(&alpha, &["commit", "-qm", "init"], None);
    ok(&home, &["root", "add", root.to_str().unwrap()]);
    ok(&home, &["scan", "--offline"]);

    let wf = s.0.join("list.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .rule_expand("items", "item", "todo-items", 10)
            .emit_doc("write", #{ path: "OPEN.md", text: "- {text}" })
            .output();
        document(#{}, [
            workflow("list", graph, #{ caps: #{ max_units: 20, max_edits: 0 }}),
        ])
        "#,
    )
    .unwrap();
    ok(&home, &["workflow", "propose", wf.to_str().unwrap()]);
    ok(&home, &["workflow", "activate", "1"]);
    ok(&home, &["workflow", "run", "list", "alpha"]);
    let doc = alpha.join("OPEN.md");
    assert_eq!(
        fs::read_to_string(&doc).unwrap(),
        "- first thing\n\n- second thing\n"
    );
    ok(&home, &["workflow", "run", "list", "alpha"]);
    assert_eq!(
        fs::read_to_string(&doc).unwrap(),
        "- first thing\n\n- second thing\n",
        "a second instance writes nothing twice"
    );
    assert!(git_out(&alpha, &["status", "--porcelain"]).contains("?? OPEN.md"));
}

/// Tiering a portfolio by hand is the pain this file replaces: a dump of every
/// project, edited, read back.
#[test]
fn tiers_and_tags_go_out_to_a_file_and_come_back() {
    let s = Scratch::new("bulk");
    let home = s.0.join("home");
    let root = s.0.join("root");
    for name in ["alpha", "beta", "gamma"] {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"], None);
    }
    ok(&home, &["root", "add", root.to_str().unwrap()]);
    ok(&home, &["scan", "--offline"]);

    // One tier for many projects, in one command.
    ok(&home, &["project", "tier", "2", "alpha", "beta", "gamma"]);
    ok(&home, &["project", "tag", "add", "rust", "alpha"]);

    let csv = s.0.join("projects.csv");
    let out = ok(&home, &["project", "export", csv.to_str().unwrap()]);
    assert!(out.starts_with("3 projects to "), "{out}");
    assert_eq!(
        fs::read_to_string(&csv).unwrap(),
        "name,tier,tags\nalpha,2,rust\nbeta,2\ngamma,2\n"
    );

    // Edited by hand: a retier, a tag added, a tag dropped, a row deleted.
    fs::write(&csv, "name,tier,tags\nalpha,1\nbeta,none,python,cli\n").unwrap();
    let out = ok(&home, &["project", "import", csv.to_str().unwrap()]);
    assert!(out.contains("alpha: tier 2 -> 1"), "{out}");
    assert!(out.contains("alpha: tags rust -> none"), "{out}");
    assert!(out.contains("beta: tier 2 -> none"), "{out}");
    assert!(out.contains("beta: tags none -> python,cli"), "{out}");
    assert!(out.contains("4 changes; --apply to make them"), "{out}");
    assert!(
        out.contains("1 projects the file leaves out keep what they have"),
        "{out}"
    );
    // A dry run changed nothing.
    assert!(ok(&home, &["project"]).contains("alpha  tier 2  rust"));

    let out = ok(
        &home,
        &["project", "import", csv.to_str().unwrap(), "--apply"],
    );
    assert!(out.contains("4 changes applied"), "{out}");
    let listed = ok(&home, &["project"]);
    assert!(listed.contains("alpha  tier 1"), "{listed}");
    assert!(listed.contains("beta   untiered  cli,python"), "{listed}");
    assert!(listed.contains("gamma  tier 2"), "{listed}");
    // Applied once, it is a no-op.
    let out = ok(
        &home,
        &["project", "import", csv.to_str().unwrap(), "--apply"],
    );
    assert!(out.contains("nothing to change"), "{out}");

    // JSON is the same round trip, and the extension is what picks it.
    let json = s.0.join("projects.json");
    ok(&home, &["project", "export", json.to_str().unwrap()]);
    let text = fs::read_to_string(&json).unwrap();
    assert!(
        text.starts_with(r#"[{"name":"alpha","tags":[],"tier":1}"#),
        "{text}"
    );
    fs::write(&json, text.replace(r#""tier":1"#, r#""tier":5"#)).unwrap();
    ok(
        &home,
        &["project", "import", json.to_str().unwrap(), "--apply"],
    );
    assert!(ok(&home, &["project"]).contains("alpha  tier 5"));

    let (_, err, success) = pma_in(
        &home,
        &["project", "export", &s.0.join("p.txt").to_string_lossy()],
    );
    assert!(!success && err.contains(".csv` or `.json"), "{err}");

    // A file naming something that is not a project is refused whole.
    fs::write(&csv, "name,tier,tags\nalpha,3\ndelta,1\n").unwrap();
    let (_, err, success) = pma_in(
        &home,
        &["project", "import", csv.to_str().unwrap(), "--apply"],
    );
    assert!(!success && err.contains("not projects: delta"), "{err}");
    assert!(
        ok(&home, &["project"]).contains("alpha  tier 5"),
        "nothing applied"
    );
}

/// An instance is frozen at the revision it started under. Activating another
/// between passes must not change the graph an open instance walks, and the
/// name on the command line must be the one the instance runs.
#[test]
fn an_instance_resumes_under_its_own_revision() {
    let s = Scratch::new("resume");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = root.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    git(&alpha, &["init", "-q"], None);
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] a thing\n",
    )
    .unwrap();
    git(&alpha, &["add", "."], None);
    git(&alpha, &["commit", "-qm", "init"], None);
    ok(&home, &["root", "add", root.to_str().unwrap()]);
    ok(&home, &["scan", "--offline"]);

    // Two revisions of one workflow, differing only in the node an agent
    // decides, so the plan names which graph a pass is walking.
    let write = |file: &str, node: &str| {
        let path = s.0.join(file);
        fs::write(
            &path,
            format!(
                r#"
                let graph = source("project")
                    .expand("{node}", "finding", 3, "Review `{{name}}`. Findings to {{out}}.")
                    .output();
                document(#{{ finding: #{{ fields: #{{ title: req(line(200)) }}}}}}, [
                    workflow("look", graph, #{{ caps: #{{ max_units: 10, max_edits: 0 }}}}),
                ])
                "#
            ),
        )
        .unwrap();
        path
    };
    let first = write("one.rhai", "review");
    let second = write("two.rhai", "audit");

    ok(&home, &["workflow", "propose", first.to_str().unwrap()]);
    ok(&home, &["workflow", "activate", "1"]);
    let out = ok(&home, &["workflow", "run", "look", "alpha"]);
    assert!(out.contains("next: review"), "{out}");

    // Revision 2 is in effect, and instance 1 still walks revision 1.
    ok(&home, &["workflow", "propose", second.to_str().unwrap()]);
    ok(&home, &["workflow", "activate", "2"]);
    let out = ok(&home, &["workflow", "run", "look", "--instance", "1"]);
    assert!(
        out.contains("next: review") && !out.contains("audit"),
        "an open instance keeps the graph it started with: {out}"
    );

    // A name that is not the instance's is refused, not quietly resolved
    // against whichever document is in effect.
    let (_, err, success) = pma_in(&home, &["workflow", "run", "other", "--instance", "1"]);
    assert!(
        !success && err.contains("runs `look`, not `other`"),
        "{err}"
    );

    let (_, err, success) = pma_in(&home, &["workflow", "run", "look", "--instance", "9"]);
    assert!(!success && err.contains("no workflow instance 9"), "{err}");

    // A target beside `--instance` would be silently dropped, so it is an
    // error: the instance already holds the bag it started with.
    let (_, err, success) = pma_in(
        &home,
        &["workflow", "run", "look", "alpha", "--instance", "1"],
    );
    assert!(
        !success && err.contains("target and arguments it started with"),
        "{err}"
    );
}

/// The caps `activate` weighed are enforced where units are written. A rule
/// that yields more than its declared width stops the pass and names the cap,
/// rather than minting past the figure the developer approved.
#[test]
fn a_rule_that_overflows_its_cap_stops_the_pass() {
    let s = Scratch::new("cap");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = root.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    git(&alpha, &["init", "-q"], None);
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] one\n- [ ] two\n- [ ] three\n",
    )
    .unwrap();
    git(&alpha, &["add", "."], None);
    git(&alpha, &["commit", "-qm", "init"], None);
    ok(&home, &["root", "add", root.to_str().unwrap()]);
    ok(&home, &["scan", "--offline"]);

    // The node declares two units per project; the project holds three items.
    let wf = s.0.join("narrow.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .rule_expand("items", "item", "todo-items", 2)
            .emit_note("record", #{ text: "open: {text}" })
            .output();
        document(#{}, [
            workflow("narrow", graph, #{ caps: #{ max_units: 50, max_edits: 0 }}),
        ])
        "#,
    )
    .unwrap();
    ok(&home, &["workflow", "propose", wf.to_str().unwrap()]);
    ok(&home, &["workflow", "activate", "1"]);

    let (_, err, success) = pma_in(&home, &["workflow", "run", "narrow", "alpha"]);
    assert!(!success, "an over-cap expansion must not be written");
    assert!(
        err.contains("node `items`") && err.contains("max_units") && err.contains("3"),
        "the cap and the overflow are named: {err}"
    );
    assert!(
        ok(&home, &["note"]).contains("no notes"),
        "nothing downstream ran"
    );

    // The instance records why it stopped, and does not resume into the same
    // wall.
    assert!(ok(&home, &["workflow"]).contains("capped"));
    let (_, err, success) = pma_in(&home, &["workflow", "run", "narrow", "--instance", "1"]);
    assert!(!success && err.contains("stopped: capped"), "{err}");
}

/// An `edit` node changes a repository, so it goes through the same gates a
/// dispatch does: a worktree of its own, the class rules, the verify command,
/// and a run left for review. The verdict it writes is what the graph routes
/// on.
#[test]
fn an_edit_node_dispatches_a_run_and_routes_on_its_verdict() {
    let s = Scratch::new("wfedit");
    let worker = r#"#!/bin/sh
echo hi > hello.txt
echo '{"type":"result","subtype":"success","is_error":false,"result":"did it","total_cost_usd":0.1}'
"#;
    let (env, _origin, _alpha) = dispatch_env(&s, &[("claude", worker)]);

    let wf = s.0.join("fix.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .edit("greet", "verify", "Say hello in hello.txt for `{name}`.")
            .when(#{ "@verify": ["passed"] })
            .emit_note("record", #{ text: "verified: {name}" })
            .output();
        document(#{}, [
            workflow("greet", graph, #{ caps: #{ max_units: 10, max_edits: 1 }}),
        ])
        "#,
    )
    .unwrap();
    env.ok(&["config", "workflow_budget", "20"]);
    let out = env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    assert!(out.contains("!repo") || out.contains("repo"), "{out}");
    env.ok(&["workflow", "activate", "1"]);

    // Priced and left alone, as any agent node is.
    let out = env.ok(&["workflow", "run", "greet", "alpha"]);
    assert!(out.contains("next: greet") && out.contains("edit"), "{out}");
    assert!(
        env.ok(&["review"]).contains("no runs to review"),
        "no run yet: {out}"
    );

    // Approved: one run, in a worktree of its own, verified and waiting for a
    // decision.
    let id = plan_id(&out);
    let (out, err, success) = env.run(&[
        "workflow",
        "run",
        "greet",
        "--instance",
        "1",
        "--approve",
        &id,
    ]);
    assert!(success, "{err}\n{out}");
    let review = env.ok(&["review"]);
    assert!(
        review.contains("alpha"),
        "the run is in the queue: {review}"
    );
    let report = env.ok(&["report"]);
    assert!(report.contains("0.10"), "the spend is recorded: {report}");

    // `verify` passed, so the guarded edge carried the unit to the sink.
    assert!(
        env.ok(&["note"]).contains("verified: alpha"),
        "the verdict routed the unit"
    );
}

/// `--local --unattended` runs every plan without an approval, starts each
/// edit from the clone's working tree with no remote, and leaves only the
/// edits that passed verify in the clone, unstaged. One at a time, each edit
/// sees the ones before it, and two failures do not refuse a third finding.
#[test]
fn a_local_unattended_instance_leaves_verified_edits_unstaged() {
    let s = Scratch::new("wflocal");
    let worker = r#"#!/bin/sh
case "$2" in
  *"findings to "*)
    out=$(printf '%s' "$2" | sed -n 's/.*findings to //p')
    printf '[{"title":"greet"},{"title":"break"},{"title":"break2"},{"title":"append"}]' > "$out" ;;
  *"Fix greet"*) echo hi > hello.txt; printf 'A\nb\nc\n\n' > notes.txt ;;
  *"Fix append"*) echo more >> hello.txt ;;
  *) rm -f hello.txt; echo x > other.txt ;;
esac
echo '{"type":"result","subtype":"success","is_error":false,"result":"ok","total_cost_usd":0.1}'
"#;
    let (env, _origin, alpha) = dispatch_env(&s, &[("claude", worker)]);
    // A commit the remote never saw, and no remote to fetch from.
    fs::write(alpha.join("local.txt"), "mine\n").unwrap();
    // Changing its first line gives a diff whose last line is a blank
    // context line: a single space.
    fs::write(alpha.join("notes.txt"), "a\nb\nc\n\n").unwrap();
    git(&alpha, &["add", "local.txt", "notes.txt"], None);
    git(&alpha, &["commit", "-qm", "local"], None);
    git(&alpha, &["remote", "remove", "origin"], None);
    let head = git_out(&alpha, &["rev-parse", "HEAD"]);

    let wf = s.0.join("review-fix.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .expand("review", "finding", 4, "Review `{name}`. Write findings to {out}")
            .edit("fix", "verify", "Fix {title}.")
            .output();
        document(#{ finding: #{ fields: #{ title: req(line(200)) }}}, [
            workflow("review-fix", graph, #{ caps: #{ max_units: 10, max_edits: 4 }}),
        ])
        "#,
    )
    .unwrap();
    env.ok(&["config", "workflow_budget", "20"]);
    env.ok(&["config", "max_parallel", "1"]);
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);

    let (out, err, success) = env.run(&[
        "workflow",
        "run",
        "review-fix",
        "alpha",
        "--local",
        "--unattended",
    ]);
    assert!(success, "{err}\n{out}");
    assert_eq!(
        out.matches("approved unattended").count(),
        2,
        "the review plan, then the fix plan: {out}"
    );
    assert!(out.contains("nothing left to run"), "{out}");
    assert!(out.contains("Fix greet.: applied"), "{out}");
    assert!(
        out.contains("Fix break.: discarded: verify failed"),
        "{out}"
    );
    assert!(
        out.contains("Fix break2.: discarded: verify failed"),
        "{out}"
    );
    assert!(out.contains("Fix append.: applied"), "{out}");
    assert!(!out.contains("refused"), "{out}");

    // Both verified edits are in the working tree, unstaged, the second on
    // top of the first; the failed ones left nothing.
    assert_eq!(
        fs::read_to_string(alpha.join("hello.txt")).unwrap(),
        "hi\nmore\n"
    );
    assert!(!alpha.join("other.txt").exists());
    assert_eq!(
        fs::read_to_string(alpha.join("notes.txt")).unwrap(),
        "A\nb\nc\n\n"
    );
    assert_eq!(
        git_out(&alpha, &["status", "--porcelain"]),
        " M notes.txt\n?? hello.txt\n"
    );
    assert_eq!(
        git_out(&alpha, &["rev-parse", "HEAD"]),
        head,
        "nothing committed"
    );
    // Both edit runs are settled. The read node's run stays listed, as any
    // read node's does.
    let review = env.ok(&["review"]);
    assert!(!review.contains("fix"), "{review}");
}

/// Polls `pma job` until `done` holds for its output, or panics after
/// `seconds`.
fn wait_for_job(env: &Env, seconds: u64, done: impl Fn(&str) -> bool) -> String {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    loop {
        let jobs = env.ok(&["job"]);
        if done(&jobs) {
            return jobs;
        }
        assert!(std::time::Instant::now() < until, "timed out: {jobs}");
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

const GREET: &str = r#"
    let graph = source("project")
        .edit("greet", "verify", "Say hello in hello.txt for `{name}`.")
        .output();
    document(#{}, [
        workflow("greet", graph, #{ caps: #{ max_units: 10, max_edits: 1 }}),
    ])
"#;

/// `--unattended` without `--local` approves each plan, not each change: the
/// edit runs in its own worktree of the remote default branch and waits in
/// `pma review` like any other run. Nothing is pushed and the clone is left
/// as it was.
#[test]
fn an_unattended_instance_without_local_leaves_its_edits_for_review() {
    let s = Scratch::new("wfunattended");
    let worker = r#"#!/bin/sh
echo hi > hello.txt
echo '{"type":"result","subtype":"success","is_error":false,"result":"ok","total_cost_usd":0.1}'
"#;
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", worker)]);
    let wf = s.0.join("greet.rhai");
    fs::write(&wf, GREET).unwrap();
    env.ok(&["config", "workflow_budget", "20"]);
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);
    let before = git_out(&origin, &["rev-parse", "HEAD"]);

    let out = env.ok(&["workflow", "run", "greet", "alpha", "--unattended"]);
    assert!(out.contains("approved unattended"), "{out}");
    assert!(out.contains("nothing left to run"), "{out}");
    assert!(
        env.ok(&["review"]).contains("#1  ready"),
        "the edit waits for a person"
    );
    assert!(!alpha.join("hello.txt").exists(), "the clone is untouched");
    assert_eq!(
        git_out(&origin, &["rev-parse", "HEAD"]),
        before,
        "nothing pushed"
    );
    assert!(!env.push_log.exists(), "no push was attempted");
}

/// `instance_budget` stops unattended approval before a plan would pass it,
/// counting what the instance's runs reported; a plan approved by its id is
/// not held, and `off` lifts the cap.
#[test]
fn an_unattended_instance_stops_at_its_budget() {
    let s = Scratch::new("wfbudget");
    let worker = r#"#!/bin/sh
case "$2" in
  *"findings to "*)
    out=$(printf '%s' "$2" | sed -n 's/.*findings to //p')
    printf '[{"title":"a"},{"title":"b"},{"title":"c"},{"title":"d"}]' > "$out" ;;
  *"Confirm into "*)
    out=$(printf '%s' "$2" | sed -n 's/.*Confirm into //p')
    printf '[]' > "$out" ;;
esac
echo '{"type":"result","subtype":"success","is_error":false,"result":"ok","total_cost_usd":0.1}'
"#;
    let (env, _origin, _alpha) = dispatch_env(&s, &[("claude", worker)]);
    let wf = s.0.join("look.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .expand("review", "finding", 4, "Review `{name}`. Write findings to {out}")
            .filter("confirm", ["reason"], "Confirm into {out}")
            .output();
        document(#{ finding: #{ fields: #{ title: req(line(200)), reason: line(200) }}}, [
            workflow("look", graph, #{ caps: #{ max_units: 10, max_edits: 0 }}),
        ])
        "#,
    )
    .unwrap();
    assert_eq!(env.ok(&["config", "instance_budget"]).trim(), "off");
    let (_, err, ok) = env.run(&["config", "instance_budget", "cheap"]);
    assert!(!ok && err.contains("or `off`"), "{err}");
    env.ok(&["config", "workflow_budget", "20"]);
    // Plans of two runs at $1 each, at most.
    env.ok(&["config", "batch_budget", "2"]);
    env.ok(&["config", "instance_budget", "2"]);
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);

    // The review fits: $1 at most, nothing spent. Two confirms would not:
    // $0.10 spent and $2 more is over $2.
    let out = env.ok(&["workflow", "run", "look", "alpha", "--unattended"]);
    assert!(out.contains("instance_budget is $2.00"), "{out}");
    assert_eq!(out.matches("approved unattended").count(), 1, "{out}");
    assert!(
        out.contains(
            "instance 1 has spent $0.10 of instance_budget $2.00; \
             the next plan, at most $2.00, would pass it"
        ),
        "{out}"
    );
    assert!(out.contains("--instance 1 --approve"), "{out}");

    // Approved by its id, the plan runs whatever the cap.
    let id = plan_id(&out);
    let out = env.ok(&[
        "workflow",
        "run",
        "look",
        "--instance",
        "1",
        "--approve",
        &id,
    ]);
    assert!(out.contains("the approved plan ran"), "{out}");
    // Resumed unattended, the cap holds over every pass of the instance.
    let out = env.ok(&["workflow", "run", "look", "--instance", "1", "--unattended"]);
    assert!(
        out.contains("has spent $0.30 of instance_budget $2.00"),
        "{out}"
    );
    assert!(!out.contains("approved unattended"), "{out}");

    env.ok(&["config", "instance_budget", "off"]);
    let out = env.ok(&["workflow", "run", "look", "--instance", "1", "--unattended"]);
    assert!(out.contains("nothing left to run"), "{out}");
}

/// `--detach` queues the command and returns; a runner it started runs it
/// to the end with no terminal, and `pma job` reports how it ended.
#[test]
fn a_detached_workflow_runs_to_the_end_in_the_background() {
    let s = Scratch::new("detach");
    let worker = r#"#!/bin/sh
echo hi > hello.txt
echo '{"type":"result","subtype":"success","is_error":false,"result":"ok","total_cost_usd":0.1}'
"#;
    let (env, _origin, alpha) = dispatch_env(&s, &[("claude", worker)]);
    let wf = s.0.join("greet.rhai");
    fs::write(&wf, GREET).unwrap();
    env.ok(&["config", "workflow_budget", "20"]);
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);

    let out = env.ok(&[
        "workflow",
        "run",
        "greet",
        "alpha",
        "--local",
        "--unattended",
        "--detach",
    ]);
    assert!(out.contains("job 1 queued"), "{out}");

    let jobs = wait_for_job(&env, 60, |j| {
        !j.contains("queued") && !j.contains("running")
    });
    assert!(jobs.contains("#1") && jobs.contains("done"), "{jobs}");
    assert!(
        jobs.contains("pma workflow run greet alpha --local --unattended"),
        "the command, less --detach: {jobs}"
    );
    let log = env.ok(&["job", "log", "1"]);
    assert!(log.contains("nothing left to run"), "{log}");
    assert_eq!(fs::read_to_string(alpha.join("hello.txt")).unwrap(), "hi\n");
}

/// Stopping a running job kills its process group, and the agent with it:
/// the agent's watchdog sees its `pma` exit.
#[test]
fn stopping_a_running_job_kills_its_agent() {
    let s = Scratch::new("jobstop");
    let pidfile = s.0.join("agent.pid");
    let worker = format!(
        "#!/bin/sh\n\
         echo '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{{\"command\":\"make test\"}}}}]}}}}'\n\
         echo $$ > {}\nexec sleep 60\n",
        pidfile.display()
    );
    let (env, _origin, _alpha) = dispatch_env(&s, &[("claude", &worker)]);
    let wf = s.0.join("greet.rhai");
    fs::write(&wf, GREET).unwrap();
    env.ok(&["config", "workflow_budget", "20"]);
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);
    env.ok(&[
        "workflow",
        "run",
        "greet",
        "alpha",
        "--local",
        "--unattended",
        "--detach",
    ]);

    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let pid = loop {
        if let Ok(p) = fs::read_to_string(&pidfile)
            && !p.trim().is_empty()
        {
            break p.trim().to_string();
        }
        assert!(std::time::Instant::now() < until, "the agent never started");
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let alive = |pid: &str| {
        command("kill")
            .args(["-0", pid])
            .status()
            .unwrap()
            .success()
    };
    assert!(alive(&pid));

    // `pma watch` reads the running agent's log.
    let watch = env.ok(&["watch", "--once"]);
    assert!(watch.contains("job #1 running"), "{watch}");
    assert!(
        watch.contains("1 tool calls") && watch.contains("Bash: make test"),
        "{watch}"
    );

    let out = env.ok(&["job", "stop", "1"]);
    assert!(out.contains("job 1 stopped"), "{out}");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while alive(&pid) {
        assert!(
            std::time::Instant::now() < until,
            "the agent outlived its job"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let jobs = wait_for_job(&env, 10, |j| j.contains("stopped"));
    assert!(!jobs.contains("running"), "{jobs}");
}

/// Without `--unattended`, a detached workflow stops at its plan and waits;
/// `pma job approve` queues the approval with the flags the plan was priced
/// at, and `notify` hears of each job's end.
#[test]
fn a_detached_job_waits_for_approval_and_notifies() {
    let s = Scratch::new("jobapprove");
    let worker = r#"#!/bin/sh
echo hi > hello.txt
echo '{"type":"result","subtype":"success","is_error":false,"result":"ok","total_cost_usd":0.1}'
"#;
    let (env, _origin, alpha) = dispatch_env(&s, &[("claude", worker)]);
    let wf = s.0.join("greet.rhai");
    fs::write(&wf, GREET).unwrap();
    let heard = s.0.join("heard.log");
    env.ok(&["config", "workflow_budget", "20"]);
    env.ok(&[
        "config",
        "notify",
        &format!("echo \"$PMA_JOB_ID $PMA_JOB_STATE\" >> {}", heard.display()),
    ]);
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);

    env.ok(&[
        "workflow", "run", "greet", "alpha", "--local", "-m", "haiku", "--detach",
    ]);
    let jobs = wait_for_job(&env, 60, |j| j.contains("awaiting-approval"));
    assert!(jobs.contains("pma job approve 1"), "{jobs}");
    assert!(env.ok(&["job", "log", "1"]).contains("next: greet"));
    assert!(!alpha.join("hello.txt").exists(), "nothing ran unapproved");

    let out = env.ok(&["job", "approve", "1"]);
    assert!(out.contains("job 2 queued"), "{out}");
    assert!(
        out.contains("-m haiku"),
        "the plan was priced at haiku: {out}"
    );
    let (_, err, success) = env.run(&["job", "approve", "1"]);
    assert!(!success && err.contains("not awaiting approval"), "{err}");

    let jobs = wait_for_job(&env, 60, |j| j.contains("#2") && j.contains("done"));
    assert!(jobs.contains("approved"), "job 1 is settled: {jobs}");
    assert_eq!(fs::read_to_string(alpha.join("hello.txt")).unwrap(), "hi\n");
    let log = env.ok(&["job", "log", "2"]);
    assert!(log.contains("nothing left to run"), "{log}");
    // The hook runs after the job is marked done, so it may be a moment.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !fs::read_to_string(&heard).unwrap().contains("2 done") {
        assert!(
            std::time::Instant::now() < until,
            "notify did not hear job 2"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(
        fs::read_to_string(&heard).unwrap(),
        "1 awaiting-approval\n2 done\n"
    );
}

/// A child process killed when dropped, so a failed assertion does not leave
/// a server running that holds the test's output open.
struct Killed(std::process::Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Two local edits in one group that change the same line: the first lands,
/// the second no longer applies to the clone it changed, and its unit goes
/// back in the queue to be done again from that clone, rather than lost.
#[test]
fn a_local_edit_overtaken_in_its_group_is_done_again() {
    let s = Scratch::new("wfconflict");
    let worker = r#"#!/bin/sh
case "$2" in
  *"findings to "*)
    out=$(printf '%s' "$2" | sed -n 's/.*findings to //p')
    printf '[{"title":"one"},{"title":"two"}]' > "$out" ;;
  *"Fix one"*) echo hi > hello.txt; echo one > notes.txt ;;
  *"Fix two"*) echo hi > hello.txt; echo two > notes.txt ;;
esac
echo '{"type":"result","subtype":"success","is_error":false,"result":"ok","total_cost_usd":0.1}'
"#;
    let (env, _origin, alpha) = dispatch_env(&s, &[("claude", worker)]);
    fs::write(alpha.join("notes.txt"), "base\n").unwrap();
    git(&alpha, &["add", "notes.txt"], None);
    git(&alpha, &["commit", "-qm", "notes"], None);
    let wf = s.0.join("review-fix.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .expand("review", "finding", 2, "Review `{name}`. Write findings to {out}")
            .edit("fix", "verify", "Fix {title}.")
            .output();
        document(#{ finding: #{ fields: #{ title: req(line(200)) }}}, [
            workflow("review-fix", graph, #{ caps: #{ max_units: 10, max_edits: 4 }}),
        ])
        "#,
    )
    .unwrap();
    env.ok(&["config", "workflow_budget", "20"]);
    env.ok(&["config", "max_parallel", "2"]);
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);

    let out = env.ok(&[
        "workflow",
        "run",
        "review-fix",
        "alpha",
        "--local",
        "--unattended",
    ]);
    assert_eq!(
        out.matches("its unit goes back in the queue").count(),
        1,
        "one of the two was overtaken: {out}"
    );
    assert_eq!(
        out.matches(": applied to").count(),
        2,
        "both landed in the end: {out}"
    );
    assert!(out.contains("nothing left to run"), "{out}");
    let notes = fs::read_to_string(alpha.join("notes.txt")).unwrap();
    assert!(notes == "one\n" || notes == "two\n", "{notes}");
    // The overtaken edit was refused by the other's landing, not judged.
    let report = env.ok(&["report"]);
    assert!(report.starts_with("nothing decided yet"), "{report}");
    assert!(
        report.contains("rejected for infrastructure, in no share: #"),
        "{report}"
    );
}

/// One request to a local server, read to the end; the server closes the
/// connection after each answer.
fn http(port: u16, raw: &str) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    s.write_all(raw.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

fn post(port: u16, path: &str, origin: &str, cookie: &str, body: &str) -> String {
    http(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: {origin}\r\n\
             Content-Type: application/json\r\nCookie: {cookie}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
}

/// `pma web` serves this machine only: a request naming another host, a POST
/// from another origin or not in JSON, and a data request without the
/// session cookie are each refused. With the cookie, the page reads and acts
/// on the same store the CLI does.
#[test]
fn the_web_view_is_local_and_shares_the_store() {
    use std::io::{BufRead, BufReader, Read};
    let s = Scratch::new("web");
    let env = Env {
        home: s.0.join("home"),
        bin: s.0.join("bin"),
        push_log: s.0.join("push.log"),
    };
    let mut child = env
        .command(&["web", "--no-open"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map(Killed)
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.0.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    let url = first.trim().strip_prefix("pma web: ").expect("the address");
    let rest = url
        .strip_prefix("http://127.0.0.1:")
        .expect("loopback only");
    let (port, token) = rest.split_once("/#").expect("the token in the fragment");
    let port: u16 = port.parse().unwrap();
    let origin = format!("http://127.0.0.1:{port}");
    let get = |path: &str, host: &str, cookie: &str| {
        http(
            port,
            &format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nCookie: {cookie}\r\n\r\n"),
        )
    };
    let host = format!("127.0.0.1:{port}");

    let page = get("/", &host, "");
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    assert!(page.contains("Content-Security-Policy: default-src 'none'; script-src 'self'"));
    assert!(!page.contains(token), "the page holds no secret");
    assert!(get("/", &format!("evil.example:{port}"), "").starts_with("HTTP/1.1 421"));
    assert!(get("/api/keys", &host, "").starts_with("HTTP/1.1 403"));
    assert!(get("/api/keys", &host, "pma_session=nope").starts_with("HTTP/1.1 403"));

    let ok = format!("{{\"token\":\"{token}\"}}");
    assert!(post(port, "/session", "http://evil.example", "", &ok).starts_with("HTTP/1.1 403"));
    assert!(
        post(port, "/session", &origin, "", "{\"token\":\"0000\"}").starts_with("HTTP/1.1 403")
    );
    let session = post(port, "/session", &origin, "", &ok);
    assert!(session.starts_with("HTTP/1.1 204"), "{session}");
    assert!(session.contains("HttpOnly; SameSite=Strict"), "{session}");
    let cookie = format!("pma_session={token}");

    // Through `ssh -L 9000:127.0.0.1:<port>`, the browser names the local
    // port: that works, as long as the origin is the host's own.
    assert!(get("/api/keys", "127.0.0.1:9000", &cookie).starts_with("HTTP/1.1 200"));
    let forwarded = http(
        port,
        &format!(
            "POST /session HTTP/1.1\r\nHost: localhost:9000\r\nOrigin: http://localhost:9000\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{ok}",
            ok.len()
        ),
    );
    assert!(forwarded.starts_with("HTTP/1.1 204"), "{forwarded}");
    let crossed = http(
        port,
        &format!(
            "POST /session HTTP/1.1\r\nHost: 127.0.0.1:9000\r\nOrigin: {origin}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{ok}",
            ok.len()
        ),
    );
    assert!(crossed.starts_with("HTTP/1.1 403"), "{crossed}");

    let keys = get("/api/keys", &host, &cookie);
    assert!(keys.contains("\"AddNote\""), "{keys}");
    let note =
        r#"{"tab":"notes","act":"AddNote","pick":{"kind":"nothing"},"input":"from the web"}"#;
    assert!(
        post(port, "/api/act", "http://evil.example", &cookie, note).starts_with("HTTP/1.1 403")
    );
    let plain = http(
        port,
        &format!(
            "POST /api/act HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nCookie: {cookie}\r\n\
             Content-Type: text/plain\r\nContent-Length: {}\r\n\r\n{note}",
            note.len()
        ),
    );
    assert!(plain.starts_with("HTTP/1.1 415"), "{plain}");
    let acted = post(port, "/api/act", &origin, &cookie, note);
    assert!(acted.contains("\"said\":[\"#1\"]"), "{acted}");
    assert!(
        env.ok(&["note"]).contains("from the web"),
        "the CLI sees it"
    );
    let notes = get("/api/screen?tab=notes", &host, &cookie);
    assert!(
        notes.contains("from the web") && notes.contains("\"kind\":\"note\""),
        "{notes}"
    );

    // A confirmed act that cannot apply is refused before its question.
    let ask = post(
        port,
        "/api/ask",
        &origin,
        &cookie,
        r#"{"tab":"inbox","act":"RejectRun","pick":{"kind":"nothing"}}"#,
    );
    assert!(
        ask.starts_with("HTTP/1.1 400") && ask.contains("select a run first"),
        "{ask}"
    );
    let runs = get("/api/screen?tab=inbox", &host, &cookie);
    assert!(
        runs.contains("Waiting on you") && runs.contains("\"items\":[]"),
        "{runs}"
    );

    let mut events = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    events
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    std::io::Write::write_all(
        &mut events,
        format!("GET /api/events HTTP/1.1\r\nHost: {host}\r\nCookie: {cookie}\r\n\r\n").as_bytes(),
    )
    .unwrap();
    let mut buf = [0u8; 2048];
    let mut got = String::new();
    while !got.contains("\n\n") || !got.contains("event: tick") {
        let n = events.read(&mut buf).unwrap();
        assert!(n > 0, "the stream closed: {got}");
        got.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    assert!(got.contains("text/event-stream"), "{got}");

    drop(child);
}

/// `--port` names the port, so an SSH forward can be written once.
#[test]
fn the_web_view_listens_on_the_port_named() {
    use std::io::{BufRead, BufReader};
    let s = Scratch::new("webport");
    let env = Env {
        home: s.0.join("home"),
        bin: s.0.join("bin"),
        push_log: s.0.join("push.log"),
    };
    let free = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = env
        .command(&["web", "--no-open", "--port", &free.to_string()])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map(Killed)
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.0.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    assert!(
        first.starts_with(&format!("pma web: http://127.0.0.1:{free}/#")),
        "{first}"
    );
    let page = http(
        free,
        &format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{free}\r\n\r\n"),
    );
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    drop(child);
    let (_, err, success) = env.run(&["web", "--no-open", "--app"]);
    assert!(!success && err.contains("--app"), "{err}");
}

/// Starts `pma web` for `env` and signs in: the child, its port and the
/// session cookie.
fn start_web(env: &Env) -> (Killed, u16, String) {
    use std::io::{BufRead, BufReader};
    let mut child = env
        .command(&["web", "--no-open"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map(Killed)
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.0.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    let rest = first
        .trim()
        .strip_prefix("pma web: http://127.0.0.1:")
        .expect("the address");
    let (port, token) = rest.split_once("/#").unwrap();
    let port: u16 = port.parse().unwrap();
    let signed = post(
        port,
        "/session",
        &format!("http://127.0.0.1:{port}"),
        "",
        &format!("{{\"token\":\"{token}\"}}"),
    );
    assert!(signed.starts_with("HTTP/1.1 204"), "{signed}");
    (child, port, format!("pma_session={token}"))
}

/// A web request's status and JSON body.
fn web(
    port: u16,
    cookie: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let raw = match body {
        None => http(
            port,
            &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {cookie}\r\n\r\n"),
        ),
        Some(b) => post(
            port,
            path,
            &format!("http://127.0.0.1:{port}"),
            cookie,
            &b.to_string(),
        ),
    };
    let status = raw[9..12].parse().unwrap();
    let body = raw.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    (
        status,
        serde_json::from_str(body).unwrap_or(serde_json::Value::Null),
    )
}

/// The first row of a tab, as the page sees it.
fn first_pick(port: u16, cookie: &str, tab: &str) -> serde_json::Value {
    let (status, screen) = web(port, cookie, &format!("/api/screen?tab={tab}"), None);
    assert_eq!(status, 200, "{screen}");
    screen["body"]["panes"][0]["items"][0]["pick"].clone()
}

/// Two front ends on one store: what one was shown is what it may act on.
/// The web saw run #1 ready; the CLI approved it since; the web's reject is
/// refused until it reads the run again.
#[test]
fn front_ends_racing_on_one_run_act_only_on_what_they_were_shown() {
    let s = Scratch::new("racerun");
    let (env, _origin, _alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    env.ok(&["dispatch", "alpha:5"]);
    let (child, port, cookie) = start_web(&env);
    let seen = first_pick(port, &cookie, "inbox");
    assert_eq!(seen["state"], "ready", "{seen}");

    env.ok(&["review", "1", "--approve"]);
    let reject = |pick: &serde_json::Value| {
        web(
            port,
            &cookie,
            "/api/act",
            Some(serde_json::json!({ "tab": "inbox", "act": "RejectRun", "pick": pick })),
        )
    };
    let (status, body) = reject(&seen);
    assert_eq!(status, 400);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("is now approved, not ready"),
        "{body}"
    );
    assert!(
        env.ok(&["review"]).contains("#1  approved"),
        "nothing changed"
    );

    let fresh = first_pick(port, &cookie, "inbox");
    assert_eq!(fresh["state"], "approved");
    let (status, body) = reject(&fresh);
    assert_eq!(status, 200, "{body}");
    let (_, err, success) = env.run(&["review", "1", "--approve"]);
    assert!(!success && err.contains("rejected"), "{err}");
    drop(child);
}

/// A plan a job waits on is approved once, whichever front end gets there
/// first: the web approves the job, and the CLI's approval of the same job,
/// and of the same plan, are refused.
#[test]
fn front_ends_racing_on_one_plan_approve_it_once() {
    let s = Scratch::new("raceplan");
    let worker = r#"#!/bin/sh
echo hi > hello.txt
echo '{"type":"result","subtype":"success","is_error":false,"result":"ok","total_cost_usd":0.1}'
"#;
    let (env, _origin, alpha) = dispatch_env(&s, &[("claude", worker)]);
    let wf = s.0.join("greet.rhai");
    fs::write(&wf, GREET).unwrap();
    env.ok(&["config", "workflow_budget", "20"]);
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);
    env.ok(&[
        "workflow", "run", "greet", "alpha", "--local", "-m", "haiku", "--detach",
    ]);
    wait_for_job(&env, 60, |j| j.contains("awaiting-approval"));
    let log = env.ok(&["job", "log", "1"]);
    let at = log.find("\nplan ").expect("a plan") + "\nplan ".len();
    let plan: String = log[at..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();

    let (child, port, cookie) = start_web(&env);
    let job = first_pick(port, &cookie, "activity");
    assert_eq!(job["awaiting"], true, "{job}");
    // Queued from the CLI, the job still names its project.
    let (_, project) = web(port, &cookie, "/api/screen?tab=project&project=alpha", None);
    assert!(
        project["body"]["panes"][1]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["cells"][0]["text"] == "job 1"),
        "{project}"
    );
    let (status, body) = web(
        port,
        &cookie,
        "/api/act",
        Some(serde_json::json!({ "tab": "activity", "act": "ApproveJob", "pick": job })),
    );
    assert_eq!(status, 200, "{body}");
    let (_, err, success) = env.run(&["job", "approve", "1"]);
    assert!(!success && err.contains("not awaiting approval"), "{err}");

    wait_for_job(&env, 60, |j| j.contains("#2") && j.contains("done"));
    assert_eq!(fs::read_to_string(alpha.join("hello.txt")).unwrap(), "hi\n");
    let (_, err, success) = env.run(&[
        "workflow",
        "run",
        "greet",
        "--instance",
        "1",
        "-m",
        "haiku",
        "--approve",
        &plan,
    ]);
    assert!(!success, "a spent plan runs again: {err}");
    drop(child);
}

/// A task opens in full from its project: the item's line, description,
/// issue and the acts that fit it; a signal the scan did not find is refused.
#[test]
fn a_task_opens_in_full_from_its_project() {
    let s = Scratch::new("taskopen");
    let (env, _origin, _alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    let (child, port, cookie) = start_web(&env);
    let open = |target: &str| {
        web(
            port,
            &cookie,
            "/api/open",
            Some(serde_json::json!({
                "pick": { "kind": "task", "project": "alpha", "target": target, "retry": false },
            })),
        )
    };
    let (status, body) = open("alpha:5");
    assert_eq!(status, 200, "{body}");
    let d = &body["body"]["detail"];
    assert_eq!(d["title"], "add greeting");
    assert!(
        d["sections"].to_string().contains("say hello in hello.txt"),
        "{d}"
    );
    assert!(d["fields"].to_string().contains("TODO.md:5"), "{d}");
    assert!(d["acts"].to_string().contains("Dispatch"), "{d}");
    let (status, body) = open("alpha:7");
    assert_eq!(status, 200, "{body}");
    assert!(
        body["body"]["detail"]["fields"]
            .to_string()
            .contains("gh:7"),
        "{body}"
    );
    let (status, body) = open("alpha:ci");
    assert_eq!(status, 400);
    assert!(
        body["error"].to_string().contains("no `ci` signal"),
        "{body}"
    );
    drop(child);

    // The CLI prints the same detail.
    let out = env.ok(&["task", "alpha:5"]);
    assert!(
        out.contains("add greeting") && out.contains("TODO.md:5") && out.contains("say hello"),
        "{out}"
    );
    let (_, err, success) = env.run(&["task", "alpha"]);
    assert!(!success && err.contains("project:line"), "{err}");
}

/// Settings are edited from the web as the CLI edits them: a key, a
/// project's tier and a campaign, each visible to the CLI afterwards.
#[test]
fn settings_are_edited_from_the_web() {
    let s = Scratch::new("websettings");
    let (env, _origin, _alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    let (child, port, cookie) = start_web(&env);
    let act = |tab: &str, act: &str, pick: serde_json::Value, input: &str, project: &str| {
        web(
            port,
            &cookie,
            "/api/act",
            Some(serde_json::json!({
                "tab": tab, "act": act, "pick": pick, "input": input, "project": project,
            })),
        )
    };
    let (status, screen) = web(port, &cookie, "/api/screen?tab=settings", None);
    assert_eq!(status, 200, "{screen}");
    let panes = screen["body"]["panes"].as_array().unwrap();
    let titles: Vec<&str> = panes.iter().map(|p| p["title"].as_str().unwrap()).collect();
    assert_eq!(
        titles,
        ["Settings", "Roots", "Agents", "Presets", "Route revisions"]
    );
    let setting = panes[0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["pick"]["key"] == "batch_budget")
        .expect("batch_budget")["pick"]
        .clone();
    let (status, body) = act("settings", "EditSetting", setting, "9", "");
    assert_eq!(status, 200, "{body}");
    assert_eq!(env.ok(&["config", "batch_budget"]).trim(), "9");

    let (status, body) = act(
        "project",
        "Configure",
        serde_json::json!({ "kind": "nothing" }),
        "tier 2",
        "alpha",
    );
    assert_eq!(status, 200, "{body}");
    assert!(env.ok(&["project"]).contains("tier 2"));
    let (status, body) = act(
        "project",
        "Configure",
        serde_json::json!({ "kind": "nothing" }),
        "tier 9",
        "alpha",
    );
    assert_eq!(status, 400, "{body}");

    let (status, body) = act(
        "library",
        "AddCampaign",
        serde_json::json!({ "kind": "nothing" }),
        "greet alpha say hello",
        "",
    );
    assert_eq!(status, 200, "{body}");
    assert!(env.ok(&["campaign"]).contains("greet"), "the CLI lists it");
    let (_, library) = web(port, &cookie, "/api/screen?tab=library", None);
    assert!(
        library.to_string().contains("\"kind\":\"campaign\""),
        "{library}"
    );
    drop(child);
}

/// `lint --fix` lists the mechanical rewrites and leaves the file; with
/// `--apply` it writes them, and what it cannot fix is still reported.
#[test]
fn lint_fix_rewrites_what_a_machine_can() {
    let s = Scratch::new("lintfix");
    let env = Env {
        home: s.0.join("home"),
        bin: s.0.join("bin"),
        push_log: s.0.join("push.log"),
    };
    let dir = s.0.join("proj");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("TODO.md");
    let before = "# Todo\n\n##High\n\n- plain item\n* [ ] star item due:never\n";
    fs::write(&file, before).unwrap();
    let dir_s = dir.to_str().unwrap();
    let out = env.ok(&["lint", "--fix", dir_s]);
    assert!(
        out.contains(":1: `# Todo` -> `# TODO`")
            && out.contains(":5: `- plain item` -> `- [ ] plain item`")
            && out.contains("4 rewrites; run `pma lint --fix --apply`"),
        "{out}"
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        before,
        "listed, not written"
    );
    let out = env.ok(&["lint", "--fix", "--apply", dir_s]);
    assert!(out.contains("4 lines rewritten"), "{out}");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "# TODO\n\n## High\n\n- [ ] plain item\n- [ ] star item due:never\n"
    );
    let (out, _, success) = env.run(&["lint", dir_s]);
    assert!(
        !success && out.contains("is not a date"),
        "the date is yours: {out}"
    );
    let (_, err, success) = env.run(&["lint", "--apply", dir_s]);
    assert!(!success && err.contains("--apply needs"), "{err}");
}

/// Every act the key table binds is one the web server takes by name: asked
/// about with no row selected, it answers with a question or with the reason
/// it cannot apply, never with a request it could not read.
#[test]
fn the_web_understands_every_act_in_the_key_table() {
    let s = Scratch::new("webacts");
    let env = Env {
        home: s.0.join("home"),
        bin: s.0.join("bin"),
        push_log: s.0.join("push.log"),
    };
    let (child, port, cookie) = start_web(&env);
    let (_, keys) = web(port, &cookie, "/api/keys", None);
    let mut asked = 0;
    for b in keys["bindings"].as_array().unwrap() {
        if b["command"]["kind"] != "act" {
            continue;
        }
        let tab = b["tabs"][0].as_str().unwrap_or("inbox");
        let (status, body) = web(
            port,
            &cookie,
            "/api/ask",
            Some(serde_json::json!({
                "tab": tab,
                "act": b["command"]["act"],
                "pick": { "kind": "nothing" },
            })),
        );
        assert!(
            status == 200 || body["error"].is_string(),
            "{}: {status} {body}",
            b["command"]["act"]
        );
        asked += 1;
    }
    assert!(asked >= 21, "{asked}");
    drop(child);
}

/// `pma project show` gives one project's state, open tasks and runs.
#[test]
fn a_project_shows_its_state_tasks_and_runs() {
    let s = Scratch::new("projectshow");
    let (env, _origin, _alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    env.ok(&["dispatch", "alpha:5"]);
    let out = env.ok(&["project", "show", "alpha"]);
    assert!(out.starts_with("alpha  "), "{out}");
    assert!(
        out.contains("Open tasks:") && out.contains("add greeting"),
        "{out}"
    );
    assert!(out.contains("Runs: 1") && out.contains("#1"), "{out}");
    let (_, err, success) = env.run(&["project", "show", "nope"]);
    assert!(!success && err.contains("unknown project `nope`"), "{err}");
}

/// Configuration lives in `settings.toml`. `pma config` writes it, keeping
/// what the user wrote; a hand edit is what the next command reads; and an
/// invalid file stops every command, naming the key.
#[test]
fn settings_toml_is_the_configuration() {
    let s = Scratch::new("settingstoml");
    let env = Env {
        home: s.0.join("home"),
        bin: s.0.join("bin"),
        push_log: s.0.join("push.log"),
    };
    let file = env.home.join("settings.toml");
    env.ok(&["config", "agent_budget", "3"]);
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("[config]\nagent_budget = 3\n"), "{text}");

    fs::write(
        &file,
        text.replace("agent_budget = 3", "# mine\nagent_budget = 4"),
    )
    .unwrap();
    assert_eq!(env.ok(&["config", "agent_budget"]).trim(), "4");
    let (_, err, success) = env.run(&["config", "dispatch_quadrants"]);
    assert!(
        !success && err.contains("`dispatch_quadrants` was retired: "),
        "{err}"
    );
    env.ok(&["config", "max_parallel", "1"]);
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("# mine\nagent_budget = 4\n"), "{text}");
    assert!(text.contains("max_parallel = 1"), "{text}");

    fs::write(&file, text.replace("max_parallel = 1", "max_parallel = 0")).unwrap();
    let (_, err, success) = env.run(&["job"]);
    assert!(!success && err.contains("config.max_parallel"), "{err}");
}

/// An agent drafts changelog entries in `.pma/changelog.md` rather than in
/// `CHANGELOG.md`. pma keeps the file out of the change and shows it in
/// `pma review`; a run that drafted none is flagged, not discarded.
#[test]
fn changelog_entries_are_drafted_beside_the_change() {
    let s = Scratch::new("changelog");
    let worker = r#"#!/bin/sh
case "$2" in
  *"findings to "*)
    out=$(printf '%s' "$2" | sed -n 's/.*findings to //p')
    printf '[{"title":"drafted"},{"title":"silent"}]' > "$out" ;;
  *"Fix drafted"*".pma/changelog.md"*)
    echo hi > hello.txt; mkdir -p .pma; printf '**Greets.** It said nothing.\n' > .pma/changelog.md ;;
  *) echo more >> hello.txt ;;
esac
echo '{"type":"result","subtype":"success","is_error":false,"result":"ok","total_cost_usd":0.1}'
"#;
    let (env, _origin, alpha) = dispatch_env(&s, &[("claude", worker)]);
    fs::write(alpha.join("CHANGELOG.md"), "# Changelog\n").unwrap();
    git(&alpha, &["add", "CHANGELOG.md"], None);
    git(&alpha, &["commit", "-qm", "changelog"], None);
    let wf = s.0.join("review-fix.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .expand("review", "finding", 2, "Review `{name}`. Write findings to {out}")
            .edit("fix", "verify", "Fix {title}.")
            .output();
        document(#{ finding: #{ fields: #{ title: req(line(200)) }}}, [
            workflow("review-fix", graph, #{ caps: #{ max_units: 10, max_edits: 2 }}),
        ])
        "#,
    )
    .unwrap();
    env.ok(&["config", "workflow_budget", "20"]);
    env.ok(&["config", "max_parallel", "1"]);
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);

    let out = env.ok(&[
        "workflow",
        "run",
        "review-fix",
        "alpha",
        "--local",
        "--unattended",
    ]);
    assert!(out.contains("Fix drafted.: applied"), "{out}");
    assert!(out.contains("unstaged; changelog entries in"), "{out}");
    assert!(
        out.contains("Fix silent.: applied")
            && out.contains("unstaged; no changelog entry drafted"),
        "flagged, not discarded: {out}"
    );
    assert_eq!(
        git_out(&alpha, &["status", "--porcelain", "--untracked-files=all"]),
        "?? hello.txt\n",
        "the draft is not part of the change"
    );

    let detail = env.ok(&["review", "2"]);
    assert!(
        detail.contains("changelog:\n  **Greets.** It said nothing.\n"),
        "{detail}"
    );
    assert!(
        env.ok(&["review", "3"]).contains("changelog: none drafted"),
        "the run that drafted none says so"
    );
}

/// A plan a job waits on may also be approved in the foreground. The job is
/// then settled, so it no longer lists as awaiting or approves a spent plan.
#[test]
fn a_foreground_approval_settles_the_waiting_job() {
    let s = Scratch::new("jobfgapprove");
    let worker = r#"#!/bin/sh
echo hi > hello.txt
echo '{"type":"result","subtype":"success","is_error":false,"result":"ok","total_cost_usd":0.1}'
"#;
    let (env, _origin, alpha) = dispatch_env(&s, &[("claude", worker)]);
    let wf = s.0.join("greet.rhai");
    fs::write(&wf, GREET).unwrap();
    env.ok(&["config", "workflow_budget", "20"]);
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);

    env.ok(&[
        "workflow", "run", "greet", "alpha", "--local", "-m", "haiku", "--detach",
    ]);
    wait_for_job(&env, 60, |j| j.contains("awaiting-approval"));
    let log = env.ok(&["job", "log", "1"]);
    assert!(log.contains("Approve it with `pma job approve 1`"), "{log}");
    let at = log.find("\nplan ").expect("a plan") + "\nplan ".len();
    let plan: String = log[at..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();

    env.ok(&[
        "workflow",
        "run",
        "greet",
        "--instance",
        "1",
        "-m",
        "haiku",
        "--approve",
        &plan,
    ]);
    assert_eq!(fs::read_to_string(alpha.join("hello.txt")).unwrap(), "hi\n");
    let jobs = env.ok(&["job"]);
    assert!(
        jobs.contains("approved") && !jobs.contains("awaiting"),
        "{jobs}"
    );
    let (_, err, success) = env.run(&["job", "approve", "1"]);
    assert!(!success && err.contains("not awaiting approval"), "{err}");
}

/// `caps.max_edits` bounds what one instance may change, and it is enforced
/// where the edit is dispatched rather than only where it was estimated.
#[test]
fn an_instance_may_not_exceed_its_edit_cap() {
    let s = Scratch::new("wfcap");
    let worker = r#"#!/bin/sh
echo hi > hello.txt
echo '{"type":"result","subtype":"success","is_error":false,"result":"did it","total_cost_usd":0.1}'
"#;
    let (env, _origin, _alpha) = dispatch_env(&s, &[("claude", worker)]);

    let wf = s.0.join("none.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .edit("greet", "verify", "Say hello in hello.txt for `{name}`.")
            .output();
        document(#{}, [
            workflow("greet", graph, #{ caps: #{ max_units: 10, max_edits: 0 }}),
        ])
        "#,
    )
    .unwrap();
    env.ok(&["config", "workflow_budget", "20"]);
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);
    let id = plan_id(&env.ok(&["workflow", "run", "greet", "alpha", "--dry-run"]));
    let (_, err, success) = env.run(&["workflow", "run", "greet", "alpha", "--approve", &id]);
    assert!(!success, "an instance with no edit budget must not edit");
    assert!(err.contains("max_edits"), "{err}");
    assert!(
        env.ok(&["review"]).contains("no runs to review"),
        "no run was made"
    );
}

/// Unit ids repeat across instances, so a plan id must also bind the target
/// and the arguments: a plan approved for one project runs on no other.
#[test]
fn a_plan_id_names_its_target_and_arguments() {
    let s = Scratch::new("wfplanid");
    let (env, _origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    let root = alpha.parent().unwrap();
    git(root, &["clone", "-q", "../origin.git", "beta"], None);
    env.ok(&["project", "tier", "1", "beta"]);
    env.ok(&["scan", "--offline"]);

    let wf = s.0.join("greet.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .edit("greet", "verify", "Say {$word} in hello.txt for `{name}`.")
            .output();
        document(#{}, [
            workflow("greet", graph, #{
                params: #{ word: param("line", "hello") },
                caps: #{ max_units: 10, max_edits: 10 },
            }),
        ])
        "#,
    )
    .unwrap();
    env.ok(&["config", "workflow_budget", "20"]);
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);
    let dry = |args: &[&str]| {
        let mut all = vec!["workflow", "run", "greet"];
        all.extend_from_slice(args);
        all.push("--dry-run");
        plan_id(&env.ok(&all))
    };
    let alpha_id = dry(&["alpha"]);
    assert_ne!(alpha_id, dry(&["beta"]));
    assert_ne!(alpha_id, dry(&["alpha", "--set", "word=bye"]));
    assert_eq!(alpha_id, dry(&["alpha"]), "the same plan has the same id");

    let (_, err, success) = env.run(&["workflow", "run", "greet", "beta", "--approve", &alpha_id]);
    assert!(
        !success && err.contains("is not the plan for this pass"),
        "{err}"
    );
    assert!(
        env.ok(&["review"]).contains("no runs to review"),
        "no run was made"
    );
}

/// A target names what a workflow reads: a heading yields items, a signal
/// yields one signal, and a bare project yields the project. The type has to
/// be the one the workflow declares, and arguments are checked where they are
/// given rather than where they are read.
#[test]
fn a_target_yields_the_type_the_workflow_reads() {
    let s = Scratch::new("targets");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = root.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    git(&alpha, &["init", "-q"], None);
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] first thing\n- [ ] second thing\n\n## Low\n\n- [ ] later\n",
    )
    .unwrap();
    git(&alpha, &["add", "."], None);
    git(&alpha, &["commit", "-qm", "init"], None);
    ok(&home, &["root", "add", root.to_str().unwrap()]);
    ok(&home, &["scan", "--offline"]);

    let wf = s.0.join("items.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("item")
            .emit_note("record", #{ text: "{$label}: {text}" })
            .output();
        document(#{}, [
            workflow("log", graph, #{
                params: #{ label: param("line", "item") },
                caps: #{ max_units: 20, max_edits: 0 },
            }),
        ])
        "#,
    )
    .unwrap();
    ok(&home, &["workflow", "propose", wf.to_str().unwrap()]);
    ok(&home, &["workflow", "activate", "1"]);

    // A project where an item is read is a type error, named as one.
    let (_, err, success) = pma_in(&home, &["workflow", "run", "log", "alpha"]);
    assert!(
        !success && err.contains("reads `item`") && err.contains("yields `project`"),
        "{err}"
    );

    // A quadrant holds two types at once, so it is not an argument.
    let (_, err, success) = pma_in(&home, &["workflow", "run", "log", "alpha:q1"]);
    assert!(!success && err.contains("not an argument"), "{err}");

    // An argument is checked against its declaration at the command line.
    let (_, err, success) = pma_in(
        &home,
        &["workflow", "run", "log", "alpha:high", "--set", "depth=2"],
    );
    assert!(
        !success && err.contains("declares no parameter `depth`"),
        "{err}"
    );

    // A heading yields one unit per open item under it, and the argument
    // replaces the declared default in the prompt the sink writes.
    let out = ok(
        &home,
        &[
            "workflow",
            "run",
            "log",
            "alpha:high",
            "--set",
            "label=seen",
        ],
    );
    assert!(out.contains("2 unit(s)"), "{out}");
    let notes = ok(&home, &["note"]);
    assert!(notes.contains("seen: first thing"), "{notes}");
    assert!(notes.contains("seen: second thing"), "{notes}");
    assert!(
        !notes.contains("later"),
        "the Low heading was not named: {notes}"
    );

    // A line names exactly one item, and the default applies when nothing is
    // set.
    let out = ok(&home, &["workflow", "run", "log", "alpha:10"]);
    assert!(out.contains("1 unit(s)"), "{out}");
    assert!(
        ok(&home, &["note"]).contains("item: later"),
        "the default label"
    );
}

/// A sink writes outside the database, so each unit's write and the moves
/// that record it commit together. A unit the sink cannot write is refused on
/// its own and named; the others are written once.
#[test]
fn a_sink_refuses_a_unit_alone_and_does_not_repeat_what_it_wrote() {
    let s = Scratch::new("sink");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let good = "# TODO\n\n## Critical\n\n## High\n\n- [ ] a thing\n\n## Medium\n\n## Low\n";
    for (name, text) in [("alpha", good), ("beta", "## High\n")] {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"], None);
        fs::write(dir.join("TODO.md"), text).unwrap();
        git(&dir, &["add", "."], None);
        git(&dir, &["commit", "-qm", "init"], None);
    }
    ok(&home, &["root", "add", root.to_str().unwrap()]);
    ok(&home, &["scan", "--offline"]);

    let wf = s.0.join("stamp.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .emit_todo("stamp", "add", #{ text: "reviewed {name}", priority: "high" })
            .output();
        document(#{}, [
            workflow("stamp", graph, #{ caps: #{ max_units: 20, max_edits: 0 }}),
        ])
        "#,
    )
    .unwrap();
    ok(&home, &["workflow", "propose", wf.to_str().unwrap()]);
    ok(&home, &["workflow", "activate", "1"]);

    // `beta`'s file has lint errors, so an item cannot be identified in it.
    // That refuses `beta`'s unit alone, and says so; `alpha` is written.
    let (out, err, success) = pma_in(&home, &["workflow", "run", "stamp", "alpha", "beta"]);
    assert!(success, "{err}");
    assert!(
        err.contains("refused: stamp: u2") && err.contains("lint errors"),
        "{err}"
    );
    assert!(out.contains("nothing left to run"), "{out}");

    let alpha = root.join("alpha").join("TODO.md");
    let written = fs::read_to_string(&alpha).unwrap();
    assert_eq!(
        written.matches("reviewed alpha").count(),
        1,
        "the first project's write committed: {written}"
    );
    assert!(
        written.contains("- [ ] reviewed alpha ^"),
        "pma wrote the line, so it gave the item an id: {written}"
    );
    assert!(
        !fs::read_to_string(root.join("beta").join("TODO.md"))
            .unwrap()
            .contains("reviewed beta"),
        "a refused unit writes nothing"
    );

    // The instance is finished, and running it again writes nothing twice.
    let out = ok(&home, &["workflow", "run", "stamp", "--instance", "1"]);
    assert!(out.contains("nothing left to run"), "{out}");
    assert_eq!(
        fs::read_to_string(&alpha)
            .unwrap()
            .matches("reviewed alpha")
            .count(),
        1,
        "still once"
    );

    // A second instance finds the item it would add already open, and adds
    // no duplicate the linter would refuse.
    fs::write(
        root.join("beta").join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] something\n",
    )
    .unwrap();
    ok(&home, &["workflow", "run", "stamp", "alpha", "beta"]);
    assert_eq!(
        fs::read_to_string(&alpha)
            .unwrap()
            .matches("reviewed alpha")
            .count(),
        1,
        "an item already open is not added again"
    );
    assert!(
        fs::read_to_string(root.join("beta").join("TODO.md"))
            .unwrap()
            .contains("reviewed beta")
    );
}

/// A call is resolved before anything runs, so a caller around an all-rule
/// callee is an all-rule pass: it costs nothing and needs no approval.
#[test]
fn a_call_around_a_rule_only_callee_runs_as_one_graph() {
    let s = Scratch::new("compose");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = root.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    git(&alpha, &["init", "-q"], None);
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] first thing\n- [ ] second thing\n",
    )
    .unwrap();
    git(&alpha, &["add", "."], None);
    git(&alpha, &["commit", "-qm", "init"], None);
    ok(&home, &["root", "add", root.to_str().unwrap()]);
    ok(&home, &["scan", "--offline"]);

    let wf = s.0.join("compose.rhai");
    fs::write(
        &wf,
        r#"
        let inner = source("project")
            .rule_expand("items", "item", "todo-items", 50)
            .output();
        let outer = source("project")
            .invoke("listing", "inner", #{}, "item")
            .emit_note("record", #{ text: "{$tag}: {text}" })
            .output();
        document(#{}, [
            workflow("inner", inner, #{ caps: #{ max_units: 50, max_edits: 0 }}),
            workflow("outer", outer, #{
                params: #{ tag: param("line", "open") },
                caps: #{ max_units: 50, max_edits: 0 },
                effects: ["writes"],
            }),
        ])
        "#,
    )
    .unwrap();
    let out = ok(&home, &["workflow", "propose", wf.to_str().unwrap()]);
    assert!(
        out.contains("at most $0.00"),
        "a graph of rules is free: {out}"
    );
    // The estimate is over the flat graph, so the callee's node is named by
    // its call site.
    assert!(out.contains("listing/items"), "{out}");
    ok(&home, &["workflow", "activate", "1"]);

    let out = ok(
        &home,
        &["workflow", "run", "outer", "alpha", "--set", "tag=todo"],
    );
    assert!(out.contains("nothing left to run"), "{out}");
    let notes = ok(&home, &["note"]);
    assert!(notes.contains("todo: first thing"), "{notes}");
    assert!(notes.contains("todo: second thing"), "{notes}");
}

/// `unknown` is a result, not a failure: a check that could not run sends its
/// unit down the default edge, where an implemented rule that passed would
/// have taken the guarded one.
#[test]
fn a_check_that_cannot_run_is_unknown_and_takes_the_default_edge() {
    let s = Scratch::new("verdict");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = root.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    git(&alpha, &["init", "-q"], None);
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] a thing\n",
    )
    .unwrap();
    git(&alpha, &["add", "."], None);
    git(&alpha, &["commit", "-qm", "init"], None);
    ok(&home, &["root", "add", root.to_str().unwrap()]);
    ok(&home, &["scan", "--offline"]);

    let wf = s.0.join("verdicts.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .check("lint", "lint-todo")
            .when(#{ "@lint-todo": ["passed"] })
            .check("merged", "pr-merged")
            .otherwise(|g| g.emit_note("waiting", #{ text: "waiting: {name}" }))
            .when(#{ "@pr-merged": ["passed"] })
            .emit_note("record", #{ text: "merged: {name}" })
            .output();
        document(#{}, [
            workflow("gate", graph, #{ caps: #{ max_units: 10, max_edits: 0 }}),
        ])
        "#,
    )
    .unwrap();
    ok(&home, &["workflow", "propose", wf.to_str().unwrap()]);
    ok(&home, &["workflow", "activate", "1"]);
    let out = ok(&home, &["workflow", "run", "gate", "alpha"]);
    assert!(out.contains("nothing left to run"), "{out}");

    // `lint-todo` ran and passed, so the unit reached the second check.
    // `pr-merged` has no run to read, which is neither pass nor fail.
    let notes = ok(&home, &["note"]);
    assert!(notes.contains("waiting: alpha"), "{notes}");
    assert!(!notes.contains("merged: alpha"), "{notes}");
}

/// A `reduce` applies the rule it names. `limit:` truncates each group and
/// every unit it dropped says so, where `dedupe` keeps one per group.
#[test]
fn a_reduce_applies_the_rule_it_names() {
    let s = Scratch::new("reduce");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = root.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    git(&alpha, &["init", "-q"], None);
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] first\n- [ ] second\n- [ ] third\n\n## Low\n\n- [ ] later\n",
    )
    .unwrap();
    git(&alpha, &["add", "."], None);
    git(&alpha, &["commit", "-qm", "init"], None);
    ok(&home, &["root", "add", root.to_str().unwrap()]);
    ok(&home, &["scan", "--offline"]);

    let write = |file: &str, rule: &str| {
        let path = s.0.join(file);
        fs::write(
            &path,
            format!(
                r#"
                let graph = source("project")
                    .rule_expand("items", "item", "todo-items", 50)
                    .join_by("pick", ["priority"], "{rule}")
                    .emit_note("record", #{{ text: "kept: {{text}}" }})
                    .output();
                document(#{{}}, [
                    workflow("pick", graph, #{{ caps: #{{ max_units: 50, max_edits: 0 }}}}),
                ])
                "#
            ),
        )
        .unwrap();
        path
    };

    // One per priority group, so one High and one Low.
    ok(
        &home,
        &[
            "workflow",
            "propose",
            write("a.rhai", "limit:1").to_str().unwrap(),
        ],
    );
    ok(&home, &["workflow", "activate", "1"]);
    ok(&home, &["workflow", "run", "pick", "alpha"]);
    let notes = ok(&home, &["note"]);
    assert_eq!(notes.matches("kept:").count(), 2, "{notes}");
    assert!(
        notes.contains("kept: later"),
        "the Low group kept one: {notes}"
    );

    // Two per group instead: three High items yield two, and the one Low
    // item yields one.
    ok(
        &home,
        &[
            "workflow",
            "propose",
            write("b.rhai", "limit:2").to_str().unwrap(),
        ],
    );
    ok(&home, &["workflow", "activate", "2"]);
    ok(&home, &["workflow", "run", "pick", "alpha"]);
    let notes = ok(&home, &["note"]);
    assert_eq!(
        notes.matches("kept:").count(),
        5,
        "two more High, one more Low: {notes}"
    );

    // A count that keeps nothing is refused where the document is read.
    let bad = write("c.rhai", "limit:0");
    let (_, err, success) = pma_in(&home, &["workflow", "check", bad.to_str().unwrap()]);
    assert!(!success && err.contains("a count of 1 or more"), "{err}");
}

/// Agent nodes run `max_parallel` at a time, and `batch_budget` bounds a whole
/// pass rather than one node's turn: the runs it did not admit keep their
/// place in the frontier, and the next pass takes them.
#[test]
fn agent_runs_go_in_parallel_and_the_batch_budget_bounds_the_pass() {
    let s = Scratch::new("parallel");
    let home = s.0.join("home");
    let root = s.0.join("root");
    for name in ["alpha", "beta"] {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"], None);
        fs::write(dir.join("TODO.md"), "# TODO\n\n## High\n\n- [ ] a thing\n").unwrap();
        git(&dir, &["add", "."], None);
        git(&dir, &["commit", "-qm", "init"], None);
    }

    // The worker brackets its own run in a shared log. Two runs in flight
    // write `start start end end`; one after the other writes `start end`
    // twice.
    let bin = s.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let log = s.0.join("order.log");
    fs::write(
        bin.join("claude"),
        format!(
            "#!/bin/sh\n\
             echo start >> {0}\n\
             sleep 1\n\
             out=$(printf '%s' \"$2\" | sed -n 's/.*findings to //p')\n\
             printf '[{{\"title\":\"a finding\"}}]' > \"$out\"\n\
             echo end >> {0}\n\
             printf '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"ok\",\"total_cost_usd\":0.1}}\\n'\n",
            log.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let run = |args: &[&str]| {
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = command(env!("CARGO_BIN_EXE_pma"))
            .env("PMA_HOME", &home)
            .env("PATH", path)
            .args(args)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.success(),
        )
    };
    run(&["root", "add", root.to_str().unwrap()]);
    run(&["scan", "--offline"]);

    let wf = s.0.join("review.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .expand("review", "finding", 3, "Review `{name}`. Write findings to {out}")
            .output();
        document(#{ finding: #{ fields: #{ title: req(line(200)) }}}, [
            workflow("review", graph, #{ caps: #{ max_units: 10, max_edits: 0 }}),
        ])
        "#,
    )
    .unwrap();
    run(&["workflow", "propose", wf.to_str().unwrap()]);
    run(&["workflow", "activate", "1"]);

    // Two projects, two threads, room for both.
    run(&["config", "max_parallel", "2"]);
    let id = plan_id(&run(&["workflow", "run", "review", "alpha", "beta", "--dry-run"]).0);
    let (out, err, success) = run(&[
        "workflow",
        "run",
        "review",
        "alpha",
        "beta",
        "--approve",
        &id,
    ]);
    assert!(success, "{err}\n{out}");
    let order: Vec<String> = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(order.len(), 4, "two runs, bracketed: {order:?}");
    assert_eq!(
        &order[..2],
        ["start", "start"],
        "the second run started before the first finished: {order:?}"
    );

    // One run's worth of budget, two units: the pass takes one and says what
    // is left, and the next pass takes the other.
    fs::remove_file(&log).unwrap();
    run(&["config", "batch_budget", "1"]);
    let dry = run(&["workflow", "run", "review", "alpha", "beta", "--dry-run"]).0;
    assert!(
        dry.contains("1 unit(s)"),
        "the plan holds what the budget admits: {dry}"
    );
    let (out, err, success) = run(&[
        "workflow",
        "run",
        "review",
        "alpha",
        "beta",
        "--approve",
        &plan_id(&dry),
    ]);
    assert!(success, "{err}\n{out}");
    assert!(out.contains("next: review"), "the rest is priced: {out}");
    assert_eq!(
        fs::read_to_string(&log).unwrap().matches("start").count(),
        1,
        "the batch budget admitted one run"
    );

    let (out, err, success) = run(&[
        "workflow",
        "run",
        "review",
        "--instance",
        "2",
        "--approve",
        &plan_id(&out),
    ]);
    assert!(success, "{err}\n{out}");
    assert!(out.contains("nothing left to run"), "{out}");
    assert_eq!(
        fs::read_to_string(&log).unwrap().matches("start").count(),
        2,
        "the next pass took the other"
    );
}

/// An agent that swaps its worktree's `.git` for a repository of its own,
/// whose config names a filesystem monitor that would run in pma's own git
/// calls, credentials and all.
const REPLACING_CLAUDE: &str = r#"#!/bin/sh
printf '#!/bin/sh\ntouch "%s"\n' "$PMA_HOME/../pwned" > "$PMA_HOME/../payload"
chmod +x "$PMA_HOME/../payload"
echo hi > hello.txt
rm .git
git init -q .
git config core.fsmonitor "$PMA_HOME/../payload"
echo '{"type":"result","subtype":"success","is_error":false,"result":"did it","total_cost_usd":0.1}'
"#;

#[test]
fn a_replaced_git_dir_is_refused_before_pma_runs_git_in_it() {
    let s = Scratch::new("gitdir");
    let (env, _, alpha) = dispatch_env(&s, &[("claude", REPLACING_CLAUDE)]);
    let out = env.ok(&["dispatch", "alpha:5"]);
    assert!(out.contains("0 ready, 1 failed"), "{out}");
    let (out, err, _) = env.run(&["review", "1"]);
    assert!(
        format!("{out}{err}").contains("is not the file `git worktree add` wrote"),
        "{out}{err}"
    );
    env.ok(&["review", "1", "--reject"]);
    assert!(!env.home.join("state/worktrees/alpha/add-greeting").exists());
    assert_eq!(git_out(&alpha, &["branch", "--list", "pma/*"]), "");
    assert!(!s.0.join("pwned").exists(), "the monitor ran");
}

/// The user's own hooks run at publishing. One added since dispatch does not:
/// nothing in the diff shows it, and `pma push` commits and pushes with the
/// user's credentials. Here it is added after the run is staged, as an agent
/// that outlived its run would, so the run was clean at review.
#[test]
fn a_hook_added_during_a_run_stops_its_push() {
    use std::os::unix::fs::PermissionsExt;

    let s = Scratch::new("hooks");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    let own = alpha.join(".git/hooks/post-commit");
    fs::write(
        &own,
        format!("#!/bin/sh\necho ran >> {}\n", s.0.join("own.log").display()),
    )
    .unwrap();
    fs::set_permissions(&own, fs::Permissions::from_mode(0o755)).unwrap();
    let hooked = s.0.join("hooked");
    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    assert!(env.ok(&["push", "1"]).contains("; pushed "));
    assert!(s.0.join("own.log").exists(), "the user's hook did not run");

    env.ok(&["dispatch", "alpha:7"]);
    env.ok(&["review", "2", "--approve"]);
    env.ok(&["stage", "2"]);
    let hook = alpha.join(".git/hooks/pre-push");
    fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", hooked.display())).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let (out, _, success) = env.run(&["push", "2"]);
    assert!(!success, "{out}");
    assert!(
        out.contains("hooks or git settings changed since run #2 was dispatched")
            && out.contains("  + hook pre-push "),
        "{out}"
    );
    assert!(!hooked.exists(), "the added hook ran");
    assert_eq!(
        git_out(&origin, &["log", "--format=%s", "main"]),
        "add greeting\ninit\n"
    );

    // Restoring the hooks is enough.
    fs::remove_file(&hook).unwrap();
    assert!(env.ok(&["push", "2"]).contains("; pushed "));
}

/// An agent that writes a clean filter and a filesystem monitor into the
/// clone's config and a hook into its hooks. A git call that reads the
/// worktree's files would run the filter, `git status` in the clone the
/// monitor, and a commit the hook, with pma's environment rather than the
/// agent's.
const FILTERING_CLAUDE: &str = r#"#!/bin/sh
echo hi > hello.txt
git config filter.x.clean "touch MARKER; cat"
echo '* filter=x' > .gitattributes
hook="$(git rev-parse --git-common-dir)/hooks/pre-commit"
printf '#!/bin/sh\ntouch MARKER\n' > "$hook"
chmod +x "$hook"
cp "$hook" MARKER.sh
git config core.fsmonitor MARKER.sh
echo '{"type":"result","subtype":"success","is_error":false,"result":"did it","total_cost_usd":0.1}'
"#;

/// A setting the agent added is caught before pma's first git call after it,
/// and every later call in the worktree refuses, so review and reject run
/// none of it.
#[test]
fn a_git_setting_added_by_the_agent_fails_the_run_before_pma_runs_git() {
    let s = Scratch::new("filter");
    let marker = s.0.join("filtered");
    let agent = FILTERING_CLAUDE.replace("MARKER", marker.to_str().unwrap());
    let (env, _, alpha) = dispatch_env(&s, &[("claude", &agent)]);
    // The check reads hello.txt; the filter is what this test is about.
    env.ok(&["config", "projects.alpha.verify", "none"]);

    let out = env.ok(&["dispatch", "alpha:5"]);
    assert!(out.contains("0 ready, 1 failed"), "{out}");
    let (out, err, _) = env.run(&["review", "1"]);
    let shown = format!("{out}{err}");
    assert!(
        shown.contains("hooks or git settings changed since run #1 was dispatched")
            && shown.contains("  + config filter.x.clean ")
            && shown.contains("  + hook pre-commit "),
        "{shown}"
    );
    let (_, err, ok) = env.run(&["review", "1", "--approve"]);
    assert!(!ok, "a failed run is not approved: {err}");

    // The settings stay in the clone, so nothing runs git there, and
    // rejecting the run does not lift that.
    let (_, err, ok) = env.run(&["dispatch", "alpha:7"]);
    assert!(!ok && err.contains("and they are still in"), "{err}");
    let (_, err, _) = env.run(&["verify", "alpha"]);
    assert!(err.contains("and they are still in"), "{err}");
    let (_, err, ok) = env.run(&["review", "1", "--reject"]);
    assert!(!ok && err.contains("Restore them first"), "{err}");
    // `git status` would run the monitor, so the clone is not read.
    let (_, err, ok) = env.run(&["scan", "--offline"]);
    assert!(
        ok && err.contains("warning: alpha: ") && err.contains("still in"),
        "{err}"
    );
    assert!(!marker.exists(), "scan ran the monitor");

    // Restored, both clear.
    git(&alpha, &["config", "--unset", "filter.x.clean"], None);
    git(&alpha, &["config", "--unset", "core.fsmonitor"], None);
    fs::remove_file(alpha.join(".git/hooks/pre-commit")).unwrap();
    let (_, err, _) = env.run(&["verify", "alpha"]);
    assert!(!err.contains("still in"), "{err}");
    env.ok(&["review", "1", "--reject"]);
    assert!(!marker.exists(), "the filter ran");
    assert_eq!(git_out(&alpha, &["branch", "--list", "pma/*"]), "");
}

/// A refusal that comes after the worktree exists, here a policy with no
/// route for the task's class, takes the worktree and its branch with it.
#[test]
fn a_refused_dispatch_leaves_no_worktree_behind() {
    let s = Scratch::new("refused");
    let (env, _, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    let doc = s.0.join("policy.json");
    fs::write(
        &doc,
        r#"{"route":[{"name":"chores","match":{"class":"A"},"approval":"each"}]}"#,
    )
    .unwrap();
    env.ok(&["route", "propose", doc.to_str().unwrap(), "--by", "me"]);
    env.ok(&["route", "activate", "1", "--by", "me"]);
    let (_, err, success) = env.run(&["dispatch", "alpha:5"]);
    assert!(!success && err.contains("matches no route"), "{err}");
    assert!(!env.home.join("state/worktrees/alpha/add-greeting").exists());
    assert_eq!(git_out(&alpha, &["branch", "--list", "pma/*"]), "");
}

/// An agent that records its pid, then waits.
const LINGERING_CLAUDE: &str = r#"#!/bin/sh
echo $$ > "$PMA_HOME/agent.pid"
exec sleep 60
"#;

/// A session killed mid-run, as by Ctrl-C or a closed terminal, takes its
/// agent with it. Otherwise the lock would be free while the agent still
/// wrote to a worktree the next session may reject.
#[test]
fn an_agent_does_not_outlive_a_killed_session() {
    let s = Scratch::new("killed");
    let (env, _, _) = dispatch_env(&s, &[("claude", LINGERING_CLAUDE)]);
    let pid_file = env.home.join("agent.pid");
    let mut dispatch = env
        .command(&["dispatch", "alpha:5"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    let pid = loop {
        if let Ok(pid) = fs::read_to_string(&pid_file)
            && !pid.trim().is_empty()
        {
            break pid.trim().to_string();
        }
        if started.elapsed().as_secs() >= 30 {
            let _ = dispatch.kill();
            panic!("the agent never started");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    dispatch.kill().unwrap();
    dispatch.wait().unwrap();
    let alive = || {
        Command::new("kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    let killed = std::time::Instant::now();
    while alive() && killed.elapsed().as_secs() < 5 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(!alive(), "agent {pid} outlived its session");
    // The next session fails the interrupted run, and rejecting it is safe.
    assert!(env.ok(&["review"]).contains("#1  failed"));
    env.ok(&["review", "1", "--reject"]);
}

/// `pma verify` runs the check where a dispatch would: at origin's head, in a
/// worktree of its own, under the agent's environment. A check that fails
/// only there shows before any agent is paid.
#[test]
fn verify_runs_the_check_where_a_dispatch_would() {
    let s = Scratch::new("preflight");
    let (env, _, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    // origin's test needs hello.txt, which the base lacks.
    let (out, err, success) = env.run(&["verify", "alpha"]);
    assert!(!success, "{out}");
    assert!(out.starts_with("alpha: `make test` FAILED at "), "{out}");
    assert!(
        err.contains("1 of 1 projects did not pass their check"),
        "{err}"
    );
    // A push is blocked there, as it is for an agent.
    env.ok(&[
        "config",
        "projects.alpha.verify",
        "git push -q origin HEAD:refs/heads/x",
    ]);
    let (out, _, success) = env.run(&["verify", "alpha"]);
    assert!(!success && out.contains("FAILED"), "{out}");
    env.ok(&["config", "projects.alpha.verify", "true"]);
    let out = env.ok(&["verify", "alpha"]);
    assert!(out.starts_with("alpha: `true` passed at "), "{out}");
    assert!(!env.home.join("state/verify-trees/alpha").exists());
    assert_eq!(git_out(&alpha, &["worktree", "list"]).lines().count(), 1);
    let (_, err, success) = env.run(&["verify"]);
    assert!(!success && err.contains("name a project"), "{err}");
}

/// With `sandbox on`, verify runs confined to its worktree. A check that
/// writes elsewhere fails and leaves nothing on disk; one that writes the
/// tree and the temp directory and reads git passes. The escape target is
/// under `CARGO_TARGET_TMPDIR` because the sandbox grants the temp
/// directory, which holds the scratch. Off, the default, the same check
/// writes it.
#[test]
fn verify_cannot_write_outside_its_worktree() {
    let s = Scratch::new("sandbox");
    let (env, _, _) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    let outside = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pma-sandbox-{}", std::process::id()));
    let _ = fs::remove_file(&outside);
    let escape = format!("touch '{}'", outside.display());

    env.ok(&["config", "projects.alpha.verify", &escape]);
    env.ok(&["verify", "alpha"]);
    assert!(outside.exists(), "verify runs unconfined unless asked");
    fs::remove_file(&outside).unwrap();

    env.ok(&["config", "projects.alpha.sandbox", "on"]);
    let (out, _, success) = env.run(&["verify", "alpha"]);
    assert!(!success && out.contains("FAILED"), "{out}");
    assert!(
        !outside.exists(),
        "a confined verify wrote {}",
        outside.display()
    );

    env.ok(&[
        "config",
        "projects.alpha.verify",
        "touch built && t=$(mktemp) && rm \"$t\" && git status --short && git diff --quiet HEAD",
    ]);
    let out = env.ok(&["verify", "alpha"]);
    assert!(out.contains("passed"), "{out}");

    env.ok(&["config", "projects.alpha.verify", &escape]);
    env.ok(&["config", "projects.alpha.sandbox", "off"]);
    env.ok(&["verify", "alpha"]);
    assert!(outside.exists(), "`sandbox off` runs verify unconfined");
    fs::remove_file(&outside).unwrap();
}

/// A stand-in for `claude -p` that plays a workflow's agent nodes. `Review`
/// writes the findings listed in `$PMA_HOME/../findings`, one title per
/// line; `Confirm` keeps each unit it reads with a `reason`, and fails when
/// `$PMA_HOME/../fail-confirm` exists. Every invocation is logged.
const NODE_CLAUDE: &str = r#"#!/usr/bin/env python3
import json, os, re, sys
home = os.path.dirname(os.environ["PMA_HOME"])
prompt = sys.argv[2]
with open(os.path.join(home, "worker.log"), "a") as f:
    f.write(" ".join(sys.argv[1:]) + "\n")
out = re.search(r"write (\S+)", prompt).group(1)
if prompt.startswith("Review"):
    titles = open(os.path.join(home, "findings")).read().split("\n")
    json.dump([{"title": t} for t in titles if t], open(out, "w"))
elif prompt.startswith("Confirm"):
    if os.path.exists(os.path.join(home, "fail-confirm")):
        sys.exit(1)
    units = json.load(open(re.search(r"read (\S+)", prompt).group(1)))
    json.dump([{"@id": u["@id"], "reason": "proved"} for u in units], open(out, "w"))
print(json.dumps({"type": "result", "subtype": "success", "is_error": False,
                  "result": "ok", "total_cost_usd": 0.1}))
"#;

/// A stand-in for `claude -p` that plays the library's agent nodes: a review
/// writes one high finding, a brief one unit, a confirm keeps each unit it
/// reads with a `reason`, and a fix writes the file the verify wants. Every
/// invocation is logged.
const LIBRARY_CLAUDE: &str = r#"#!/usr/bin/env python3
import json, os, re, sys
home = os.path.dirname(os.environ["PMA_HOME"])
prompt = sys.argv[2]
with open(os.path.join(home, "worker.log"), "a") as f:
    f.write(" ".join(sys.argv[1:]) + "\n")
def out():
    return re.search(r"(\S*out\.json)", prompt).group(1)
if "Fix this in" in prompt:
    open("hello.txt", "w").write("hi\n")
elif "`summary`" in prompt:
    json.dump([{"summary": "says hello", "stack": "sh"}], open(out(), "w"))
elif "in.json" in prompt:
    units = json.load(open(re.search(r"(\S*in\.json)", prompt).group(1)))
    json.dump([{"@id": u["@id"], "reason": "proved"} for u in units], open(out(), "w"))
else:
    doc = re.search(r"(\S*REVIEW\.md)", prompt)
    if doc:
        open(doc.group(1), "w").write("prose\n")
    json.dump([{"severity": "high", "title": "a real bug"}], open(out(), "w"))
print(json.dumps({"type": "result", "subtype": "success", "is_error": False,
                  "result": "ok", "total_cost_usd": 0.1}))
"#;

/// `pma run`: the names, the bundled library put in effect within
/// `workflow_budget`, a path as a target, the review's default model and
/// `-w` over it, a bulk run that stops at its plan, and the dispatch names.
#[test]
fn pma_run_reviews_a_path_with_the_bundled_library() {
    let s = Scratch::new("pmarun");
    let (env, _, alpha) = dispatch_env(&s, &[("claude", LIBRARY_CLAUDE)]);
    let log = s.0.join("worker.log");
    let list = env.ok(&["run"]);
    assert!(list.contains("review") && list.contains("fix-ci"), "{list}");
    assert!(
        list.contains("settle") && list.contains("portfolio-sweep"),
        "every workflow of the library is listed: {list}"
    );
    assert!(
        list.contains("not runnable from here") && list.contains("record (reads finding)"),
        "{list}"
    );

    let (_, err, success) = env.run(&["run", "review", "alpha"]);
    assert!(
        !success && err.contains("`pma config workflow_budget 63`"),
        "the library is held to the budget: {err}"
    );
    env.ok(&["config", "workflow_budget", "63"]);

    let out = env.ok(&["run", "review", "--all"]);
    assert!(out.contains("revision 1: the bundled workflows"), "{out}");
    assert!(out.contains("rerun with `--yes`"), "{out}");
    assert!(!log.exists(), "a bulk run without --yes ran an agent");

    let out = env.ok(&["run", "review", alpha.to_str().unwrap()]);
    assert!(out.contains("REVIEW.md"), "the prose is pointed to: {out}");
    assert!(out.contains("model opus"), "{out}");
    assert!(out.contains("high  alpha  a real bug"), "{out}");
    assert!(out.contains("proved"), "{out}");
    assert!(fs::read_to_string(&log).unwrap().contains("opus"));

    fs::remove_file(&log).unwrap();
    let out = env.ok(&["run", "review", "alpha", "-w", "claude:haiku"]);
    assert!(
        !out.contains("revision 2"),
        "the library in effect is kept: {out}"
    );
    assert!(!out.contains("model opus"), "{out}");
    let worker = fs::read_to_string(&log).unwrap();
    assert!(
        worker.contains("haiku") && !worker.contains("opus"),
        "{worker}"
    );

    // The dispatch names: a target of another signal is refused, and
    // nothing to fix is a quiet result.
    let (_, err, success) = env.run(&["run", "fix-ci", "alpha:deps"]);
    assert!(
        !success && err.contains("`project:ci` targets only"),
        "{err}"
    );
    let out = env.ok(&["run", "fix-ci", "--all"]);
    assert!(out.contains("no project has failing CI"), "{out}");
    let out = env.ok(&["run", "fix-ci", "alpha", "--dry-run"]);
    assert!(
        out.contains("would dispatch 1, each edit that passes verify written into its clone"),
        "{out}"
    );
    let out = env.ok(&["run", "fix-ci", "alpha", "--dry-run", "--review"]);
    assert!(out.contains("each edit left in `pma review`"), "{out}");
    let out = env.ok(&["run", "deps-upgrade", "alpha", "--dry-run"]);
    assert!(out.contains("written into its clone"), "{out}");
    let (_, err, success) = env.run(&["run", "review", "/nowhere/at/all"]);
    assert!(!success && err.contains("/nowhere/at/all"), "{err}");

    // The added workflows run end to end on the same stand-in.
    for name in ["security-review", "test-gaps"] {
        let out = env.ok(&["run", name, "alpha"]);
        assert!(out.contains("high  alpha  a real bug"), "{name}: {out}");
    }
    let out = env.ok(&["run", "project-brief", "alpha"]);
    assert!(out.contains("1 brief(s)"), "{out}");
    let notes = env.ok(&["note"]);
    assert!(notes.contains("alpha (sh): says hello"), "{notes}");
    let out = env.ok(&["run", "review-and-fix", "alpha"]);
    assert!(out.contains("high  alpha  a real bug"), "{out}");
    let review = env.ok(&["review"]);
    assert!(
        review.contains("ready") && review.contains("verify ok"),
        "the fix passed verify and waits in review: {review}"
    );
}

/// A review, then a confirm that keeps what it can prove, then a note.
const REVIEW_CONFIRM: &str = r#"
let graph = source("project")
    .expand("review", "finding", 3, "Review {name}. write {out}")
    .filter("confirm", ["reason"], "Confirm: read {in} write {out}")
    .emit_note("record", #{ text: "confirmed: {title}: {reason}" })
    .output();
document(#{ finding: #{ fields: #{ title: req(line(200)), reason: line(200) }}}, [
    workflow("look", graph, #{ caps: #{ max_units: 10, max_edits: 0 }}),
])
"#;

/// Runs a pass and approves whatever it prints, once. Returns the output of
/// the approving pass.
fn approve_next(env: &Env, name: &str, instance: &str) -> String {
    let out = env.ok(&["workflow", "run", name, "--instance", instance]);
    env.ok(&[
        "workflow",
        "run",
        name,
        "--instance",
        instance,
        "--approve",
        &plan_id(&out),
    ])
}

/// Each node runs on the worker and model its route names. A validator that
/// fails passes nothing on, and one that succeeds cannot drop a field it was
/// not asked to write: the title the model left out is still minted.
#[test]
fn nodes_are_routed_apart_and_a_failed_validator_passes_nothing_on() {
    let s = Scratch::new("routed");
    let (env, _, _) = dispatch_env(&s, &[("claude", NODE_CLAUDE)]);
    fs::write(s.0.join("findings"), "a finding\n").unwrap();
    let wf = s.0.join("look.rhai");
    fs::write(&wf, REVIEW_CONFIRM).unwrap();
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);

    let policy = s.0.join("policy.json");
    fs::write(
        &policy,
        r#"{"route": [
            {"name": "reviewers", "match": {"node": "*/review"}, "model": "opus", "approval": "each"},
            {"name": "confirmers", "match": {"node": "confirm"}, "model": "haiku", "approval": "each"},
            {"name": "tasks", "approval": "each"}
        ]}"#,
    )
    .unwrap();
    env.ok(&["route", "propose", policy.to_str().unwrap()]);
    env.ok(&["route", "activate", "1"]);

    let out = env.ok(&["workflow", "run", "look", "alpha"]);
    assert!(
        out.contains("next: review") && out.contains("claude/opus"),
        "{out}"
    );
    env.ok(&[
        "workflow",
        "run",
        "look",
        "--instance",
        "1",
        "--approve",
        &plan_id(&out),
    ]);
    let out = env.ok(&["workflow", "run", "look", "--instance", "1"]);
    assert!(
        out.contains("next: confirm") && out.contains("claude/haiku"),
        "{out}"
    );
    let out = approve_next(&env, "look", "1");
    assert!(out.contains("nothing left to run"), "{out}");
    let log = fs::read_to_string(s.0.join("worker.log")).unwrap();
    assert!(log.contains("opus") && log.contains("haiku"), "{log}");
    assert!(
        env.ok(&["note"]).contains("confirmed: a finding: proved"),
        "the title the confirm left out is the one it was given"
    );

    // The confirm fails: the finding it was given stops there.
    fs::write(s.0.join("fail-confirm"), "").unwrap();
    let out = env.ok(&["workflow", "run", "look", "alpha"]);
    env.ok(&[
        "workflow",
        "run",
        "look",
        "--instance",
        "2",
        "--approve",
        &plan_id(&out),
    ]);
    let out = env.ok(&["workflow", "run", "look", "--instance", "2"]);
    let (out, err, success) = env.run(&[
        "workflow",
        "run",
        "look",
        "--instance",
        "2",
        "--approve",
        &plan_id(&out),
    ]);
    assert!(success, "{err}");
    assert!(err.contains("refused: confirm"), "{err}");
    assert!(out.contains("nothing left to run"), "{out}");
    assert_eq!(
        env.ok(&["note"]).matches("confirmed:").count(),
        1,
        "an unconfirmed finding does not reach the sink"
    );
}

/// A node runs once everything before it has finished, so a `reduce` joining
/// two branches sees both: two readings of the same three items dedupe to
/// three, not six.
#[test]
fn a_reduce_waits_for_every_branch() {
    let s = Scratch::new("barrier");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = root.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    git(&alpha, &["init", "-q"], None);
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] alpha\n- [ ] beta\n- [ ] gamma\n",
    )
    .unwrap();
    ok(&home, &["root", "add", root.to_str().unwrap()]);
    ok(&home, &["scan", "--offline"]);
    let wf = s.0.join("join.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .fan([
                |g| g.rule_expand("first", "item", "todo-items", 10),
                |g| g.rule_expand("second", "item", "todo-items", 10),
            ])
            .join("a-join", ["text"])
            .emit_note("record", #{ text: "kept: {text}" })
            .output();
        document(#{}, [
            workflow("join", graph, #{ caps: #{ max_units: 50, max_edits: 0 }}),
        ])
        "#,
    )
    .unwrap();
    ok(&home, &["workflow", "propose", wf.to_str().unwrap()]);
    ok(&home, &["workflow", "activate", "1"]);
    let out = ok(&home, &["workflow", "run", "join", "alpha"]);
    assert!(out.contains("nothing left to run"), "{out}");
    assert_eq!(ok(&home, &["note"]).matches("kept:").count(), 3);
}

/// A check about a pull request waits while the run is on its way: the unit
/// stays at the check, the instance stays open, and the pass says so. Once
/// the run is rejected, the check fails and the instance finishes.
#[test]
fn a_check_waits_until_the_run_is_decided() {
    let s = Scratch::new("waiting");
    let worker = r#"#!/bin/sh
echo hi > hello.txt
echo '{"type":"result","subtype":"success","is_error":false,"result":"did it","total_cost_usd":0.1}'
"#;
    let (env, _, _) = dispatch_env(&s, &[("claude", worker)]);
    let wf = s.0.join("merge.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .edit("greet", "verify", "Say hello in hello.txt for `{name}`.")
            .check("merged", "pr-merged")
            .when(#{ "@pr-merged": ["passed"] })
            .emit_note("done", #{ text: "merged: {name}" })
            .output();
        document(#{}, [
            workflow("greet", graph, #{ caps: #{ max_units: 10, max_edits: 1 }}),
        ])
        "#,
    )
    .unwrap();
    env.ok(&["config", "workflow_budget", "20"]);
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);
    let out = env.ok(&["workflow", "run", "greet", "alpha"]);
    let out = env.ok(&[
        "workflow",
        "run",
        "greet",
        "--instance",
        "1",
        "--approve",
        &plan_id(&out),
    ]);
    assert!(out.contains("waiting at merged: 1 unit(s)"), "{out}");
    let out = env.ok(&["workflow", "run", "greet", "--instance", "1"]);
    assert!(out.contains("waiting at merged"), "still waiting: {out}");
    assert!(!env.ok(&["workflow"]).contains("finished"));

    env.ok(&["review", "1", "--reject"]);
    let out = env.ok(&["workflow", "run", "greet", "--instance", "1"]);
    assert!(out.contains("nothing left to run"), "{out}");
    assert!(
        env.ok(&["note"]).contains("no notes"),
        "a rejected run never merged"
    );
}

/// A cap hit while a batch is recorded rolls the batch back whole, records
/// the cap on the instance, and a raised cap resumes it.
#[test]
fn a_cap_rolls_the_batch_back_and_a_raised_cap_resumes() {
    let s = Scratch::new("capped");
    let (env, _, _) = dispatch_env(&s, &[("claude", NODE_CLAUDE)]);
    fs::write(s.0.join("findings"), "one\ntwo\n").unwrap();
    let wf = s.0.join("review.rhai");
    fs::write(
        &wf,
        r#"
        let graph = source("project")
            .expand("review", "finding", 3, "Review {name}. write {out}")
            .emit_note("record", #{ text: "found: {title}" })
            .output();
        document(#{ finding: #{ fields: #{ title: req(line(200)) }}}, [
            workflow("look", graph, #{ caps: #{ max_units: 2, max_edits: 0 }}),
        ])
        "#,
    )
    .unwrap();
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);
    let out = env.ok(&["workflow", "run", "look", "alpha"]);
    let (_, err, success) = env.run(&[
        "workflow",
        "run",
        "look",
        "--instance",
        "1",
        "--approve",
        &plan_id(&out),
    ]);
    assert!(!success && err.contains("max_units"), "{err}");
    assert!(env.ok(&["workflow"]).contains("capped"));
    assert!(
        env.ok(&["note"]).contains("no notes"),
        "no finding was half minted"
    );

    let (_, err, success) = env.run(&["workflow", "run", "look", "--instance", "1"]);
    assert!(!success && err.contains("--cap max_units"), "{err}");
    let (_, err, success) = env.run(&[
        "workflow",
        "run",
        "look",
        "--instance",
        "1",
        "--cap",
        "max_units=1",
    ]);
    assert!(!success && err.contains("must be higher"), "{err}");

    let out = env.ok(&[
        "workflow",
        "run",
        "look",
        "--instance",
        "1",
        "--cap",
        "max_units=5",
    ]);
    assert!(
        out.contains("next: review"),
        "the batch is waiting again: {out}"
    );
    let out = env.ok(&[
        "workflow",
        "run",
        "look",
        "--instance",
        "1",
        "--approve",
        &plan_id(&out),
    ]);
    assert!(out.contains("nothing left to run"), "{out}");
    let notes = env.ok(&["note"]);
    assert!(
        notes.contains("found: one") && notes.contains("found: two"),
        "{notes}"
    );
}

/// `workflow stop` ends an instance: nothing further runs under it.
#[test]
fn a_stopped_instance_runs_nothing_more() {
    let s = Scratch::new("stop");
    let (env, _, _) = dispatch_env(&s, &[("claude", NODE_CLAUDE)]);
    let wf = s.0.join("look.rhai");
    fs::write(&wf, REVIEW_CONFIRM).unwrap();
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);
    env.ok(&["workflow", "run", "look", "alpha"]);
    env.ok(&["workflow", "stop", "1"]);
    let (_, err, success) = env.run(&["workflow", "run", "look", "--instance", "1"]);
    assert!(!success && err.contains("was stopped"), "{err}");
    assert!(!s.0.join("worker.log").exists());
}

/// A revision is what runs, so one stating a construct the runtime does not
/// take is refused at propose. `check` still prices it and says why.
#[test]
fn a_document_the_runtime_cannot_take_is_refused_at_propose() {
    let s = Scratch::new("unbuilt");
    let home = s.0.join("home");
    fs::create_dir_all(&home).unwrap();
    let wf = s.0.join("split.json");
    fs::write(
        &wf,
        r#"{"types": {"task": {"fields": {"text": {"type": "line", "required": true}}}},
        "workflow": [{"name": "decompose", "in": "task", "out": "task", "effects": [],
          "params": {"depth": {"type": "int", "default": 2, "max": 3}},
          "caps": {"max_units": 20, "max_edits": 0},
          "nodes": [{"name": "split", "op": "map", "out": "0..n", "in": "task", "emits": "task",
                     "max_units": 2, "max_depth": "{$depth}", "task": "split {text} into {out}"}],
          "edges": [{"from": "@input", "to": "split"},
                    {"from": "split", "to": "split", "when": {"@depth": {"<": "{$depth}"}}},
                    {"from": "split", "to": "@output"}]}]}"#,
    )
    .unwrap();
    // One unit, split three levels deep at two apiece: 1 + 2 + 4 + 8 runs.
    let out = ok(&home, &["workflow", "check", wf.to_str().unwrap()]);
    assert!(out.contains("worst case: 15 agent runs"), "{out}");
    assert!(
        out.contains("recursion (a self-edge) is not taken yet"),
        "{out}"
    );
    let (_, err, success) = pma_in(&home, &["workflow", "propose", wf.to_str().unwrap()]);
    assert!(!success && err.contains("not runnable yet"), "{err}");
}

/// Under a policy, an agent node no route names is refused when it runs. A
/// dry run says so first, naming the node.
#[test]
fn a_dry_run_names_a_node_the_policy_would_refuse() {
    let s = Scratch::new("unrouted");
    let (env, _, _) = dispatch_env(&s, &[("claude", NODE_CLAUDE)]);
    let wf = s.0.join("look.rhai");
    fs::write(&wf, REVIEW_CONFIRM).unwrap();
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);
    let policy = s.0.join("policy.json");
    fs::write(
        &policy,
        r#"{"route": [
            {"name": "reviewers", "match": {"node": "*/review"}, "approval": "each"},
            {"name": "tasks", "approval": "each"}
        ]}"#,
    )
    .unwrap();
    env.ok(&["route", "propose", policy.to_str().unwrap()]);
    env.ok(&["route", "activate", "1"]);
    let (_, err, success) = env.run(&["workflow", "run", "look", "alpha", "--dry-run"]);
    assert!(success, "{err}");
    assert!(err.contains("node `confirm`: no route"), "{err}");
    assert!(
        !err.contains("node `review`"),
        "a named node is not warned about: {err}"
    );
}

/// An id keeps an item's identity when its text is reworded: its age, and
/// the run it already has.
#[test]
fn an_id_keeps_an_items_age_and_its_run_across_a_reword() {
    let s = Scratch::new("ids");
    let (env, _, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    let alpha_dir = alpha.to_str().unwrap();

    // Listed, then written, then there is nothing left to write.
    let out = env.ok(&["lint", "--ids", alpha_dir]);
    assert!(
        out.contains(":5: ^") && out.contains("4 items without an id"),
        "{out}"
    );
    let before = fs::read_to_string(alpha.join("TODO.md")).unwrap();
    assert!(!before.contains(" ^"), "a listing writes nothing");

    // A run on line 5 before its id exists moves to the id when it is written.
    env.ok(&["dispatch", "alpha:5"]);
    let out = env.ok(&["lint", "--ids", "--apply", alpha_dir]);
    assert!(out.contains("4 ids written"), "{out}");
    assert!(
        env.ok(&["lint", "--ids", alpha_dir])
            .contains("every open item has an id")
    );
    let written = fs::read_to_string(alpha.join("TODO.md")).unwrap();
    assert_eq!(written.matches(" ^").count(), 4, "{written}");
    assert!(
        env.ok(&["lint", alpha_dir]).is_empty(),
        "the file stays clean"
    );

    // Reworded in the clone, the item keeps its id, so its run still holds it.
    let reworded = written.replace(
        "- [ ] add greeting #agent",
        "- [ ] add a friendly greeting #agent",
    );
    assert_ne!(reworded, written, "the item was reworded");
    fs::write(alpha.join("TODO.md"), reworded).unwrap();
    env.ok(&["scan", "--offline"]);
    let (_, err, success) = env.run(&["dispatch", "alpha:5"]);
    assert!(
        !success && err.contains("already has a run"),
        "the reworded item is the task the run holds: {err}"
    );
}

/// An item keeps the age git gave it when its text is reworded, because the
/// id carries it from one scan to the next.
#[test]
fn an_id_keeps_the_age_git_gave_an_item() {
    let s = Scratch::new("idage");
    let home = s.0.join("home");
    let root = s.0.join("root");
    let alpha = root.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    git(&alpha, &["init", "-q"], None);
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] old thing\n",
    )
    .unwrap();
    git(&alpha, &["add", "."], None);
    git(&alpha, &["commit", "-qm", "init"], Some(now() - 100 * DAY));
    ok(&home, &["root", "add", root.to_str().unwrap()]);
    ok(&home, &["project", "tier", "1", "alpha"]);
    ok(
        &home,
        &["lint", "--ids", "--apply", alpha.to_str().unwrap()],
    );
    ok(&home, &["scan", "--offline"]);
    assert!(ok(&home, &["stale"]).contains("open 100d  old thing"));

    let text = fs::read_to_string(alpha.join("TODO.md")).unwrap();
    fs::write(
        alpha.join("TODO.md"),
        text.replace("old thing", "renamed thing"),
    )
    .unwrap();
    ok(&home, &["scan", "--offline"]);
    let stale = ok(&home, &["stale"]);
    assert!(stale.contains("open 100d  renamed thing"), "{stale}");
}

/// `pma next`, and `pma` alone: tasks for agents in the order `--auto` takes
/// them, then what waits on you. A task with a run leaves the agents' list and
/// comes back as the run to review.
#[test]
fn next_lists_work_for_agents_and_for_you() {
    let s = Scratch::new("next");
    let (env, _, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    // Critical items no agent may take, written in the clone the scan reads.
    // `#manual` wins over `#agent`.
    let text = fs::read_to_string(alpha.join("TODO.md")).unwrap();
    fs::write(
        alpha.join("TODO.md"),
        text.replace(
            "# TODO\n",
            "# TODO\n\n## Critical\n\n- [ ] rotate the signing key\n\
             - [ ] revoke the old tokens #agent #manual\n",
        ),
    )
    .unwrap();
    env.ok(&["scan", "--offline"]);

    let out = env.ok(&["next"]);
    assert!(out.contains("For agents: 3\n"), "{out}");
    assert!(
        out.contains("add greeting") && out.contains("third task"),
        "{out}"
    );
    assert!(
        !out.contains("guarded task"),
        "#manual is not for agents: {out}"
    );
    assert!(
        out.contains("For you: 2\n  tasks: 2\n")
            && out.contains("not for agents  rotate the signing key")
            && out.contains("not for agents  revoke the old tokens"),
        "{out}"
    );
    assert_eq!(env.ok(&[]), out, "`pma` alone is `pma next`");

    env.ok(&["dispatch", "alpha:10"]);
    let out = env.ok(&["next"]);
    assert!(
        out.contains("For agents: 2\n"),
        "the dispatched task is held: {out}"
    );
    assert!(
        out.contains("runs: 1\n    #1  ready  to review  alpha  add greeting  pma review 1"),
        "{out}"
    );
}

/// The library in `docs/dev/workflows.md` section 15 is what a reader copies,
/// so each example must read and price, and `propose` must take exactly the
/// ones the text lists as runnable. Types come from section 6's document.
#[test]
fn the_documented_workflow_library_reads_as_documented() {
    let doc = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/dev/workflows.md"),
    )
    .unwrap();
    let blocks = |text: &str| -> Vec<serde_json::Value> {
        text.split("```json\n")
            .skip(1)
            .map(|b| serde_json::from_str(b.split("\n```").next().unwrap()).unwrap())
            .collect()
    };
    let section = |from: &str, to: &str| {
        let start = doc.find(from).unwrap();
        doc[start..start + doc[start..].find(to).unwrap()].to_string()
    };
    let types = blocks(&section("## 6. The document", "### 6.1"))[0]["types"].clone();
    let library = section("## 15. A library", "## 16. Acceptance");
    let workflows: Vec<serde_json::Value> = blocks(&library);
    assert_eq!(workflows.len(), 19, "the library's nineteen examples");
    let named = |name: &str| {
        workflows
            .iter()
            .find(|w| w["name"] == name)
            .unwrap()
            .clone()
    };

    let s = Scratch::new("library");
    let home = s.0.join("home");
    fs::create_dir_all(&home).unwrap();
    let runnable = [
        "find-issues",
        "ensemble-review",
        "triage-issues",
        "specify",
        "record",
        "settle",
        "portfolio-sweep",
        "todo-audit",
        "doc-drift",
        "security-review",
        "test-gaps",
        "project-brief",
        "fix-findings",
        "review-and-fix",
    ];
    for w in &workflows {
        let name = w["name"].as_str().unwrap();
        // A caller's document carries its callees, as a revision would.
        let mut members = vec![w.clone()];
        let mut i = 0;
        while i < members.len() {
            for node in members[i]["nodes"].as_array().unwrap().clone() {
                if node["op"] == "call" {
                    let callee = named(node["workflow"].as_str().unwrap());
                    if !members.iter().any(|m| m["name"] == callee["name"]) {
                        members.push(callee);
                    }
                }
            }
            i += 1;
        }
        let file = s.0.join(format!("{name}.json"));
        fs::write(
            &file,
            serde_json::json!({"types": types, "workflow": members}).to_string(),
        )
        .unwrap();
        let (_, err, success) = pma_in(&home, &["workflow", "check", file.to_str().unwrap()]);
        assert!(success, "{name} does not read: {err}");
        let (_, err, success) = pma_in(&home, &["workflow", "propose", file.to_str().unwrap()]);
        assert_eq!(
            success,
            runnable.contains(&name),
            "{name}: the text says it is {}runnable: {err}",
            if runnable.contains(&name) { "" } else { "not " }
        );
    }
    // `pma workflow library` is the runnable examples, in the text's order,
    // with the text's types, and it proposes as printed.
    let shipped: serde_json::Value =
        serde_json::from_str(&ok(&home, &["workflow", "library"])).unwrap();
    let expected: Vec<serde_json::Value> = workflows
        .iter()
        .filter(|w| runnable.contains(&w["name"].as_str().unwrap()))
        .cloned()
        .collect();
    assert_eq!(
        shipped,
        serde_json::json!({"types": types, "workflow": expected}),
        "crates/pma-core/workflows/library.json differs from docs/dev/workflows.md"
    );
    let file = s.0.join("library.json");
    fs::write(&file, shipped.to_string()).unwrap();
    let out = ok(&home, &["workflow", "propose", file.to_str().unwrap()]);
    assert!(out.contains("stored as a draft"), "{out}");
}

/// A review that writes no `out.json` failed; it did not find nothing. Its
/// files are kept under `artifacts/` after its tree is gone.
/// A read node's agent that adds a setting to the clone fails its run, as a
/// dispatch does, rather than leaving it as the next run's baseline.
#[test]
fn a_git_setting_added_by_a_read_node_fails_its_run() {
    let s = Scratch::new("read-filter");
    let marker = s.0.join("filtered");
    let agent = format!(
        "#!/bin/sh\ngit config filter.x.clean \"touch {}; cat\"\n\
         echo '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"total_cost_usd\":0.1}}'\n",
        marker.display()
    );
    let (env, _, _) = dispatch_env(&s, &[("claude", &agent)]);
    let wf = s.0.join("look.rhai");
    fs::write(&wf, REVIEW_CONFIRM).unwrap();
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);
    let out = env.ok(&["workflow", "run", "look", "alpha"]);
    let (_, err, _) = env.run(&[
        "workflow",
        "run",
        "look",
        "--instance",
        "1",
        "--approve",
        &plan_id(&out),
    ]);
    assert!(
        err.contains("refused: review")
            && err.contains("hooks or git settings changed")
            && err.contains("+ config filter.x.clean"),
        "{err}"
    );
    assert!(!marker.exists(), "the filter ran");

    // The user may say the change is theirs, which lifts the hold.
    let (_, err, _) = env.run(&["verify", "alpha"]);
    assert!(err.contains("and they are still in"), "{err}");
    env.ok(&["review", "1", "--reject", "--keep-git-changes"]);
    let (_, err, _) = env.run(&["verify", "alpha"]);
    assert!(!err.contains("still in"), "{err}");
}

#[test]
fn a_node_that_writes_no_output_failed() {
    let s = Scratch::new("silent");
    let silent = r#"#!/bin/sh
echo '{"type":"result","subtype":"success","is_error":false,"result":"done","total_cost_usd":0.1}'
"#;
    let (env, _, _) = dispatch_env(&s, &[("claude", silent)]);
    let wf = s.0.join("look.rhai");
    fs::write(&wf, REVIEW_CONFIRM).unwrap();
    env.ok(&["workflow", "propose", wf.to_str().unwrap()]);
    env.ok(&["workflow", "activate", "1"]);
    let out = env.ok(&["workflow", "run", "look", "alpha"]);
    let (out, err, success) = env.run(&[
        "workflow",
        "run",
        "look",
        "--instance",
        "1",
        "--approve",
        &plan_id(&out),
    ]);
    assert!(success, "{err}");
    assert!(
        err.contains("refused: review") && err.contains("wrote no"),
        "{err}"
    );
    assert!(out.contains("nothing left to run"), "{out}");
    let kept = env.home.join("state/artifacts/1/review/1/in.json");
    assert!(kept.exists(), "the run's input is kept: {}", kept.display());
    assert!(
        !env.home.join("state/workflow-trees/1").exists(),
        "the scratch trees are gone"
    );
}

/// Moves `uv.lock` the way `uv lock --upgrade` would, and measures it dry.
const FAKE_UV: &str = "#!/bin/sh
case \"$*\" in
  'lock --upgrade --dry-run') echo 'Update ruff v1 -> v2' ;;
  'lock --upgrade') echo 'ruff 2' > uv.lock ;;
esac
";

/// The `refresh` worker moves the lock in a worktree and the project's check
/// judges it. A refresh the check refused hands its output to the agent that
/// takes the task next.
#[test]
fn a_refresh_the_check_refuses_reaches_the_next_agent() {
    use std::os::unix::fs::PermissionsExt;

    let s = Scratch::new("refresh");
    let prompt_log = s.0.join("prompt.txt");
    let catcher = format!(
        "#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n",
        prompt_log.display()
    );
    let (env, _, alpha) = dispatch_env(&s, &[("uv", FAKE_UV), ("catcher", &catcher)]);
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_pma"), env.bin.join("pma")).unwrap();
    fs::set_permissions(env.bin.join("catcher"), fs::Permissions::from_mode(0o755)).unwrap();

    fs::write(alpha.join("uv.lock"), "ruff 1\n").unwrap();
    git(&alpha, &["add", "uv.lock"], None);
    git(&alpha, &["commit", "-qm", "lock"], None);
    git(&alpha, &["push", "-q", "origin", "main"], None);
    env.ok(&["scan", "--deps"]);
    env.ok(&[
        "config",
        "projects.alpha.verify",
        "grep -q 'ruff 1' uv.lock || { echo ruff-2-broke-the-tests; false; }",
    ]);

    let agents = env.ok(&["agent"]);
    assert!(agents.contains("refresh"), "seeded: {agents}");
    // Below one agent's budget: a refresh runs no model, so it still starts.
    env.ok(&["config", "batch_budget", "0.5"]);
    let out = env.ok(&["dispatch", "alpha:deps", "-a", "refresh"]);
    assert!(!out.contains("not started"), "{out}");
    env.ok(&["config", "batch_budget", "--reset"]);
    let review = env.ok(&["review", "1"]);
    assert!(review.contains("+ruff 2"), "the lock moved: {review}");
    assert!(review.contains("base passed, head FAILED"), "{review}");
    env.ok(&["review", "1", "--reject"]);

    env.ok(&["agent", "set", "catcher", "command", "catcher"]);
    env.ok(&["agent", "set", "catcher", "args", r#"["{prompt}"]"#]);
    env.ok(&["dispatch", "alpha:deps", "-a", "catcher"]);
    let prompt = fs::read_to_string(&prompt_log).unwrap();
    assert!(prompt.contains("uv: ruff v1 -> v2"), "{prompt}");
    assert!(
        prompt.contains("`pma deps refresh` took every update")
            && prompt.contains("ruff-2-broke-the-tests"),
        "{prompt}"
    );
}
