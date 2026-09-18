//! Collects facts about each project: open TODO.md items with their age,
//! local git state, the last commit that counts as activity, and CI status.
//!
//! Runs the `git` and `gh` binaries, so the user's own configuration and
//! GitHub authentication apply.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::todo::{self, Priority, normal_text};

#[derive(Debug, Clone, PartialEq)]
pub struct Facts {
    pub name: String,
    pub path: PathBuf,
    /// `owner/name` from the origin URL; `None` without a GitHub origin.
    pub slug: Option<String>,
    /// `None` when the project has no TODO.md.
    pub todo: Option<TodoFacts>,
    pub dirty: i64,
    /// Local branches under `pma/`, each made for a run's worktree.
    pub pma_branches: Vec<String>,
    /// Commits not on the upstream; `None` without an upstream.
    pub ahead: Option<i64>,
    /// Unix time of the newest commit touching a path outside `activity.ignore`.
    pub last_activity: Option<i64>,
    pub ci: Ci,
    /// `None` when this scan did not measure dependencies.
    pub deps: Option<DepsFacts>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DepsFacts {
    /// Outdated dependencies; `None` when no ecosystem applies or every
    /// tool failed.
    pub outdated: Option<i64>,
    /// One `ecosystem: name current -> latest`, or `ecosystem: error: ...`, per line.
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TodoFacts {
    pub lint_errors: i64,
    /// Open items in priority sections.
    pub items: Vec<ScannedItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScannedItem {
    /// Identity across scans: `gh:N`, or the lowercased text.
    pub key: String,
    pub line: i64,
    pub priority: Priority,
    pub text: String,
    pub tags: Vec<String>,
    pub due: Option<String>,
    pub gh: Option<i64>,
    /// The nearest `###` heading above the item.
    pub group: Option<String>,
    /// Commit time at which the item's text first appeared in TODO.md.
    pub added_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Ci {
    Unknown(String),
    NoRuns,
    Passing,
    /// Workflows whose latest completed run on the default branch failed.
    Failing(Vec<String>),
}

impl Ci {
    pub fn to_columns(&self) -> (&'static str, String) {
        match self {
            Ci::Unknown(why) => ("unknown", why.clone()),
            Ci::NoRuns => ("none", String::new()),
            Ci::Passing => ("passing", String::new()),
            Ci::Failing(names) => ("failing", names.join("\n")),
        }
    }

    pub fn from_columns(state: &str, detail: String) -> Ci {
        match state {
            "none" => Ci::NoRuns,
            "passing" => Ci::Passing,
            "failing" => Ci::Failing(detail.lines().map(String::from).collect()),
            _ => Ci::Unknown(detail),
        }
    }
}

/// Git repositories directly under each root, or the root itself when it is
/// one. Names are directory names; a repeated name is reported and skipped.
pub fn discover(roots: &[PathBuf]) -> (Vec<(String, PathBuf)>, Vec<String>) {
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    let mut warnings = Vec::new();
    for root in roots {
        let candidates: Vec<PathBuf> = if root.join(".git").exists() {
            vec![root.clone()]
        } else {
            match std::fs::read_dir(root) {
                Ok(entries) => {
                    let mut dirs: Vec<PathBuf> = entries
                        .filter_map(|e| e.ok().map(|e| e.path()))
                        .filter(|p| p.join(".git").exists())
                        .collect();
                    dirs.sort();
                    dirs
                }
                Err(e) => {
                    warnings.push(format!("{}: {e}", root.display()));
                    continue;
                }
            }
        };
        for path in candidates {
            let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
                warnings.push(format!("{}: name is not valid UTF-8", path.display()));
                continue;
            };
            if let Some((_, first)) = found.iter().find(|(n, _)| *n == name) {
                warnings.push(format!(
                    "{}: skipped, same name as {}",
                    path.display(),
                    first.display()
                ));
                continue;
            }
            found.push((name, path));
        }
    }
    (found, warnings)
}

/// Scans projects on up to 8 threads, returning facts in input order.
/// Dependencies are measured only with `deps`, since it takes seconds per
/// project.
pub fn scan_all(
    projects: &[(String, PathBuf)],
    ignore: &[String],
    offline: bool,
    deps: bool,
) -> Vec<Facts> {
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(projects.len()));
    std::thread::scope(|s| {
        for _ in 0..projects.len().clamp(1, 8) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((name, path)) = projects.get(i) else {
                        break;
                    };
                    let facts = scan_project(name, path, ignore, offline, deps);
                    results.lock().unwrap().push((i, facts));
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|(i, _)| *i);
    results.into_iter().map(|(_, f)| f).collect()
}

pub fn scan_project(
    name: &str,
    path: &Path,
    ignore: &[String],
    offline: bool,
    deps: bool,
) -> Facts {
    let mut facts = Facts {
        name: name.into(),
        path: path.into(),
        slug: None,
        todo: None,
        dirty: 0,
        pma_branches: Vec::new(),
        ahead: None,
        last_activity: None,
        ci: Ci::Unknown("offline".into()),
        deps: None,
        error: None,
    };

    match std::fs::read_to_string(path.join("TODO.md")) {
        Ok(text) => facts.todo = Some(todo_facts(&text, &added_times(path))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => facts.error = Some(format!("TODO.md: {e}")),
    }

    facts.slug =
        git(path, &["remote", "get-url", "origin"]).and_then(|url| github_slug(url.trim()));

    match git(path, &["status", "--porcelain"]) {
        Some(out) => facts.dirty = out.lines().count() as i64,
        None => {
            facts.error = Some("git status failed".into());
            return facts;
        }
    }
    facts.pma_branches = git(
        path,
        &[
            "for-each-ref",
            "--format=%(refname:lstrip=2)",
            "refs/heads/pma/",
        ],
    )
    .map(|out| out.lines().map(String::from).collect())
    .unwrap_or_default();
    facts.ahead =
        git(path, &["rev-list", "--count", "@{u}..HEAD"]).and_then(|s| s.trim().parse().ok());
    facts.last_activity = last_activity(path, ignore);
    if !offline {
        facts.ci = ci_state(path);
        if deps {
            facts.deps = Some(crate::deps::measure(path));
        }
    }
    facts
}

fn todo_facts(text: &str, added: &HashMap<String, i64>) -> TodoFacts {
    let parsed = todo::parse(text);
    let lint_errors = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity == todo::Severity::Error)
        .count();
    let mut keys = HashSet::new();
    let items = parsed
        .items
        .into_iter()
        .filter_map(|item| {
            if item.done {
                return None;
            }
            let normal = normal_text(&item.text);
            let key = item.key();
            // Duplicates are lint errors; the first occurrence stands.
            if !keys.insert(key.clone()) {
                return None;
            }
            let line = item.line as i64;
            Some(ScannedItem {
                key,
                line,
                priority: item.priority,
                text: item.text,
                tags: item.tags,
                due: item.due,
                gh: item.gh.map(|n| n as i64),
                group: item.group,
                added_at: added.get(&normal).copied(),
            })
        })
        .collect();
    TodoFacts {
        lint_errors: lint_errors as i64,
        items,
    }
}

/// Runs `git -C dir`; stdout when it succeeds.
pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Commit time at which each item text first appeared in TODO.md, keyed by
/// `normalise`. Matching on text rather than on lines keeps an item's age when
/// a later commit moves it, retags it, or changes the file's format, all of
/// which `git blame` reports as a new line.
fn added_times(dir: &Path) -> HashMap<String, i64> {
    git(
        dir,
        &[
            "log",
            "--reverse",
            "--no-merges",
            "--no-color",
            "--format=%x00%ct",
            "-p",
            "--unified=0",
            "--",
            "TODO.md",
        ],
    )
    .map(|log| parse_history(&log))
    .unwrap_or_default()
}

fn parse_history(log: &str) -> HashMap<String, i64> {
    let mut first = HashMap::new();
    for record in log.split('\0').skip(1) {
        let mut lines = record.lines();
        let Some(time) = lines.next().and_then(|l| l.trim().parse::<i64>().ok()) else {
            continue;
        };
        for line in lines {
            if line.starts_with("+++ ") {
                continue;
            }
            if let Some(added) = line.strip_prefix('+') {
                let key = normalise(added);
                if !key.is_empty() {
                    first.entry(key).or_insert(time);
                }
            }
        }
    }
    first
}

/// Reduces a raw TODO.md line to the text `normal_text` gives for the item it
/// holds: no heading marks, list marker, checkbox or trailing tokens.
fn normalise(line: &str) -> String {
    let mut s = line.trim_start();
    if s.starts_with('#') {
        s = s.trim_start_matches('#');
    }
    // A marker counts only when a space follows, so `**bold**` keeps its stars.
    if let Some(rest) = ["- ", "* ", "+ "].iter().find_map(|m| s.strip_prefix(m)) {
        s = rest;
    } else {
        let digits = s.trim_start_matches(|c: char| c.is_ascii_digit());
        if digits.len() < s.len()
            && let Some(rest) = digits
                .strip_prefix(". ")
                .or_else(|| digits.strip_prefix(") "))
        {
            s = rest;
        }
    }
    s = s.trim_start();
    for mark in ["[ ]", "[x]", "[X]"] {
        if let Some(rest) = s.strip_prefix(mark) {
            s = rest.trim_start();
            break;
        }
    }
    let mut words: Vec<&str> = s.split_whitespace().collect();
    while words.last().is_some_and(|w| todo::is_token(w)) {
        words.pop();
    }
    normal_text(&words.join(" "))
}

fn last_activity(dir: &Path, ignore: &[String]) -> Option<i64> {
    let log = git(
        dir,
        &[
            "log",
            "-n",
            "1000",
            "--no-merges",
            "--format=%x00%ct",
            "--name-only",
        ],
    )?;
    newest_counted_commit(&log, ignore)
}

fn newest_counted_commit(log: &str, ignore: &[String]) -> Option<i64> {
    log.split('\0').skip(1).find_map(|record| {
        let mut lines = record.lines();
        let time: i64 = lines.next()?.trim().parse().ok()?;
        lines
            .filter(|p| !p.is_empty())
            .any(|p| !ignore.iter().any(|g| glob_match(g, p)))
            .then_some(time)
    })
}

/// `*` matches within one path segment, `**` any number of segments. A
/// pattern without `/` matches the file name in any directory.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    if !pattern.contains('/') {
        return segment_match(
            pattern.as_bytes(),
            path.rsplit('/').next().unwrap_or(path).as_bytes(),
        );
    }
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    parts_match(&p, &s)
}

fn parts_match(p: &[&str], s: &[&str]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((&"**", rest)) => (0..=s.len()).any(|i| parts_match(rest, &s[i..])),
        Some((first, rest)) => {
            !s.is_empty()
                && segment_match(first.as_bytes(), s[0].as_bytes())
                && parts_match(rest, &s[1..])
        }
    }
}

fn segment_match(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if pi < p.len() && p[pi] == s[si] {
            pi += 1;
            si += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&b| b == b'*')
}

pub const DEFAULT_BRANCH_UNKNOWN: &str =
    "default branch unknown; run `git remote set-head origin -a`";

/// The branch `origin/HEAD` points at, without the remote prefix.
pub fn default_branch(dir: &Path) -> Option<String> {
    let full = git(
        dir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )?;
    Some(full.trim().trim_start_matches("origin/").to_string())
}

fn ci_state(dir: &Path) -> Ci {
    let Some(url) = git(dir, &["remote", "get-url", "origin"]) else {
        return Ci::Unknown("no origin remote".into());
    };
    let Some(slug) = github_slug(url.trim()) else {
        return Ci::Unknown("origin is not on GitHub".into());
    };
    let Some(branch) = default_branch(dir) else {
        return Ci::Unknown(DEFAULT_BRANCH_UNKNOWN.into());
    };
    let branch = branch.as_str();
    let out = Command::new("gh")
        .args([
            "run", "list", "-R", &slug, "--branch", branch, "--limit", "50",
        ])
        .args(["--json", "workflowName,status,conclusion"])
        .args(["--jq", ".[] | [.workflowName, .status, .conclusion] | @tsv"])
        .output();
    match out {
        Ok(o) if o.status.success() => parse_runs(&String::from_utf8_lossy(&o.stdout)),
        Ok(o) => Ci::Unknown(gh_error(&String::from_utf8_lossy(&o.stderr))),
        Err(e) => Ci::Unknown(format!("gh: {e}")),
    }
}

/// The first line of `gh`'s stderr. It names the cause; later lines add
/// alternatives or an update notice.
fn gh_error(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("gh failed")
        .to_string()
}

/// Whether a run failed: `None` for a run that is unfinished, cancelled or
/// skipped, which does not decide a workflow's state.
fn failed(status: &str, conclusion: &str) -> Option<bool> {
    if status != "completed" {
        return None;
    }
    match conclusion {
        "success" => Some(false),
        "failure" | "timed_out" | "startup_failure" => Some(true),
        _ => None,
    }
}

/// The id of a workflow's latest decisive run when that run failed, from
/// `gh run list --json databaseId,status,conclusion`, newest first. `None`
/// when the workflow now passes or has no decisive run.
pub fn latest_failed_run(json: &str) -> Result<Option<u64>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("gh run list: {e}"))?;
    let runs = value.as_array().ok_or("gh run list: expected an array")?;
    Ok(runs
        .iter()
        .find_map(|r| {
            failed(r["status"].as_str()?, r["conclusion"].as_str()?)
                .map(|f| f.then(|| r["databaseId"].as_u64()).flatten())
        })
        .flatten())
}

/// Reads `gh run list` rows, newest first, as `workflow \t status \t conclusion`.
/// Each workflow is judged by its latest run that succeeded or failed.
fn parse_runs(tsv: &str) -> Ci {
    let mut decided = HashSet::new();
    let mut failing = Vec::new();
    for line in tsv.lines() {
        let mut f = line.split('\t');
        let (Some(name), Some(status), Some(conclusion)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        if decided.contains(name) {
            continue;
        }
        match failed(status, conclusion) {
            None => continue,
            Some(true) => failing.push(name.to_string()),
            Some(false) => {}
        }
        decided.insert(name);
    }
    failing.sort();
    match (failing.is_empty(), decided.is_empty()) {
        (false, _) => Ci::Failing(failing),
        (true, false) => Ci::Passing,
        (true, true) => Ci::NoRuns,
    }
}

pub fn github_slug(url: &str) -> Option<String> {
    let rest = url.split_once("github.com")?.1;
    let rest = rest.strip_prefix(':').or_else(|| rest.strip_prefix('/'))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    (!owner.is_empty() && !repo.is_empty()).then(|| format!("{owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        for (pattern, path, want) in [
            (".github/**", ".github/workflows/test.yml", true),
            (".github/**", ".github", true),
            (".github/**", "src/.github/x", false),
            ("*.lock", "Cargo.lock", true),
            ("*.lock", "sub/dir/uv.lock", true),
            ("*.lock", "lock", false),
            ("TODO.md", "TODO.md", true),
            ("TODO.md", "docs/TODO.md", true),
            ("docs/*.md", "docs/a.md", true),
            ("docs/*.md", "docs/sub/a.md", false),
            ("docs/**/*.md", "docs/sub/a.md", true),
            ("docs/**/*.md", "docs/a.md", true),
            ("a*b*c", "aXXbYYc", true),
            ("a*b*c", "aXXbYY", false),
        ] {
            assert_eq!(glob_match(pattern, path), want, "{pattern} vs {path}");
        }
    }

    #[test]
    fn activity_skips_commits_that_only_touch_ignored_paths() {
        let ignore = [".github/**".to_string(), "TODO.md".to_string()];
        let log = "\x00300\n\nTODO.md\n\x00200\n\n.github/workflows/a.yml\n.github/workflows/b.yml\n\x00100\n\nsrc/main.rs\nTODO.md\n";
        assert_eq!(newest_counted_commit(log, &ignore), Some(100));
        assert_eq!(newest_counted_commit("\x00300\n\nTODO.md\n", &ignore), None);
        assert_eq!(newest_counted_commit("", &ignore), None);
    }

    #[test]
    fn ci_is_judged_by_each_workflows_latest_decisive_run() {
        let tsv = "test\tin_progress\t\n\
                   test\tcompleted\tcancelled\n\
                   test\tcompleted\tfailure\n\
                   wheels\tcompleted\tsuccess\n\
                   wheels\tcompleted\tfailure\n\
                   test\tcompleted\tsuccess\n";
        assert_eq!(parse_runs(tsv), Ci::Failing(vec!["test".into()]));
        assert_eq!(
            parse_runs("a\tcompleted\tsuccess\nb\tcompleted\tskipped\n"),
            Ci::Passing
        );
        assert_eq!(parse_runs("a\tcompleted\tcancelled\n"), Ci::NoRuns);
        assert_eq!(parse_runs(""), Ci::NoRuns);
    }

    #[test]
    fn a_workflow_failed_when_its_latest_decisive_run_failed() {
        let json = |rows: &str| format!("[{rows}]");
        let run = |id: u64, status: &str, conclusion: &str| {
            format!(r#"{{"databaseId":{id},"status":"{status}","conclusion":"{conclusion}"}}"#)
        };
        let failing = json(
            &[
                run(9, "in_progress", ""),
                run(8, "completed", "cancelled"),
                run(7, "completed", "timed_out"),
                run(6, "completed", "success"),
            ]
            .join(","),
        );
        assert_eq!(latest_failed_run(&failing), Ok(Some(7)));
        let fixed = json(
            &[
                run(5, "completed", "success"),
                run(4, "completed", "failure"),
            ]
            .join(","),
        );
        assert_eq!(latest_failed_run(&fixed), Ok(None), "fixed since");
        assert_eq!(latest_failed_run("[]"), Ok(None));
        assert!(latest_failed_run("{}").is_err());
    }

    #[test]
    fn gh_errors_keep_the_first_line() {
        let unauthenticated = "To get started with GitHub CLI, please run:  gh auth login\n\
            Alternatively, populate the GH_TOKEN environment variable with a GitHub API authentication token.\n";
        assert_eq!(
            gh_error(unauthenticated),
            "To get started with GitHub CLI, please run:  gh auth login"
        );
        assert_eq!(gh_error("\n  HTTP 404  \n"), "HTTP 404");
        assert_eq!(gh_error(""), "gh failed");
    }

    #[test]
    fn ci_columns_round_trip() {
        for ci in [
            Ci::Unknown("offline".into()),
            Ci::NoRuns,
            Ci::Passing,
            Ci::Failing(vec!["a, b".into(), "c".into()]),
        ] {
            let (state, detail) = ci.to_columns();
            assert_eq!(Ci::from_columns(state, detail), ci);
        }
    }

    #[test]
    fn github_slugs() {
        for (url, want) in [
            ("git@github.com:shakfu/pma.git", Some("shakfu/pma")),
            ("https://github.com/shakfu/pma", Some("shakfu/pma")),
            ("https://github.com/shakfu/pma.git/", Some("shakfu/pma")),
            ("ssh://git@github.com/shakfu/pma.git", Some("shakfu/pma")),
            ("git@gitlab.com:shakfu/pma.git", None),
            ("https://github.com/shakfu", None),
        ] {
            assert_eq!(github_slug(url).as_deref(), want, "{url}");
        }
    }

    #[test]
    fn normalise_strips_structure_and_tokens() {
        for (line, want) in [
            (
                "- [ ] Fix  the Lexer #bug due:2026-01-01 gh:3",
                "fix the lexer",
            ),
            ("### Phrase arrays leaked", "phrase arrays leaked"),
            ("* [X] done thing", "done thing"),
            ("12. [ ] numbered", "numbered"),
            ("  - plain bullet", "plain bullet"),
            ("port C# #bindings", "port c#"),
            ("- [ ] **Bold** lead #bugs", "**bold** lead"),
            ("**Bold** paragraph", "**bold** paragraph"),
            ("-dash start", "-dash start"),
            ("", ""),
        ] {
            assert_eq!(normalise(line), want, "{line:?}");
        }
    }

    #[test]
    fn history_keeps_the_first_appearance_of_each_text() {
        let log = "\x00100\n\ndiff --git a/TODO.md b/TODO.md\n--- /dev/null\n+++ b/TODO.md\n@@ -0,0 +1,3 @@\n+# TODO\n+### Phrase arrays leaked\n+- [ ] other\n\
                   \x00200\n\ndiff --git a/TODO.md b/TODO.md\n--- a/TODO.md\n+++ b/TODO.md\n@@ -2 +2,2 @@\n-### Phrase arrays leaked\n+- [ ] Phrase arrays leaked #tracker\n+- [ ] new\n";
        let first = parse_history(log);
        assert_eq!(
            first.get("phrase arrays leaked"),
            Some(&100),
            "migrating the line keeps its age"
        );
        assert_eq!(first.get("new"), Some(&200));
        assert_eq!(first.get("other"), Some(&100));
        assert!(
            !first.contains_key("b/todo.md"),
            "diff headers are not items"
        );
    }

    #[test]
    fn items_match_their_history_lines() {
        for line in [
            "- [ ] **Ctrl-C** stops `sd` #bugs",
            "### Phrase *arrays* leaked",
            "* [X] 1. odd",
        ] {
            let parsed = todo::parse(&format!("# TODO\n\n## High\n\n- [ ] {}\n", normalise(line)));
            let text = &parsed.items[0].text;
            assert_eq!(normal_text(text), normalise(line), "{line:?}");
        }
        let history = "\x00100\n+- [ ] **Ctrl-C** stops `sd`\n";
        let facts = todo_facts(
            "# TODO\n\n## High\n\n- [ ] **Ctrl-C** stops `sd` #bugs\n",
            &parse_history(history),
        );
        assert_eq!(facts.items[0].added_at, Some(100));
    }

    #[test]
    fn todo_facts_keep_open_prioritised_items_once() {
        let text = "# TODO\n\n## High\n\n- [ ] one #a due:2026-10-01\n- [ ] linked gh:7\n- [ ] One\n- [x] finished\n";
        let facts = todo_facts(text, &HashMap::from([("one".to_string(), 50)]));
        assert_eq!(facts.lint_errors, 1, "the duplicate is a lint error");
        let got: Vec<_> = facts
            .items
            .iter()
            .map(|i| (i.key.as_str(), i.line, i.added_at))
            .collect();
        assert_eq!(got, [("one", 5, Some(50)), ("gh:7", 6, None)]);
        assert_eq!(facts.items[0].tags, ["a"]);
        assert_eq!(facts.items[0].due.as_deref(), Some("2026-10-01"));
    }
}
