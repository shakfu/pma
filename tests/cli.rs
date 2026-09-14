use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

/// A scratch directory unique to one test, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("pma-test-{}-{name}", std::process::id()));
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

fn pma(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pma"))
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
    let dir = s.project("a", "# TODO\n\n## Low\n\n- [x] finished\n");
    let out = pma(&["lint".as_ref(), dir.as_os_str()]);
    assert!(out.status.success());
    let todo = dir.join("TODO.md");
    assert_eq!(
        stdout(&out),
        format!(
            "{}:5: warning: finished item; move it to `## Done`\n",
            todo.display()
        )
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
    let mut cmd = Command::new("git");
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
    let out = Command::new(env!("CARGO_BIN_EXE_pma"))
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
        "# TODO\n\n## High\n\n- [ ] old high\n\n## Low\n\n- [ ] old low\n",
    )
    .unwrap();
    fs::write(alpha.join("lib.rs"), "\n").unwrap();
    git(&alpha, &["add", "."], None);
    git(&alpha, &["commit", "-qm", "init"], Some(now() - 100 * DAY));
    // 50 days ago the low item was retagged. Its text is unchanged, so it keeps
    // its age; the commit only touched TODO.md, so it is not activity.
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] old high\n\n## Low\n\n- [ ] old low #later\n",
    )
    .unwrap();
    git(&alpha, &["commit", "-qam", "retag"], Some(now() - 50 * DAY));
    // Not committed: a new item, which also leaves the tree dirty.
    fs::write(
        alpha.join("TODO.md"),
        "# TODO\n\n## High\n\n- [ ] old high\n- [ ] new high\n\n## Low\n\n- [ ] old low #later\n",
    )
    .unwrap();

    let (_, err, success) = pma_in(&home, &["scan"]);
    assert!(!success && err.contains("pma root add"), "{err}");
    let (_, err, success) = pma_in(&home, &["matrix"]);
    assert!(!success && err.contains("pma scan"), "{err}");

    ok(&home, &["root", "add", root.to_str().unwrap()]);
    ok(&home, &["tier", "alpha", "1"]);
    assert_eq!(ok(&home, &["tier", "alpha"]), "1\n");
    let (_, err, success) = pma_in(&home, &["tier", "alpha", "6"]);
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
Q1 Do right away: 1
  alpha:5  T1  high  open 100d  old high

Q2 Schedule for later: 1
  alpha:6  T1  high  open 0d  new high

Q3 Delegate or avoid: 2
  alpha:10  T1  low  open 100d  old low
  alpha     T1  low  open 70d   review project: no code commits in 100 days

Q4 Remove: 1
  alpha  T1  medium  open 0d  resolve local changes: 1 changed file
";
    let (head, body) = matrix.split_once("\n\n").unwrap();
    assert_eq!(
        head,
        "last scan just now; 1 tiered projects, 1 untiered (pma tier <project> <1-5>)"
    );
    assert_eq!(body, expected);

    let status = ok(&home, &["status", "--explain"]);
    assert!(status.contains("\nalpha    1     0."), "{status}");
    assert!(
        status.contains("  ci        n/a     w=3         unknown: offline"),
        "{status}"
    );
    assert!(status.contains("open: 2 high, 1 low"), "{status}");

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
    assert!(ok(&home, &["matrix", "-q", "q1"]).contains("Q1 Do right away: 1\n"));

    // A rescan keeps the tier and each task's first sighting.
    ok(&home, &["scan", "--offline", "alpha"]);
    assert_eq!(ok(&home, &["tier", "alpha"]), "1\n");
}
