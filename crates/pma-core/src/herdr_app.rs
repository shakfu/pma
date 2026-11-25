//! Runs agents in Herdr.app: `pma integrate herdr-app`. One space per project,
//! one tab per run. The app arranges spaces and tabs (`herdr-app do`);
//! herdr-core (`hc`) runs the terminals. Both are driven through their CLIs,
//! which find their own sockets. Design: docs/dev/herdr-integration.md.
//!
//! A pane is stored as `hc:<address>`, so a run started under one integration
//! is still closed and focused by the right one after the setting changes.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use serde_json::Value;

const PREFIX: &str = "hc:";
const READ_TICK: Duration = Duration::from_secs(1);
/// As for herdr: this many failed reconnects, two seconds apart, mean the core
/// has gone, and its terminals with it.
const SERVER_GONE: u32 = 15;

/// Whether `pane` names an herdr-core terminal rather than a herdr pane.
pub fn is_app(pane: &str) -> bool {
    pane.starts_with(PREFIX)
}

fn address(pane: &str) -> &str {
    pane.strip_prefix(PREFIX).unwrap_or(pane)
}

/// `{"error": "..."}` from `herdr-app`, `{"error": {"message": ...}}` from `hc`.
fn error_of(v: &Value) -> Option<String> {
    let e = &v["error"];
    e.as_str()
        .or_else(|| e["message"].as_str())
        .map(String::from)
}

fn json_of(program: &str, args: &[&str]) -> Result<Value, String> {
    let what = format!("{program} {}", args.first().unwrap_or(&""));
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("{what}: {e}"))?;
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    if let Some(message) = error_of(&v) {
        return Err(message);
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("{what}: {}", err.trim()));
    }
    Ok(v)
}

fn hc(args: &[&str]) -> Result<Value, String> {
    json_of("hc", args)
}

/// `herdr-app ls` and `status` talk to the open window and never open one.
fn window(args: &[&str]) -> Result<Value, String> {
    json_of("herdr-app", args)
}

fn act(command: &str, args: &[&str]) -> Result<Value, String> {
    let mut all = vec!["do", command];
    all.extend_from_slice(args);
    window(&all)
}

/// What answers: the window, or only the core, which runs terminals unseen.
#[derive(Debug, PartialEq)]
pub enum Reach {
    Window,
    CoreOnly,
}

/// Whether the window is open; this never opens it or starts a core.
pub fn window_open() -> bool {
    window(&["status"]).is_ok()
}

pub fn check() -> Result<Reach, String> {
    if window(&["status"]).is_ok() {
        return Ok(Reach::Window);
    }
    hc(&["server", "status"])
        .map(|_| Reach::CoreOnly)
        .map_err(|e| format!("no herdr-core: {e}"))
}

/// Where the open window shows `address`: its pane, if it has one.
fn pane_of(listed: &Value, address: &str) -> Option<String> {
    listed["spaces"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| s["tabs"].as_array().into_iter().flatten())
        .flat_map(|t| t["terminals"].as_array().into_iter().flatten())
        .find(|t| t["terminal"] == address)
        .and_then(|t| t["pane"].as_str().map(String::from))
}

/// `listed`'s space with id `id`.
fn space_of<'a>(listed: &'a Value, id: &str) -> Option<&'a Value> {
    listed["spaces"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["id"] == id)
}

/// A space id for `project`, the same on every run: `pma-` and the name, with
/// each byte outside `a-z0-9` written as `-` and two hex digits, so two
/// projects never share one.
fn space_id(project: &str) -> String {
    let mut id = String::from("pma-");
    for b in project.bytes() {
        match b {
            b'a'..=b'z' | b'0'..=b'9' => id.push(b as char),
            _ => id.push_str(&format!("-{b:02x}")),
        }
    }
    id
}

/// A tab id no other run has.
fn tab_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("pma-{nanos:x}-{:x}", std::process::id())
}

/// `argv` as the app runs it: in a login shell, which stays once it ends.
/// The words go to the shell as `"$@"`, so only Bourne-style shells are used.
pub(crate) fn wrapped(argv: &[String], shell: Option<&str>) -> Vec<String> {
    if argv.is_empty() {
        return Vec::new();
    }
    let bourne = |s: &&str| {
        let name = s.rsplit('/').next().unwrap_or(s);
        matches!(name, "zsh" | "bash" | "ksh" | "sh")
    };
    let shell = shell.filter(bourne).unwrap_or("/bin/sh").to_string();
    let mut all = vec![
        shell.clone(),
        "-lic".into(),
        r#""$@"; exec "$0" -li"#.into(),
        shell,
    ];
    all.extend_from_slice(argv);
    all
}

/// Whether Herdr named the space after its folder, as it does one nobody named.
fn unnamed(space: &Value) -> bool {
    let folder = space["folder"].as_str().unwrap_or("");
    let base = folder.rsplit('/').next().unwrap_or("");
    let name = space["name"].as_str().unwrap_or("");
    name == base || name == "~"
}

/// The window's listing once it shows `address`, polling briefly: the app
/// places a labelled terminal a moment after it starts.
fn placed(address: &str) -> Option<Value> {
    for _ in 0..25 {
        if let Ok(listed) = window(&["ls"])
            && pane_of(&listed, address).is_some()
        {
            return Some(listed);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    None
}

/// How a new terminal is shown.
#[derive(Clone, Copy, PartialEq)]
pub enum Show {
    /// In front: a person asked for it.
    Front,
    /// In a tab, leaving the person's view as it was.
    Back,
    /// As `Back`, or, with the app quit, placed when it next starts.
    Anywhere,
}

/// Starts `argv` (a shell when empty) in a new tab of `project`'s space and
/// returns its pane. `hc` starts it labelled with the space and tab, which the
/// app places, making either if need be, now or when it next starts.
pub fn start(
    project: &str,
    title: &str,
    cwd: &Path,
    argv: &[String],
    show: Show,
) -> Result<String, String> {
    let window_up = window(&["status"]).is_ok();
    if !window_up && show != Show::Anywhere {
        return Err("Herdr.app is not running: start it and try again".into());
    }
    let title: String = title.chars().take(40).collect();
    let cwd = cwd.to_string_lossy();
    let space = space_id(project);
    let tab = tab_id();
    let labels = [
        format!("herdr.space={space}"),
        format!("herdr.tab={tab}"),
        format!("pma={title}"),
    ];
    // No parent: from inside a terminal, `hc` would type notes about the run
    // into the one that started `pma`.
    let mut args = vec!["run", "--bg", "--no-parent", "--no-notify", "--cwd", &cwd];
    for label in &labels {
        args.extend(["--label", label]);
    }
    let argv = wrapped(argv, std::env::var("SHELL").ok().as_deref());
    if !argv.is_empty() {
        args.push("--");
        args.extend(argv.iter().map(String::as_str));
    }
    let ran = hc(&args)?;
    let address = ran["address"]
        .as_str()
        .ok_or("hc ran it and named no terminal")?;
    if window_up && let Some(listed) = placed(address) {
        let _ = act("tab.rename", &["--id", &tab, "--name", &title]);
        if space_of(&listed, &space).is_some_and(unnamed) {
            let name = format!("pma: {project}");
            let _ = act("space.rename", &["--id", &space, "--name", &name]);
        }
        if show == Show::Front {
            act("pane.focus", &["--terminal", address])?;
        }
    }
    Ok(format!("{PREFIX}{address}"))
}

/// Closes a pane and ends its terminal. It may be gone already.
pub fn close(pane: &str) {
    let address = address(pane);
    let shown = window(&["ls"])
        .ok()
        .and_then(|listed| pane_of(&listed, address));
    let closed = shown.is_some_and(|p| act("pane.close", &["--pane", &p]).is_ok());
    if !closed {
        let _ = hc(&["kill", address]);
    }
}

/// Brings a terminal to the front, giving it a tab if it ran unseen: one
/// started before runs were labelled with their space.
pub fn focus(pane: &str) -> Result<(), String> {
    let address = address(pane);
    if pane_of(&window(&["ls"])?, address).is_none() {
        act("terminal.keep", &["--terminal", address, "--place", "tab"])?;
    }
    act("pane.focus", &["--terminal", address]).map(|_| ())
}

/// What one line of `hc events` says about a terminal.
#[derive(Debug, PartialEq)]
enum Heard {
    Closed,
    Status(String),
    Nothing,
}

/// The first line is a snapshot of matching terminals; after it, `event`
/// notifications. A subscriber that fell behind gets `missed`, then a fresh
/// snapshot, which this reads as it reads the first.
///
/// A kept terminal whose program dies (`lost`) is `restarted` as a shell at
/// the same address and stays live, so either event, or a snapshot with a pid
/// other than the first one seen (`pid`), means the run's program has gone.
fn hear(line: &str, address: &str, pid: &mut Option<u64>) -> Heard {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
        return Heard::Nothing;
    };
    let snapshot = v["terminals"]
        .as_array()
        .or_else(|| v["params"]["terminals"].as_array());
    if let Some(terminals) = snapshot {
        return match terminals.iter().find(|t| t["address"] == address) {
            None => Heard::Closed,
            Some(t) if t["state"] != "live" || replaced(t, pid) => Heard::Closed,
            Some(t) => t["status"]
                .as_str()
                .map_or(Heard::Nothing, |s| Heard::Status(s.to_string())),
        };
    }
    let p = &v["params"];
    if p["terminal"] != address {
        return Heard::Nothing;
    }
    match p["type"].as_str() {
        Some("finished" | "removed" | "lost" | "restarted") => Heard::Closed,
        Some("status") => {
            let d = &p["data"];
            d["status"]
                .as_str()
                .or_else(|| d["state"].as_str())
                .map_or(Heard::Nothing, |s| Heard::Status(s.to_string()))
        }
        _ => Heard::Nothing,
    }
}

/// Whether `t` runs another process than the first one seen, which it records.
fn replaced(t: &Value, pid: &mut Option<u64>) -> bool {
    let now = t["pid"].as_u64();
    match *pid {
        Some(first) => now.is_some_and(|n| n != first),
        None => {
            *pid = now;
            false
        }
    }
}

struct Events(Child);

impl Drop for Events {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Follows `pane` until `stop`, setting `closed` once its terminal has ended
/// and passing each change of state to `on_state`.
pub fn follow(pane: &str, closed: &AtomicBool, stop: &AtomicBool, on_state: &dyn Fn(&str)) {
    let address = address(pane);
    let mut failures = 0;
    let mut last = String::new();
    let mut pid = None;
    while !stop.load(Ordering::Relaxed) {
        match subscribe_once(address, closed, stop, on_state, &mut last, &mut pid) {
            Ok(()) => failures = 0,
            Err(_) => failures += 1,
        }
        if closed.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
            return;
        }
        let gone = match hc(&["get", address]) {
            Ok(t) => t["state"] != "live" || replaced(&t, &mut pid),
            Err(e) => e.contains("no terminal") || failures >= SERVER_GONE,
        };
        if gone {
            closed.store(true, Ordering::Relaxed);
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn subscribe_once(
    address: &str,
    closed: &AtomicBool,
    stop: &AtomicBool,
    on_state: &dyn Fn(&str),
    last: &mut String,
    pid: &mut Option<u64>,
) -> Result<(), String> {
    let mut child = Command::new("hc")
        .args(["events", "--terminal", address])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("hc events: {e}"))?;
    let stdout = child.stdout.take().ok_or("hc events: no output")?;
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
            Err(RecvTimeoutError::Disconnected) => return Err("hc events ended".into()),
        };
        match hear(&line, address, pid) {
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

    /// As captured from herdr-core 0.1.20.
    #[test]
    fn events_are_read_as_hc_sends_them() {
        let finished = r#"{"jsonrpc":"2.0","method":"event","params":{"data":{"exit_code":1,"finished_ms":1,"reason":"killed","signal":null},"seq":816,"terminal":"t1f","type":"finished"}}"#;
        assert_eq!(hear(finished, "t1f", &mut None), Heard::Closed);
        assert_eq!(
            hear(finished, "t1d", &mut None),
            Heard::Nothing,
            "another terminal"
        );
        let labels = r#"{"jsonrpc":"2.0","method":"event","params":{"data":{"labels":{}},"seq":812,"terminal":"t1f","type":"labels"}}"#;
        assert_eq!(hear(labels, "t1f", &mut None), Heard::Nothing);
        let status = r#"{"jsonrpc":"2.0","method":"event","params":{"data":{"status":"blocked"},"seq":9,"terminal":"t1f","type":"status"}}"#;
        assert_eq!(
            hear(status, "t1f", &mut None),
            Heard::Status("blocked".into())
        );
        let record = r#"{"jsonrpc":"2.0","method":"event","params":{"data":{"state":"working"},"seq":9,"terminal":"t1f","type":"status"}}"#;
        assert_eq!(
            hear(record, "t1f", &mut None),
            Heard::Status("working".into())
        );
    }

    /// A snapshot opens each subscription, and a fresh one follows `missed`.
    #[test]
    fn a_snapshot_says_whether_the_terminal_still_runs() {
        let live = r#"{"channel":null,"seq":811,"terminals":[{"address":"t1f","state":"live","status":"working"}]}"#;
        assert_eq!(
            hear(live, "t1f", &mut None),
            Heard::Status("working".into())
        );
        let ended =
            r#"{"seq":811,"terminals":[{"address":"t1f","state":"finished","status":null}]}"#;
        assert_eq!(hear(ended, "t1f", &mut None), Heard::Closed);
        let absent = r#"{"seq":811,"terminals":[]}"#;
        assert_eq!(hear(absent, "t1f", &mut None), Heard::Closed);
    }

    /// As captured from herdr-core 0.1.21 after `kill -9` of a kept terminal.
    #[test]
    fn a_kept_terminal_that_lost_its_program_is_closed() {
        let lost = r#"{"jsonrpc":"2.0","method":"event","params":{"data":{"exit_code":null,"finished_ms":null,"reason":"signaled","signal":9},"seq":150,"terminal":"t2t","type":"lost"}}"#;
        assert_eq!(hear(lost, "t2t", &mut None), Heard::Closed);
        let restarted = r#"{"jsonrpc":"2.0","method":"event","params":{"data":{"address":"t2t","argv":["/bin/zsh","-l"],"keep":true,"pid":3605,"state":"live"},"seq":151,"terminal":"t2t","type":"restarted"}}"#;
        assert_eq!(hear(restarted, "t2t", &mut None), Heard::Closed);
    }

    /// A loss missed while reconnecting shows only as another pid.
    #[test]
    fn a_new_pid_in_a_later_snapshot_is_closed() {
        let at = |pid: u32| {
            format!(
                r#"{{"seq":1,"terminals":[{{"address":"t2t","state":"live","pid":{pid},"status":"working"}}]}}"#
            )
        };
        let mut pid = None;
        assert_eq!(
            hear(&at(3467), "t2t", &mut pid),
            Heard::Status("working".into())
        );
        assert_eq!(pid, Some(3467));
        assert_eq!(
            hear(&at(3467), "t2t", &mut pid),
            Heard::Status("working".into())
        );
        assert_eq!(hear(&at(3605), "t2t", &mut pid), Heard::Closed);
    }

    #[test]
    fn a_pane_names_its_backend() {
        assert!(is_app("hc:t1c"));
        assert!(!is_app("w1:p2"));
        assert_eq!(address("hc:rohan/t7"), "rohan/t7");
    }

    #[test]
    fn the_window_lists_where_a_terminal_is() {
        let listed: Value = serde_json::from_str(
            r#"{"spaces":[{"id":"sp1","name":"pma: alpha","tabs":[{"id":"tb1","terminals":[
                {"terminal":"t6","pane":"p3"}]}]}]}"#,
        )
        .unwrap();
        assert_eq!(pane_of(&listed, "t6").as_deref(), Some("p3"));
        assert_eq!(pane_of(&listed, "t7"), None);
        assert_eq!(space_of(&listed, "sp1").unwrap()["name"], "pma: alpha");
        assert!(space_of(&listed, "sp2").is_none());
    }

    #[test]
    fn a_space_id_is_stable_and_distinct_per_project() {
        assert_eq!(space_id("alpha"), "pma-alpha");
        assert_eq!(space_id("my-app"), "pma-my-2dapp");
        assert_eq!(space_id("My_App"), "pma--4dy-5f-41pp");
        assert_ne!(space_id("a-b"), space_id("a_b"));
        assert_ne!(space_id("a-2d"), space_id("a--2d"));
    }

    /// As measured: the app runs argv as `zsh -lic '"$@"; exec "$0" -li' zsh ...`.
    #[test]
    fn argv_is_wrapped_as_the_app_wraps_it() {
        let argv = ["sh".to_string(), "/x/run.sh".to_string()];
        assert_eq!(
            wrapped(&argv, Some("/bin/zsh")),
            [
                "/bin/zsh",
                "-lic",
                r#""$@"; exec "$0" -li"#,
                "/bin/zsh",
                "sh",
                "/x/run.sh"
            ]
        );
        assert_eq!(wrapped(&argv, Some("/opt/homebrew/bin/fish"))[0], "/bin/sh");
        assert_eq!(wrapped(&argv, None)[0], "/bin/sh");
        assert!(
            wrapped(&[], Some("/bin/zsh")).is_empty(),
            "the core's own shell"
        );
    }

    #[test]
    fn a_space_named_after_its_folder_is_unnamed() {
        let space = |name: &str, folder: &str| serde_json::json!({"name": name, "folder": folder});
        assert!(unnamed(&space("tmp", "/private/tmp")));
        assert!(unnamed(&space("~", "/Users/sa")));
        assert!(!unnamed(&space("pma: alpha", "/w/alpha")));
        assert!(!unnamed(&space("mine", "/w/alpha")));
    }

    #[test]
    fn both_error_shapes_are_read() {
        let app: Value = serde_json::from_str(r#"{"error":"no such pane"}"#).unwrap();
        assert_eq!(error_of(&app).as_deref(), Some("no such pane"));
        let core: Value =
            serde_json::from_str(r#"{"error":{"kind":"not_found","message":"no terminal \"x\""}}"#)
                .unwrap();
        assert_eq!(error_of(&core).as_deref(), Some("no terminal \"x\""));
        assert_eq!(error_of(&serde_json::json!({"ok": true})), None);
    }

    /// Against the open window: `cargo test -p pma-core -- --ignored herdr_app`.
    #[test]
    #[ignore]
    fn herdr_app_live_round_trip() {
        let before = window(&["status"]).expect("open the window")["tab"].clone();
        let argv = ["sh".into(), "-c".into(), "exit 0".into()];
        let pane = start("live-probe", "probe", Path::new("/tmp"), &argv, Show::Back).unwrap();
        assert!(is_app(&pane), "{pane}");
        assert_eq!(window(&["status"]).unwrap()["tab"], before, "view kept");
        let listed = window(&["ls"]).unwrap();
        let space = space_of(&listed, &space_id("live-probe")).expect("placed by label");
        assert_eq!(space["name"], "pma: live-probe");
        focus(&pane).unwrap();
        assert_ne!(
            window(&["status"]).unwrap()["tab"],
            before,
            "focus shows it"
        );
        let _ = act("tab.select", &["--id", before.as_str().unwrap()]);
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
        let space = space_id("live-probe");
        let _ = act("space.close", &["--id", &space, "--confirmed", "true"]);
        let _ = hc(&["rm", address(&pane)]);
    }
}
