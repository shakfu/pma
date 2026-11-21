//! How far each lock can move within the project's declared constraints, as
//! each tool reports it. Each measure is its fix run dry; see
//! `docs/dev/deps-refresh.md`.
//!
//! - cargo: `cargo update --dry-run`, transitive crates included.
//! - uv: `uv lock --upgrade --dry-run`, transitive packages included.
//! - go: `go list -u -m all`, direct modules with any newer version.
//! - npm, pnpm: `update` with a lock-only flag on a copy of `package.json`
//!   and the lock, compared with the lock; transitive packages included.
//!
//! None writes to the project. Each needs the network.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::agent;
use crate::scan::DepsFacts;

/// How long one tool may run.
const TIMEOUT: Duration = Duration::from_secs(180);

/// Each ecosystem, the root file that says it applies, and its lock.
pub const ECOSYSTEMS: [(&str, &str, &str); 5] = [
    ("cargo", "Cargo.lock", "Cargo.lock"),
    ("uv", "uv.lock", "uv.lock"),
    ("go", "go.mod", "go.sum"),
    ("npm", "package-lock.json", "package-lock.json"),
    ("pnpm", "pnpm-lock.yaml", "pnpm-lock.yaml"),
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
                    "go" => parse_go(&output),
                    _ => parse_moves(&output),
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
        "go" => &[&["go", "get", "-u", "./..."], &["go", "mod", "tidy"]],
        "npm" => &[&["npm", "update"]],
        _ => &[&["pnpm", "update"]],
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
        let markers: Vec<&str> = ECOSYSTEMS.iter().map(|(_, m, _)| *m).collect();
        return Err(format!("{}: none of {}", dir.display(), markers.join(", ")));
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
    let cmd = match ecosystem {
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
        "npm" | "pnpm" => return node_moves(ecosystem, dir),
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
    limited(cmd, dir)
}

/// `cmd`'s output in `dir`, bounded by `TIMEOUT`, or why it failed.
fn limited(mut cmd: Command, dir: &Path) -> Result<String, String> {
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

/// What `npm update` or `pnpm update` would move, as `name current -> latest`
/// lines. Neither has a dry run that reads the lock alone: npm's compares
/// with `node_modules`, which a clone lacks. So each updates the lock alone
/// in a copy of the manifest and lock, and the two locks are compared.
fn node_moves(ecosystem: &str, dir: &Path) -> Result<String, String> {
    let lock = match ecosystem {
        "npm" => "package-lock.json",
        _ => "pnpm-lock.yaml",
    };
    let manifest = std::fs::read_to_string(dir.join("package.json"))
        .map_err(|e| format!("package.json: {e}"))?;
    let workspaces = serde_json::from_str::<serde_json::Value>(&manifest)
        .is_ok_and(|v| v.get("workspaces").is_some());
    if workspaces || dir.join("pnpm-workspace.yaml").is_file() {
        return Err("workspaces are not measured".into());
    }
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let copy = std::env::temp_dir().join(format!(
        "pma-deps-{ecosystem}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&copy).map_err(|e| format!("{}: {e}", copy.display()))?;
    let moved = (|| -> Result<Vec<Outdated>, String> {
        for file in ["package.json", lock, ".npmrc"] {
            if dir.join(file).is_file() {
                std::fs::copy(dir.join(file), copy.join(file))
                    .map_err(|e| format!("{file}: {e}"))?;
            }
        }
        let read =
            |d: &Path| std::fs::read_to_string(d.join(lock)).map_err(|e| format!("{lock}: {e}"));
        let before = read(&copy)?;
        let mut cmd = Command::new(ecosystem);
        cmd.args(match ecosystem {
            "npm" => &[
                "update",
                "--package-lock-only",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
            ][..],
            _ => &["update", "--lockfile-only", "--ignore-scripts"],
        });
        limited(cmd, &copy)?;
        let after = read(&copy)?;
        let versions = |t: &str| match ecosystem {
            "npm" => npm_versions(t),
            _ => Ok(pnpm_versions(t)),
        };
        Ok(moves(&versions(&before)?, &versions(&after)?))
    })();
    let _ = std::fs::remove_dir_all(&copy);
    Ok(moved?
        .iter()
        .map(|o| format!("{} {} -> {}", o.name, o.current, o.latest))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Each installed package and its version in a `package-lock.json`, from its
/// `packages` map; a linked workspace package has none.
fn npm_versions(lock: &str) -> Result<Vec<(String, String)>, String> {
    let v: serde_json::Value =
        serde_json::from_str(lock).map_err(|e| format!("package-lock.json: {e}"))?;
    let packages = v["packages"]
        .as_object()
        .ok_or("package-lock.json has no `packages`; npm 7 or later writes it")?;
    Ok(packages
        .iter()
        .filter_map(|(path, p)| {
            let name = path.rsplit_once("node_modules/")?.1;
            Some((name.to_string(), p["version"].as_str()?.to_string()))
        })
        .collect())
}

/// Each `name@version` key of a `pnpm-lock.yaml`'s `packages` section;
/// lockfile 6 starts keys with `/`, and either may carry a `(peer)` suffix.
fn pnpm_versions(lock: &str) -> Vec<(String, String)> {
    lock.lines()
        .skip_while(|l| *l != "packages:")
        .skip(1)
        .take_while(|l| l.is_empty() || l.starts_with(' '))
        .filter(|l| l.starts_with("  ") && !l.starts_with("   ") && l.ends_with(':'))
        .filter_map(|l| {
            let key = l.trim().trim_end_matches(':').trim_matches('\'');
            let key = key.strip_prefix('/').unwrap_or(key);
            let key = key.split('(').next()?;
            let at = key.rfind('@').filter(|&i| i > 0)?;
            Some((key[..at].to_string(), key[at + 1..].to_string()))
        })
        .collect()
}

/// The packages whose locked versions differ, each version list joined as
/// uv joins one; `none` where a lock has no such package.
fn moves(before: &[(String, String)], after: &[(String, String)]) -> Vec<Outdated> {
    use std::collections::BTreeMap;
    let index = |list: &[(String, String)]| {
        let mut by: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (name, version) in list {
            by.entry(name.clone()).or_default().push(version.clone());
        }
        for versions in by.values_mut() {
            versions.sort();
            versions.dedup();
        }
        by
    };
    let (was, now) = (index(before), index(after));
    let joined = |v: Option<&Vec<String>>| v.map_or("none".to_string(), |v| v.join(", "));
    let mut names: Vec<&String> = was.keys().chain(now.keys()).collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter(|n| was.get(*n) != now.get(*n))
        .map(|n| Outdated {
            name: n.clone(),
            current: joined(was.get(n)),
            latest: joined(now.get(n)),
        })
        .collect()
}

/// `name current -> latest` lines, as `node_moves` writes them.
fn parse_moves(output: &str) -> Vec<Outdated> {
    output
        .lines()
        .filter_map(|l| {
            let (name, versions) = l.split_once(' ')?;
            let (current, latest) = versions.split_once(" -> ")?;
            Some(Outdated {
                name: name.into(),
                current: current.into(),
                latest: latest.into(),
            })
        })
        .collect()
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
    fn npm_locks_list_each_installed_version() {
        let lock = r#"{"name":"y","lockfileVersion":3,"packages":{
            "":{"name":"y","dependencies":{"debug":"^2.0.0"}},
            "node_modules/debug":{"version":"2.6.0"},
            "node_modules/ms":{"version":"0.7.2"},
            "node_modules/debug/node_modules/ms":{"version":"0.7.1"},
            "node_modules/@scope/pkg":{"version":"1.0.0"},
            "node_modules/local":{"resolved":"packages/local","link":true}}}"#;
        let mut got = npm_versions(lock).unwrap();
        got.sort();
        let want: Vec<(String, String)> = [
            ("@scope/pkg", "1.0.0"),
            ("debug", "2.6.0"),
            ("ms", "0.7.1"),
            ("ms", "0.7.2"),
        ]
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
        assert_eq!(got, want);
        assert!(
            npm_versions(r#"{"lockfileVersion":1,"dependencies":{}}"#)
                .unwrap_err()
                .contains("npm 7")
        );
    }

    #[test]
    fn pnpm_locks_list_their_packages_section() {
        let v9 = "lockfileVersion: '9.0'\n\nimporters:\n\n  .:\n    dependencies:\n      debug:\n        specifier: ^2.0.0\n        version: 2.6.0\n\npackages:\n\n  '@sindresorhus/is@4.0.0':\n    resolution: {integrity: sha512-x}\n    engines: {node: '>=10'}\n\n  debug@2.6.0:\n    resolution: {integrity: sha512-y}\n    peerDependencies:\n      supports-color: '*'\n\n  ms@0.7.2:\n    resolution: {integrity: sha512-z}\n\nsnapshots:\n\n  debug@2.6.0:\n    dependencies:\n      ms: 0.7.2\n";
        assert_eq!(
            pnpm_versions(v9),
            [
                ("@sindresorhus/is".to_string(), "4.0.0".to_string()),
                ("debug".into(), "2.6.0".into()),
                ("ms".into(), "0.7.2".into()),
            ]
        );
        let v6 = "lockfileVersion: '6.0'\n\npackages:\n\n  /react-dom@18.2.0(react@18.2.0):\n    resolution: {integrity: sha512-a}\n";
        assert_eq!(
            pnpm_versions(v6),
            [("react-dom".to_string(), "18.2.0".to_string())]
        );
    }

    #[test]
    fn a_lock_moves_where_a_package_changes_versions() {
        let list = |l: &[(&str, &str)]| -> Vec<(String, String)> {
            l.iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect()
        };
        let before = list(&[
            ("debug", "2.6.0"),
            ("ms", "0.7.2"),
            ("ms", "0.7.1"),
            ("old", "1.0.0"),
            ("same", "1.0.0"),
        ]);
        let after = list(&[
            ("debug", "2.6.9"),
            ("ms", "2.0.0"),
            ("new", "1.0.0"),
            ("same", "1.0.0"),
        ]);
        let out = names(moves(&before, &after));
        assert_eq!(
            out,
            [
                "debug 2.6.0 2.6.9",
                "ms 0.7.1, 0.7.2 2.0.0",
                "new none 1.0.0",
                "old 1.0.0 none"
            ]
        );
        let written =
            "debug 2.6.0 -> 2.6.9\nms 0.7.1, 0.7.2 -> 2.0.0\nnew none -> 1.0.0\nold 1.0.0 -> none";
        assert_eq!(names(parse_moves(written)), out);
    }

    #[test]
    fn each_fix_is_its_measure_without_the_dry_run() {
        assert_eq!(fixes("cargo"), [["cargo", "update"]]);
        assert_eq!(fixes("uv"), [["uv", "lock", "--upgrade"]]);
        assert_eq!(fixes("go").len(), 2);
        assert_eq!(fixes("npm"), [["npm", "update"]]);
        assert_eq!(fixes("pnpm"), [["pnpm", "update"]]);
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
            refresh(&dir).unwrap_err().ends_with(
                "none of Cargo.lock, uv.lock, go.mod, package-lock.json, pnpm-lock.yaml"
            )
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
