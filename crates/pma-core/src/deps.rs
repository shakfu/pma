//! How far each lock can move within the project's declared constraints, as
//! each tool reports it. Each measure is its fix run dry; see
//! `docs/dev/deps-refresh.md`.
//!
//! - cargo: `cargo update --dry-run`, transitive crates included.
//! - uv: `uv lock --upgrade --dry-run`, transitive packages included.
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

/// Each ecosystem, the root file that says it applies, and its lock.
pub const ECOSYSTEMS: [(&str, &str, &str); 3] = [
    ("cargo", "Cargo.lock", "Cargo.lock"),
    ("uv", "uv.lock", "uv.lock"),
    ("go", "go.mod", "go.sum"),
];

/// One package the lock would move. `current` is `none` for a package the
/// lock would add, and `latest` is `none` for one it would remove.
#[derive(Debug, Clone, PartialEq)]
pub struct Outdated {
    pub name: String,
    pub current: String,
    pub latest: String,
}

/// Runs every tool the project's root files call for. `outdated` is `None`
/// when no ecosystem applies or any tool failed: a partial sum reads as the
/// whole count. `detail` still lists what the other tools found.
pub fn measure(dir: &Path) -> DepsFacts {
    measure_with(dir, run)
}

fn measure_with(dir: &Path, run: impl Fn(&str, &Path) -> Result<String, String>) -> DepsFacts {
    let mut failed = false;
    let mut found: Option<i64> = None;
    let mut held_back: Option<i64> = None;
    let mut detail = Vec::new();
    for (ecosystem, marker, _) in ECOSYSTEMS {
        if !dir.join(marker).is_file() {
            continue;
        }
        match run(ecosystem, dir) {
            Ok(output) => {
                let list = match ecosystem {
                    "cargo" => {
                        held_back = parse_cargo_held_back(&output);
                        parse_cargo(&output)
                    }
                    "uv" => parse_uv(&output),
                    _ => parse_go(&output),
                };
                found = Some(found.unwrap_or(0) + list.len() as i64);
                detail.extend(
                    list.iter()
                        .map(|o| format!("{ecosystem}: {} {} -> {}", o.name, o.current, o.latest)),
                );
            }
            Err(e) => {
                failed = true;
                detail.push(format!("{ecosystem}: error: {e}"));
            }
        }
    }
    DepsFacts {
        outdated: found.filter(|_| !failed),
        held_back,
        detail: detail.join("\n"),
    }
}

/// The commands that move an ecosystem's lock within its constraints: each
/// measure in `run` without its dry run.
fn fixes(ecosystem: &str) -> &'static [&'static [&'static str]] {
    match ecosystem {
        "cargo" => &[&["cargo", "update"]],
        "uv" => &[&["uv", "lock", "--upgrade"]],
        _ => &[&["go", "get", "-u", "./..."], &["go", "mod", "tidy"]],
    }
}

/// Moves every lock the project's root files call for, printing each command
/// before its own output. Stops at the first that fails.
pub fn refresh(dir: &Path) -> Result<(), String> {
    let present: Vec<&str> = ECOSYSTEMS
        .iter()
        .filter(|(_, marker, _)| dir.join(marker).is_file())
        .map(|(ecosystem, _, _)| *ecosystem)
        .collect();
    if present.is_empty() {
        return Err(format!(
            "{}: no Cargo.lock, uv.lock or go.mod",
            dir.display()
        ));
    }
    for argv in present.into_iter().flat_map(fixes) {
        let line = argv.join(" ");
        println!("$ {line}");
        let status = Command::new(argv[0])
            .args(&argv[1..])
            .current_dir(dir)
            .status()
            .map_err(|e| format!("{line}: {e}"))?;
        if !status.success() {
            return Err(format!("`{line}` failed: {status}"));
        }
    }
    Ok(())
}

/// The tool's output, or why it failed.
fn run(ecosystem: &str, dir: &Path) -> Result<String, String> {
    let mut cmd = match ecosystem {
        "cargo" => {
            let mut c = Command::new("cargo");
            c.args(["update", "--dry-run"]);
            c
        }
        "uv" => {
            let mut c = Command::new("uv");
            c.args(["lock", "--upgrade", "--dry-run"]);
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
    let finished = agent::run_limited(cmd, None, &log, TIMEOUT).map_err(|e| e.to_string());
    let output = agent::read_log(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    match finished?.success {
        None => Err(format!("timed out after {}s", TIMEOUT.as_secs())),
        Some(false) => Err(agent::tail(&output, 1)),
        Some(true) => Ok(output),
    }
}

/// `Updating clap v4.6.6 -> v4.6.7`, `Downgrading`, `Adding` and `Removing`
/// lines.
fn parse_cargo(output: &str) -> Vec<Outdated> {
    output
        .lines()
        .filter_map(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            let (name, current, latest) = match w[..] {
                ["Updating" | "Downgrading", name, current, "->", latest] => {
                    (name, current, latest)
                }
                ["Adding", name, latest, ..] => (name, "none", latest),
                ["Removing", name, current, ..] => (name, current, "none"),
                _ => return None,
            };
            Some(Outdated {
                name: name.into(),
                current: current.into(),
                latest: latest.into(),
            })
        })
        .collect()
}

/// Crates the constraints hold back, from cargo's closing note:
/// `pass `--verbose` to see 6 unchanged dependencies behind latest`.
fn parse_cargo_held_back(output: &str) -> Option<i64> {
    output
        .lines()
        .filter(|l| l.contains("behind latest"))
        .find_map(|l| {
            l.split_once("to see ")?
                .1
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
}

/// `Update ruff v0.14.10 -> v0.16.7`, `Add x v1.0`, `Remove y v2.0` lines. A
/// package locked at several versions lists them comma-separated.
fn parse_uv(output: &str) -> Vec<Outdated> {
    output
        .lines()
        .filter_map(|l| {
            let (verb, rest) = l.split_once(' ')?;
            let (name, versions) = rest.split_once(' ')?;
            let (current, latest) = match verb {
                "Update" => versions.split_once(" -> ")?,
                "Add" => ("none", versions),
                "Remove" => (versions, "none"),
                _ => return None,
            };
            Some(Outdated {
                name: name.into(),
                current: current.into(),
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
        let out = "    Updating crates.io index\n     Locking 4 packages to latest Rust 1.88 compatible versions\n    Updating clap v4.6.6 -> v4.6.7\n    Updating clap_lex v1.1.0 -> v1.1.1\n Downgrading cc v1.2.1 -> v1.2.0\n      Adding static_vcruntime v3.0.0\n    Removing windows-sys v0.59.0\nnote: pass `--verbose` to see 6 unchanged dependencies behind latest\nwarning: not updating lockfile due to dry run\n";
        assert_eq!(
            names(parse_cargo(out)),
            [
                "clap v4.6.6 v4.6.7",
                "clap_lex v1.1.0 v1.1.1",
                "cc v1.2.1 v1.2.0",
                "static_vcruntime none v3.0.0",
                "windows-sys v0.59.0 none"
            ]
        );
        assert_eq!(parse_cargo_held_back(out), Some(6));
    }

    #[test]
    fn cargo_without_the_note_holds_nothing_back() {
        let out =
            "    Updating clap v4.6.6 -> v4.6.7\nwarning: not updating lockfile due to dry run\n";
        assert_eq!(parse_cargo_held_back(out), None);
        let one = "note: pass `--verbose` to see 1 unchanged dependency behind latest\n";
        assert_eq!(parse_cargo_held_back(one), Some(1));
    }

    #[test]
    fn uv_lock_dry_run_output() {
        let out = "warning: No `requires-python` value found in the workspace. Defaulting to `>=3.13`.\nResolved 11 packages in 731ms\nUpdate ruff v0.14.10 -> v0.16.7\nAdd py2tosc v0.6.0\nRemove setuptools v80.4.0\nUpdate tomli v1.2.3, v2.4.0 -> v1.2.3, v2.5.0\n";
        assert_eq!(
            names(parse_uv(out)),
            [
                "ruff v0.14.10 v0.16.7",
                "py2tosc none v0.6.0",
                "setuptools v80.4.0 none",
                "tomli v1.2.3, v2.4.0 v1.2.3, v2.5.0"
            ]
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
    fn each_fix_is_its_measure_without_the_dry_run() {
        assert_eq!(fixes("cargo"), [["cargo", "update"]]);
        assert_eq!(fixes("uv"), [["uv", "lock", "--upgrade"]]);
        assert_eq!(fixes("go").len(), 2);
    }

    #[test]
    fn one_failed_tool_leaves_the_count_unmeasured() {
        let dir = std::env::temp_dir().join(format!("pma-deps-partial-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.lock"), "").unwrap();
        std::fs::write(dir.join("uv.lock"), "").unwrap();
        let facts = measure_with(&dir, |ecosystem, _| match ecosystem {
            "cargo" => Err("no network".to_string()),
            _ => Ok("Update ruff v1 -> v2\n".to_string()),
        });
        assert_eq!(facts.outdated, None);
        assert_eq!(facts.detail, "cargo: error: no network\nuv: ruff v1 -> v2");
        let facts = measure_with(&dir, |_, _| Ok(String::new()));
        assert_eq!(facts.outdated, Some(0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn projects_without_a_known_lock_file_are_unmeasured() {
        let dir = std::env::temp_dir().join(format!("pma-deps-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            measure(&dir),
            DepsFacts {
                outdated: None,
                held_back: None,
                detail: String::new()
            }
        );
        assert!(
            refresh(&dir)
                .unwrap_err()
                .ends_with("no Cargo.lock, uv.lock or go.mod")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
