//! Runs agents in herdr panes: `pma integrate herdr`. One workspace per
//! project, one tab per run. Design: docs/dev/herdr-integration.md.
//!
//! A pane holds a terminal, not a child of `pma`, so the agent is started by a
//! generated script that applies the environment `agent::restrict` built and
//! leaves its exit status in a file: herdr reports no exit status for a command
//! typed into a pane's shell. Everything else is the socket API, newline
//! delimited JSON. The server answers one request per connection; a
//! subscription keeps its connection and streams events.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::agent::{self, Finished};
use crate::config::{Config, Integration};

/// How long a request may take; a subscription polls its stop flag this often.
const REQUEST: Duration = Duration::from_secs(10);
const READ_TICK: Duration = Duration::from_secs(1);
/// How many failed reconnects, two seconds apart, mean the server has gone,
/// and the terminals it holds with it. Long enough for a live handoff.
const SERVER_GONE: u32 = 15;

/// herdr's own lookup, for this process's environment. A named session outside
/// a pane is not handled: its socket's path is herdr's to choose.
fn socket() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("HERDR_SOCKET_PATH") {
        return Ok(path.into());
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))
        .ok_or("neither HERDR_SOCKET_PATH nor HOME is set")?;
    Ok(config.join("herdr").join("herdr.sock"))
}

fn connect() -> Result<UnixStream, String> {
    let path = socket()?;
    UnixStream::connect(&path).map_err(|e| format!("no herdr server at {}: {e}", path.display()))
}

fn send(stream: &mut UnixStream, method: &str, params: &Value) -> Result<(), String> {
    let request = json!({ "id": "pma", "method": method, "params": params });
    writeln!(stream, "{request}").map_err(|e| format!("herdr {method}: {e}"))
}

/// One request, returning its `result`, or what went wrong.
fn call(method: &str, params: Value) -> Result<Value, String> {
    let mut stream = connect()?;
    stream
        .set_read_timeout(Some(REQUEST))
        .map_err(|e| e.to_string())?;
    send(&mut stream, method, &params)?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| format!("herdr {method}: {e}"))?;
    let value: Value =
        serde_json::from_str(line.trim()).map_err(|e| format!("herdr {method}: {e}"))?;
    match value["error"]["message"].as_str() {
        Some(message) => Err(message.to_string()),
        None => Ok(value["result"].clone()),
    }
}

/// Whether a herdr server answers, or why not.
pub fn check() -> Result<(), String> {
    call("ping", json!({})).map(|_| ())
}

/// A new tab for `title` in `project`'s workspace, made if need be; its pane.
pub fn open(project: &str, title: &str, cwd: &Path) -> Result<String, String> {
    let label = format!("pma: {project}");
    let cwd = cwd.to_string_lossy();
    let listed = call("workspace.list", json!({}))?;
    let found = listed["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|w| w["label"] == label.as_str())
        .and_then(|w| w["workspace_id"].as_str().map(String::from));
    let title: String = title.chars().take(40).collect();
    let workspace = match found {
        Some(id) => id,
        None => {
            let made = call(
                "workspace.create",
                json!({ "label": label, "cwd": cwd, "focus": false }),
            )?;
            // A new workspace comes with a tab, which this run uses: a second
            // would leave the first as an idle shell.
            let _ = call(
                "tab.rename",
                json!({ "tab_id": made["tab"]["tab_id"], "label": title }),
            );
            return made["root_pane"]["pane_id"]
                .as_str()
                .map(String::from)
                .ok_or_else(|| "herdr made a workspace and named no pane".to_string());
        }
    };
    let tab = call(
        "tab.create",
        json!({ "workspace_id": workspace, "label": title, "cwd": cwd, "focus": false }),
    )?;
    tab["root_pane"]["pane_id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| "herdr made a tab and named no pane".to_string())
}

/// Closes a pane, and whatever runs in it. It may be gone already.
pub fn close(pane: &str) {
    let _ = call("pane.close", json!({ "pane_id": pane }));
}

/// Types a line into a pane's shell and presses Enter.
fn type_line(pane: &str, line: &str) -> Result<(), String> {
    call(
        "pane.send_input",
        json!({ "pane_id": pane, "text": line, "keys": ["Enter"] }),
    )
    .map(|_| ())
}

/// Brings a pane's tab to the front.
pub fn focus(pane: &str) -> Result<(), String> {
    let got =
        call("pane.get", json!({ "pane_id": pane })).map_err(|e| format!("pane {pane}: {e}"))?;
    let tab = got["pane"]["tab_id"]
        .as_str()
        .ok_or_else(|| format!("pane {pane} names no tab"))?;
    call("tab.focus", json!({ "tab_id": tab })).map(|_| ())
}

/// Waits until `pane` has closed, or the server holding it has gone.
pub fn wait_closed(pane: &str) {
    let (closed, stop) = (AtomicBool::new(false), AtomicBool::new(false));
    follow(pane, &closed, &stop, &|_| {});
}

/// Opens `cmd`, an interactive agent, on a run's directory, in front. A
/// person is at its keyboard, so it has their environment rather than
/// `restrict`'s; only what the worker sets itself is added.
pub fn take_over(project: &str, title: &str, cwd: &Path, cmd: &Command) -> Result<String, String> {
    let pane = open(project, title, cwd)?;
    type_line(&pane, &command_line(cmd))?;
    focus(&pane)?;
    Ok(pane)
}

/// `cmd` as one `sh` line: its set variables, its program, then its words.
fn command_line(cmd: &Command) -> String {
    let mut words: Vec<String> = cmd
        .get_envs()
        .filter_map(|(k, v)| {
            v.map(|v| format!("{}={}", k.to_string_lossy(), quote(&v.to_string_lossy())))
        })
        .collect();
    words.push(quote(&cmd.get_program().to_string_lossy()));
    words.extend(cmd.get_args().map(|a| quote(&a.to_string_lossy())));
    words.join(" ")
}

/// `idle`, not `done`: herdr refuses a reported `done` and infers it itself.
fn report(pane: &str, agent: &str, state: &str) {
    let _ = call(
        "pane.report_agent",
        json!({ "pane_id": pane, "source": "custom:pma", "agent": agent, "state": state }),
    );
}

/// What one line of a subscription says about `pane`.
#[derive(Debug, PartialEq)]
enum Heard {
    Closed,
    Status(String),
    /// The server dropped this subscription; what it held is stale.
    Lost,
    Nothing,
}

/// A status event is named as subscribed, `pane.agent_status_changed`; a
/// closed one is `pane_closed`, though it is subscribed as `pane.closed`.
fn hear(line: &str, pane: &str) -> Heard {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
        return Heard::Nothing;
    };
    if v["error"]["code"] == "events_lost" {
        return Heard::Lost;
    }
    if v["data"]["pane_id"] != pane {
        return Heard::Nothing;
    }
    match v["event"].as_str() {
        Some("pane_closed" | "pane.closed" | "pane_exited" | "pane.exited") => Heard::Closed,
        Some("pane.agent_status_changed" | "pane_agent_status_changed") => {
            v["data"]["agent_status"]
                .as_str()
                .map_or(Heard::Nothing, |s| Heard::Status(s.to_string()))
        }
        _ => Heard::Nothing,
    }
}

/// Follows `pane` until `stop`, setting `closed` once it has gone and passing
/// each change of state to `on_state`. Per the API, a lost or dropped
/// subscription is followed by a fresh read rather than a replay.
fn follow(pane: &str, closed: &AtomicBool, stop: &AtomicBool, on_state: &dyn Fn(&str)) {
    let mut failures = 0;
    let mut last = String::new();
    while !stop.load(Ordering::Relaxed) {
        match subscribe_once(pane, closed, stop, on_state, &mut last) {
            Ok(()) => failures = 0,
            Err(_) => failures += 1,
        }
        if closed.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
            return;
        }
        match call("pane.get", json!({ "pane_id": pane })) {
            Err(e) if e.contains("not found") => {
                closed.store(true, Ordering::Relaxed);
                return;
            }
            Err(_) if failures >= SERVER_GONE => {
                closed.store(true, Ordering::Relaxed);
                return;
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn subscribe_once(
    pane: &str,
    closed: &AtomicBool,
    stop: &AtomicBool,
    on_state: &dyn Fn(&str),
    last: &mut String,
) -> Result<(), String> {
    let mut stream = connect()?;
    stream
        .set_read_timeout(Some(READ_TICK))
        .map_err(|e| e.to_string())?;
    send(
        &mut stream,
        "events.subscribe",
        &json!({ "subscriptions": [
            { "type": "pane.agent_status_changed", "pane_id": pane },
            { "type": "pane.closed" },
            { "type": "pane.exited" },
        ]}),
    )?;
    let mut reader = BufReader::new(stream);
    // Kept across read timeouts: a timeout can land inside a line.
    let mut line = String::new();
    while !stop.load(Ordering::Relaxed) {
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e.to_string()),
        }
        match hear(&line, pane) {
            Heard::Closed => {
                closed.store(true, Ordering::Relaxed);
                return Ok(());
            }
            Heard::Status(state) if state != *last => {
                on_state(&state);
                *last = state;
            }
            Heard::Lost => return Ok(()),
            _ => {}
        }
        line.clear();
    }
    Ok(())
}

/// Tells whoever is at the terminal, in herdr's own toast.
fn notify(title: &str, body: &str) {
    let _ = call("notification.show", json!({ "title": title, "body": body }));
}

/// Quotes one word for `sh`.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The script a pane runs. `view` renders a headless agent's event stream;
/// the log keeps it raw, for `pma watch` and the report. An interactive agent
/// needs the terminal, so it has neither.
/// Variables a script may not hold, as it stays on disk: `minosb`'s password.
/// They go to a file of their own that the script reads and removes.
const SECRETS: [&str; 1] = [crate::minos::PASSWORD];

/// The `export` lines for the secrets `cmd` sets.
fn secrets(cmd: &Command) -> String {
    cmd.get_envs()
        .filter(|(k, _)| SECRETS.iter().any(|s| *k == *s))
        .filter_map(|(k, v)| {
            v.map(|v| {
                format!(
                    "export {}={}\n",
                    k.to_string_lossy(),
                    quote(&v.to_string_lossy())
                )
            })
        })
        .collect()
}

fn secrets_file(status: &Path) -> PathBuf {
    status.with_extension("env")
}

fn script(cmd: &Command, log: &Path, status: &Path, view: Option<&Path>) -> String {
    let path = |p: &Path| quote(&p.to_string_lossy());
    let mut out = String::from("#!/bin/sh\n");
    // A pane has its shell's environment, not this process's. What decides which
    // agent runs and how git behaves comes along; nothing else, since this
    // is a file and an inherited provider key would land in it.
    let set: Vec<_> = cmd.get_envs().map(|(k, _)| k.to_os_string()).collect();
    for (key, value) in std::env::vars_os() {
        let name = key.to_string_lossy();
        let carried = name == "PATH" || name.starts_with("GIT_") || name.starts_with("PMA_");
        if carried && !set.contains(&key) {
            out.push_str(&format!(
                "export {name}={}\n",
                quote(&value.to_string_lossy())
            ));
        }
    }
    for (key, value) in cmd.get_envs() {
        let key = key.to_string_lossy();
        match value {
            Some(_) if SECRETS.contains(&key.as_ref()) => {}
            Some(v) => out.push_str(&format!("export {key}={}\n", quote(&v.to_string_lossy()))),
            None => out.push_str(&format!("unset {key}\n")),
        }
    }
    if !secrets(cmd).is_empty() {
        let file = path(&secrets_file(status));
        out.push_str(&format!(". {file}; rm -f {file}\n"));
    }
    if let Some(dir) = cmd.get_current_dir() {
        out.push_str(&format!("cd {} || exit 1\n", path(dir)));
    }
    let mut words = vec![quote(&cmd.get_program().to_string_lossy())];
    words.extend(cmd.get_args().map(|a| quote(&a.to_string_lossy())));
    let words = words.join(" ");
    let tmp = path(&status.with_extension("tmp"));
    let leave = format!("echo $? > {tmp}; mv {tmp} {}", path(status));
    match view {
        Some(view) => out.push_str(&format!(
            "( {words} </dev/null; {leave} ) 2>&1 | tee {} | {} pane-view\n",
            path(log),
            path(view)
        )),
        None => out.push_str(&format!("{words}\n{leave}\n")),
    }
    out
}

/// One line for a person, from one line of a headless agent's output.
pub fn view_line(line: &str) -> Option<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return Some(line.to_string());
    };
    match v["type"].as_str() {
        Some("assistant") => {
            let parts: Vec<String> = v["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| match p["type"].as_str() {
                    Some("text") => p["text"].as_str().map(String::from),
                    Some("tool_use") => {
                        let input = &p["input"];
                        let arg = ["command", "file_path", "pattern", "path", "url"]
                            .iter()
                            .find_map(|k| input[*k].as_str())
                            .unwrap_or_default();
                        Some(format!("> {} {arg}", p["name"].as_str().unwrap_or("?")))
                    }
                    _ => None,
                })
                .collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Some("result") => Some(format!(
            "\n{}\n[{}, ${:.2}]",
            v["result"].as_str().unwrap_or_default(),
            v["subtype"].as_str().unwrap_or("done"),
            v["total_cost_usd"].as_f64().unwrap_or(0.0)
        )),
        _ => None,
    }
}

fn pane_files(log: &Path) -> (PathBuf, PathBuf) {
    (log.with_extension("pane.sh"), log.with_extension("status"))
}

/// Closes the pane if `pma` dies, as the watchdog does for a child: a stopped
/// job must not leave its agent running. Its own process group, so what kills
/// the job leaves this to act. `pma pane-stop` runs `guard_until`.
fn guard(exe: &Path, pane: &str, status: &Path) -> std::io::Result<()> {
    let mut child = Command::new(exe)
        .arg("pane-stop")
        .arg(std::process::id().to_string())
        .arg(status)
        .arg(pane)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Waits until the status file exists or `owner` has gone, closing `pane` in
/// the second case.
pub fn guard_until(owner: u32, status: &Path, pane: &str) {
    let alive = || {
        Command::new("kill")
            .args(["-0", &owner.to_string()])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    while !status.exists() {
        if !alive() {
            close(pane);
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Runs `cmd` in `pane` and waits for it, up to `timeout` when it has one.
fn run_in(
    pane: &str,
    cmd: &Command,
    log: &Path,
    timeout: Option<Duration>,
    headless: bool,
    title: &str,
    on_state: impl Fn(&str) + Send + 'static,
) -> std::io::Result<Finished> {
    let started = Instant::now();
    let (path, status) = pane_files(log);
    let _ = std::fs::remove_file(&status);
    std::fs::File::create(log)?;
    let exe = std::env::current_exe()?;
    let text = script(cmd, log, &status, headless.then_some(exe.as_path()));
    let secret = secrets_file(&status);
    let _ = std::fs::remove_file(&secret);
    let held = secrets(cmd);
    if !held.is_empty() {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&secret)?
            .write_all(held.as_bytes())?;
    }
    // It can hold a worker's environment.
    {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o700)
            .open(&path)?
            .write_all(text.as_bytes())?;
    }
    guard(&exe, pane, &status)?;
    // Before the line is typed, so no event of this run is missed.
    let closed = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let follower = {
        let (closed, stop) = (closed.clone(), stop.clone());
        let (pane, title) = (pane.to_string(), title.to_string());
        std::thread::spawn(move || {
            follow(&pane, &closed, &stop, &|state| {
                on_state(state);
                if state == "blocked" {
                    notify("pma: an agent needs you", &title);
                }
            });
        })
    };
    let agent_name = cmd.get_program().to_string_lossy().into_owned();
    let line = format!("sh {}", quote(&path.to_string_lossy()));
    type_line(pane, &line).map_err(std::io::Error::other)?;
    if headless {
        report(pane, &agent_name, "working");
    }
    let end = |code: &str| std::fs::write(&status, code);
    let success = loop {
        if let Ok(code) = std::fs::read_to_string(&status) {
            break Some(code.trim() == "0");
        }
        if timeout.is_some_and(|t| started.elapsed() >= t) {
            close(pane);
            end("124")?;
            break None;
        }
        if closed.load(Ordering::Relaxed) {
            end("125")?;
            let mut file = std::fs::OpenOptions::new().append(true).open(log)?;
            writeln!(file, "the herdr pane closed before the agent finished")?;
            break Some(false);
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    // Read and removed by the script, unless the pane closed before it ran.
    let _ = std::fs::remove_file(&secret);
    stop.store(true, Ordering::Relaxed);
    let _ = follower.join();
    if headless && success.is_some() {
        report(pane, &agent_name, "idle");
    }
    Ok(Finished {
        success,
        seconds: started.elapsed().as_secs() as i64,
    })
}

/// What a run asks of the terminal it lands in.
pub struct Host<'a> {
    pub project: &'a str,
    pub title: &'a str,
    /// A person drives the agent, so it has no timeout, and no pane means no
    /// run.
    pub interactive: bool,
    /// Whether a run that succeeds closes its tab: a workflow's many nodes
    /// have no review to close them at.
    pub close_after: bool,
}

/// Runs an agent: in a herdr pane under `pma integrate herdr`, otherwise as a
/// child. `on_pane` hears which pane. With no server the run goes on as a
/// child and says so, so unattended work does not stop with the terminal.
pub fn host(
    cfg: &Config,
    host: Host,
    cmd: Command,
    log: &Path,
    timeout: Duration,
    on_pane: impl FnOnce(&str),
    on_state: impl Fn(&str) + Send + 'static,
) -> std::io::Result<Finished> {
    if cfg.integration != Integration::Herdr {
        if host.interactive {
            return Err(std::io::Error::other(
                "an interactive run needs `pma integrate herdr`",
            ));
        }
        return agent::run_limited(cmd, None, log, timeout);
    }
    let cwd = cmd
        .get_current_dir()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let pane = match open(host.project, host.title, &cwd) {
        Ok(pane) => pane,
        Err(e) if host.interactive => {
            return Err(std::io::Error::other(format!(
                "an interactive run needs herdr: {e}"
            )));
        }
        Err(e) => {
            eprintln!("warning: {e}; running `{}` without a pane", host.title);
            return agent::run_limited(cmd, None, log, timeout);
        }
    };
    on_pane(&pane);
    let finished = match host.interactive {
        true => {
            // From the command line the person is waiting on it, so it comes
            // to the front. A job came from the TUI or web, where the person
            // may be launching more; `H` brings it forward when they choose.
            if crate::jobs::current().is_none()
                && let Err(e) = focus(&pane)
            {
                eprintln!("warning: {e}");
            }
            run_in(&pane, &cmd, log, None, false, host.title, on_state)?
        }
        false => run_in(&pane, &cmd, log, Some(timeout), true, host.title, on_state)?,
    };
    if host.close_after && finished.success == Some(true) {
        close(&pane);
    }
    Ok(finished)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A take-over line is typed into the person's shell, so each word is
    /// quoted and only what the worker set is prefixed.
    #[test]
    fn a_take_over_line_quotes_each_word() {
        let mut cmd = Command::new("codex");
        cmd.args(["-C", "/w d", "it's done"])
            .env("BASE", "http://x")
            .env_remove("GH_TOKEN");
        assert_eq!(
            command_line(&cmd),
            r"BASE='http://x' 'codex' '-C' '/w d' 'it'\''s done'"
        );
    }

    /// As captured from herdr 0.9.5: the closed event's name differs from its
    /// subscription type.
    #[test]
    fn events_are_read_as_herdr_sends_them() {
        let status = r#"{"data":{"agent":"claude","agent_status":"blocked","pane_id":"w1:p2","workspace_id":"w1"},"event":"pane.agent_status_changed"}"#;
        assert_eq!(hear(status, "w1:p2"), Heard::Status("blocked".into()));
        assert_eq!(hear(status, "w1:p3"), Heard::Nothing, "another pane");
        let closed = r#"{"data":{"pane_id":"w1:p2","type":"pane_closed","workspace_id":"w1"},"event":"pane_closed"}"#;
        assert_eq!(hear(closed, "w1:p2"), Heard::Closed);
        assert_eq!(hear(closed, "w1:p9"), Heard::Nothing);
        let lost = r#"{"id":"pma","error":{"code":"events_lost","message":"behind"}}"#;
        assert_eq!(hear(lost, "w1:p2"), Heard::Lost);
        let started = r#"{"id":"pma","result":{"type":"subscription_started"}}"#;
        assert_eq!(hear(started, "w1:p2"), Heard::Nothing);
    }

    /// The pane's own environment holds what `restrict` removes, so the script
    /// must remove it again. A quote in a prompt stays one word.
    #[test]
    fn the_script_applies_the_environment_and_quotes_each_word() {
        let mut cmd = Command::new("claude");
        cmd.args(["-p", "it's done"])
            .env("GH_CONFIG_DIR", "/g d")
            .env_remove("SSH_AUTH_SOCK")
            .current_dir("/w");
        let out = script(
            &cmd,
            Path::new("/l/a.log"),
            Path::new("/l/a.status"),
            Some(Path::new("/bin/pma")),
        );
        assert!(out.contains("export PATH='"), "{out}");
        assert!(out.contains("export GH_CONFIG_DIR='/g d'\n"), "{out}");
        assert!(out.contains("unset SSH_AUTH_SOCK\n"), "{out}");
        assert!(out.contains("cd '/w' || exit 1\n"), "{out}");
        assert!(out.contains(r"'-p' 'it'\''s done'"), "{out}");
        assert!(
            out.contains("| tee '/l/a.log' | '/bin/pma' pane-view"),
            "{out}"
        );
        assert!(!out.contains(". "), "no secret, no file: {out}");
        let by_hand = script(&cmd, Path::new("/l/a.log"), Path::new("/l/a.status"), None);
        assert!(
            !by_hand.contains("tee") && !by_hand.contains("</dev/null"),
            "{by_hand}"
        );
    }

    /// A script stays on disk, so the broker's password is read from a file
    /// of its own, which the script removes before the agent starts.
    #[test]
    fn a_password_never_lands_in_the_script() {
        let mut cmd = Command::new("minosb");
        cmd.args(["-room", "r1"])
            .env(crate::minos::PASSWORD, "s3cret")
            .env("GH_CONFIG_DIR", "/g");
        let status = Path::new("/l/a.status");
        let out = script(&cmd, Path::new("/l/a.log"), status, None);
        assert!(!out.contains("s3cret"), "{out}");
        assert!(out.contains(". '/l/a.env'; rm -f '/l/a.env'\n"), "{out}");
        let before = out.find(". '/l/a.env'").unwrap();
        assert!(before < out.find("'minosb'").unwrap(), "{out}");
        assert_eq!(secrets(&cmd), "export MINOS_PASSWORD='s3cret'\n");
    }

    #[test]
    fn the_view_shows_tools_and_words_and_passes_other_text_through() {
        let event = r#"{"type":"assistant","message":{"content":[
            {"type":"text","text":"looking"},
            {"type":"tool_use","name":"Bash","input":{"command":"make test"}}]}}"#
            .replace('\n', "");
        assert_eq!(view_line(&event).unwrap(), "looking\n> Bash make test");
        assert_eq!(view_line(r#"{"type":"system"}"#), None);
        assert_eq!(view_line("plain").unwrap(), "plain");
        let end = r#"{"type":"result","subtype":"success","result":"ok","total_cost_usd":0.5}"#;
        assert!(view_line(end).unwrap().contains("[success, $0.50]"));
    }
}
