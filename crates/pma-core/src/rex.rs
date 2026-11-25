//! Runs agents in Rex: `pma integrate rex`. One session per project, one
//! window per run, driven through the `rex` CLI. Design:
//! docs/dev/rex-integration.md.
//!
//! A pane is stored as `rex:<session>/<block>`: `rex events` takes a session,
//! not a block.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use serde_json::Value;

use crate::config::RexSessions;

const PREFIX: &str = "rex:";
/// The oldest server API measured.
const MIN_API: u64 = 18;
const READ_TICK: Duration = Duration::from_secs(1);
/// As for herdr: this many failed reconnects, two seconds apart, mean the
/// server has gone, and its terminals with it.
const SERVER_GONE: u32 = 15;

/// The report a headless run's wrapper sends around its script, in Rex's
/// Program Status Protocol (OSC 7501). `$1` is the script, `$2` its status.
pub const REPORTED: &str = r#"printf '\033]7501;state=working:app=pma\033\\'; sh "$1"; if [ "$(cat "$2" 2>/dev/null)" = 0 ]; then s=done; else s=error; fi; printf '\033]7501;state=%s:app=pma\033\\' "$s""#;

/// Whether `pane` names a Rex block.
pub fn is_rex(pane: &str) -> bool {
    pane.starts_with(PREFIX)
}

/// A pane's session and block.
fn parts(pane: &str) -> (&str, &str) {
    pane.strip_prefix(PREFIX)
        .unwrap_or(pane)
        .split_once('/')
        .unwrap_or(("", pane))
}

/// The CLI: the one the pane `pma` runs in names, the server's own copy, or
/// `rex` on `PATH`.
fn bin() -> PathBuf {
    let named = std::env::var_os("REX_BIN_DIR").map(|d| Path::new(&d).join("rex"));
    let installed = std::env::var_os("HOME").map(|h| {
        Path::new(&h).join(
            "Library/Application Support/com.superlogical.rex/RexServer/Versions/Current/\
             Rex Server.app/Contents/MacOS/rex",
        )
    });
    [named, installed]
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("rex"))
}

/// `rex`'s own message: its last `Error:` line, else all of stderr.
fn error_of(stderr: &str) -> String {
    stderr
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("Error: "))
        .unwrap_or(stderr.trim())
        .to_string()
}

/// Runs `rex`, never starting a server, and reads its JSON.
fn rex(global: &[&str], args: &[&str]) -> Result<Value, String> {
    let what = format!("rex {}", args.first().unwrap_or(&""));
    let out = Command::new(bin())
        .arg("--autostart=false")
        .args(global)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("{what}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("{what}: {}", error_of(&err)));
    }
    Ok(serde_json::from_slice(&out.stdout).unwrap_or(Value::Null))
}

/// Whether a Rex server answers with an API this was measured against.
pub fn check() -> Result<(), String> {
    let me = rex(&[], &["whoami", "--json"])?;
    match me["api_revision"].as_u64() {
        Some(n) if n >= MIN_API => Ok(()),
        n => Err(format!(
            "Rex server API revision {} is older than {MIN_API}",
            n.map_or("unknown".into(), |n| n.to_string())
        )),
    }
}

/// The session a run's tab goes in, and the tab's title. A task's title is
/// cut to 40 characters before the project is put in front of it.
fn placement(sessions: RexSessions, project: &str, title: &str) -> (String, String) {
    let title: String = title.chars().take(40).collect();
    match sessions {
        RexSessions::Single => ("pma".into(), format!("{project}: {title}")),
        RexSessions::Project => (format!("pma: {project}"), title),
    }
}

/// The session labelled `label` in a `rex ls --json` listing.
fn session_of(listed: &Value, label: &str) -> Option<String> {
    listed["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["label"] == label)
        .and_then(|s| s["session_id"].as_str().map(String::from))
}

/// How a new tab is shown.
#[derive(Clone, Copy, PartialEq)]
pub enum Show {
    /// In front: a person asked for it.
    Front,
    /// In a tab, leaving the person's view as it was.
    Back,
}

/// Starts `argv` (a shell when empty) in a new tab of the session `sessions`
/// picks, made if need be, and returns its pane. A shell stays once `argv`
/// ends.
pub fn start(
    sessions: RexSessions,
    project: &str,
    title: &str,
    cwd: &Path,
    argv: &[String],
    show: Show,
) -> Result<String, String> {
    let (label, title) = placement(sessions, project, title);
    let cwd = cwd.to_string_lossy();
    let argv = crate::herdr_app::wrapped(argv, std::env::var("SHELL").ok().as_deref());
    let mut tail: Vec<&str> = Vec::new();
    if !argv.is_empty() {
        tail.push("--");
        tail.extend(argv.iter().map(String::as_str));
    }
    let listed = rex(&[], &["ls", "--json"])?;
    let (session, block) = match session_of(&listed, &label) {
        Some(session) => {
            let focus = format!("--focus={}", show == Show::Front);
            let mut args = vec!["window", "new", &title, "-s", &session, "-c", &cwd];
            args.extend([focus.as_str(), "--json"]);
            args.extend(&tail);
            let made = rex(&[], &args)?;
            let block = made["block_ids"][0].as_str().map(String::from);
            (session, block)
        }
        None => {
            let mut args = vec!["new", &label, "--window", &title, "-c", &cwd, "--json"];
            args.extend(&tail);
            let made = rex(&[], &args)?;
            let session = made["session_id"]
                .as_str()
                .ok_or("rex made a session and named none")?
                .to_string();
            let block = made["initial_windows"][0]["block_ids"][0]
                .as_str()
                .map(String::from);
            (session, block)
        }
    };
    let block = block.ok_or("rex made a tab and named no block")?;
    let pane = format!("{PREFIX}{session}/{block}");
    if show == Show::Front
        && let Err(e) = focus(&pane)
    {
        eprintln!("warning: {e}");
    }
    Ok(pane)
}

/// Closes a pane, and whatever runs in it. It may be gone already.
pub fn close(pane: &str) {
    let _ = rex(&[], &["block", "close", parts(pane).1]);
}

/// The app clients in a `rex client ls --json` listing.
fn apps(listed: &Value) -> Vec<String> {
    listed["clients"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["info"]["kind"] == "app")
        .filter_map(|c| c["client_id"].as_str().map(String::from))
        .collect()
}

/// Selects the pane's tab in its session, then shows that session in every
/// connected Rex.app. The second needs Remote Control on in the app.
pub fn focus(pane: &str) -> Result<(), String> {
    let (session, block) = parts(pane);
    rex(&[], &["focus", block, "-s", session])?;
    let clients = apps(&rex(&[], &["client", "ls", "--json"])?);
    if clients.is_empty() {
        return Err("no Rex.app is connected to show it".into());
    }
    let arg = format!("session_id={session}");
    let mut failed = None;
    let mut shown = false;
    for client in &clients {
        match rex(&["-C", client], &["do", "session.select", &arg]) {
            Ok(_) => shown = true,
            Err(e) => failed = Some(e),
        }
    }
    match (shown, failed) {
        (false, Some(e)) => Err(e),
        _ => Ok(()),
    }
}

/// What one line of `rex events --json` says about a block.
#[derive(Debug, PartialEq)]
enum Heard {
    Closed,
    Status(String),
    Nothing,
}

/// herdr's states: `error` is read as `done`, and `clear` as nothing, since
/// the verdict comes from the status file.
fn state_of(state: &str) -> Option<&'static str> {
    match state {
        "idle" => Some("idle"),
        "working" => Some("working"),
        "blocked" => Some("blocked"),
        "done" | "error" => Some("done"),
        _ => None,
    }
}

fn hear(line: &str, block: &str) -> Heard {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
        return Heard::Nothing;
    };
    let ev = &v["payload"]["event"];
    let p = &ev["payload"];
    if p["block_id"] != block {
        return Heard::Nothing;
    }
    match (ev["type"].as_str(), p["name"].as_str()) {
        (Some("block_closed"), _) | (Some("block_event"), Some("child_exited")) => Heard::Closed,
        (Some("block_event"), Some("program_status_changed")) => p["payload"]["record"]["state"]
            .as_str()
            .and_then(state_of)
            .map_or(Heard::Nothing, |s| Heard::Status(s.into())),
        _ => Heard::Nothing,
    }
}

/// Whether the block is still there; an error when the server does not say.
fn exists(block: &str) -> Result<bool, String> {
    match rex(&[], &["block", "inspect", block, "--json"]) {
        Ok(_) => Ok(true),
        Err(e) if e.contains("not found") => Ok(false),
        Err(e) => Err(e),
    }
}

struct Events(Child);

impl Drop for Events {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Follows `pane` until `stop`, setting `closed` once its block has gone and
/// passing each change of state to `on_state`. Rex sends no snapshot, so the
/// block is checked after each subscription ends.
pub fn follow(pane: &str, closed: &AtomicBool, stop: &AtomicBool, on_state: &dyn Fn(&str)) {
    let (session, block) = parts(pane);
    let mut failures = 0;
    let mut last = String::new();
    while !stop.load(Ordering::Relaxed) {
        match subscribe_once(session, block, closed, stop, on_state, &mut last) {
            Ok(()) => failures = 0,
            Err(_) => failures += 1,
        }
        if closed.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
            return;
        }
        let gone = match exists(block) {
            Ok(there) => !there,
            Err(_) => failures >= SERVER_GONE,
        };
        if gone {
            closed.store(true, Ordering::Relaxed);
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn subscribe_once(
    session: &str,
    block: &str,
    closed: &AtomicBool,
    stop: &AtomicBool,
    on_state: &dyn Fn(&str),
    last: &mut String,
) -> Result<(), String> {
    let mut child = Command::new(bin())
        .args(["--autostart=false", "events", "-s", session, "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("rex events: {e}"))?;
    let stdout = child.stdout.take().ok_or("rex events: no output")?;
    let _events = Events(child);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line.map(|l| tx.send(l)).is_err() {
                return;
            }
        }
    });
    while !stop.load(Ordering::Relaxed) {
        let line = match rx.recv_timeout(READ_TICK) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Err("rex events ended".into()),
        };
        match hear(&line, block) {
            Heard::Closed => {
                closed.store(true, Ordering::Relaxed);
                return Ok(());
            }
            Heard::Status(state) if state != *last => {
                on_state(&state);
                *last = state;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: &str = "block:01a212d9-ac0f";

    fn block_event(block: &str, name: &str, payload: &str) -> String {
        format!(
            r#"{{"type":"session_event","payload":{{"session_id":"session:1","event":{{"type":"block_event","payload":{{"block_id":"{block}","creator_name":"com.superlogical.terminal","name":"{name}","payload":{payload}}}}}}}}}"#
        )
    }

    /// As captured from Rex 0.1.0-preview.21.
    #[test]
    fn a_status_report_is_read_as_its_state() {
        let record = |s: &str| {
            format!(
                r#"{{"id":"","record":{{"state":"{s}","app":"pma","owner":{{"pid":1,"name":"bash"}}}},"reason":"report"}}"#
            )
        };
        let heard = |s: &str| hear(&block_event(B, "program_status_changed", &record(s)), B);
        assert_eq!(heard("working"), Heard::Status("working".into()));
        assert_eq!(heard("blocked"), Heard::Status("blocked".into()));
        assert_eq!(heard("error"), Heard::Status("done".into()));
        assert_eq!(heard("clear"), Heard::Nothing);
        let other = block_event("block:2", "program_status_changed", &record("blocked"));
        assert_eq!(hear(&other, B), Heard::Nothing, "another block");
    }

    #[test]
    fn an_exit_or_a_close_is_closed() {
        let exited = block_event(
            B,
            "child_exited",
            r#"{"exit_code":3,"error":"exit status 3"}"#,
        );
        assert_eq!(hear(&exited, B), Heard::Closed);
        let closed = format!(
            r#"{{"type":"session_event","payload":{{"session_id":"session:1","event":{{"type":"block_closed","payload":{{"block_id":"{B}"}}}}}}}}"#
        );
        assert_eq!(hear(&closed, B), Heard::Closed);
        let changed = block_event(B, "process_changed", r#"{"generation":2}"#);
        assert_eq!(hear(&changed, B), Heard::Nothing);
        assert_eq!(hear("Terminated", B), Heard::Nothing);
    }

    #[test]
    fn a_pane_names_its_session_and_block() {
        let pane = "rex:session:01a2/block:01b3";
        assert!(is_rex(pane));
        assert!(!is_rex("hc:t1c"));
        assert!(!is_rex("w1:p2"));
        assert_eq!(parts(pane), ("session:01a2", "block:01b3"));
    }

    #[test]
    fn listings_are_read() {
        let sessions: Value = serde_json::from_str(
            r#"{"sessions":[{"session_id":"session:1","label":"wispy-orchard"},
                {"session_id":"session:2","label":"pma: alpha"}]}"#,
        )
        .unwrap();
        assert_eq!(
            session_of(&sessions, "pma: alpha").as_deref(),
            Some("session:2")
        );
        assert_eq!(session_of(&sessions, "pma: beta"), None);
        let clients: Value = serde_json::from_str(
            r#"{"clients":[{"client_id":"client:a","info":{"kind":"app"}},
                {"client_id":"client:b"},{"client_id":"client:c","info":{"kind":"cli"}}]}"#,
        )
        .unwrap();
        assert_eq!(apps(&clients), ["client:a"]);
    }

    #[test]
    fn a_run_is_placed_by_the_setting() {
        assert_eq!(
            placement(RexSessions::Single, "alpha", "#4 add a greeting"),
            ("pma".into(), "alpha: #4 add a greeting".into())
        );
        assert_eq!(
            placement(RexSessions::Project, "alpha", "#4 add a greeting"),
            ("pma: alpha".into(), "#4 add a greeting".into())
        );
        let long = "x".repeat(50);
        let (_, tab) = placement(RexSessions::Single, "alpha", &long);
        assert_eq!(
            tab,
            format!("alpha: {}", "x".repeat(40)),
            "the task is cut, not the project"
        );
    }

    #[test]
    fn the_cli_error_line_is_kept() {
        let err = "Error: Rex.app is not accepting actions. Turn on Remote Control.\n";
        assert_eq!(
            error_of(err),
            "Rex.app is not accepting actions. Turn on Remote Control."
        );
        assert_eq!(error_of(" plain \n"), "plain");
    }

    /// The wrapper reports `done` or `error` from the status file it reads.
    #[test]
    fn the_wrapper_reports_the_script_status() {
        let dir = std::env::temp_dir().join(format!("pma-rex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("run.sh");
        let status = dir.join("run.status");
        for (code, state) in [("0", "done"), ("1", "error")] {
            std::fs::write(&script, format!("echo {code} > '{}'\n", status.display())).unwrap();
            let out = Command::new("sh")
                .args(["-c", REPORTED, "sh"])
                .arg(&script)
                .arg(&status)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(
                text.starts_with("\x1b]7501;state=working:app=pma\x1b\\"),
                "{text:?}"
            );
            assert!(
                text.ends_with(&format!("\x1b]7501;state={state}:app=pma\x1b\\")),
                "{text:?}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Against a live server: `cargo test -p pma-core -- --ignored rex`.
    #[test]
    #[ignore]
    fn rex_live_round_trip() {
        check().expect("a Rex server");
        let argv = ["sh".into(), "-c".into(), "exit 0".into()];
        let pane = start(
            RexSessions::Project,
            "live-probe",
            "probe",
            Path::new("/tmp"),
            &argv,
            Show::Back,
        )
        .unwrap();
        assert!(is_rex(&pane), "{pane}");
        let (closed, stop) = (AtomicBool::new(false), AtomicBool::new(false));
        std::thread::scope(|s| {
            s.spawn(|| follow(&pane, &closed, &stop, &|_| {}));
            std::thread::sleep(Duration::from_secs(3));
            assert!(
                !closed.load(Ordering::Relaxed),
                "a shell stays after the program"
            );
            close(&pane);
            for _ in 0..20 {
                if closed.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            stop.store(true, Ordering::Relaxed);
        });
        assert!(closed.load(Ordering::Relaxed), "closing was heard");
        assert_eq!(exists(parts(&pane).1), Ok(false));
        let _ = rex(&[], &["kill", parts(&pane).0]);
    }

    /// The shared session may hold the person's runs: only this test's tab is
    /// closed, and the session only if this test made it.
    #[test]
    #[ignore]
    fn rex_live_single_session() {
        check().expect("a Rex server");
        let had = session_of(&rex(&[], &["ls", "--json"]).unwrap(), "pma");
        let argv = ["sh".into(), "-c".into(), "exit 0".into()];
        let pane = start(
            RexSessions::Single,
            "live-probe",
            "probe",
            Path::new("/tmp"),
            &argv,
            Show::Back,
        )
        .unwrap();
        let (session, block) = parts(&pane);
        let inspected = rex(&[], &["block", "inspect", block, "--json"]).unwrap();
        let window = inspected["block"]["window_id"]
            .as_str()
            .unwrap()
            .to_string();
        let view = rex(&[], &["session", "inspect", session, "--json"]).unwrap();
        let tab = view["windows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["window_id"] == window.as_str())
            .map(|w| w["label"].clone());
        assert_eq!(view["label"], "pma");
        assert_eq!(tab, Some("live-probe: probe".into()));
        close(&pane);
        if had.is_none() {
            let _ = rex(&[], &["kill", session]);
        } else {
            assert_eq!(
                had.as_deref(),
                Some(session),
                "the existing session is reused"
            );
        }
    }
}
