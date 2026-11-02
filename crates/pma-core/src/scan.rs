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

/// Why a project's facts are missing or suspect, worst first. Each ranks
/// just below the state it could hide; see `rank::State`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Fault {
    /// A failed run left hooks or git settings changed, so pma runs no git
    /// there until they are restored. See `dispatch::clone_held`.
    GitSettingsChanged,
    /// `git status` failed or timed out, so nothing after it was read.
    GitUnreadable,
    /// `gh` failed or timed out, or the default branch is unknown, so
    /// failing CI would go unseen. Not a fault offline or off GitHub.
    CiUnknown,
    /// The `pma/` branches could not be listed, or the upstream is gone, so
    /// unpublished work would go unseen.
    UnpublishedUnknown,
    /// TODO.md exists but could not be read.
    TodoUnreadable,
    /// TODO.md has lint errors, so its items may be misread.
    TodoLint,
    /// Another git call failed or timed out; the facts it reads are named.
    ScanPartial,
}

impl Fault {
    pub const ALL: [Fault; 7] = [
        Fault::GitSettingsChanged,
        Fault::GitUnreadable,
        Fault::CiUnknown,
        Fault::UnpublishedUnknown,
        Fault::TodoUnreadable,
        Fault::TodoLint,
        Fault::ScanPartial,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Fault::GitSettingsChanged => "git settings changed",
            Fault::GitUnreadable => "git unreadable",
            Fault::CiUnknown => "CI unknown",
            Fault::TodoUnreadable => "TODO unreadable",
            Fault::TodoLint => "TODO lint",
            Fault::UnpublishedUnknown => "unpublished unknown",
            Fault::ScanPartial => "scan partial",
        }
    }

    /// As stored.
    pub fn column(self) -> &'static str {
        match self {
            Fault::GitSettingsChanged => "git_settings_changed",
            Fault::GitUnreadable => "git_unreadable",
            Fault::CiUnknown => "ci_unknown",
            Fault::TodoUnreadable => "todo_unreadable",
            Fault::TodoLint => "todo_lint",
            Fault::UnpublishedUnknown => "unpublished_unknown",
            Fault::ScanPartial => "scan_partial",
        }
    }

    pub fn from_column(s: &str) -> Option<Fault> {
        Fault::ALL.into_iter().find(|f| f.column() == s)
    }
}

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
    /// Each failing workflow's latest failed run, as `(workflow, url)`.
    pub ci_runs: Vec<(String, String)>,
    /// When the stalest lock last changed; see `lock_changed_at`.
    pub lock_at: Option<i64>,
    /// `None` when this scan did not measure dependencies.
    pub deps: Option<DepsFacts>,
    /// Each fault the scan found, worst first, with what caused it.
    pub faults: Vec<(Fault, String)>,
    /// CI was asked: not offline, and git could be read.
    pub ci_read: bool,
    /// Not read, because git there would run a hook or setting a failed run
    /// found added. The store keeps what the last scan recorded, and `error`.
    pub held: bool,
    /// What `stamp` said before this read, so a change during it is seen later.
    pub stamp: String,
}

/// Changes when `TODO.md` or the repository's state does: a commit, a checkout,
/// a stage, a fetch, a push. An edit to a tracked file that is not staged does
/// not change it, so `dirty` can lag until the next full scan.
pub fn stamp(path: &Path) -> String {
    [
        "TODO.md",
        ".git/index",
        ".git/HEAD",
        ".git/logs/HEAD",
        ".git/FETCH_HEAD",
        // A push writes neither of the above. Git renames a ref's lock file
        // into this directory, which changes its time.
        ".git/refs/remotes/origin",
        ".git/packed-refs",
    ]
    .iter()
    .map(|f| match std::fs::metadata(path.join(f)) {
        Ok(m) => {
            let at = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{at}:{}", m.len())
        }
        Err(_) => "-".into(),
    })
    .collect::<Vec<_>>()
    .join(" ")
}

impl Facts {
    /// A project left unread, with the reason.
    pub fn held(name: &str, path: &Path, error: String) -> Facts {
        Facts {
            held: true,
            faults: vec![(Fault::GitSettingsChanged, error)],
            ..Facts::blank(name, path)
        }
    }

    fn blank(name: &str, path: &Path) -> Facts {
        Facts {
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
            ci_runs: Vec::new(),
            lock_at: None,
            deps: None,
            faults: Vec::new(),
            ci_read: false,
            held: false,
            stamp: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DepsFacts {
    /// Packages the locks would move; `None` when no ecosystem applies or
    /// every tool failed.
    pub outdated: Option<i64>,
    /// Crates cargo's constraints hold back from their latest release.
    pub held_back: Option<i64>,
    /// One `ecosystem: name current -> latest`, or `ecosystem: error: ...`, per line.
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TodoFacts {
    pub lint_errors: i64,
    /// The first lint error, as `line N: message`.
    pub first_lint: Option<String>,
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
    let mut facts = Facts::blank(name, path);
    facts.stamp = stamp(path);

    let git = Git::new(path);
    let status = call(
        git.command(&["status", "--porcelain=v2", "--branch"]),
        CALL_TIMEOUT,
    )
    .map(|out| Status::parse(&out));
    if let Err(e) = &status {
        facts
            .faults
            .push((Fault::GitUnreadable, format!("git status: {e}")));
    }
    // With no commit, `git log` fails and has nothing to read.
    let history = status.as_ref().is_ok_and(|s| !s.initial);
    match std::fs::read_to_string(path.join("TODO.md")) {
        Ok(text) => {
            let added = match history {
                true => added_times(&git),
                false => HashMap::new(),
            };
            let todo = todo_facts(&text, &added);
            if let Some(first) = &todo.first_lint {
                let more = match todo.lint_errors - 1 {
                    0 => String::new(),
                    n => format!(" (+{n} more)"),
                };
                facts
                    .faults
                    .push((Fault::TodoLint, format!("{first}{more}")));
            }
            facts.todo = Some(todo);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            facts.todo_unread = true;
            facts
                .faults
                .push((Fault::TodoUnreadable, format!("TODO.md: {e}")));
        }
    }
    let Ok(status) = status else {
        git.note_lost(&mut facts.faults);
        return facts;
    };

    // No origin is an answer, not a lost fact.
    facts.slug = git
        .read_optional(Fact::Origin, &["remote", "get-url", "origin"])
        .and_then(|url| github_slug(url.trim()));
    facts.dirty = status.changes.len() as i64;
    facts.pma_branches = git
        .read(
            Fact::PmaBranches,
            &[
                "for-each-ref",
                "--format=%(refname:lstrip=2)",
                "refs/heads/pma/",
            ],
        )
        .map(|out| out.lines().map(String::from).collect())
        .unwrap_or_default();
    facts.ahead = status.ahead;
    if let (Some(up), None) = (&status.upstream, status.ahead) {
        git.lost(Fact::Unpushed, format!("upstream {up} is gone"));
    }
    if history {
        facts.last_activity = last_activity(&git, ignore);
    }
    facts.lock_at = lock_changed_at(&git, path, &status);
    if !offline {
        facts.ci_read = true;
        match ci_state(path) {
            Ok((ci, runs)) => (facts.ci, facts.ci_runs) = (ci, runs),
            Err(why) => {
                facts.faults.push((Fault::CiUnknown, why.clone()));
                facts.ci = Ci::Unknown(why);
            }
        }
        if deps {
            facts.deps = Some(crate::deps::measure(path));
        }
    }
    git.note_lost(&mut facts.faults);
    facts
}

/// What `git status --porcelain=v2 --branch` says. Its `# branch.` headers
/// tell an empty repository, a missing upstream and a gone one apart, which
/// a failed `git log` or `git rev-list` cannot.
#[derive(Debug, Default, PartialEq)]
struct Status {
    /// One entry per changed or untracked path.
    changes: Vec<String>,
    /// No commit yet.
    initial: bool,
    upstream: Option<String>,
    /// Commits not on the upstream; `None` without one, or when it is gone.
    ahead: Option<i64>,
}

impl Status {
    fn parse(out: &str) -> Status {
        let mut s = Status::default();
        for line in out.lines() {
            match line.strip_prefix("# ") {
                Some(h) => {
                    if h == "branch.oid (initial)" {
                        s.initial = true;
                    } else if let Some(up) = h.strip_prefix("branch.upstream ") {
                        s.upstream = Some(up.to_string());
                    } else if let Some(ab) = h.strip_prefix("branch.ab +") {
                        s.ahead = ab.split(' ').next().and_then(|n| n.parse().ok());
                    }
                }
                None if !line.is_empty() => s.changes.push(line.to_string()),
                None => {}
            }
        }
        s
    }
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
pub fn call(cmd: Command, timeout: Duration) -> Result<String, CallError> {
    let ran = bounded(cmd, timeout)?;
    match ran.success {
        true => Ok(ran.stdout),
        false => Err(CallError::Failed(gh_error(&ran.stderr))),
    }
}

/// What a bounded command printed, and whether it succeeded.
pub struct Ran {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// `call`, keeping the whole of stderr and leaving a failure to the caller.
/// An error only when the command could not start or timed out.
pub fn bounded(mut cmd: Command, timeout: Duration) -> Result<Ran, CallError> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| {
            let program = cmd.get_program().display();
            CallError::Failed(match e.kind() {
                std::io::ErrorKind::NotFound => format!("`{program}` is not on PATH"),
                _ => format!("{program}: {e}"),
            })
        })?;
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
    let status = status.ok_or(CallError::TimedOut(timeout))?;
    Ok(Ran {
        success: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// What one git call in a scan reads, named when the call times out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fact {
    Origin,
    PmaBranches,
    Unpushed,
    Activity,
    LockAge,
    TaskAges,
}

impl Fact {
    fn name(self) -> &'static str {
        match self {
            Fact::Origin => "GitHub origin",
            Fact::PmaBranches => "pma branches",
            Fact::Unpushed => "unpushed commits",
            Fact::Activity => "last activity",
            Fact::LockAge => "lock age",
            Fact::TaskAges => "task ages",
        }
    }
}

/// `git -C dir` for one project's scan. A call that fails or times out is
/// remembered with the fact it reads, so the project's detail says which
/// fact is missing rather than reading it as empty.
struct Git<'a> {
    dir: &'a Path,
    lost: Mutex<Vec<(Fact, String)>>,
}

impl<'a> Git<'a> {
    fn new(dir: &'a Path) -> Git<'a> {
        Git {
            dir,
            lost: Mutex::new(Vec::new()),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(self.dir).args(args);
        cmd
    }

    fn read(&self, fact: Fact, args: &[&str]) -> Option<String> {
        self.call(fact, args, true)
    }

    /// As `read`, for a call whose failure is an answer: only a timeout
    /// loses the fact.
    fn read_optional(&self, fact: Fact, args: &[&str]) -> Option<String> {
        self.call(fact, args, false)
    }

    fn call(&self, fact: Fact, args: &[&str], failure_loses: bool) -> Option<String> {
        match call(self.command(args), CALL_TIMEOUT) {
            Ok(out) => return Some(out),
            Err(CallError::Failed(_)) if !failure_loses => {}
            Err(e) => {
                let what = args.first().copied().unwrap_or_default();
                self.lost(fact, format!("git {what}: {e}"));
            }
        }
        None
    }

    fn lost(&self, fact: Fact, why: String) {
        let why = format!("{}: {why}", fact.name());
        self.lost.lock().unwrap().push((fact, why));
    }

    /// The lost facts as faults: one for those that show unpublished work,
    /// one for the rest. The faults are kept worst first.
    fn note_lost(&self, faults: &mut Vec<(Fault, String)>) {
        let lost = self.lost.lock().unwrap();
        for (fault, publish) in [
            (Fault::UnpublishedUnknown, true),
            (Fault::ScanPartial, false),
        ] {
            let why: Vec<&str> = lost
                .iter()
                .filter(|(f, _)| matches!(f, Fact::PmaBranches | Fact::Unpushed) == publish)
                .map(|(_, why)| why.as_str())
                .collect();
            if !why.is_empty() {
                faults.push((fault, why.join("; ")));
            }
        }
        faults.sort_by_key(|(f, _)| *f);
    }
}

fn todo_facts(text: &str, added: &HashMap<String, i64>) -> TodoFacts {
    let parsed = todo::parse(text);
    let errors: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity == todo::Severity::Error)
        .collect();
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
        lint_errors: errors.len() as i64,
        first_lint: errors
            .first()
            .map(|d| format!("line {}: {}", d.line, d.message)),
        items,
    }
}

/// Runs `git -C dir`, bounded by `CALL_TIMEOUT`; stdout when it succeeds.
pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
    call(Git::new(dir).command(args), CALL_TIMEOUT).ok()
}

/// Commit time at which each item text first appeared in TODO.md, keyed by
/// `normalise`. Matching on text rather than on lines keeps an item's age when
/// a later commit moves it, retags it, or changes the file's format, all of
/// which `git blame` reports as a new line.
fn added_times(git: &Git) -> HashMap<String, i64> {
    git.read(
        Fact::TaskAges,
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

fn last_activity(git: &Git, ignore: &[String]) -> Option<i64> {
    let log = git.read(
        Fact::Activity,
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

/// The oldest of each lock's newest commit on HEAD or its upstream, so a
/// refresh published but not yet pulled counts. A lock with uncommitted
/// changes is being refreshed and counts as changed now. `None` without a
/// committed lock.
fn lock_changed_at(git: &Git, dir: &Path, status: &Status) -> Option<i64> {
    if status.initial {
        return None;
    }
    // `@{u}` only while the upstream exists; git fails on it otherwise.
    let upstream: &[&str] = match status.ahead {
        Some(_) => &["@{u}"],
        None => &[],
    };
    crate::deps::ECOSYSTEMS
        .iter()
        .map(|(_, _, lock)| *lock)
        .filter(|lock| dir.join(lock).is_file())
        .filter_map(|lock| {
            if modified(status, lock) {
                return Some(crate::dates::now());
            }
            let mut args = vec!["log", "-1", "--format=%ct", "HEAD"];
            args.extend(upstream);
            args.extend(["--", lock]);
            git.read(Fact::LockAge, &args)?.trim().parse().ok()
        })
        .min()
}

/// Whether `git status` lists `path` as changed. Untracked is not changed: a
/// lock never committed has no age to reset. Tracked entries are `1`, `2`
/// (renamed, `path<TAB>orig`) and `u`, with 8, 9 and 10 fields before the
/// path.
fn modified(status: &Status, path: &str) -> bool {
    status.changes.iter().any(|l| {
        let fields = match l.as_bytes().first() {
            Some(b'1') => 9,
            Some(b'2') => 10,
            Some(b'u') => 11,
            _ => return false,
        };
        l.splitn(fields, ' ')
            .nth(fields - 1)
            .and_then(|p| p.split('\t').next())
            == Some(path)
    })
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
/// The CI state, and each failing workflow's latest failed run. `Err` with
/// why when a GitHub project's CI could not be read; no origin, or one off
/// GitHub, is `Ci::Unknown` and no error.
fn ci_state(dir: &Path) -> Result<(Ci, Vec<(String, String)>), String> {
    let Some(url) = git(dir, &["remote", "get-url", "origin"]) else {
        return Ok((Ci::Unknown("no origin remote".into()), Vec::new()));
    };
    let Some(slug) = github_slug(url.trim()) else {
        return Ok((Ci::Unknown("origin is not on GitHub".into()), Vec::new()));
    };
    let branch = default_branch(dir).ok_or(DEFAULT_BRANCH_UNKNOWN)?;
    let gh = |args: &[&str]| {
        let mut cmd = Command::new("gh");
        cmd.args(args);
        call(cmd, CALL_TIMEOUT).map_err(|e| format!("gh {}: {e}", args[..2].join(" ")))
    };
    let runs = |extra: &[&str]| {
        let mut args = vec!["run", "list", "-R", &slug, "--branch", &branch];
        args.extend_from_slice(extra);
        args.extend([
            "--json",
            "workflowName,status,conclusion,url",
            "--jq",
            ".[] | [.workflowName, .status, .conclusion, .url] | @tsv",
        ]);
        gh(&args).map(|tsv| (latest_verdicts(&tsv), decisive_urls(&tsv)))
    };
    (|| {
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
        let active = active_workflows(&listed)?;
        let (mut verdicts, mut urls) = runs(&["--limit", "100"])?;
        // A workflow that runs rarely can fall outside that window, so it is
        // asked about alone rather than read as having no runs.
        for (id, name) in &active {
            if !verdicts.contains_key(name) {
                let id = id.to_string();
                let (alone, alone_urls) = runs(&["--workflow", &id, "--limit", "20"])?;
                if let Some(&failed) = alone.get(name) {
                    verdicts.insert(name.clone(), failed);
                }
                urls.extend(alone_urls);
            }
        }
        let ci = ci_of(&active, &verdicts);
        let failed_runs = match &ci {
            Ci::Failing(names) => names
                .iter()
                .filter_map(|n| urls.get(n).map(|u| (n.clone(), u.clone())))
                .collect(),
            _ => Vec::new(),
        };
        Ok((ci, failed_runs))
    })()
}

/// Enabled workflows as `(id, name)`, from `gh workflow list --json id,name,state`.
/// `gh` prints nothing, not `[]`, for a repository with no workflows.
pub fn active_workflows(json: &str) -> Result<Vec<(u64, String)>, String> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }
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

/// The URL of each workflow's latest decisive run, from the rows
/// `latest_verdicts` reads, with the URL as a fourth column.
fn decisive_urls(tsv: &str) -> HashMap<String, String> {
    let mut urls = HashMap::new();
    for line in tsv.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if let [name, status, conclusion, url] = f[..]
            && failed(status, conclusion).is_some()
        {
            urls.entry(name.to_string())
                .or_insert_with(|| url.to_string());
        }
    }
    urls
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

/// The repository's page on GitHub, for a slug of the characters GitHub
/// allows in an owner or a name.
pub fn github_url(slug: &str) -> Option<String> {
    let ok = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match slug.split_once('/') {
        Some((owner, name)) if ok(owner) && ok(name) => {
            Some(format!("https://github.com/{owner}/{name}"))
        }
        _ => None,
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

    /// A lost fact that shows unpublished work is its own fault; the rest
    /// are one `ScanPartial` that names each fact lost.
    #[test]
    fn lost_facts_are_named() {
        let git = Git::new(Path::new("/r"));
        git.lost(Fact::Activity, "git log: timed out after 60s".into());
        git.lost(Fact::Unpushed, "upstream origin/main is gone".into());
        git.lost(Fact::LockAge, "git log: bad object HEAD".into());
        let mut faults = vec![(Fault::TodoLint, "line 1: x".to_string())];
        git.note_lost(&mut faults);
        assert_eq!(
            faults,
            [
                (
                    Fault::UnpublishedUnknown,
                    "unpushed commits: upstream origin/main is gone".to_string()
                ),
                (Fault::TodoLint, "line 1: x".to_string()),
                (
                    Fault::ScanPartial,
                    "last activity: git log: timed out after 60s; \
                     lock age: git log: bad object HEAD"
                        .to_string()
                ),
            ]
        );
        let none = Git::new(Path::new("/r"));
        let mut faults = Vec::new();
        none.note_lost(&mut faults);
        assert!(faults.is_empty());
    }

    #[test]
    fn status_headers_tell_no_upstream_from_a_gone_one() {
        let s = Status::parse(
            "# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n\
             # branch.ab +2 -1\n? new.txt\n",
        );
        assert_eq!(
            (s.initial, s.upstream.as_deref(), s.ahead, s.changes.len()),
            (false, Some("origin/main"), Some(2), 1)
        );
        let gone = Status::parse("# branch.oid abc\n# branch.upstream origin/main\n");
        assert_eq!((gone.upstream.is_some(), gone.ahead), (true, None));
        let none = Status::parse("# branch.oid abc\n# branch.head main\n");
        assert_eq!((none.upstream, none.ahead), (None, None));
        assert!(Status::parse("# branch.oid (initial)\n# branch.head main\n").initial);
    }

    /// A push changes the stamp, so a view stops calling pushed commits
    /// unpublished without waiting for a full scan.
    #[test]
    fn a_push_changes_the_stamp() {
        let root = std::env::temp_dir().join(format!("pma-push-stamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (dir, bare) = (root.join("clone"), root.join("origin.git"));
        std::fs::create_dir_all(&dir).unwrap();
        let sh = |at: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(at)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        sh(&root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        sh(&dir, &["init", "-q", "-b", "main"]);
        sh(&dir, &["remote", "add", "origin", bare.to_str().unwrap()]);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "first"]);
        sh(&dir, &["push", "-q", "-u", "origin", "main"]);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "second"]);
        let before = stamp(&dir);
        // Coarse file times would hide a change made in the same tick.
        std::thread::sleep(std::time::Duration::from_millis(20));
        sh(&dir, &["push", "-q", "origin", "main"]);
        assert_ne!(stamp(&dir), before, "the push went unseen");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Against real repositories: an empty one and one with no upstream lose
    /// nothing; a gone upstream loses the unpushed count.
    #[test]
    fn a_scan_loses_only_what_git_could_not_answer() {
        let dir = std::env::temp_dir().join(format!("pma-lost-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        let scan = || scan_project("p", &dir, &[], true, false);
        sh(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("TODO.md"), "# TODO\n\n## High\n\n- [ ] x\n").unwrap();
        std::fs::write(dir.join("uv.lock"), "").unwrap();
        let empty = scan();
        assert_eq!(
            empty.faults,
            [],
            "an empty repository answers every question"
        );
        assert_eq!(empty.dirty, 2);

        sh(&["add", "."]);
        sh(&["commit", "-qm", "first"]);
        let alone = scan();
        assert_eq!(alone.faults, [], "no upstream is an answer");
        assert_eq!(alone.ahead, None);
        assert!(alone.last_activity.is_some() && alone.lock_at.is_some());

        sh(&["remote", "add", "origin", "/nonexistent"]);
        sh(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        sh(&["branch", "-q", "--set-upstream-to=origin/main"]);
        sh(&["commit", "-q", "--allow-empty", "-m", "second"]);
        let tracked = scan();
        assert_eq!((tracked.faults.len(), tracked.ahead), (0, Some(1)));
        assert!(tracked.lock_at.is_some(), "read on HEAD and its upstream");
        // Online, an origin off GitHub has no CI to read; nothing is lost.
        let online = scan_project("p", &dir, &[], false, false);
        assert_eq!(online.faults, []);
        assert!(online.ci_read && matches!(online.ci, Ci::Unknown(_)));

        sh(&["update-ref", "-d", "refs/remotes/origin/main"]);
        let gone = scan();
        assert_eq!(
            gone.faults,
            [(
                Fault::UnpublishedUnknown,
                "unpushed commits: upstream origin/main is gone".to_string()
            )]
        );
        assert!(gone.lock_at.is_some(), "read on HEAD alone");
        let _ = std::fs::remove_dir_all(dir);
    }

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

    const Z: &str = "0000000000000000000000000000000000000000";

    /// A `--porcelain=v2` entry for a tracked path.
    fn changed(xy: &str, path: &str) -> String {
        format!("1 {xy} N... 100644 100644 100644 {Z} {Z} {path}")
    }

    #[test]
    fn a_staged_or_edited_lock_is_modified_and_an_untracked_one_is_not() {
        let status = Status::parse(
            &[
                changed(".M", "Cargo.lock"),
                changed("M.", "uv.lock"),
                "? go.sum".into(),
                changed(".M", "sub/Cargo.lock"),
                format!("2 R. N... 100644 100644 100644 {Z} {Z} R100 go.mod\told.mod"),
            ]
            .join("\n"),
        );
        assert!(modified(&status, "Cargo.lock"));
        assert!(modified(&status, "uv.lock"));
        assert!(modified(&status, "go.mod"), "renamed");
        assert!(!modified(&status, "go.sum"));
        assert!(!modified(&Status::default(), "Cargo.lock"));
    }

    /// The oldest lock decides; an edited lock counts as now; a lock never
    /// committed has no age.
    #[test]
    fn lock_age_is_the_stalest_committed_lock() {
        let dir = std::env::temp_dir().join(format!("pma-lock-age-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = |args: &[&str], date: &str| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        let at = |changed: &[&str]| {
            let lines: Vec<String> = changed
                .iter()
                .map(|p| super::tests::changed(".M", p))
                .collect();
            lock_changed_at(&Git::new(&dir), &dir, &Status::parse(&lines.join("\n")))
        };
        sh(&["init", "-q"], "@0");
        std::fs::write(dir.join("uv.lock"), "").unwrap();
        assert_eq!(at(&[]), None, "an uncommitted lock has no age");

        sh(&["add", "uv.lock"], "@0");
        sh(&["commit", "-qm", "uv"], "@1000 +0000");
        std::fs::write(dir.join("Cargo.lock"), "").unwrap();
        sh(&["add", "Cargo.lock"], "@0");
        sh(&["commit", "-qm", "cargo"], "@2000 +0000");
        assert_eq!(at(&[]), Some(1000));

        let now = crate::dates::now();
        assert!(
            at(&["uv.lock"]).is_some_and(|t| t == 2000),
            "the edited lock counts as now"
        );
        assert!(at(&["uv.lock", "Cargo.lock"]).is_some_and(|t| t >= now));
        let _ = std::fs::remove_dir_all(dir);
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

    #[test]
    fn each_workflow_links_to_its_latest_decisive_run() {
        let tsv = "test\tin_progress\t\thttps://x/3\n\
                   test\tcompleted\tfailure\thttps://x/2\n\
                   test\tcompleted\tsuccess\thttps://x/1\n\
                   lint\tcompleted\tsuccess\n";
        let urls = decisive_urls(tsv);
        assert_eq!(urls.get("test").map(String::as_str), Some("https://x/2"));
        assert_eq!(urls.get("lint"), None, "a row without a URL gives none");
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
        assert_eq!(active_workflows("").unwrap(), [], "no workflows");
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
        assert_eq!(
            call(Command::new("/no/such/program"), limit),
            Err(CallError::Failed(
                "`/no/such/program` is not on PATH".into()
            ))
        );
        let ran = bounded(sh("echo one >&2; echo two >&2; exit 1"), limit).unwrap();
        assert!(!ran.success);
        assert_eq!(ran.stderr, "one\ntwo\n", "the whole of stderr is kept");
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
    fn github_urls_take_only_a_plain_slug() {
        assert_eq!(
            github_url("shakfu/pma").as_deref(),
            Some("https://github.com/shakfu/pma")
        );
        for bad in ["shakfu", "a/b/c", "a/\"x", "/b", "a/"] {
            assert_eq!(github_url(bad), None, "{bad}");
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
