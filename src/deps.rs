//! Outdated dependencies, per ecosystem, as each tool reports them.
//!
//! - cargo: `cargo update --dry-run`, semver-compatible updates in `Cargo.lock`,
//!   transitive ones included.
//! - uv: `uv tree --frozen --outdated --depth 1`, direct dependencies with any
//!   newer release.
//! - go: `go list -u -m all`, direct modules with any newer version.
//!
//! None of the three writes to the project. Each needs the network.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::agent;
use crate::scan::DepsFacts;

/// How long one tool may run.
const TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, PartialEq)]
pub struct Outdated {
    pub name: String,
    pub current: String,
    pub latest: String,
}

/// Runs every tool the project's root files call for. `outdated` is `None`
/// when no ecosystem applies or every tool failed.
pub fn measure(dir: &Path) -> DepsFacts {
    let mut found: Option<i64> = None;
    let mut detail = Vec::new();
    for (ecosystem, marker) in [("cargo", "Cargo.lock"), ("uv", "uv.lock"), ("go", "go.mod")] {
        if !dir.join(marker).is_file() {
            continue;
        }
        match run(ecosystem, dir) {
            Ok(list) => {
                found = Some(found.unwrap_or(0) + list.len() as i64);
                detail.extend(
                    list.iter()
                        .map(|o| format!("{ecosystem}: {} {} -> {}", o.name, o.current, o.latest)),
                );
            }
            Err(e) => detail.push(format!("{ecosystem}: error: {e}")),
        }
    }
    DepsFacts {
        outdated: found,
        detail: detail.join("\n"),
    }
}

fn run(ecosystem: &str, dir: &Path) -> Result<Vec<Outdated>, String> {
    let mut cmd = match ecosystem {
        "cargo" => {
            let mut c = Command::new("cargo");
            c.args(["update", "--dry-run"]);
            c
        }
        "uv" => {
            let mut c = Command::new("uv");
            c.args(["tree", "--frozen", "--outdated", "--depth", "1"]);
            c
        }
        _ => {
            let mut c = Command::new("go");
            c.args([
                "list",
                "-u",
                "-m",
                "-f",
                "{{if and .Update (not .Indirect)}}{{.Path}} {{.Version}} {{.Update.Version}}{{end}}",
                "all",
            ]);
            c
        }
    };
    cmd.current_dir(dir)
        .env("NO_COLOR", "1")
        .env("CARGO_TERM_COLOR", "never");

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let log = std::env::temp_dir().join(format!(
        "pma-deps-{}-{}.log",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let finished = agent::run_limited(cmd, &log, TIMEOUT).map_err(|e| e.to_string());
    let output = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    match finished?.success {
        None => Err(format!("timed out after {}s", TIMEOUT.as_secs())),
        Some(false) => Err(agent::tail(&output, 1)),
        Some(true) => Ok(match ecosystem {
            "cargo" => parse_cargo(&output),
            "uv" => parse_uv(&output),
            _ => parse_go(&output),
        }),
    }
}

/// `Updating clap v4.6.6 -> v4.6.7` lines.
fn parse_cargo(output: &str) -> Vec<Outdated> {
    output
        .lines()
        .filter_map(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            match w[..] {
                ["Updating", name, current, "->", latest] => Some(Outdated {
                    name: name.into(),
                    current: current.into(),
                    latest: latest.into(),
                }),
                _ => None,
            }
        })
        .collect()
}

/// `├── ruff v0.14.10 (group: dev) (latest: v0.16.7)` lines.
fn parse_uv(output: &str) -> Vec<Outdated> {
    output
        .lines()
        .filter_map(|l| {
            let latest = l.split("(latest: ").nth(1)?.strip_suffix(')')?;
            let mut w = l
                .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
                .split_whitespace();
            Some(Outdated {
                name: w.next()?.into(),
                current: w.next()?.into(),
                latest: latest.into(),
            })
        })
        .collect()
}

/// `path current latest` lines from the `go list` template.
fn parse_go(output: &str) -> Vec<Outdated> {
    output
        .lines()
        .filter_map(|l| match l.split_whitespace().collect::<Vec<_>>()[..] {
            [name, current, latest] if !name.ends_with(':') && current.starts_with('v') => {
                Some(Outdated {
                    name: name.into(),
                    current: current.into(),
                    latest: latest.into(),
                })
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: Vec<Outdated>) -> Vec<String> {
        list.into_iter()
            .map(|o| format!("{} {} {}", o.name, o.current, o.latest))
            .collect()
    }

    #[test]
    fn cargo_dry_run_output() {
        let out = "    Updating crates.io index\n     Locking 2 packages to latest Rust 1.88 compatible versions\n    Updating clap v4.6.6 -> v4.6.7\n    Updating clap_lex v1.1.0 -> v1.1.1\nwarning: not updating lockfile due to dry run\n";
        assert_eq!(
            names(parse_cargo(out)),
            ["clap v4.6.6 v4.6.7", "clap_lex v1.1.0 v1.1.1"]
        );
    }

    #[test]
    fn uv_tree_output() {
        let out = "aldakit v0.4.0\n├── mkdocs v1.6.1 (group: dev)\n├── ruff v0.14.10 (group: dev) (latest: v0.16.7)\n└── ty v0.0.8 (latest: v0.0.81)\n";
        assert_eq!(
            names(parse_uv(out)),
            ["ruff v0.14.10 v0.16.7", "ty v0.0.8 v0.0.81"]
        );
    }

    #[test]
    fn go_list_output() {
        let out = "go: downloading github.com/x/y v1.0.0\ngithub.com/openai/openai-go/v3 v3.33.0 v3.61.0\n\ngithub.com/ledongthuc/pdf v0.0.0-2025 v0.0.0-2026\n";
        assert_eq!(
            names(parse_go(out)),
            [
                "github.com/openai/openai-go/v3 v3.33.0 v3.61.0",
                "github.com/ledongthuc/pdf v0.0.0-2025 v0.0.0-2026"
            ]
        );
    }

    #[test]
    fn projects_without_a_known_lock_file_are_unmeasured() {
        let dir = std::env::temp_dir().join(format!("pma-deps-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            measure(&dir),
            DepsFacts {
                outdated: None,
                detail: String::new()
            }
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
