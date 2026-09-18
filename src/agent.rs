//! Runs a coding agent, and then the project's verify command, in a worktree.
//!
//! Both run without push credentials, in their own process group, under a
//! timeout. Which agent runs, and how it is invoked, is a record in the
//! store: see `worker.rs`.

use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Removes what lets a child push. It stops an accidental push, not a
/// determined process.
pub fn restrict(cmd: &mut Command, empty_dir: &Path) {
    cmd.env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("SSH_AUTH_SOCK")
        .env("GH_CONFIG_DIR", empty_dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        // An empty helper clears the user's credential helpers, and an
        // unusable push URL makes `git push` to origin fail.
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "credential.helper")
        .env("GIT_CONFIG_VALUE_0", "")
        .env("GIT_CONFIG_KEY_1", "remote.origin.pushurl")
        .env("GIT_CONFIG_VALUE_1", "pma-agents-cannot-push");
}

#[derive(Debug)]
pub struct Finished {
    /// `None` when killed at the timeout.
    pub success: Option<bool>,
    pub seconds: i64,
}

/// Runs `cmd` with stdout and stderr in `log`, killing its process group
/// after `timeout`.
pub fn run_limited(mut cmd: Command, log: &Path, timeout: Duration) -> std::io::Result<Finished> {
    use std::os::unix::process::CommandExt;

    let out = File::create(log)?;
    cmd.stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .process_group(0);
    let started = Instant::now();
    let mut child = cmd.spawn()?;
    let success = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status.success());
        }
        if started.elapsed() >= timeout {
            let group = format!("-{}", child.id());
            let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
            let _ = child.kill();
            child.wait()?;
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    Ok(Finished {
        success,
        seconds: started.elapsed().as_secs() as i64,
    })
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

    #[test]
    fn restricted_children_cannot_see_tokens() {
        let dir = scratch("env");
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "echo \"${GH_TOKEN-unset} $GIT_TERMINAL_PROMPT\"; git config remote.origin.pushurl",
        ])
        .env("GH_TOKEN", "secret")
        .current_dir(&dir);
        restrict(&mut cmd, &dir);
        let out = cmd.output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "unset 0\npma-agents-cannot-push\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
