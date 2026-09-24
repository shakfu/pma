//! Runs a coding agent, and then the project's verify command, in a worktree.
//!
//! Both run without push credentials, in their own process group, under a
//! timeout. Which agent runs, and how it is invoked, is a record in the
//! store: see `worker.rs`.

use std::ffi::OsStr;
use std::fs::File;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

/// Removes what lets a child push, using `dir`, a directory `pma` owns outside
/// every worktree, for an empty `gh` configuration and a `pre-push` hook that
/// refuses. It stops an accidental push, not a process set on pushing:
/// `git push --no-verify` to a remote with an explicit `pushurl` still gets
/// through, and so does anything that reads a credential from disk.
pub fn restrict(cmd: &mut Command, dir: &Path) -> std::io::Result<()> {
    let gh = dir.join("gh");
    std::fs::create_dir_all(&gh)?;
    let hooks = dir.join("hooks");
    write_hook(&hooks)?;
    cmd.env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("SSH_AUTH_SOCK")
        .env("GH_CONFIG_DIR", &gh)
        .env("GIT_TERMINAL_PROMPT", "0");
    let settings: [(&str, &OsStr); 3] = [
        // An empty helper clears the user's credential helpers.
        ("credential.helper", OsStr::new("")),
        // An empty prefix matches every URL, so a push to any remote or URL
        // is rewritten to one that cannot be reached. Git ignores this for
        // a remote with an explicit `pushurl`, which the hook covers.
        ("url.pma-agents-cannot-push:.pushInsteadOf", OsStr::new("")),
        // Also keeps the project's own hooks off the agent's git commands;
        // it is told not to commit.
        ("core.hooksPath", hooks.as_os_str()),
    ];
    // Appended after any the user set, which a fixed count would drop.
    let first = cmd
        .get_envs()
        .find(|(k, _)| *k == "GIT_CONFIG_COUNT")
        .map(|(_, v)| v.map(OsStr::to_os_string))
        .unwrap_or_else(|| std::env::var_os("GIT_CONFIG_COUNT"))
        .and_then(|v| v.to_str()?.parse::<usize>().ok())
        .unwrap_or(0);
    for (i, (key, value)) in settings.iter().enumerate() {
        cmd.env(format!("GIT_CONFIG_KEY_{}", first + i), key)
            .env(format!("GIT_CONFIG_VALUE_{}", first + i), value);
    }
    cmd.env("GIT_CONFIG_COUNT", (first + settings.len()).to_string());
    Ok(())
}

const PRE_PUSH: &str = "#!/bin/sh\necho 'pma: agents cannot push' >&2\nexit 1\n";

/// Writes the refusing `pre-push` hook unless it is already there. Through a
/// rename, because agents running in parallel share the file.
fn write_hook(hooks: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let path = hooks.join("pre-push");
    let executable =
        |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 == 0o111);
    if std::fs::read_to_string(&path).is_ok_and(|t| t == PRE_PUSH) && executable(&path) {
        return Ok(());
    }
    std::fs::create_dir_all(hooks)?;
    let tmp = hooks.join(format!(
        ".pre-push.{}.{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&tmp, PRE_PUSH)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, &path)
}

#[derive(Debug)]
pub struct Finished {
    /// `None` when killed at the timeout.
    pub success: Option<bool>,
    pub seconds: i64,
}

/// Runs `cmd` with stdout and stderr in `log`, then kills its process group:
/// at `timeout`, and also after a normal exit, so nothing it started in the
/// background outlives it.
pub fn run_limited(cmd: Command, log: &Path, timeout: Duration) -> std::io::Result<Finished> {
    let started = Instant::now();
    // Held until the group is killed. Dropped any earlier, it ends the run.
    let (mut child, _lifeline) = spawn_guarded(cmd, log)?;
    let success = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status.success());
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // The watchdog is still waiting on its pipe, so the group exists and its
    // id cannot have been reused.
    let group = format!("-{}", child.id());
    let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
    child.wait()?;
    Ok(Finished {
        success,
        seconds: started.elapsed().as_secs() as i64,
    })
}

/// Runs the program with its arguments, and kills the whole process group
/// once its standard input, a pipe `pma` holds, reaches end of file. `pma`
/// exiting for any reason closes the pipe, so a child cannot outlive the
/// session whose lock it ran under. `kill -KILL -$$` rather than `kill --`,
/// which `dash` refuses.
const WATCHDOG: &str = r#"exec 3<&0 0</dev/null; "$@" 3<&- & pid=$!; { read _ <&3; kill -KILL -$$; } & exec 3<&-; wait "$pid""#;

/// Starts `cmd` under the watchdog, as the leader of a new process group,
/// with stdout and stderr in `log`. Dropping the returned pipe kills the
/// group.
fn spawn_guarded(cmd: Command, log: &Path) -> std::io::Result<(Child, ChildStdin)> {
    use std::os::unix::process::CommandExt;

    let mut guarded = Command::new("sh");
    guarded
        .arg("-c")
        .arg(WATCHDOG)
        .arg("pma-watchdog")
        .arg(cmd.get_program())
        .args(cmd.get_args());
    if let Some(dir) = cmd.get_current_dir() {
        guarded.current_dir(dir);
    }
    for (key, value) in cmd.get_envs() {
        match value {
            Some(v) => guarded.env(key, v),
            None => guarded.env_remove(key),
        };
    }
    let out = File::create(log)?;
    guarded
        .stdin(Stdio::piped())
        .stdout(out.try_clone()?)
        .stderr(out)
        .process_group(0);
    let mut child = guarded.spawn()?;
    let lifeline = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("the watchdog's pipe was not created"))?;
    Ok((child, lifeline))
}

/// The subcommands of a compound command. Claude Code checks each part of
/// `a && b` against its rules separately, so an allowlist needs one rule per
/// part. Quoting is not parsed: a rule that fails to match only denies the
/// agent that command.
pub fn bash_parts(command: &str) -> Vec<String> {
    // `>&` and `&>` are redirections, not separators.
    let command = command.replace(">&", "\u{0}").replace("&>", "\u{1}");
    command
        .replace("|&", "\n")
        .split(['\n', ';', '|', '&'])
        .map(|part| part.trim().replace('\u{0}', ">&").replace('\u{1}', "&>"))
        .filter(|part| !part.is_empty())
        .collect()
}

#[derive(Debug, PartialEq)]
pub struct Report {
    pub ok: bool,
    pub summary: String,
    pub cost_usd: Option<f64>,
}

/// Reads `claude -p --output-format json` output. Anything else is reported as
/// a failure with the output's tail as the summary.
pub fn parse_claude(output: &str) -> Report {
    let value = output
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("result"));
    match value {
        Some(v) => Report {
            ok: !v["is_error"].as_bool().unwrap_or(true)
                && v["subtype"].as_str() == Some("success"),
            summary: v["result"]
                .as_str()
                .map(String::from)
                .or_else(|| v["subtype"].as_str().map(String::from))
                .unwrap_or_default(),
            cost_usd: v["total_cost_usd"].as_f64(),
        },
        None => Report {
            ok: false,
            summary: tail(output, 20),
            cost_usd: None,
        },
    }
}

pub fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// The first of `make test`, `cargo test`, `go test ./...`, `npm test` and
/// pytest that the project's files support.
pub fn detect_verify(dir: &Path) -> Option<String> {
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
    let has_test_target = ["GNUmakefile", "makefile", "Makefile"]
        .iter()
        .filter_map(|m| read(m))
        .any(|text| text.lines().any(|l| l.starts_with("test:")));
    if has_test_target {
        return Some("make test".into());
    }
    if dir.join("Cargo.toml").is_file() {
        return Some("cargo test".into());
    }
    if dir.join("go.mod").is_file() {
        return Some("go test ./...".into());
    }
    if read("package.json").is_some_and(|t| t.contains("\"test\"")) {
        return Some("npm test".into());
    }
    if dir.join("pyproject.toml").is_file() {
        return Some(if dir.join("uv.lock").is_file() {
            "uv run pytest".into()
        } else {
            "python3 -m pytest".into()
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pma-agent-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn claude_results_are_read() {
        let ok = r#"{"type":"result","subtype":"success","is_error":false,"result":"Fixed it.","total_cost_usd":0.42}"#;
        assert_eq!(
            parse_claude(ok),
            Report {
                ok: true,
                summary: "Fixed it.".into(),
                cost_usd: Some(0.42)
            }
        );
        let over = r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"total_cost_usd":1.01}"#;
        let r = parse_claude(&format!("noise\n{over}\n"));
        assert!(!r.ok);
        assert_eq!(
            (r.summary.as_str(), r.cost_usd),
            ("error_max_budget_usd", Some(1.01))
        );
        let junk = parse_claude("Error: not logged in\n");
        assert!(!junk.ok);
        assert_eq!(junk.summary, "Error: not logged in");
    }

    #[test]
    fn a_compound_verify_command_splits_into_its_parts() {
        assert_eq!(
            bash_parts("make check && uv run pytest -q 2>&1 | tail -5; cargo test &> log"),
            [
                "make check",
                "uv run pytest -q 2>&1",
                "tail -5",
                "cargo test &> log"
            ]
        );
        assert_eq!(bash_parts("a || b |& c"), ["a", "b", "c"]);
    }

    #[test]
    fn verify_is_detected_from_project_files() {
        let dir = scratch("verify");
        assert_eq!(detect_verify(&dir), None);
        std::fs::write(dir.join("pyproject.toml"), "").unwrap();
        assert_eq!(detect_verify(&dir).as_deref(), Some("python3 -m pytest"));
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        assert_eq!(detect_verify(&dir).as_deref(), Some("cargo test"));
        std::fs::write(dir.join("Makefile"), "build:\n\tcc\n").unwrap();
        assert_eq!(
            detect_verify(&dir).as_deref(),
            Some("cargo test"),
            "a Makefile without a test target"
        );
        std::fs::write(dir.join("Makefile"), "test: build\n\tcc\n").unwrap();
        assert_eq!(detect_verify(&dir).as_deref(), Some("make test"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn runs_are_logged_and_killed_at_the_timeout() {
        let dir = scratch("run");
        let log = dir.join("log");
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo out; echo err >&2; exit 3"]);
        let done = run_limited(cmd, &log, Duration::from_secs(10)).unwrap();
        assert_eq!(done.success, Some(false));
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "out\nerr\n");

        // The grandchild `sleep` must die with its group.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & wait"]);
        let started = Instant::now();
        let done = run_limited(cmd, &log, Duration::from_millis(300)).unwrap();
        assert_eq!(done.success, None);
        assert!(started.elapsed() < Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Whether `pid` is gone within 5 s. A killed process can linger as a
    /// zombie until its new parent reaps it, and `kill -0` still finds one.
    fn gone(pid: &str) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            let alive = Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// Reads the pid a child wrote to `file`, waiting for it to appear.
    fn pid_in(file: &Path) -> String {
        for _ in 0..50 {
            if let Ok(pid) = std::fs::read_to_string(file)
                && !pid.trim().is_empty()
            {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("{} never appeared", file.display());
    }

    #[test]
    fn a_background_process_does_not_outlive_a_normal_exit() {
        let dir = scratch("orphan");
        let pid = dir.join("pid");
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & echo $! > pid; exit 0"])
            .current_dir(&dir);
        let done = run_limited(cmd, &dir.join("log"), Duration::from_secs(10)).unwrap();
        assert_eq!(done.success, Some(true), "the child's own status");
        assert!(gone(&pid_in(&pid)), "the background sleep was left running");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// What happens when `pma` dies mid-run: its end of the pipe closes.
    #[test]
    fn a_child_dies_with_the_process_that_started_it() {
        let dir = scratch("lifeline");
        let pid = dir.join("pid");
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & echo $! > pid; wait"])
            .current_dir(&dir);
        let (mut child, lifeline) = spawn_guarded(cmd, &dir.join("log")).unwrap();
        let grandchild = pid_in(&pid);
        drop(lifeline);
        let started = Instant::now();
        child.wait().unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(gone(&grandchild));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn restricted_children_cannot_see_tokens() {
        let dir = scratch("env");
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo \"${GH_TOKEN-unset} $GIT_TERMINAL_PROMPT\""])
            .env("GH_TOKEN", "secret")
            .current_dir(&dir);
        restrict(&mut cmd, &dir).unwrap();
        let out = cmd.output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "unset 0\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// No remote, URL or explicit `pushurl` takes a push, and the settings a
    /// user passed through the environment survive.
    #[test]
    fn restricted_children_cannot_push_anywhere() {
        let dir = scratch("push");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        git(&["init", "-q", "--bare", "real.git"]);
        git(&["init", "-q", "--bare", "other.git"]);
        git(&["init", "-q", "wt"]);
        let wt = dir.join("wt");
        let real = dir.join("real.git");
        let url = real.to_str().unwrap();
        git(&["-C", "wt", "commit", "-q", "--allow-empty", "-m", "a"]);
        git(&["-C", "wt", "remote", "add", "origin", url]);
        git(&["-C", "wt", "remote", "add", "upstream", "../other.git"]);
        git(&["-C", "wt", "config", "remote.upstream.pushurl", url]);

        let env = dir.join("env");
        let pushes = |args: &[&str]| {
            let mut cmd = Command::new("git");
            cmd.arg("-C")
                .arg(&wt)
                .arg("push")
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .stderr(Stdio::null());
            restrict(&mut cmd, &env).unwrap();
            cmd.status().unwrap().success()
        };
        assert!(!pushes(&["origin", "HEAD:refs/heads/a"]));
        assert!(!pushes(&[url, "HEAD:refs/heads/b"]));
        assert!(
            !pushes(&["upstream", "HEAD:refs/heads/c"]),
            "explicit pushurl"
        );
        let refs = Command::new("git")
            .args(["-C", url, "for-each-ref"])
            .output()
            .unwrap();
        assert!(
            refs.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&refs.stdout)
        );

        let mut cmd = Command::new("git");
        cmd.args(["config", "user.name"])
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "user.name")
            .env("GIT_CONFIG_VALUE_0", "kept");
        restrict(&mut cmd, &env).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&cmd.output().unwrap().stdout),
            "kept\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
