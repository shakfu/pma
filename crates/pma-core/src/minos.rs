//! A minos room for each run under `pma integrate minos`. A person reads the run
//! there and corrects it between turns. Design:
//! docs/dev/pma-minos-integration.md.
//!
//! `pma` writes no wire client: it calls `minos room ensure` to found the
//! room, and starts `minosb`, the per-run broker, as a child. A worker with a
//! `relay` runs under `minosb`, which writes the task and each correction to
//! its stdin. Any other worker runs as usual beside a `minosb` that serves
//! only the socket, and reaches the room through `minosa` if it calls that.
//!
//! An unattended run goes on without its room when minos cannot be reached,
//! and says so. A run a person drives from the room fails without it.

use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::config::{Minos, Rooms};
use crate::store::Run;

/// Where the worker's password is read; the administrator's is
/// `ADMIN_PASSWORD`. Neither is stored, and `agent::restrict` removes both.
pub const PASSWORD: &str = "MINOS_PASSWORD";
pub const ADMIN_PASSWORD: &str = "MINOS_ADMIN_PASSWORD";

/// `var`, else, under `minos.dev_passwords`, the account name: minosd's
/// development accounts have their name as password. Against any other server
/// a wrong one only costs the room.
fn password(var: &str, account: &str, dev: bool) -> Result<std::ffi::OsString, String> {
    match std::env::var_os(var) {
        Some(p) => Ok(p),
        None if dev => Ok(account.into()),
        None => Err(format!("{var} is not set")),
    }
}

/// Which passwords fall back to the development default.
pub fn dev_passwords(m: &Minos) -> Vec<String> {
    [(PASSWORD, &m.user), (ADMIN_PASSWORD, &m.admin)]
        .into_iter()
        .filter(|(var, _)| m.dev_passwords && std::env::var_os(var).is_none())
        .map(|(var, account)| {
            format!("{var} is not set; using `{account}`, as minosd's development accounts do")
        })
        .collect()
}

/// How long a broker may take to enter its room.
const READY: Duration = Duration::from_secs(10);

/// `sun_path` on macOS holds 104 bytes with its NUL; Linux holds 108.
const SOCKET_MAX: usize = 103;

/// Told to an agent that has the socket but no relay: nothing reaches it
/// unless it asks.
pub const SIDECAR_NOTE: &str = "\n\nA person may follow this run in a minos room. Between steps, \
run `minosa messages` and act on anything they said. `minosa say TEXT` answers them.\n";

/// One run's room, and the files its broker uses.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub room: String,
    /// Outside every worktree, so a diff never holds it.
    pub socket: PathBuf,
    pub events: PathBuf,
}

/// The `-task` key the room is filed under: the task, or the task and run.
pub fn room_key(rooms: Rooms, run: &Run) -> String {
    match rooms {
        Rooms::Task => run.task_key.clone(),
        Rooms::Run => format!("{}#{}", run.task_key, run.id),
    }
}

/// Founds or finds the run's room, and names its files. `Err` says why the
/// run goes without one.
pub fn open(m: &Minos, home: &Path, run: &Run, n: usize) -> Result<Link, String> {
    password(PASSWORD, &m.user, m.dev_passwords)?;
    let socket = socket_path(home, run.id, n)?;
    let _ = std::fs::remove_file(&socket);
    let events = home
        .join("runs")
        .join(run.id.to_string())
        .join(format!("minos-{n}.events"));
    let _ = std::fs::remove_file(&events);
    let room = ensure(m, &run.project, &room_key(m.rooms, run))?;
    Ok(Link {
        room,
        socket,
        events,
    })
}

/// Under `home` when the path fits a socket's. Otherwise in `/tmp/pma-<uid>`,
/// which must be this user's own and closed to others, with a hash of `home`
/// so two homes' runs do not share a name.
fn socket_path(home: &Path, id: i64, n: usize) -> Result<PathBuf, String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let name = format!("{id}-{n}.sock");
    let made =
        |dir: &Path| std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()));
    let first = home.join("minos").join(&name);
    if first.as_os_str().len() <= SOCKET_MAX {
        made(&home.join("minos"))?;
        return Ok(first);
    }
    let uid = std::fs::metadata(home)
        .map_err(|e| format!("{}: {e}", home.display()))?
        .uid();
    let dir = PathBuf::from(format!("/tmp/pma-{uid}"));
    made(&dir)?;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    let meta = std::fs::symlink_metadata(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(format!(
            "{} is not a directory only this user may enter",
            dir.display()
        ));
    }
    // FNV-1a: stable across builds, unlike the standard hasher.
    let hash = home
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
        });
    Ok(dir.join(format!("{:08x}-{name}", hash as u32)))
}

/// `minos room ensure`, as the administrator: founding a room needs one.
fn ensure(m: &Minos, project: &str, key: &str) -> Result<String, String> {
    let password = password(ADMIN_PASSWORD, &m.admin, m.dev_passwords)
        .map_err(|e| format!("{e}; founding a room needs `{}`", m.admin))?;
    let mut cmd = Command::new("minos");
    cmd.args(["room", "ensure", "-server", &m.server])
        .args(["-project", project, "-task", key]);
    // The run's account first: a room it is not invited to refuses its broker.
    let mut invite = vec![&m.user];
    invite.extend(m.invite.iter().filter(|w| **w != m.user));
    for who in invite {
        cmd.args(["-invite", who]);
    }
    let out = cmd
        .env("MINOS_USER", &m.admin)
        .env("MINOS_PASSWORD", password)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("minos: {e}"))?;
    let said = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    if !out.status.success() {
        return Err(said(&out.stderr));
    }
    let room = said(&out.stdout);
    match room.is_empty() || room.contains(char::is_whitespace) {
        true => Err(format!("minos room ensure printed no room id: {room:?}")),
        false => Ok(room),
    }
}

/// `minosb`'s flags for `link`, before any run command.
fn broker(m: &Minos, link: &Link) -> Result<Command, String> {
    let password = password(PASSWORD, &m.user, m.dev_passwords)?;
    let mut cmd = Command::new("minosb");
    cmd.args(["-server", &m.server, "-user", &m.user, "-room", &link.room])
        .arg("-socket")
        .arg(&link.socket)
        .arg("-events")
        .arg(&link.events)
        .env(PASSWORD, password);
    if !m.interrupters.is_empty() {
        cmd.args(["-interrupters", &m.interrupters.join(",")]);
    }
    Ok(cmd)
}

/// `agent` under `minosb`, which writes `task` as its first turn and closes
/// its stdin after `linger` idle. A run a person drives never lingers out:
/// `0s` is minosb's never. The agent keeps `agent`'s directory and
/// environment; `minosb` removes the password from what it passes on.
pub fn wrap(
    m: &Minos,
    link: &Link,
    adapter: &str,
    task: &str,
    agent: &Command,
    person: bool,
) -> Result<Command, String> {
    let linger = if person { 0 } else { m.linger };
    let mut cmd = broker(m, link)?;
    // A relay naming a JSON file is a one-shot spec (minos `docs/one-shot-agents.md`).
    let flag = if adapter.ends_with(".json") {
        "-agent-spec"
    } else {
        "-agent"
    };
    cmd.args([flag, adapter, "-task", task])
        .args(["-linger", &format!("{linger}s")])
        .arg("--")
        .arg(agent.get_program())
        .args(agent.get_args());
    if let Some(dir) = agent.get_current_dir() {
        cmd.current_dir(dir);
    }
    for (key, value) in agent.get_envs() {
        match value {
            Some(v) => cmd.env(key, v),
            None => cmd.env_remove(key),
        };
    }
    // After the agent's own, which `restrict` cleared of it.
    cmd.env(PASSWORD, password(PASSWORD, &m.user, m.dev_passwords)?);
    Ok(cmd)
}

/// What a run's state becomes on one `-events` line, as minos
/// `docs/minosb-events.md` defines them. A turn that ends with nothing to
/// deliver waits on a person; only one who drives the run is needed then.
pub fn state_of(line: &str, person: bool) -> Option<&'static str> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    match v["event"].as_str()? {
        "pushed" => Some("working"),
        "turn" if v["delivering"].as_i64() == Some(0) => {
            Some(if person { "blocked" } else { "idle" })
        }
        _ => None,
    }
}

/// Reads `events` as the broker writes it, until `stop`, passing each change
/// of state to `on_state`.
fn follow(events: &Path, person: bool, stop: &AtomicBool, on_state: &dyn Fn(&str)) {
    let (mut read, mut last) = (0, "");
    loop {
        let done = stop.load(Ordering::Relaxed);
        let text = std::fs::read_to_string(events).unwrap_or_default();
        // A line is complete once its newline is written.
        let end = text.rfind('\n').map_or(0, |i| i + 1);
        for line in text.get(read..end).unwrap_or_default().lines() {
            if let Some(state) = state_of(line, person).filter(|s| *s != last) {
                on_state(state);
                last = state;
            }
        }
        read = end.max(read);
        if done {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// How a broker's run is watched.
pub struct Watched<'a> {
    /// The room the person's client opens in, for a run they drive here.
    pub room: Option<&'a str>,
    /// The broker's events, read for the run's state.
    pub events: Option<&'a Path>,
    /// Whether a person drives the run.
    pub person: bool,
}

/// Runs `cmd`, a broker, to its end, following its events for its state. A
/// run a person drives from a terminal opens their client in the room
/// meanwhile, which returns once the broker leaves the room; one that ends
/// first, or never entered, closes it.
pub fn host(
    m: &Minos,
    watched: Watched,
    cmd: Command,
    log: &Path,
    timeout: Duration,
    on_state: impl Fn(&str) + Send + 'static,
) -> std::io::Result<crate::agent::Finished> {
    use std::io::IsTerminal;
    use std::sync::Arc;
    let Watched {
        room,
        events,
        person,
    } = watched;
    let stop = Arc::new(AtomicBool::new(false));
    let follower = events.map(|e| {
        let (e, stop) = (e.to_path_buf(), Arc::clone(&stop));
        std::thread::spawn(move || follow(&e, person, &stop, &on_state))
    });
    let log_path = log.to_path_buf();
    let agent =
        std::thread::spawn(move || crate::agent::run_limited(cmd, None, &log_path, timeout));
    if let Some(room) = room.filter(|_| person && std::io::stdin().is_terminal()) {
        let client = Command::new("minos")
            .args(["-server", &m.server, "-room", room, "-exit-with", &m.user])
            // The person signs in as themselves, not as the run's accounts.
            .env_remove(PASSWORD)
            .env_remove(ADMIN_PASSWORD)
            .spawn();
        match client {
            Ok(mut client) => loop {
                if agent.is_finished() {
                    let _ = client.kill();
                    let _ = client.wait();
                    break;
                }
                if let Ok(Some(_)) = client.try_wait() {
                    if !agent.is_finished() {
                        eprintln!("The run goes on: `/end` in the room, or Ctrl-C here, ends it.");
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            },
            Err(e) => eprintln!("warning: minos: {e}; enter room {room} from your client"),
        }
    }
    let finished = agent
        .join()
        .map_err(|_| std::io::Error::other("the run's thread panicked"))?;
    stop.store(true, Ordering::Relaxed);
    if let Some(f) = follower {
        let _ = f.join();
    }
    finished
}

/// Whether the broker entered its room. It writes `ready` once it has, so a
/// run without it never had a room: no server, a refused seat, a bad password.
pub fn ready(events: &Path) -> bool {
    std::fs::read_to_string(events).is_ok_and(|text| {
        text.lines().any(|l| {
            serde_json::from_str::<serde_json::Value>(l).is_ok_and(|v| v["event"] == "ready")
        })
    })
}

/// A broker with no run command: it serves the socket and nothing else.
pub struct Sidecar {
    child: Child,
    lifeline: Option<ChildStdin>,
    socket: PathBuf,
}

/// Starts a sidecar broker and waits until it is in the room. Its output goes
/// to `log`. Under the watchdog, so it dies with `pma`.
pub fn serve(m: &Minos, link: &Link, log: &Path) -> Result<Sidecar, String> {
    let cmd = broker(m, link)?;
    let (child, lifeline) =
        crate::agent::spawn_guarded(cmd, None, log).map_err(|e| format!("minosb: {e}"))?;
    let mut side = Sidecar {
        child,
        lifeline: Some(lifeline),
        socket: link.socket.clone(),
    };
    let started = Instant::now();
    while !ready(&link.events) {
        let exited = side.child.try_wait().ok().flatten().is_some();
        if exited || started.elapsed() >= READY {
            side.stop();
            let said = crate::agent::read_log(log).unwrap_or_default();
            return Err(format!(
                "minosb did not enter room {}: {}",
                link.room,
                crate::agent::tail(&said, 1)
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(side)
}

impl Sidecar {
    /// Posts to the room as the run's account. A post that fails is not the
    /// run's failure.
    pub fn say(&self, text: &str) {
        let out = Command::new("minosa")
            .arg("say")
            .arg("-socket")
            .arg(&self.socket)
            .arg("--")
            .arg(text)
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => eprintln!(
                "warning: minosa say: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Err(e) => eprintln!("warning: minosa: {e}"),
        }
    }

    /// `TERM` first, so the broker leaves the room and logs out; the watchdog
    /// kills what is left.
    pub fn stop(&mut self) {
        let group = format!("-{}", self.child.id());
        let _ = Command::new("kill")
            .args(["-TERM", "--", &group])
            .stderr(Stdio::null())
            .status();
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            if self.child.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.lifeline.take();
        let _ = self.child.wait();
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        if self.lifeline.is_some() {
            self.stop();
        }
    }
}

/// What a worker's `relay` needs of minos beyond `NEEDS`: a JSON spec is the
/// one-shot kind, `acp` the ACP kind.
pub fn relay_needs(relay: &str) -> Option<&'static str> {
    match relay {
        "acp" => Some("minosb.acp"),
        r if r.ends_with(".json") => Some("minosb.agent-spec"),
        _ => None,
    }
}

/// What minos must offer for what pma asks of it, as `minos ping` names it.
const NEEDS: [&str; 8] = [
    "minos.room-ensure",
    "minos.exit-with",
    "minosb.task",
    "minosb.events",
    "minosb.linger",
    "minosb.interrupters",
    "minosb.end",
    "minosb.pushed",
];

/// What `pma integrate minos` reports: the programs it needs, the passwords,
/// and what `minos ping` says of the server, each account, and the
/// capabilities pma uses.
/// `extra` names what the configured workers' relays need beyond `NEEDS`.
pub fn check(m: &Minos, extra: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    for program in ["minos", "minosb", "minosa"] {
        if !crate::agent::on_path(program) {
            problems.push(format!("{program} is not on PATH"));
        }
    }
    let can_ping = crate::agent::on_path("minos");
    for (var, account) in [(PASSWORD, &m.user), (ADMIN_PASSWORD, &m.admin)] {
        match password(var, account, m.dev_passwords) {
            Ok(_) if !can_ping => {}
            Ok(pass) => match ping(&m.server, account, &pass) {
                Ok(offered) => problems.extend(
                    NEEDS
                        .iter()
                        .chain(extra)
                        .filter(|n| !offered.iter().any(|o| o == *n))
                        .map(|n| format!("this minos lacks `{n}`; update minos")),
                ),
                Err(e) => problems.push(e),
            },
            Err(e) => problems.push(e),
        }
    }
    problems.sort();
    problems.dedup();
    problems
}

/// `minos ping` as `account`: the capabilities it lists, or why it failed. A
/// minos without `ping` would start its client, so it gets no terminal and
/// five seconds.
fn ping(server: &str, account: &str, pass: &std::ffi::OsStr) -> Result<Vec<String>, String> {
    let mut child = Command::new("minos")
        .args(["ping", "-server", server])
        .env("MINOS_USER", account)
        .env("MINOS_PASSWORD", pass)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("minos: {e}"))?;
    let until = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait().map_err(|e| format!("minos: {e}"))? {
            Some(status) => break status,
            None if Instant::now() >= until => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("`minos ping` did not answer; update minos".into());
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let out = child
        .wait_with_output()
        .map_err(|e| format!("minos: {e}"))?;
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    match status.code() {
        Some(0) => {
            let v: serde_json::Value = serde_json::from_slice(&out.stdout)
                .map_err(|_| "`minos ping` printed no capabilities; update minos".to_string())?;
            Ok(v["capabilities"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect())
        }
        Some(3) => Err(format!("{server}: no minos server answers: {said}")),
        Some(4) => Err(format!("{server} refused `{account}`: {said}")),
        _ => Err(format!(
            "`minos ping` failed ({status}); update minos: {said}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As minos `docs/minosb-events.md` names them: a push starts work, a
    /// turn with nothing to deliver waits, and only a driven run waits on you.
    #[test]
    fn events_become_a_runs_state() {
        let turn = r#"{"event":"turn","delivering":0}"#;
        assert_eq!(
            state_of(r#"{"event":"pushed","messages":1}"#, true),
            Some("working")
        );
        assert_eq!(state_of(turn, true), Some("blocked"));
        assert_eq!(state_of(turn, false), Some("idle"));
        assert_eq!(state_of(r#"{"event":"turn","delivering":2}"#, true), None);
        assert_eq!(state_of(r#"{"event":"ready","room":"r"}"#, true), None);
        assert_eq!(state_of("not json", true), None);

        let events = std::env::temp_dir().join(format!("pma-follow-{}.events", std::process::id()));
        let lines = [
            r#"{"event":"ready","room":"r"}"#,
            r#"{"event":"pushed","messages":1}"#,
            r#"{"event":"pushed","messages":1}"#,
            turn,
        ];
        std::fs::write(&events, lines.join("\n") + "\n" + r#"{"event":"pushed""#).unwrap();
        let seen = std::sync::Mutex::new(Vec::new());
        let stop = AtomicBool::new(true);
        follow(&events, true, &stop, &|s| {
            seen.lock().unwrap().push(s.to_string())
        });
        assert_eq!(
            *seen.lock().unwrap(),
            ["working", "blocked"],
            "each change once; a line still being written waits"
        );
        let _ = std::fs::remove_file(events);
    }

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn link() -> Link {
        Link {
            room: "r1".into(),
            socket: "/s/1-1.sock".into(),
            events: "/e/minos-1.events".into(),
        }
    }

    /// The broker takes the room, socket, events, task and linger; the agent
    /// follows `--` with its own words, directory and environment.
    #[test]
    fn an_agent_is_wrapped_after_the_brokers_flags() {
        let m = Minos {
            interrupters: vec!["sa".into(), "demo".into()],
            ..Minos::default()
        };
        let mut agent = Command::new("claude");
        agent
            .args(["-p", "--input-format", "stream-json"])
            .current_dir("/w")
            .env("GH_CONFIG_DIR", "/g")
            .env_remove(PASSWORD);
        let cmd = wrap(&m, &link(), "claude", "fix it", &agent, false).unwrap();
        assert_eq!(cmd.get_program(), "minosb");
        let args = args_of(&cmd);
        let after = |flag: &str| &args[args.iter().position(|a| a == flag).unwrap() + 1];
        assert_eq!(after("-room"), "r1");
        assert_eq!(after("-socket"), "/s/1-1.sock");
        assert_eq!(after("-events"), "/e/minos-1.events");
        assert_eq!(after("-agent"), "claude");
        assert_eq!(after("-task"), "fix it");
        assert_eq!(after("-linger"), "120s");
        let spec = wrap(&m, &link(), "/m/agents/gilda.json", "fix it", &agent, false).unwrap();
        let spec = args_of(&spec);
        let at = spec.iter().position(|a| a == "-agent-spec").unwrap();
        assert_eq!(spec[at + 1], "/m/agents/gilda.json");
        assert!(!spec.iter().any(|a| a == "-agent"));
        let person = wrap(&m, &link(), "claude", "fix it", &agent, true).unwrap();
        let args = args_of(&person);
        let at = args.iter().position(|a| a == "-linger").unwrap();
        assert_eq!(args[at + 1], "0s", "a person ends the run, not the linger");
        assert_eq!(after("-interrupters"), "sa,demo");
        assert_eq!(after("-user"), "worker");
        let dash = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(
            args[dash + 1..],
            ["claude", "-p", "--input-format", "stream-json"]
        );
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/w")));
        let env: Vec<_> = cmd.get_envs().collect();
        assert!(env.contains(&("GH_CONFIG_DIR".as_ref(), Some("/g".as_ref()))));
        // The broker holds the password, the account's own when none is set;
        // `restrict` took it from the agent.
        assert!(env.contains(&(PASSWORD.as_ref(), Some("worker".as_ref()))));
    }

    /// A task's runs share its room unless the setting gives each its own.
    #[test]
    fn a_room_is_per_task_or_per_run() {
        let run = Run {
            id: 7,
            task_key: "item:fix-the-parser".into(),
            ..Run::blank()
        };
        assert_eq!(room_key(Rooms::Task, &run), "item:fix-the-parser");
        assert_eq!(room_key(Rooms::Run, &run), "item:fix-the-parser#7");
    }

    /// A deep home puts the socket in a short directory of this user's own,
    /// named apart from another home's.
    #[test]
    fn a_socket_path_always_fits() {
        let base = std::env::temp_dir().join(format!("pma-minos-sock-{}", std::process::id()));
        let short = base.join("h");
        std::fs::create_dir_all(&short).unwrap();
        let near = socket_path(&short, 3, 1).unwrap();
        assert_eq!(near, short.join("minos").join("3-1.sock"));
        // Longer than the limit by itself, so the path falls back wherever
        // the temporary directory is.
        let deep = base.join("d".repeat(SOCKET_MAX));
        let other = base.join("e".repeat(SOCKET_MAX));
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let far = socket_path(&deep, 3, 1).unwrap();
        assert!(far.as_os_str().len() <= SOCKET_MAX, "{}", far.display());
        assert!(
            far.to_string_lossy().ends_with("-3-1.sock"),
            "{}",
            far.display()
        );
        assert_ne!(far, socket_path(&other, 3, 1).unwrap());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_broker_is_ready_once_it_says_so() {
        let dir = std::env::temp_dir().join(format!("pma-minos-ready-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let events = dir.join("e");
        assert!(!ready(&events), "no file");
        std::fs::write(&events, "{\"event\":\"stopped\",\"reason\":\"exited\"}\n").unwrap();
        assert!(!ready(&events));
        std::fs::write(&events, "{\"event\":\"ready\",\"room\":\"r1\"}\n").unwrap();
        assert!(ready(&events));
        let _ = std::fs::remove_dir_all(dir);
    }
}
