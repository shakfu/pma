//! Collects facts about each project: open TODO.md items with their age,
//! local git state, the last commit that counts as activity, and CI status.
//!
//! Runs the `git` and `gh` binaries, so the user's own configuration and
//! GitHub authentication apply.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::todo::{self, Priority, normal_text};

#[derive(Debug, Clone, PartialEq)]
pub struct Facts {
    pub name: String,
    pub path: PathBuf,
    /// `owner/name` from the origin URL; `None` without a GitHub origin.
    pub slug: Option<String>,
    /// `None` when the project has no TODO.md, or when it could not be read.
    pub todo: Option<TodoFacts>,
    /// TODO.md exists but could not be read. The store keeps the tasks the
    /// last good scan recorded rather than taking the file as empty.
    pub todo_unread: bool,
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
/// project. `finished` is called with each project's name as it lands, from
/// whichever thread scanned it.
pub fn scan_all(
    projects: &[(String, PathBuf)],
    ignore: &[String],
    offline: bool,
    deps: bool,
    finished: &(dyn Fn(&str) + Sync),
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
                    finished(name);
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
        todo_unread: false,
        dirty: 0,
        pma_branches: Vec::new(),
        ahead: None,
        last_activity: None,
        ci: Ci::Unknown("offline".into()),
        deps: None,
        error: None,
    };

    let git = Git::new(path);
    match std::fs::read_to_string(path.join("TODO.md")) {
        Ok(text) => facts.todo = Some(todo_facts(&text, &added_times(&git))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            facts.todo_unread = true;
            facts.error = Some(format!("TODO.md: {e}"));
        }
    }

    facts.slug = git
        .run(&["remote", "get-url", "origin"])
        .and_then(|url| github_slug(url.trim()));

    match call(git.command(&["status", "--porcelain"]), CALL_TIMEOUT) {
        Ok(out) => facts.dirty = out.lines().count() as i64,
        Err(e) => {
            facts.error = Some(git.noted(Some(format!("git status: {e}"))));
            return facts;
        }
    }
    facts.pma_branches = git
        .run(&[
            "for-each-ref",
            "--format=%(refname:lstrip=2)",
            "refs/heads/pma/",
        ])
        .map(|out| out.lines().map(String::from).collect())
        .unwrap_or_default();
    facts.ahead = git
        .run(&["rev-list", "--count", "@{u}..HEAD"])
        .and_then(|s| s.trim().parse().ok());
    facts.last_activity = last_activity(&git, ignore);
    if !offline {
        facts.ci = ci_state(path);
        if deps {
            facts.deps = Some(crate::deps::measure(path));
        }
    }
    let noted = git.noted(facts.error.take());
    facts.error = (!noted.is_empty()).then_some(noted);
    facts
}

/// How long one `git` or `gh` call in a scan may take. One hung call would
/// otherwise hold the whole scan.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a bounded call produced no output.
#[derive(Debug, Clone, PartialEq)]
pub enum CallError {
    TimedOut(Duration),
    /// The first line of stderr, or why the program could not start.
    Failed(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::TimedOut(d) => write!(f, "timed out after {}s", d.as_secs()),
            CallError::Failed(why) => f.write_str(why),
        }
    }
}

/// Runs `cmd` with no input and returns its stdout when it succeeds. At
/// `timeout` its process group is killed, so a helper it started goes too.
pub fn call(mut cmd: Command, timeout: Duration) -> Result<String, CallError> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| CallError::Failed(format!("{}: {e}", cmd.get_program().display())))?;
    // Read on threads, so a child that fills a pipe is not stalled by it.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let mut pause = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= timeout => {
                let group = format!("-{}", child.id());
                let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(20));
            }
            Err(e) => return Err(CallError::Failed(e.to_string())),
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    match status {
        None => Err(CallError::TimedOut(timeout)),
        Some(s) if s.success() => Ok(String::from_utf8_lossy(&stdout).into_owned()),
        Some(_) => Err(CallError::Failed(gh_error(&String::from_utf8_lossy(
            &stderr,
        )))),
    }
}

/// `git -C dir` for one project's scan. A call that fails reads as absent, as
/// it always has; one that times out is also remembered, so the project's
/// detail says the facts are incomplete rather than that they are empty.
struct Git<'a> {
    dir: &'a Path,
    timed_out: Mutex<Vec<String>>,
}

impl<'a> Git<'a> {
    fn new(dir: &'a Path) -> Git<'a> {
        Git {
            dir,
            timed_out: Mutex::new(Vec::new()),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(self.dir).args(args);
        cmd
    }

    fn run(&self, args: &[&str]) -> Option<String> {
        match call(self.command(args), CALL_TIMEOUT) {
            Ok(out) => Some(out),
            Err(e @ CallError::TimedOut(_)) => {
                let what = args.first().copied().unwrap_or_default();
                self.timed_out
                    .lock()
                    .unwrap()
                    .push(format!("git {what}: {e}"));
                None
            }
            Err(CallError::Failed(_)) => None,
        }
    }

    /// `first`, then every timeout, joined for the project's detail.
    fn noted(&self, first: Option<String>) -> String {
        first
            .into_iter()
            .chain(self.timed_out.lock().unwrap().iter().cloned())
            .collect::<Vec<_>>()
            .join("; ")
    }
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

/// Runs `git -C dir`, bounded by `CALL_TIMEOUT`; stdout when it succeeds.
pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
    Git::new(dir).run(args)
}

/// Commit time at which each item text first appeared in TODO.md, keyed by
/// `normalise`. Matching on text rather than on lines keeps an item's age when
/// a later commit moves it, retags it, or changes the file's format, all of
/// which `git blame` reports as a new line.
fn added_times(git: &Git) -> HashMap<String, i64> {
    git.run(&[
        "log",
        "--reverse",
        "--no-merges",
        "--no-color",
        "--format=%x00%ct",
        "-p",
        "--unified=0",
        "--",
        "TODO.md",
    ])
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

fn last_activity(git: &Git, ignore: &[String]) -> Option<i64> {
    let log = git.run(&[
        "log",
        "-n",
        "1000",
        "--no-merges",
        "--format=%x00%ct",
        "--name-only",
    ])?;
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

/// Judged over the workflows that still exist and are enabled. `gh run list`
/// keeps a deleted workflow's runs, whose last failure would otherwise read as
/// failing CI until newer runs pushed it out of the window.
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
    let gh = |args: &[&str]| {
        let mut cmd = Command::new("gh");
        cmd.args(args);
        call(cmd, CALL_TIMEOUT).map_err(|e| Ci::Unknown(format!("gh {}: {e}", args[..2].join(" "))))
    };
    let runs = |extra: &[&str]| {
        let mut args = vec!["run", "list", "-R", &slug, "--branch", &branch];
        args.extend_from_slice(extra);
        args.extend([
            "--json",
            "workflowName,status,conclusion",
            "--jq",
            ".[] | [.workflowName, .status, .conclusion] | @tsv",
        ]);
        gh(&args).map(|tsv| latest_verdicts(&tsv))
    };
    let judged = (|| {
        let listed = gh(&[
            "workflow",
            "list",
            "-R",
            &slug,
            "--limit",
            "200",
            "--json",
            "id,name,state",
        ])?;
        let active = active_workflows(&listed).map_err(Ci::Unknown)?;
        let mut verdicts = runs(&["--limit", "100"])?;
        // A workflow that runs rarely can fall outside that window, so it is
        // asked about alone rather than read as having no runs.
        for (id, name) in &active {
            if !verdicts.contains_key(name) {
                let id = id.to_string();
                if let Some(&failed) = runs(&["--workflow", &id, "--limit", "20"])?.get(name) {
                    verdicts.insert(name.clone(), failed);
                }
            }
        }
        Ok(ci_of(&active, &verdicts))
    })();
    judged.unwrap_or_else(|unknown| unknown)
}

/// Enabled workflows as `(id, name)`, from `gh workflow list --json id,name,state`.
fn active_workflows(json: &str) -> Result<Vec<(u64, String)>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("gh workflow list: {e}"))?;
    let rows = value
        .as_array()
        .ok_or("gh workflow list: expected an array")?;
    Ok(rows
        .iter()
        .filter(|w| w["state"].as_str() == Some("active"))
        .filter_map(|w| Some((w["id"].as_u64()?, w["name"].as_str()?.to_string())))
        .collect())
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
/// Each workflow is judged by its latest run that succeeded or failed; the value
/// is whether that run failed.
fn latest_verdicts(tsv: &str) -> HashMap<String, bool> {
    let mut verdicts = HashMap::new();
    for line in tsv.lines() {
        let mut f = line.split('\t');
        let (Some(name), Some(status), Some(conclusion)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        if let Some(failed) = failed(status, conclusion) {
            verdicts.entry(name.to_string()).or_insert(failed);
        }
    }
    verdicts
}

/// The project's CI state over its active workflows. A verdict for a workflow
/// that is not active is ignored.
fn ci_of(active: &[(u64, String)], verdicts: &HashMap<String, bool>) -> Ci {
    let judged: Vec<(&String, bool)> = active
        .iter()
        .filter_map(|(_, name)| verdicts.get(name).map(|f| (name, *f)))
        .collect();
    let mut failing: Vec<String> = judged
        .iter()
        .filter(|(_, failed)| *failed)
        .map(|(name, _)| (*name).clone())
        .collect();
    failing.sort();
    failing.dedup();
    match (failing.is_empty(), judged.is_empty()) {
        (false, _) => Ci::Failing(failing),
        (true, false) => Ci::Passing,
        (true, true) => Ci::NoRuns,
    }
}

/// `owner/name` when the URL's host is exactly `github.com`, in the forms git
/// accepts: `https://[user@]github.com/o/r`, `ssh://git@github.com[:port]/o/r`
/// and `git@github.com:o/r`.
pub fn github_slug(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    // Drop userinfo: an `@` before the path's first `/`.
    let path_at = rest.find('/').unwrap_or(rest.len());
    let rest = match rest[..path_at].rfind('@') {
        Some(at) => &rest[at + 1..],
        None => rest,
    };
    let rest = rest.strip_prefix("github.com")?;
    // An explicit port, as `ssh://git@github.com:22/o/r` writes it.
    let rest = match rest.strip_prefix(':') {
        Some(r)
            if r.split('/')
                .next()
                .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) =>
        {
            &r[r.find('/').unwrap_or(r.len())..]
        }
        Some(r) => r,
        None => rest.strip_prefix('/')?,
    };
    let rest = rest.strip_prefix('/').unwrap_or(rest);
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
        let active = |names: &[&str]| -> Vec<(u64, String)> {
            names
                .iter()
                .enumerate()
                .map(|(i, n)| (i as u64, n.to_string()))
                .collect()
        };
        let tsv = "test\tin_progress\t\n\
                   test\tcompleted\tcancelled\n\
                   test\tcompleted\tfailure\n\
                   wheels\tcompleted\tsuccess\n\
                   wheels\tcompleted\tfailure\n\
                   test\tcompleted\tsuccess\n";
        let verdicts = latest_verdicts(tsv);
        assert_eq!(
            ci_of(&active(&["test", "wheels"]), &verdicts),
            Ci::Failing(vec!["test".into()])
        );
        assert_eq!(
            ci_of(
                &active(&["a", "b"]),
                &latest_verdicts("a\tcompleted\tsuccess\nb\tcompleted\tskipped\n")
            ),
            Ci::Passing
        );
        assert_eq!(
            ci_of(
                &active(&["a"]),
                &latest_verdicts("a\tcompleted\tcancelled\n")
            ),
            Ci::NoRuns
        );
        assert_eq!(ci_of(&active(&["a"]), &latest_verdicts("")), Ci::NoRuns);
    }

    /// A deleted or disabled workflow keeps its runs in `gh run list`; its last
    /// failure is not the project's CI state.
    #[test]
    fn only_active_workflows_are_judged() {
        let listed = r#"[{"id": 1, "name": "test", "state": "active"},
                         {"id": 2, "name": "nightly", "state": "disabled_manually"}]"#;
        let active = active_workflows(listed).unwrap();
        assert_eq!(active, [(1, "test".to_string())]);
        let verdicts = latest_verdicts(
            "old\tcompleted\tfailure\nnightly\tcompleted\tfailure\ntest\tcompleted\tsuccess\n",
        );
        assert_eq!(ci_of(&active, &verdicts), Ci::Passing);
        assert!(active_workflows("{}").is_err());
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

    /// A hung `git` or `gh` would hold the whole scan. It is killed at the
    /// deadline, with anything it started, and the call reads as timed out.
    #[test]
    fn a_call_is_killed_at_its_deadline() {
        let sh = |script: &str| {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", script]);
            cmd
        };
        let started = Instant::now();
        let limit = Duration::from_millis(300);
        assert_eq!(
            call(sh("sleep 30 & sleep 30"), limit),
            Err(CallError::TimedOut(limit))
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(call(sh("echo out"), limit), Ok("out\n".into()));
        assert_eq!(
            call(sh("echo why >&2; exit 3"), limit),
            Err(CallError::Failed("why".into()))
        );
        assert!(matches!(
            call(Command::new("/no/such/program"), limit),
            Err(CallError::Failed(_))
        ));
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
            ("https://notgithub.com/shakfu/pma", None),
            ("https://github.com.evil.io/shakfu/pma", None),
            ("https://me@github.com/shakfu/pma.git", Some("shakfu/pma")),
            ("ssh://git@github.com:22/shakfu/pma.git", Some("shakfu/pma")),
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
            ("- [ ] with an id #bug ^k3f9q", "with an id"),
            ("- [ ] bump to ^1.2", "bump to ^1.2"),
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
