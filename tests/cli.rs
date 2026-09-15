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
  alpha:7  T1  high  open 100d  core  old high

Q2 Schedule for later: 1
  alpha:8  T1  high  open 0d  core  new high

Q3 Delegate or avoid: 2
  alpha:12  T1  low  open 100d  old low
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

struct Env {
    home: PathBuf,
    bin: PathBuf,
    push_log: PathBuf,
}

impl Env {
    fn run(&self, args: &[&str]) -> (String, String, bool) {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new(env!("CARGO_BIN_EXE_pma"))
            .env("PMA_HOME", &self.home)
            .env("PATH", path)
            .env("PUSH_LOG", &self.push_log)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "u")
            .env("GIT_AUTHOR_EMAIL", "u@example.com")
            .env("GIT_COMMITTER_NAME", "u")
            .env("GIT_COMMITTER_EMAIL", "u@example.com")
            .args(args)
            .output()
            .unwrap();
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
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn dispatch_review_rework_and_ship() {
    use std::os::unix::fs::PermissionsExt;

    let s = Scratch::new("stage3");
    let bin = s.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("claude"), FAKE_CLAUDE).unwrap();
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
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
        "# TODO\n\n## High\n\n- [ ] add greeting\n  say hello in hello.txt\n- [ ] second task gh:7\n- [ ] third task\n\n## Done\n",
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
    env.ok(&["tier", "alpha", "1"]);
    env.ok(&["scan", "--offline"]);
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
    assert!(detail.contains("`make test` passed"), "{detail}");
    assert!(detail.contains("+hi"), "{detail}");
    let (out, _, _) = env.run(&["review", "2", "--approve"]);
    assert_eq!(out, "");

    // Rework keeps the worktree and adds the reviewer's fix.
    let out = env.ok(&["review", "2", "--rework", "tests need hello.txt"]);
    assert!(out.contains("verify ok  $0.20"), "{out}");
    env.ok(&["review", "1", "--approve"]);
    env.ok(&["review", "2", "--approve"]);
    let (_, err, success) = env.run(&["review", "2", "--approve"]);
    assert!(!success && err.contains("only a ready run"), "{err}");

    // An item that exists only in the local file cannot be dispatched.
    let local = fs::read_to_string(alpha.join("TODO.md"))
        .unwrap()
        .replace("- [ ] third task\n", "- [ ] third task\n- [ ] local only\n");
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
        "# TODO\n\n## High\n\n- [ ] third task\n\n## Done\n\n- [x] second task gh:7\n- [x] add greeting\n  say hello in hello.txt\n"
    );
    assert_eq!(git_out(&origin, &["show", "main:second.txt"]), "two\n");
    assert_eq!(git_out(&origin, &["branch", "--list", "agent"]), "");
    assert!(!env.home.join("worktrees/alpha/add-greeting").exists());
    assert_eq!(git_out(&alpha, &["branch", "--list", "pma/*"]), "");
    assert_eq!(env.ok(&["review"]), "no runs to review\n");
    assert_eq!(env.ok(&["ship"]), "nothing approved\n");

    // A run whose budget would exceed the batch's does not start.
    env.ok(&["config", "batch_budget", "0.05"]);
    let out = env.ok(&["dispatch", "alpha:8"]);
    assert!(
        out.contains("not started: batch budget $0.05 reached"),
        "{out}"
    );
    assert!(
        out.ends_with("0 ready, 1 failed, $0.00 spent; see `pma review`\n"),
        "{out}"
    );
    env.ok(&["review", "4", "--reject"]);
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
        let out = Command::new(env!("CARGO_BIN_EXE_pma"))
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
        "# TODO\n\n## Critical\n\n- [ ] segfault on empty input gh:4\n  when stdin is empty\n- [ ] renamed crash gh:2\n\n## High\n\n## Done\n\n- [x] fixed elsewhere gh:1\n"
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
        let out = Command::new(env!("CARGO_BIN_EXE_pma"))
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
    ok(&["tier", "alpha", "1"]);
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
