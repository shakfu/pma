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
        Scratch(dir)
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
/// the fixtures here push to local repositories.
fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS");
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
    let text = "# TODO\n\n## High\n\n- [x] shipped\n  notes\n- [ ] open\n\n## Done\n\n- [x] old\n- [ ] stray\n";
    let a = s.project("a", text);
    let bad = s.project("bad", "## High\n\n- [x] kept\n");
    let todo = a.join("TODO.md");

    let out = pma(&["prune".as_ref(), a.as_os_str(), bad.as_os_str()]);
    assert!(out.status.success());
    assert_eq!(
        stdout(&out),
        format!(
            "{0}:9: remove `## Done`, lines 9-12\n\
             {0}:12: remove open item with `## Done`: `- [ ] stray`\n\
             {0}:5: remove `shipped`\n\
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
        "# TODO\n\n## High\n\n- [ ] open\n"
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

    let out = pma(&[
        "lint".as_ref(),
        bad.as_os_str(),
        good.as_os_str(),
        missing.as_os_str(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(
        text.contains(&format!("{}:1: error:", bad.join("TODO.md").display())),
        "{text}"
    );
    assert!(
        text.contains(&format!("{}: error:", missing.join("TODO.md").display())),
        "{text}"
    );
    assert!(!text.contains(&good.display().to_string()), "{text}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "2 errors, 0 warnings\n"
    );
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

fn ok(home: &std::path::Path, args: &[&str]) -> String {
    let (out, err, success) = pma_in(home, args);
    assert!(success, "pma {args:?} failed\nstdout: {out}\nstderr: {err}");
    out
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
        scanned.ends_with(": 1 with TODO.md, 3 open items\n"),
        "{scanned}"
    );

    let matrix = ok(&home, &["matrix", "--all"]);
    let expected = "\
Q1 Do right away: 0

Q2 Schedule for later: 2
  alpha:7  T1  high  open 100d  core  old high
  alpha:8  T1  high  open 0d    core  new high

Q3 Delegate or avoid: 0

Q4 Remove: 3
  alpha     T1  medium  open 0d    resolve local changes: 1 changed file, 1 leftover pma branch
  alpha:12  T1  low     open 100d  old low
  alpha     T1  low     open 70d   review project: no code commits in 100 days
";
    let (head, body) = matrix.split_once("\n\n").unwrap();
    assert_eq!(
        head,
        "last scan just now; 1 tiered projects, 1 untiered (pma project tier <1-5> <project>...)"
    );
    assert_eq!(body, expected);

    let status = ok(&home, &["status", "--explain"]);
    assert!(status.contains("\nalpha    1     0."), "{status}");
    assert!(
        status.contains("  ci        n/a     w=3         unknown: offline"),
        "{status}"
    );
    assert!(status.contains("open: 2 high, 1 low"), "{status}");
    assert!(!status.contains("beta"), "{status}");

    // `--all` ranks the untiered project as tier 5 and marks it so.
    let status = ok(&home, &["status", "--all", "--explain"]);
    assert!(
        status.starts_with("last scan just now; 2 projects, 1 untiered ranked as tier 5\n"),
        "{status}"
    );
    assert!(status.contains("\nbeta     -     0."), "{status}");
    assert!(status.contains("\nbeta  tier 5, untiered (x0.2)"), "{status}");

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

    // An untiered project joins the matrix once there is a tier for it.
    assert!(ok(&home, &["matrix"]).contains("1 untiered"));
    ok(&home, &["config", "default_tier", "5"]);
    let matrix = ok(&home, &["matrix"]);
    assert!(matrix.contains("2 tiered projects, 0 untiered"), "{matrix}");
    ok(&home, &["config", "default_tier", "--reset"]);

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
    env.ok(&["scan", "--offline"]);
    (env, origin, alpha)
}

#[test]
fn dispatch_review_rework_and_ship() {
    let s = Scratch::new("stage3");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    env.ok(&["config", "publish", "push"]);

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
    assert!(!env.home.join("worktrees/alpha/local-only").exists());

    // A rejected run leaves no worktree or branch.
    env.ok(&["dispatch", "alpha:8"]);
    env.ok(&["review", "3", "--reject"]);
    assert!(!env.home.join("worktrees/alpha/third-task").exists());
    assert_eq!(git_out(&alpha, &["branch", "--list", "pma/third-task"]), "");

    let out = env.ok(&["ship"]);
    assert!(out.contains("#1 alpha: pushed "), "{out}");
    assert!(out.contains("#2 alpha: pushed "), "{out}");
    let log = git_out(&origin, &["log", "--format=%B|", "main"]);
    assert!(
        log.starts_with("second task\n\nCloses #7\n|\nadd greeting\n|"),
        "{log:?}"
    );
    assert_eq!(
        git_out(&origin, &["show", "main:TODO.md"]),
        "# TODO\n\n## High\n\n- [x] add greeting #agent\n  say hello in hello.txt\n- [x] second task #agent gh:7\n- [ ] third task #agent\n\n## Low\n\n- [ ] guarded task #manual\n"
    );
    assert_eq!(git_out(&origin, &["show", "main:second.txt"]), "two\n");
    assert_eq!(git_out(&origin, &["branch", "--list", "agent"]), "");
    assert!(!env.home.join("worktrees/alpha/add-greeting").exists());
    assert_eq!(git_out(&alpha, &["branch", "--list", "pma/*"]), "");
    assert_eq!(env.ok(&["review"]), "no runs to review\n");
    assert_eq!(env.ok(&["ship"]), "nothing approved\n");

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
    // Four runs: two shipped, `third task` rejected, and one refused at the
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
        &b[..8],
        ["B", "4", "1", "2", "2", "0", "4", "$0.40+1?"],
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

    // The budget refusal never reached the agent, so it consumed nothing:
    // rejecting it leaves one attempt against the task, not two, and no
    // second reset is needed.
    env.ok(&["config", "batch_budget", "5.0"]);
    env.ok(&["review", "4", "--reject"]);
    env.ok(&["dispatch", "alpha:8"]);
    env.ok(&["review", "5", "--reject"]);
}

/// A stand-in for `gh pr`, driven by files in `$PMA_HOME`: `pr-state` for
/// `pr view`, `pr-list` for an open pull request, `pr-create-fails`.
const FAKE_GH_PR: &str = r#"#!/bin/sh
case "$1 $2" in
  "pr create")
    if [ -e "$PMA_HOME/pr-create-fails" ]; then echo "HTTP 502" >&2; exit 1; fi
    echo "https://github.com/me/alpha/pull/1" ;;
  "pr list") if [ -e "$PMA_HOME/pr-list" ]; then cat "$PMA_HOME/pr-list"; fi ;;
  "pr view") cat "$PMA_HOME/pr-state" ;;
  *) echo "fake gh: unexpected $*" >&2; exit 1 ;;
esac
"#;

#[test]
fn a_pull_request_holds_its_task_until_merged_or_closed() {
    let s = Scratch::new("pr");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE), ("gh", FAKE_GH_PR)]);
    let pr_state = env.home.join("pr-state");

    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    let out = env.ok(&["ship"]);
    assert_eq!(out, "#1 alpha: https://github.com/me/alpha/pull/1\n");
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
    let out = env.ok(&["dispatch", "alpha:5"]);
    assert!(out.contains("#3 alpha: add greeting"), "{out}");
    assert!(env.ok(&["review", "3"]).contains("pma/add-greeting-2 in"));

    env.ok(&["review", "3", "--approve"]);
    env.ok(&["ship"]);
    fs::write(&pr_state, "MERGED\n").unwrap();
    let out = env.ok(&["review"]);
    assert_eq!(
        out,
        "#3 alpha: pull request merged: https://github.com/me/alpha/pull/1\nno runs to review\n"
    );
    assert!(!alpha.join("hello.txt").exists(), "the clone is untouched");
}

#[test]
fn ship_resumes_after_a_partial_failure() {
    let s = Scratch::new("resume");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE), ("gh", FAKE_GH_PR)]);
    env.ok(&["config", "publish", "push"]);

    // The push succeeds and removing the worktree fails.
    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    let worktree = env.home.join("worktrees/alpha/add-greeting");
    git(
        &alpha,
        &["worktree", "lock", worktree.to_str().unwrap()],
        None,
    );
    let out = env.ok(&["ship"]);
    assert!(
        out.starts_with("#1 alpha: pushed ") && out.contains("; warning: worktree not removed: "),
        "{out}"
    );
    assert_eq!(
        env.ok(&["review"]),
        "no runs to review\n",
        "shipped all the same"
    );

    // As if pma had stopped between the push and recording it.
    rusqlite::Connection::open(env.home.join("projects.db"))
        .unwrap()
        .execute("UPDATE runs SET state = 'approved' WHERE id = 1", [])
        .unwrap();
    git(
        &alpha,
        &["worktree", "unlock", worktree.to_str().unwrap()],
        None,
    );
    let out = env.ok(&["ship"]);
    assert!(out.starts_with("#1 alpha: already pushed "), "{out}");
    assert!(!worktree.exists());
    assert_eq!(
        git_out(&origin, &["log", "--format=%s", "main"]),
        "add greeting\ninit\n",
        "pushed once"
    );

    // `gh pr create` fails after the push; the retry finds the pull request.
    env.ok(&["config", "publish", "pr"]);
    env.ok(&["dispatch", "alpha:7"]);
    env.ok(&["review", "2", "--approve"]);
    fs::write(env.home.join("pr-create-fails"), "").unwrap();
    let (out, _, success) = env.run(&["ship"]);
    assert!(!success && out.contains("gh pr create: HTTP 502"), "{out}");
    assert!(env.ok(&["review"]).contains("#2  approved"));
    fs::remove_file(env.home.join("pr-create-fails")).unwrap();
    fs::write(
        env.home.join("pr-list"),
        "https://github.com/me/alpha/pull/9\n",
    )
    .unwrap();
    assert_eq!(
        env.ok(&["ship"]),
        "#2 alpha: https://github.com/me/alpha/pull/9\n"
    );
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
    assert!(!env.home.join("worktrees/alpha/guarded-task").exists());
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

/// A second worker runs through the same dispatch, review and ship path as
/// `claude`, with no code that knows its name.
#[test]
fn a_worker_without_json_output_or_an_allowlist_runs_the_same_path() {
    let s = Scratch::new("adapter");
    let (env, origin, _) = dispatch_env(&s, &[("plain", PLAIN_WORKER)]);
    env.ok(&["config", "publish", "push"]);

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
    assert!(env.ok(&["ship"]).contains("#1 alpha: pushed "), "shipped");
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

/// Approval names a tree. Ship publishes that tree or nothing, and rechecks
/// what the rebase produced rather than what was approved in isolation.
#[test]
fn an_approval_is_evidence_about_a_tree_not_a_state() {
    let s = Scratch::new("evidence");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", FAKE_CLAUDE)]);
    env.ok(&["config", "publish", "push"]);

    // A worktree edited after approval is not published.
    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    let worktree = env.home.join("worktrees/alpha/add-greeting");
    fs::write(worktree.join("sneaked.txt"), "later\n").unwrap();
    let (out, err, success) = env.run(&["ship"]);
    assert!(!success, "{out}{err}");
    assert!(
        out.contains("the worktree changed after it was approved"),
        "{out}"
    );
    assert_eq!(git_out(&origin, &["log", "--format=%s", "main"]), "init\n");

    // Reviewing it again approves the tree that is actually there.
    fs::remove_file(worktree.join("sneaked.txt")).unwrap();
    env.ok(&["review", "1", "--approve"]);
    assert!(env.ok(&["ship"]).contains("#1 alpha: pushed "), "shipped");

    // A rework withdraws an approval: the approver read another tree.
    env.ok(&["dispatch", "alpha:7"]);
    env.ok(&["review", "2", "--approve"]);
    env.ok(&["review", "2", "--rework", "add hello.txt too"]);
    let detail = env.ok(&["review", "2"]);
    assert!(!detail.contains("approved by"), "{detail}");

    // The integrated tree is checked, not the approved one in isolation:
    // another commit lands upstream that the approved change breaks.
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
    let (out, err, success) = env.run(&["ship"]);
    assert!(!success, "{out}{err}");
    assert!(
        out.contains("`make test` failed on the integrated tree"),
        "{out}"
    );
    assert_eq!(
        git_out(&origin, &["log", "--format=%s", "main"]),
        "stricter test\nadd greeting\ninit\n",
        "nothing of the run reached the branch"
    );
    let _ = alpha;
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
    let (_, err, success) = env.run(&["review", "1", "2", "--approve"]);
    assert!(!success && err.contains("#2 is not clean"), "{err}");
    assert!(err.contains("verify failed at the head"), "{err}");
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
    assert!(detail.contains("specified in revision 1"), "{detail}");
    assert!(detail.contains("approval batch"), "{detail}");
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
        &["ship"],
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
        "#!/bin/sh\nprintf 'alpha v1\\n|-- ruff v0.14.10 (group: dev) (latest: v0.16.7)\\n`-- ty v0.0.8 (latest: v0.0.81)\\n'\n",
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
        scanned.ends_with(", outdated dependencies in 1\n"),
        "{scanned}"
    );
    ok(&["scan", "--offline"]);
    let status = ok(&["status", "--explain"]);
    assert!(
        status.contains("2 outdated, measured today"),
        "kept: {status}"
    );
    assert!(
        ok(&["matrix", "--all"])
            .contains("alpha  T1  medium  open 0d  update dependencies: 2 outdated"),
        "the deps signal task"
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
    let out = env.ok(&["dispatch", "alpha:q2"]);
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
    let (_, err, ok) = env.run(&["config", "model", "haiku"]);
    assert!(!ok && err.contains("pma agent set"), "{err}");

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
    assert!(out.contains("--yes"), "it says how to approve: {out}");
    assert!(!log.exists(), "the worker must not have run");

    // Approval is what unlocks it. The finding the model wrote enters the
    // graph as a unit and reaches the sink downstream of it.
    let (out, err, success) = run(&["workflow", "run", "review", "--instance", "1", "--yes"]);
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

    // The spend is recorded against the workflow, not loose in the run table.
    let report = run(&["report"]).0;
    assert!(
        report.contains("0.10"),
        "the run's cost is recorded: {report}"
    );
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
    let (out, err, success) = env.run(&["workflow", "run", "greet", "--instance", "1", "--yes"]);
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
    let (_, err, success) = env.run(&["workflow", "run", "greet", "alpha", "--yes"]);
    assert!(!success, "an instance with no edit budget must not edit");
    assert!(err.contains("max_edits"), "{err}");
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
/// that record it commit together. A pass that fails on the second project
/// leaves the first one's work recorded and does not repeat it on resume.
#[test]
fn a_sink_that_fails_part_way_does_not_repeat_what_it_wrote() {
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
    let (_, err, success) = pma_in(&home, &["workflow", "run", "stamp", "alpha", "beta"]);
    assert!(!success, "the pass must report the sink it could not write");
    assert!(err.contains("lint errors"), "{err}");

    let alpha = root.join("alpha").join("TODO.md");
    let written = fs::read_to_string(&alpha).unwrap();
    assert_eq!(
        written.matches("reviewed alpha").count(),
        1,
        "the first project's write committed: {written}"
    );

    // Resuming does not write it a second time, and still names `beta`.
    let (_, err, success) = pma_in(&home, &["workflow", "run", "stamp", "--instance", "1"]);
    assert!(!success && err.contains("lint errors"), "{err}");
    assert_eq!(
        fs::read_to_string(&alpha)
            .unwrap()
            .matches("reviewed alpha")
            .count(),
        1,
        "a resumed pass does not repeat a sink it already wrote"
    );

    // Fixed, `beta` goes through and the pass settles.
    fs::write(
        root.join("beta").join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] something\n",
    )
    .unwrap();
    let out = ok(&home, &["workflow", "run", "stamp", "--instance", "1"]);
    assert!(out.contains("nothing left to run"), "{out}");
    assert!(
        fs::read_to_string(root.join("beta").join("TODO.md"))
            .unwrap()
            .contains("reviewed beta")
    );
    assert_eq!(
        fs::read_to_string(&alpha)
            .unwrap()
            .matches("reviewed alpha")
            .count(),
        1,
        "still once"
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
            .emit_note("shipped", #{ text: "merged: {name}" })
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
    let (out, err, success) = run(&["workflow", "run", "review", "alpha", "beta", "--yes"]);
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
    let (out, err, success) = run(&["workflow", "run", "review", "alpha", "beta", "--yes"]);
    assert!(success, "{err}\n{out}");
    assert!(out.contains("next: review"), "the rest is priced: {out}");
    assert_eq!(
        fs::read_to_string(&log).unwrap().matches("start").count(),
        1,
        "the batch budget admitted one run"
    );

    let (out, err, success) = run(&["workflow", "run", "review", "--instance", "2", "--yes"]);
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
    assert!(!env.home.join("worktrees/alpha/add-greeting").exists());
    assert_eq!(git_out(&alpha, &["branch", "--list", "pma/*"]), "");
    assert!(!s.0.join("pwned").exists(), "the monitor ran");
}

/// An agent that reaches from its worktree into the clone's hooks on the
/// second task, and behaves on the first.
const HOOKING_CLAUDE: &str = r#"#!/bin/sh
case "$2" in
  *"add greeting"*) echo hi > hello.txt ;;
  *"second task"*)
    echo hi > hello.txt
    hooks="$(git rev-parse --git-common-dir)/hooks"
    printf '#!/bin/sh\ntouch "%s"\n' "$PMA_HOME/../hooked" > "$hooks/pre-commit"
    chmod +x "$hooks/pre-commit" ;;
esac
echo '{"type":"result","subtype":"success","is_error":false,"result":"did it","total_cost_usd":0.1}'
"#;

/// The user's own hooks run at ship. One a run added does not: nothing in
/// the diff shows it, and ship commits and pushes with the user's
/// credentials.
#[test]
fn a_hook_added_during_a_run_stops_its_ship() {
    use std::os::unix::fs::PermissionsExt;

    let s = Scratch::new("hooks");
    let (env, origin, alpha) = dispatch_env(&s, &[("claude", HOOKING_CLAUDE)]);
    env.ok(&["config", "publish", "push"]);
    let own = alpha.join(".git/hooks/post-commit");
    fs::write(
        &own,
        format!("#!/bin/sh\necho ran >> {}\n", s.0.join("own.log").display()),
    )
    .unwrap();
    fs::set_permissions(&own, fs::Permissions::from_mode(0o755)).unwrap();

    env.ok(&["dispatch", "alpha:5"]);
    env.ok(&["review", "1", "--approve"]);
    assert!(env.ok(&["ship"]).contains("#1 alpha: pushed "));
    assert!(s.0.join("own.log").exists(), "the user's hook did not run");

    env.ok(&["dispatch", "alpha:7"]);
    env.ok(&["review", "2", "--approve"]);
    let (out, _, success) = env.run(&["ship"]);
    assert!(!success, "{out}");
    assert!(
        out.contains("hooks or git settings changed since run #2 was dispatched")
            && out.contains("  + hook pre-commit "),
        "{out}"
    );
    assert!(!s.0.join("hooked").exists(), "the added hook ran");
    assert_eq!(
        git_out(&origin, &["log", "--format=%s", "main"]),
        "add greeting\ninit\n"
    );

    // Restoring the hooks is enough.
    fs::remove_file(alpha.join(".git/hooks/pre-commit")).unwrap();
    assert!(env.ok(&["ship"]).contains("#2 alpha: pushed "));
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
    assert!(!env.home.join("worktrees/alpha/add-greeting").exists());
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
    assert!(!env.home.join("verify-trees/alpha").exists());
    assert_eq!(git_out(&alpha, &["worktree", "list"]).lines().count(), 1);
    let (_, err, success) = env.run(&["verify"]);
    assert!(!success && err.contains("name a project"), "{err}");
}
