//! A whole run against the real engine: plan, start, the caller's process, finish; and a started
//! run dropped without finish. `tests/library.rs` drives the same API against `Fake`; this file
//! checks what `Fake` cannot: the image build, the uid check, networks, the relay probe and
//! deletion by the engine itself.
//!
//! Ignored by default: needs Docker and the network, for the first build. Run with
//! `cargo test -p pma-box --test live_run -- --ignored --test-threads 1`. Mode networks
//! (`pma-open`, `pma-net`) are left in place, as a run leaves them.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pma_box::Sanduk;
use pma_box::observe::{Note, Observer};
use pma_box::run::{ImageSpec, Mode, Options};
use pma_box::util::Dirs;

#[derive(Default)]
struct Notes(Mutex<Vec<Note>>);

impl Observer for Notes {
    fn note(&self, note: Note) {
        self.0.lock().unwrap().push(note);
    }
}

impl Notes {
    fn output(&self) -> Vec<String> {
        let notes = self.0.lock().unwrap();
        notes
            .iter()
            .filter_map(|n| match n {
                Note::Output(m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    }
}

fn host_uid() -> String {
    let out = Command::new("id").arg("-u").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// One test's directories and agent, removed when dropped. The agent's image runs `sh -c TASK`
/// as the host's uid, and carries curl for the relay probe.
struct World {
    root: PathBuf,
    notes: Arc<Notes>,
}

impl World {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("pma-box-live-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("work")).unwrap();
        std::fs::create_dir_all(root.join("agent")).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let uid = host_uid();
        std::fs::write(
            root.join("agent/Containerfile"),
            format!(
                "FROM docker.io/library/alpine:3.20\n\
                 RUN apk add --no-cache curl\n\
                 LABEL sanduk.agent-uid=\"{uid}\"\n\
                 USER {uid}\n\
                 ENTRYPOINT [\"sh\", \"-c\"]\n"
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("agent/live.toml"),
            format!(
                r#"name = "live"
image = "pma-box-live:{uid}"
containerfile = "Containerfile"
protocols = ["openai-chat"]
stream = "minima"
argv = ["{{task}}"]
[wire]
key_env = "LIVE_KEY"
base_url_env = "LIVE_BASE_URL"
base_url = "{{endpoint}}{{api_prefix}}"
"#
            ),
        )
        .unwrap();
        World {
            root,
            notes: Arc::default(),
        }
    }

    fn sanduk(&self) -> Sanduk {
        Sanduk::new(Dirs::under(&self.root)).observer(self.notes.clone())
    }

    fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    fn options(&self, task: &str) -> Options {
        Options {
            task: Some(task.into()),
            report_instruction: false,
            workdir: self.work(),
            image: ImageSpec {
                agent: Some(self.root.join("agent/live.toml").display().to_string()),
                ..Default::default()
            },
            runtime: Some("docker".into()),
            provider: "openai-compat".into(),
            model: Some("m".into()),
            timeout: Duration::from_secs(120),
            skip_key_check: true,
            ..Options::default()
        }
    }

    fn records(&self) -> usize {
        std::fs::read_dir(self.root.join("state/runs"))
            .map(|d| {
                d.flatten()
                    .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                    .count()
            })
            .unwrap_or(0)
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Containers the engine holds whose name starts with `prefix`, running or not.
fn containers(prefix: &str) -> Vec<String> {
    let out = Command::new("docker")
        .args(["ps", "-a", "--format", "{{.Names}}"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|n| n.starts_with(prefix))
        .map(String::from)
        .collect()
}

fn running(name: &str) -> bool {
    Command::new("docker")
        .args(["inspect", "-f", "{{.State.Running}}", name])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
#[ignore = "needs Docker and the network"]
fn an_open_run_builds_runs_verifies_and_leaves_nothing() {
    let w = World::new("open");
    let plan = w
        .sanduk()
        .plan(Options {
            verify: Some("test -s REPORT.md && echo checked".into()),
            ..w.options("echo found > REPORT.md")
        })
        .unwrap();
    let name = plan.name().to_string();
    let mut live = plan.start().unwrap();
    assert_eq!(w.records(), 1, "a started run is recorded");
    let status = live.command().status().unwrap();
    let outcome = live.finish(status);

    assert_eq!(outcome.exit, Some(0));
    let mut report = String::new();
    std::io::Read::read_to_string(&mut outcome.report.unwrap(), &mut report).unwrap();
    assert_eq!(report, "found\n");
    let verified = outcome.verify.unwrap();
    assert_eq!(verified.ok(), Some(true), "{verified:?}");
    assert_eq!(w.notes.output(), ["checked"]);
    assert!(outcome.relay.is_none());
    assert!(containers(&name).is_empty(), "{:?}", containers(&name));
    assert_eq!(w.records(), 0);
}

/// Behind the relay the container holds a token and reaches the relay through the mode's
/// network; the probe that checks it runs in a container of its own, which goes too.
#[test]
#[ignore = "needs Docker and the network"]
fn a_relayed_run_reaches_the_relay_and_holds_a_token() {
    for mode in [Mode::KeySafe, Mode::Sealed] {
        let w = World::new(&format!("{mode:?}").to_lowercase());
        let before = containers("pma-probe-");
        let plan = w
            .sanduk()
            .plan(Options {
                mode,
                ..w.options(
                    "env > env.txt; curl -s -o /dev/null -w '%{http_code}' \
                     \"${LIVE_BASE_URL%/v1}/_sanduk/ping\" > code.txt; echo r > REPORT.md",
                )
            })
            .unwrap();
        let name = plan.name().to_string();
        let mut live = plan.start().unwrap();
        let status = live.command().status().unwrap();
        let outcome = live.finish(status);

        assert_eq!(outcome.exit, Some(0), "{mode:?}");
        let env = read(&w.work().join("env.txt"));
        let token = env
            .lines()
            .find_map(|l| l.strip_prefix("LIVE_KEY="))
            .unwrap_or_else(|| panic!("{mode:?}: no LIVE_KEY in {env}"));
        assert_eq!(token.len(), 32, "{mode:?}: {token:?}");
        let base = env
            .lines()
            .find_map(|l| l.strip_prefix("LIVE_BASE_URL="))
            .unwrap_or_default();
        assert!(base.starts_with("http://"), "{mode:?}: {base:?}");
        // The relay answers its ping with 204, as the probe expects.
        assert_eq!(
            read(&w.work().join("code.txt")),
            "204",
            "{mode:?}: the agent reached the relay at {base}"
        );
        let stats = outcome.relay.expect("a keyed run reports its relay");
        assert_eq!(stats.rejected, 0, "{mode:?}");
        assert!(containers(&name).is_empty(), "{mode:?}");
        assert_eq!(
            containers("pma-probe-"),
            before,
            "{mode:?}: the probe is gone"
        );
        assert_eq!(w.records(), 0, "{mode:?}");
    }
}

/// The caller started the agent and gave up on it: dropping `Live` deletes the running
/// container and releases the record.
#[test]
#[ignore = "needs Docker and the network"]
fn dropping_a_started_run_deletes_its_running_container() {
    let w = World::new("drop");
    let plan = w.sanduk().plan(w.options("sleep 300")).unwrap();
    let name = plan.name().to_string();
    let mut live = plan.start().unwrap();
    let mut child = live
        .command()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !running(&name) {
        assert!(Instant::now() < deadline, "{name} never started");
        std::thread::sleep(Duration::from_millis(200));
    }
    // Killing the engine's CLI leaves the container running: only the engine can stop it.
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        running(&name),
        "{name} outlived its CLI, as a container does"
    );
    drop(live);

    assert!(containers(&name).is_empty(), "{:?}", containers(&name));
    assert_eq!(w.records(), 0);
}

/// Docker on Linux keeps the host's owner on a bind mount, so an agent running as another uid
/// cannot write the workdir. That is refused before anything starts, with the facts a caller
/// needs to offer a fix.
#[test]
#[ignore = "needs Docker and the network"]
fn an_agent_uid_that_cannot_write_the_workdir_is_refused_before_it_starts() {
    let w = World::new("uid");
    // Group or other write may let another uid in, so only a 755 workdir is refused for sure.
    // A umask of 002 makes new directories 775.
    std::fs::set_permissions(
        w.work(),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let owner: u32 = host_uid().parse().unwrap();
    let other = owner + 1;
    let plan = w
        .sanduk()
        .plan(Options {
            user: Some(other.to_string()),
            ..w.options("echo never > REPORT.md")
        })
        .unwrap();
    let name = plan.name().to_string();
    let err = plan.start().err().expect("refused");

    assert_eq!(err.kind, pma_box::error::ErrorKind::Refused, "{err:?}");
    match err.detail {
        Some(pma_box::error::Detail::AgentUid {
            agent_uid,
            owner: found,
            ..
        }) => assert_eq!((agent_uid, found), (other, owner)),
        ref d => panic!("{d:?}: {}", err.message),
    }
    assert!(!w.work().join("REPORT.md").exists());
    assert!(containers(&name).is_empty());
    assert_eq!(w.records(), 0);
}
