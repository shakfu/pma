use super::*;

/// A project's state as the history compares it.
struct ScanState {
    ci: Ci,
    faults: Vec<(Fault, String)>,
}

/// Every scanned project's state, by name. A row a tier made before any
/// scan read the project has no state to compare against.
fn scan_state(conn: &Connection) -> Result<HashMap<String, ScanState>> {
    let mut stmt =
        conn.prepare("SELECT name, ci, ci_detail FROM projects WHERE scanned_at IS NOT NULL")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            ScanState {
                ci: Ci::from_columns(&r.get::<_, String>(1)?, r.get(2)?),
                faults: Vec::new(),
            },
        ))
    })?;
    let mut states: HashMap<String, ScanState> = rows.collect::<rusqlite::Result<_>>()?;
    for (project, faults) in project_faults(conn)? {
        if let Some(s) = states.get_mut(&project) {
            s.faults = faults;
        }
    }
    Ok(states)
}

/// Each project's faults, worst first.
fn project_faults(conn: &Connection) -> Result<HashMap<String, Vec<(Fault, String)>>> {
    let mut stmt = conn.prepare("SELECT project, kind, detail FROM project_faults")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out: HashMap<String, Vec<(Fault, String)>> = HashMap::new();
    for row in rows {
        let (project, kind, detail) = row?;
        if let Some(f) = Fault::from_column(&kind) {
            out.entry(project).or_default().push((f, detail));
        }
    }
    for faults in out.values_mut() {
        faults.sort_by_key(|(f, _)| *f);
    }
    Ok(out)
}

/// The last CI state an earlier scan knew for a project.
fn last_known_ci(conn: &Connection, project: &str, before: i64) -> Result<Option<Ci>> {
    Ok(conn
        .query_row(
            "SELECT ci, ci_detail FROM scan_projects
             WHERE project = ?1 AND scan < ?2 AND ci != 'unknown'
             ORDER BY scan DESC LIMIT 1",
            params![project, before],
            |r| Ok(Ci::from_columns(&r.get::<_, String>(0)?, r.get(1)?)),
        )
        .optional()?)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectRow {
    pub name: String,
    pub path: PathBuf,
    /// `owner/name` on GitHub; `None` without a GitHub origin, or until the
    /// project is scanned again.
    pub slug: Option<String>,
    /// When a full scan stopped finding the project under any root; `None`
    /// while it is present. `path` is then where it was last seen.
    pub absent_since: Option<i64>,
    pub tier: Option<u8>,
    pub scanned_at: Option<i64>,
    pub has_todo: bool,
    pub lint_errors: i64,
    pub dirty: i64,
    /// `pma/` branches that no open run owned at the last scan.
    pub leftover: i64,
    pub ahead: Option<i64>,
    pub last_activity: Option<i64>,
    pub ci: Ci,
    /// Each failing workflow's latest failed run, as `(workflow, url)`.
    pub ci_runs: Vec<(String, String)>,
    /// Each fault the last scan found, worst first, with its cause.
    pub faults: Vec<(Fault, String)>,
    /// Outdated dependencies at the last measurement; `None` if never measured.
    pub deps: Option<i64>,
    /// One `ecosystem: name current -> latest` per line.
    pub deps_detail: String,
    pub deps_at: Option<i64>,
    /// Crates cargo's constraints held back at the last measurement.
    pub held_back: Option<i64>,
    /// When the stalest lock last changed, at the last scan.
    pub lock_at: Option<i64>,
}

/// What a project's record consists of, for `pma project forget`.
#[derive(Debug, Clone, PartialEq)]
pub struct Footprint {
    pub tasks: i64,
    pub tags: i64,
    pub campaigns: i64,
    /// Runs are kept, so this is reported rather than deleted.
    pub runs: i64,
    /// Runs that are not final; each may own a worktree.
    pub open_runs: Vec<i64>,
    /// Its `projects.<name>.*` settings, such as `verify`.
    pub settings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskRow {
    pub project: String,
    pub key: String,
    pub line: i64,
    pub priority: Priority,
    pub text: String,
    pub tags: Vec<String>,
    pub due: Option<String>,
    pub gh: Option<i64>,
    pub group: Option<String>,
    pub added_at: Option<i64>,
    pub first_seen: i64,
}

impl Store {
    pub fn last_scan(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT MAX(scanned_at) FROM projects", [], |r| r.get(0))?)
    }

    pub fn project(&self, name: &str) -> Result<Option<ProjectRow>> {
        Ok(self.projects()?.into_iter().find(|p| p.name == name))
    }

    pub fn projects(&self) -> Result<Vec<ProjectRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, path, tier, scanned_at, has_todo, lint_errors, dirty, ahead,
                    last_activity, ci, ci_detail, deps, deps_detail, deps_at,
                    leftover, slug, absent_since, d.held_back, d.lock_at
             FROM projects LEFT JOIN project_deps d ON d.project = projects.name
             ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ProjectRow {
                name: r.get(0)?,
                path: PathBuf::from(r.get::<_, String>(1)?),
                tier: r.get(2)?,
                scanned_at: r.get(3)?,
                has_todo: r.get(4)?,
                lint_errors: r.get(5)?,
                dirty: r.get(6)?,
                ahead: r.get(7)?,
                last_activity: r.get(8)?,
                ci: Ci::from_columns(&r.get::<_, String>(9)?, r.get::<_, String>(10)?),
                deps: r.get(11)?,
                deps_detail: r.get(12)?,
                deps_at: r.get(13)?,
                leftover: r.get(14)?,
                slug: r.get(15)?,
                absent_since: r.get(16)?,
                held_back: r.get(17)?,
                lock_at: r.get(18)?,
                ci_runs: Vec::new(),
                faults: Vec::new(),
            })
        })?;
        let mut rows: Vec<ProjectRow> = rows.collect::<rusqlite::Result<_>>()?;
        let mut stmt = self
            .conn
            .prepare("SELECT project, workflow, url FROM ci_runs ORDER BY project, workflow")?;
        let runs = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get(2)?))
        })?;
        for run in runs {
            let (project, workflow, url) = run?;
            if let Some(row) = rows.iter_mut().find(|r| r.name == project) {
                row.ci_runs.push((workflow, url));
            }
        }
        let mut faults = project_faults(&self.conn)?;
        for row in &mut rows {
            row.faults = faults.remove(&row.name).unwrap_or_default();
        }
        Ok(rows)
    }

    pub fn tasks(&self) -> Result<Vec<TaskRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT project, line, priority, text, tags, due, gh, added_at, first_seen, heading, key
             FROM tasks ORDER BY project, line",
        )?;
        let rows = stmt.query_map([], |r| {
            let priority: String = r.get(2)?;
            let tags: String = r.get(4)?;
            Ok(TaskRow {
                project: r.get(0)?,
                line: r.get(1)?,
                priority: Priority::parse(&priority).unwrap_or(Priority::Low),
                text: r.get(3)?,
                tags: tags.split_whitespace().map(String::from).collect(),
                due: r.get(5)?,
                gh: r.get(6)?,
                added_at: r.get(7)?,
                first_seen: r.get(8)?,
                group: r.get(9)?,
                key: r.get(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records the scan, with each project's state and what changed since the
    /// scan before (`crate::history`). Each task keeps the `first_seen` it had
    /// in the previous scan. A full scan marks every project it did not find
    /// as absent and returns their names; the rows stay, since a project that
    /// leaves a root is usually a move, not a deletion.
    pub fn save_scan(&mut self, facts: &[Facts], full: bool, now: i64) -> Result<Vec<String>> {
        use crate::history::{self, Change};
        let open_runs: Vec<Run> = self
            .runs()?
            .into_iter()
            .filter(|r| !r.state.is_final())
            .collect();
        // Immediate: reading first and writing later fails at once, past
        // `busy_timeout`, if another process wrote in between.
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let before = scan_state(&tx)?;
        let returned: std::collections::HashSet<String> = {
            let mut stmt =
                tx.prepare("SELECT name FROM projects WHERE absent_since IS NOT NULL")?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        tx.execute(
            "INSERT INTO scans (at, full) VALUES (?1, ?2)",
            params![now, full],
        )?;
        let scan = tx.last_insert_rowid();
        let mut changes: Vec<(String, Change)> = Vec::new();
        for f in facts {
            match (before.get(&f.name), returned.contains(&f.name)) {
                // A first scan finds every project; none of them is news.
                (None, _) if !before.is_empty() => changes.push((f.name.clone(), Change::New)),
                (Some(_), true) => changes.push((f.name.clone(), Change::Returned)),
                _ => {}
            }
            // What this scan did not read keeps the last scan's fault: an
            // unread file its lint errors, as its tasks; CI not asked, its
            // `CiUnknown`.
            let keep_lint = f.held || f.todo_unread;
            let keep_ci = !f.ci_read;
            tx.execute(
                "DELETE FROM project_faults WHERE project = ?1
                    AND NOT (kind = ?2 AND ?3) AND NOT (kind = ?4 AND ?5)",
                params![
                    f.name,
                    Fault::TodoLint.column(),
                    keep_lint,
                    Fault::CiUnknown.column(),
                    keep_ci
                ],
            )?;
            let save_faults = |tx: &rusqlite::Transaction| -> rusqlite::Result<()> {
                for (fault, why) in &f.faults {
                    tx.execute(
                        "INSERT OR REPLACE INTO project_faults (project, kind, detail)
                         VALUES (?1, ?2, ?3)",
                        params![f.name, fault.column(), why],
                    )?;
                }
                Ok(())
            };
            if f.held {
                tx.execute(
                    "UPDATE projects SET absent_since = NULL WHERE name = ?1",
                    [&f.name],
                )?;
                save_faults(&tx)?;
                continue;
            }
            let (ci, ci_detail) = f.ci.to_columns();
            let leftover = f
                .pma_branches
                .iter()
                .filter(|b| {
                    !open_runs
                        .iter()
                        .any(|r| r.project == f.name && &r.branch == *b)
                })
                .count() as i64;
            tx.execute(
                "INSERT INTO projects (name, path, scanned_at, has_todo, lint_errors, dirty, ahead,
                                       last_activity, ci, ci_detail, leftover, slug)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT (name) DO UPDATE SET
                    path = excluded.path, scanned_at = excluded.scanned_at,
                    has_todo = CASE WHEN ?13 THEN projects.has_todo ELSE excluded.has_todo END,
                    lint_errors = CASE WHEN ?13 THEN projects.lint_errors
                                       ELSE excluded.lint_errors END,
                    dirty = excluded.dirty, ahead = excluded.ahead,
                    last_activity = excluded.last_activity, ci = excluded.ci,
                    ci_detail = excluded.ci_detail,
                    leftover = excluded.leftover, slug = excluded.slug,
                    absent_since = NULL",
                params![
                    f.name,
                    path_str(&f.path)?,
                    now,
                    f.todo.is_some() || f.todo_unread,
                    f.todo.as_ref().map_or(0, |t| t.lint_errors),
                    f.dirty,
                    f.ahead,
                    f.last_activity,
                    ci,
                    ci_detail,
                    leftover,
                    f.slug,
                    f.todo_unread,
                ],
            )?;
            save_faults(&tx)?;
            tx.execute(
                "INSERT OR REPLACE INTO project_stamps (project, stamp) VALUES (?1, ?2)",
                params![f.name, f.stamp],
            )?;
            tx.execute("DELETE FROM ci_runs WHERE project = ?1", [&f.name])?;
            for (workflow, url) in &f.ci_runs {
                tx.execute(
                    "INSERT OR REPLACE INTO ci_runs (project, workflow, url) VALUES (?1, ?2, ?3)",
                    params![f.name, workflow, url],
                )?;
            }

            tx.execute(
                "INSERT INTO project_deps (project, lock_at) VALUES (?1, ?2)
                 ON CONFLICT (project) DO UPDATE SET lock_at = excluded.lock_at",
                params![f.name, f.lock_at],
            )?;
            if let Some(d) = &f.deps {
                tx.execute(
                    "UPDATE projects SET deps = ?2, deps_detail = ?3, deps_at = ?4 WHERE name = ?1",
                    params![f.name, d.outdated, d.detail, now],
                )?;
                tx.execute(
                    "UPDATE project_deps SET held_back = ?2 WHERE project = ?1",
                    params![f.name, d.held_back],
                )?;
            }

            // An unreadable file says nothing about the tasks; the last good
            // scan's rows and their ages stand.
            if f.todo_unread {
                continue;
            }
            // By key, else by text: `sync` adding `gh:N` changes an item's key
            // but not its text, and an `id:` keeps an item's key when its text
            // is reworded. Either way the item keeps its age: when it was first
            // seen, and when its text first appeared in git, which a reworded
            // text no longer finds in the history.
            // An item matched neither way is opened; a row no item matched
            // is closed.
            let rows: Vec<(String, String, String, i64, Option<i64>)> = {
                let mut stmt = tx.prepare(
                    "SELECT key, text, priority, first_seen, added_at FROM tasks WHERE project = ?1",
                )?;
                stmt.query_map([&f.name], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<rusqlite::Result<_>>()?
            };
            let by_key: HashMap<&str, usize> = rows
                .iter()
                .enumerate()
                .map(|(i, r)| (r.0.as_str(), i))
                .collect();
            let by_text: HashMap<String, usize> = rows
                .iter()
                .enumerate()
                .map(|(i, r)| (crate::todo::normal_text(&r.1), i))
                .collect();
            let mut matched = std::collections::HashSet::new();
            let known = before.contains_key(&f.name);
            tx.execute("DELETE FROM tasks WHERE project = ?1", [&f.name])?;
            for item in f.todo.iter().flat_map(|t| &t.items) {
                let found = by_key
                    .get(item.key.as_str())
                    .or_else(|| by_text.get(&crate::todo::normal_text(&item.text)))
                    .copied();
                match found {
                    Some(i) => {
                        matched.insert(i);
                    }
                    None if known => changes.push((
                        f.name.clone(),
                        Change::Opened {
                            priority: item.priority.name().into(),
                            text: item.text.clone(),
                        },
                    )),
                    None => {}
                }
                let before = found.map(|i| (rows[i].3, rows[i].4));
                let first_seen = before.map_or(now, |(seen, _)| seen);
                let added_at = match (item.added_at, before.and_then(|(_, added)| added)) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                tx.execute(
                    "INSERT INTO tasks (project, key, line, priority, text, tags, due, gh, added_at, first_seen, heading)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        f.name,
                        item.key,
                        item.line,
                        item.priority.name(),
                        item.text,
                        item.tags.join(" "),
                        item.due,
                        item.gh,
                        added_at,
                        first_seen,
                        item.group,
                    ],
                )?;
            }
            for (i, (_, text, priority, ..)) in rows.iter().enumerate() {
                if !matched.contains(&i) {
                    changes.push((
                        f.name.clone(),
                        Change::Closed {
                            priority: priority.clone(),
                            text: text.clone(),
                        },
                    ));
                }
            }
        }

        // Each project as the scan left it, against the state before.
        let after = scan_state(&tx)?;
        for f in facts {
            let Some(state) = after.get(&f.name) else {
                continue;
            };
            let (ci, detail) = state.ci.to_columns();
            tx.execute(
                "INSERT INTO scan_projects (scan, project, ci, ci_detail, lint_errors, open)
                 VALUES (?1, ?2, ?3, ?4,
                         (SELECT lint_errors FROM projects WHERE name = ?2),
                         (SELECT count(*) FROM tasks WHERE project = ?2))",
                params![scan, f.name, ci, detail],
            )?;
            for (fault, why) in &state.faults {
                tx.execute(
                    "INSERT INTO scan_faults (scan, project, kind, detail) VALUES (?1, ?2, ?3, ?4)",
                    params![scan, f.name, fault.column(), why],
                )?;
            }
            let Some(was) = before.get(&f.name) else {
                continue;
            };
            let known_ci = match &was.ci {
                Ci::Unknown(_) => last_known_ci(&tx, &f.name, scan)?,
                ci => Some(ci.clone()),
            };
            let faults = history::fault_changes(&was.faults, &state.faults);
            for c in history::ci_change(known_ci.as_ref(), &state.ci)
                .into_iter()
                .chain(faults)
            {
                changes.push((f.name.clone(), c));
            }
        }

        let mut absent = Vec::new();
        if full {
            let scanned: std::collections::HashSet<&str> =
                facts.iter().map(|f| f.name.as_str()).collect();
            let mut stmt = tx.prepare("SELECT name FROM projects WHERE absent_since IS NULL")?;
            let present: Vec<String> = stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            for name in present {
                if !scanned.contains(name.as_str()) {
                    tx.execute(
                        "UPDATE projects SET absent_since = ?2 WHERE name = ?1",
                        params![name, now],
                    )?;
                    absent.push(name);
                }
            }
        }
        for name in &absent {
            changes.push((name.clone(), Change::Absent));
        }
        for (project, change) in &changes {
            let (kind, priority, detail) = change.to_columns();
            tx.execute(
                "INSERT INTO scan_changes (scan, project, kind, priority, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![scan, project, kind, priority, detail],
            )?;
        }
        tx.execute("DELETE FROM scans WHERE id <= ?1", [scan - history::KEPT])?;
        tx.commit()?;
        // A project this scan added may have a tier or tags in the file.
        self.reapply_settings()?;
        Ok(absent)
    }

    /// `save_scan` for a read no person asked for, so `pma history` keeps what
    /// changed rather than one row each time a file is touched. A quiet partial
    /// read leaves no row; a quiet full one replaces the quiet full row before
    /// it, so the time of the last full scan is kept.
    pub fn save_quiet_scan(&mut self, facts: &[Facts], full: bool, now: i64) -> Result<()> {
        self.save_scan(facts, full, now)?;
        const QUIET: &str = "NOT EXISTS (SELECT 1 FROM scan_changes c WHERE c.scan = scans.id)";
        let latest: i64 = self
            .conn
            .query_row("SELECT MAX(id) FROM scans", [], |r| r.get(0))?;
        match full {
            false => self.conn.execute(
                &format!("DELETE FROM scans WHERE id = ?1 AND {QUIET}"),
                [latest],
            )?,
            true => self.conn.execute(
                &format!(
                    "DELETE FROM scans WHERE id = ?1 - 1 AND full AND {QUIET}
                       AND NOT EXISTS (SELECT 1 FROM scan_changes c WHERE c.scan = ?1)"
                ),
                [latest],
            )?,
        };
        Ok(())
    }

    /// What each project's files looked like when last read.
    pub fn project_stamps(&self) -> Result<HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT project, stamp FROM project_stamps")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// When every project, and CI, was last read.
    pub fn last_full_scan(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT MAX(at) FROM scans WHERE full", [], |r| r.get(0))?)
    }

    /// The latest `limit` scans, newest first, with their totals.
    pub fn scans(&self, limit: usize) -> Result<Vec<crate::history::Scan>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.at, s.full,
                    (SELECT count(*) FROM scan_projects p WHERE p.scan = s.id),
                    (SELECT count(*) FROM scan_projects p WHERE p.scan = s.id AND p.ci = 'failing'),
                    (SELECT coalesce(sum(p.open), 0) FROM scan_projects p WHERE p.scan = s.id),
                    (SELECT count(*) FROM scan_changes c WHERE c.scan = s.id AND c.kind = 'opened'),
                    (SELECT count(*) FROM scan_changes c WHERE c.scan = s.id AND c.kind = 'closed'),
                    (SELECT count(*) FROM scan_changes c WHERE c.scan = s.id
                        AND c.kind NOT IN ('opened', 'closed'))
             FROM scans s ORDER BY s.id DESC LIMIT ?1",
        )?;
        let mut by_fault = self
            .conn
            .prepare("SELECT kind, count(*) FROM scan_faults WHERE scan = ?1 GROUP BY kind")?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(crate::history::Scan {
                id: r.get(0)?,
                at: r.get(1)?,
                full: r.get(2)?,
                projects: r.get(3)?,
                failing: r.get(4)?,
                faults: [0; 7],
                open: r.get(5)?,
                opened: r.get(6)?,
                closed: r.get(7)?,
                other: r.get(8)?,
            })
        })?;
        let mut scans: Vec<crate::history::Scan> = rows.collect::<rusqlite::Result<_>>()?;
        for s in &mut scans {
            let counts = by_fault.query_map([s.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?;
            for c in counts {
                let (kind, n) = c?;
                if let Some(i) = Fault::ALL.iter().position(|f| f.column() == kind) {
                    s.faults[i] = n;
                }
            }
        }
        Ok(scans)
    }

    /// What a scan recorded as changed, by project, in the order recorded.
    pub fn scan_changes(&self, scan: i64) -> Result<Vec<crate::history::ProjectChange>> {
        let mut stmt = self.conn.prepare(
            "SELECT project, kind, priority, detail FROM scan_changes
             WHERE scan = ?1 ORDER BY project, rowid",
        )?;
        let rows = stmt.query_map([scan], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (project, kind, priority, detail) = row?;
            if let Some(change) = crate::history::Change::from_columns(&kind, priority, detail) {
                out.push(crate::history::ProjectChange { project, change });
            }
        }
        Ok(out)
    }

    /// For each project whose CI fails at its latest recorded scan, when that
    /// run of failing scans began.
    pub fn failing_since(&self) -> Result<HashMap<String, crate::history::Since>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.project, s.at, p.ci, p.ci_detail FROM scan_projects p
             JOIN scans s ON s.id = p.scan ORDER BY p.project, p.scan DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                Ci::from_columns(&r.get::<_, String>(2)?, r.get(3)?),
            ))
        })?;
        let mut by_project: HashMap<String, Vec<(i64, Ci)>> = HashMap::new();
        for row in rows {
            let (project, at, ci) = row?;
            by_project.entry(project).or_default().push((at, ci));
        }
        Ok(by_project
            .into_iter()
            .filter(|(_, states)| matches!(states.first(), Some((_, Ci::Failing(_)))))
            .filter_map(|(p, states)| crate::history::failing_since(&states).map(|s| (p, s)))
            .collect())
    }

    /// What forgetting a project would delete, and what would hold it back.
    pub fn project_footprint(&self, name: &str) -> Result<Footprint> {
        let count =
            |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [name], |r| r.get(0))?) };
        let open_runs = self
            .runs()?
            .into_iter()
            .filter(|r| r.project == name && !r.state.is_final())
            .map(|r| r.id)
            .collect();
        Ok(Footprint {
            tasks: count("SELECT count(*) FROM tasks WHERE project = ?1")?,
            tags: count("SELECT count(*) FROM project_tags WHERE project = ?1")?,
            campaigns: count("SELECT count(*) FROM campaign_members WHERE project = ?1")?,
            runs: count("SELECT count(*) FROM runs WHERE project = ?1")?,
            open_runs,
            settings: self
                .config_rows()?
                .into_iter()
                .filter_map(|(k, _)| {
                    k.strip_prefix(&format!("projects.{name}."))
                        .map(String::from)
                })
                .collect(),
        })
    }

    /// Deletes a project's record, its settings, and everything keyed to it
    /// but its runs, whose worktrees may still exist and which the design
    /// keeps beyond a project's removal. Caller checks that nothing holds it
    /// back.
    pub fn forget_project(&self, name: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for sql in [
            "DELETE FROM project_tags WHERE project = ?1",
            "DELETE FROM campaign_members WHERE project = ?1",
            "DELETE FROM exhaustion WHERE project = ?1",
            "DELETE FROM verify_base WHERE project = ?1",
            "DELETE FROM scan_projects WHERE project = ?1",
            "DELETE FROM scan_changes WHERE project = ?1",
            // `tasks` follows by ON DELETE CASCADE.
            "DELETE FROM projects WHERE name = ?1",
        ] {
            tx.execute(sql, [name])?;
        }
        tx.commit()?;
        self.change_settings(|doc| crate::settings::forget_project(doc, name))
    }

    /// Every `(project, tag)` pair, ordered by project then tag.
    pub fn project_tags(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT project, tag FROM project_tags ORDER BY project, tag")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Returns false when the project already had the tag.
    pub fn add_project_tag(&self, project: &str, tag: &str) -> Result<bool> {
        self.change_settings(|doc| crate::settings::tag_project(doc, project, tag, true))
    }

    /// Returns false when the project did not have the tag.
    pub fn remove_project_tag(&self, project: &str, tag: &str) -> Result<bool> {
        self.change_settings(|doc| crate::settings::tag_project(doc, project, tag, false))
    }

    /// The projects carrying any of `tags`, in name order. A project in
    /// several of them appears once.
    pub fn projects_tagged(&self, tags: &[String]) -> Result<Vec<String>> {
        let mut found: Vec<String> = Vec::new();
        for (project, tag) in self.project_tags()? {
            if tags.iter().any(|t| t == &tag) && !found.contains(&project) {
                found.push(project);
            }
        }
        found.sort();
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{DepsFacts, ScannedItem};
    use crate::store::tests::*;

    #[test]
    fn a_quiet_read_leaves_history_as_it_was() {
        let dir = scratch("quiet");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        store
            .save_scan(&[facts("one", vec![])], true, 1000)
            .unwrap();
        store
            .save_scan(&[facts("one", vec![])], true, 1500)
            .unwrap();
        let count = |s: &Store| s.scans(10).unwrap().len();
        assert_eq!(count(&store), 2);

        store
            .save_quiet_scan(&[facts("one", vec![])], false, 2000)
            .unwrap();
        assert_eq!(count(&store), 2, "a quiet partial read leaves no row");
        assert_eq!(store.last_full_scan().unwrap(), Some(1500));
        assert_eq!(store.last_scan().unwrap(), Some(2000));

        store
            .save_quiet_scan(&[facts("one", vec![])], true, 3000)
            .unwrap();
        assert_eq!(
            count(&store),
            2,
            "a quiet full read replaces the one before"
        );
        assert_eq!(store.last_full_scan().unwrap(), Some(3000));

        store
            .save_quiet_scan(&[facts("two", vec![])], false, 4000)
            .unwrap();
        assert_eq!(count(&store), 3, "a new project is news");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Unlike `deps`, the slug is read on every scan, so the last scan wins
    /// and a project whose GitHub origin is gone loses it.
    #[test]
    fn the_last_scan_owns_the_slug() {
        let dir = scratch("slug");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        store
            .save_scan(&[facts("one", vec![])], true, 1000)
            .unwrap();
        assert_eq!(
            store.project("one").unwrap().unwrap().slug.as_deref(),
            Some("shakfu/one")
        );

        let mut moved = facts("one", vec![]);
        moved.slug = None;
        store.save_scan(&[moved], true, 2000).unwrap();
        assert_eq!(store.project("one").unwrap().unwrap().slug, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Forgetting clears the record but not the runs, whose worktrees may
    /// still exist.
    #[test]
    fn forgetting_a_project_keeps_its_runs() {
        let dir = scratch("forget");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        store
            .save_scan(
                &[facts("a", vec![item("t", 1)]), facts("b", vec![])],
                true,
                1,
            )
            .unwrap();
        store.set_tier("a", Path::new("/r/a"), Some(1)).unwrap();
        store.add_project_tag("a", "ai").unwrap();
        store.add_project_tag("b", "ai").unwrap();
        let mut run = Run::blank();
        run.project = "a".into();
        store.insert_run(&mut run).unwrap();
        store.consume_attempt("a", "t").unwrap();
        store.set_config("projects.a.verify", "make test").unwrap();
        store.set_config("projects.a.sandbox", "off").unwrap();

        let f = store.project_footprint("a").unwrap();
        assert_eq!((f.tasks, f.tags, f.runs), (1, 1, 1));
        assert_eq!(f.settings, ["sandbox", "verify"]);
        assert_eq!(f.open_runs, [run.id], "an open run holds a worktree");

        let mut open = store.run(run.id).unwrap();
        open.state = RunState::Merged;
        store.update_run(&open).unwrap();
        assert!(store.project_footprint("a").unwrap().open_runs.is_empty());

        store.forget_project("a").unwrap();
        assert!(store.project("a").unwrap().is_none());
        assert!(store.tasks().unwrap().is_empty(), "its tasks go");
        assert_eq!(
            store.projects_tagged(&["ai".into()]).unwrap(),
            ["b"],
            "another project keeps the tag"
        );
        assert_eq!(store.consumed_attempts("a", "t").unwrap(), 0);
        assert!(
            store.project_footprint("a").unwrap().settings.is_empty(),
            "a later repository of the name inherits no settings"
        );
        assert_eq!(store.run(run.id).unwrap().project, "a", "the run is kept");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_project_carries_several_tags() {
        let dir = scratch("tags");
        let store = Store::open(&dir.join("p.db")).unwrap();
        assert!(store.add_project_tag("cyllama", "ai").unwrap());
        assert!(
            !store.add_project_tag("cyllama", "ai").unwrap(),
            "idempotent"
        );
        store.add_project_tag("cyllama", "audio").unwrap();
        store.add_project_tag("cysox", "audio").unwrap();

        assert_eq!(
            store.project_tags().unwrap(),
            [
                ("cyllama".to_string(), "ai".to_string()),
                ("cyllama".to_string(), "audio".to_string()),
                ("cysox".to_string(), "audio".to_string()),
            ]
        );
        assert_eq!(
            store.projects_tagged(&["audio".into()]).unwrap(),
            ["cyllama", "cysox"]
        );
        assert_eq!(
            store
                .projects_tagged(&["ai".into(), "audio".into()])
                .unwrap(),
            ["cyllama", "cysox"],
            "a project in both tags is listed once"
        );

        assert!(store.remove_project_tag("cyllama", "audio").unwrap());
        assert!(!store.remove_project_tag("cyllama", "audio").unwrap());
        assert_eq!(store.projects_tagged(&["audio".into()]).unwrap(), ["cysox"]);
        assert_eq!(
            store.projects_tagged(&["ai".into()]).unwrap(),
            ["cyllama"],
            "removing one tag leaves the other"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn scan_round_trips_and_keeps_tier_and_first_seen() {
        let dir = scratch("scan");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        store.set_tier("one", Path::new("/r/one"), Some(2)).unwrap();

        store
            .save_scan(
                &[facts("one", vec![item("x", 5), item("y", 6)])],
                true,
                1000,
            )
            .unwrap();
        store
            .save_scan(
                &[facts("one", vec![item("y", 7), item("z", 8)])],
                true,
                2000,
            )
            .unwrap();

        let p = store.project("one").unwrap().unwrap();
        assert_eq!(p.tier, Some(2), "a scan keeps the tier");
        assert_eq!((p.deps, p.deps_at), (None, None));
        assert_eq!(p.slug.as_deref(), Some("shakfu/one"));

        let mut measured = facts("one", vec![item("y", 7), item("z", 8)]);
        measured.deps = Some(DepsFacts {
            outdated: Some(2),
            held_back: Some(5),
            detail: "uv: ruff 0.14 -> 0.16".into(),
        });
        measured.lock_at = Some(2500);
        store.save_scan(&[measured], true, 3000).unwrap();
        let mut plain = facts("one", vec![item("y", 7), item("z", 8)]);
        plain.lock_at = Some(3500);
        store.save_scan(&[plain], true, 4000).unwrap();
        let p = store.project("one").unwrap().unwrap();
        assert_eq!(
            (p.deps, p.deps_detail.as_str(), p.deps_at, p.held_back),
            (Some(2), "uv: ruff 0.14 -> 0.16", Some(3000), Some(5)),
            "a scan that does not measure deps keeps the last measurement"
        );
        assert_eq!(p.lock_at, Some(3500), "every scan reads the lock's age");
        assert_eq!(
            (p.scanned_at, p.has_todo, p.lint_errors, p.dirty),
            (Some(4000), true, 1, 3)
        );
        assert_eq!((p.ahead, p.last_activity), (None, Some(42)));
        assert_eq!(p.ci, Ci::Failing(vec!["test".into(), "wheels".into()]));

        let tasks = store.tasks().unwrap();
        let seen: Vec<_> = tasks
            .iter()
            .map(|t| (t.text.as_str(), t.line, t.first_seen))
            .collect();
        assert_eq!(
            seen,
            [("y", 7, 1000), ("z", 8, 2000)],
            "x is gone, y keeps its first scan"
        );
        assert_eq!(tasks[0].tags, ["a", "b"]);
        assert_eq!(tasks[0].group.as_deref(), Some("Security"));
        assert_eq!(
            (tasks[0].due.as_deref(), tasks[0].gh, tasks[0].added_at),
            (Some("2026-10-01"), Some(4), Some(100))
        );
        assert_eq!(store.last_scan().unwrap(), Some(4000));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A project left unread keeps everything the last scan recorded, and is
    /// not absent: only its scan error changes.
    #[test]
    fn a_held_project_keeps_its_last_scan() {
        let dir = scratch("held");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        store
            .save_scan(&[facts("one", vec![item("x", 5)])], true, 1000)
            .unwrap();
        let held = Facts::held("one", Path::new("/r/one"), "settings changed".into());
        assert!(store.save_scan(&[held], true, 2000).unwrap().is_empty());
        let tasks: Vec<_> = store.tasks().unwrap().into_iter().map(|t| t.key).collect();
        assert_eq!(tasks, ["x"]);
        let p = store.project("one").unwrap().unwrap();
        assert_eq!((p.dirty, p.absent_since), (3, None));
        assert!(matches!(p.ci, Ci::Failing(_)), "{:?}", p.ci);
        assert_eq!(p.ci_runs[0].1, "https://github.com/shakfu/x/actions/runs/7");
        assert_eq!(
            p.faults,
            [
                (Fault::GitSettingsChanged, "settings changed".to_string()),
                (
                    Fault::TodoLint,
                    "line 1: the file must start with `# TODO`".to_string()
                ),
            ],
            "the lint error the last scan read stands"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file that could not be read is not an empty file: the tasks and their
    /// ages stand until a scan can read it again.
    #[test]
    fn an_unreadable_todo_keeps_the_tasks() {
        let dir = scratch("unread");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        store
            .save_scan(&[facts("one", vec![item("x", 5)])], true, 1000)
            .unwrap();
        let unread = Facts {
            todo: None,
            todo_unread: true,
            faults: vec![(Fault::TodoUnreadable, "TODO.md: permission denied".into())],
            ..facts("one", vec![])
        };
        store.save_scan(&[unread], true, 2000).unwrap();
        let kept: Vec<_> = store
            .tasks()
            .unwrap()
            .into_iter()
            .map(|t| (t.key, t.first_seen))
            .collect();
        assert_eq!(kept, [("x".to_string(), 1000)]);
        let p = store.project("one").unwrap().unwrap();
        assert_eq!((p.has_todo, p.lint_errors), (true, 1));
        assert_eq!(
            p.faults,
            [
                (
                    Fault::TodoUnreadable,
                    "TODO.md: permission denied".to_string()
                ),
                (
                    Fault::TodoLint,
                    "line 1: the file must start with `# TODO`".to_string()
                ),
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A scan that did not ask CI says nothing about it: an offline scan
    /// keeps `CiUnknown`, and the next online scan that reads CI mends it.
    #[test]
    fn ci_unknown_stands_until_ci_is_read() {
        let dir = scratch("ci-unknown");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        let lost = Facts {
            faults: vec![
                (Fault::CiUnknown, "gh run list: timed out after 60s".into()),
                (Fault::TodoLint, "line 1: x".into()),
            ],
            ci: Ci::Unknown("gh run list: timed out after 60s".into()),
            ..facts("one", vec![])
        };
        let kinds = |store: &Store| -> Vec<Fault> {
            let p = store.project("one").unwrap().unwrap();
            p.faults.into_iter().map(|(f, _)| f).collect()
        };
        store.save_scan(&[lost], true, 1000).unwrap();
        assert_eq!(kinds(&store), [Fault::CiUnknown, Fault::TodoLint]);
        let offline = Facts {
            ci: Ci::Unknown("offline".into()),
            ci_read: false,
            ..facts("one", vec![])
        };
        store.save_scan(&[offline], true, 2000).unwrap();
        assert_eq!(kinds(&store), [Fault::CiUnknown, Fault::TodoLint]);
        store
            .save_scan(&[facts("one", vec![])], true, 3000)
            .unwrap();
        assert_eq!(kinds(&store), [Fault::TodoLint]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `sync` writes `gh:N` into an item, which changes its key from its text
    /// to the issue number. Its age is the item's, not the key's.
    #[test]
    fn a_task_keeps_its_first_seen_when_its_key_changes() {
        let dir = scratch("rekey");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        let unlinked = ScannedItem {
            gh: None,
            ..item("fix it", 5)
        };
        store
            .save_scan(&[facts("one", vec![unlinked])], true, 1000)
            .unwrap();
        let linked = ScannedItem {
            key: "gh:9".into(),
            text: "Fix  it".into(),
            gh: Some(9),
            ..item("fix it", 5)
        };
        store
            .save_scan(&[facts("one", vec![linked])], true, 2000)
            .unwrap();
        let tasks = store.tasks().unwrap();
        assert_eq!((tasks[0].key.as_str(), tasks[0].first_seen), ("gh:9", 1000));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn full_scan_marks_projects_no_longer_found() {
        let dir = scratch("full");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        store
            .save_scan(
                &[facts("a", vec![item("t", 1)]), facts("b", vec![])],
                true,
                1,
            )
            .unwrap();
        store.set_tier("a", Path::new("/r/a"), Some(1)).unwrap();

        assert!(
            store
                .save_scan(&[facts("b", vec![])], false, 2)
                .unwrap()
                .is_empty(),
            "a partial scan marks nothing"
        );
        assert_eq!(
            store.projects().unwrap().len(),
            2,
            "a partial scan removes nothing"
        );

        let absent = store.save_scan(&[facts("b", vec![])], true, 3).unwrap();
        assert_eq!(absent, ["a"]);
        let a = store.project("a").unwrap().unwrap();
        assert_eq!(a.absent_since, Some(3));
        assert_eq!(a.tier, Some(1), "an absent project keeps its tier");
        assert_eq!(
            store.tasks().unwrap().len(),
            1,
            "its tasks and their first_seen are kept"
        );

        // Reported once, not on every later scan.
        assert!(
            store
                .save_scan(&[facts("b", vec![])], true, 4)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.project("a").unwrap().unwrap().absent_since, Some(3));

        // Finding it again makes it present, without a second first_seen.
        store
            .save_scan(&[facts("a", vec![item("t", 1)])], false, 5)
            .unwrap();
        assert_eq!(store.project("a").unwrap().unwrap().absent_since, None);
        assert_eq!(store.tasks().unwrap()[0].first_seen, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Each scan records what changed against the state before it: items
    /// opened and closed, CI against the last state a scan knew, a project
    /// broken, mended, new or gone.
    #[test]
    fn a_scan_records_what_changed() {
        use crate::history::Change;
        let dir = scratch("history");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        let mut one = facts("one", vec![item("x", 5), item("y", 6)]);
        store.save_scan(&[one.clone()], true, 1000).unwrap();
        let first = store.scans(10).unwrap();
        assert_eq!(first.len(), 1);
        assert!(
            store.scan_changes(first[0].id).unwrap().is_empty(),
            "a first scan has no news"
        );

        // Offline: CI unknown is not news, and the failing state is kept.
        one.todo.as_mut().unwrap().items = vec![item("y", 6), item("z", 7)];
        one.todo.as_mut().unwrap().lint_errors = 0;
        one.faults.clear();
        one.ci = Ci::Unknown("offline".into());
        store
            .save_scan(&[one.clone(), facts("two", vec![])], true, 2000)
            .unwrap();
        let scan = store.scans(1).unwrap().remove(0);
        let changes: Vec<(String, Change)> = store
            .scan_changes(scan.id)
            .unwrap()
            .into_iter()
            .map(|c| (c.project, c.change))
            .collect();
        let high = |t: &str| (t.to_string(), "high".to_string());
        assert_eq!(
            changes,
            [
                (
                    "one".into(),
                    Change::Opened {
                        priority: high("z").1,
                        text: high("z").0
                    }
                ),
                (
                    "one".into(),
                    Change::Closed {
                        priority: high("x").1,
                        text: high("x").0
                    }
                ),
                ("one".into(), Change::Mended(Fault::TodoLint)),
                ("two".into(), Change::New),
            ]
        );
        assert_eq!(
            (
                scan.projects,
                scan.open,
                scan.opened,
                scan.closed,
                scan.other,
                scan.faults
            ),
            (2, 2, 1, 1, 2, [0, 0, 0, 0, 0, 1, 0])
        );

        // Online again: passing against the failing state before the offline scan.
        one.ci = Ci::Passing;
        store.save_scan(&[one.clone()], true, 3000).unwrap();
        let scan = store.scans(1).unwrap().remove(0);
        let changes: Vec<_> = store
            .scan_changes(scan.id)
            .unwrap()
            .into_iter()
            .map(|c| (c.project, c.change))
            .collect();
        assert_eq!(
            changes,
            [
                ("one".to_string(), Change::CiPassing),
                ("two".to_string(), Change::Absent),
            ]
        );
        assert_eq!(store.failing_since().unwrap().get("one"), None);

        one.ci = Ci::Failing(vec!["test".into()]);
        store.save_scan(&[one.clone()], false, 4000).unwrap();
        store.save_scan(&[one], false, 5000).unwrap();
        assert_eq!(
            store.failing_since().unwrap().get("one"),
            Some(&crate::history::Since {
                at: 4000,
                known: true
            })
        );
        assert_eq!(store.scans(10).unwrap().len(), 5);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn old_scans_are_deleted_with_their_rows() {
        let dir = scratch("history-kept");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        for at in 0..crate::history::KEPT + 3 {
            store.save_scan(&[facts("one", vec![])], true, at).unwrap();
        }
        let scans = store.scans(1000).unwrap();
        assert_eq!(scans.len() as i64, crate::history::KEPT);
        let rows: i64 = store
            .conn
            .query_row("SELECT count(*) FROM scan_projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, crate::history::KEPT);
        let _ = std::fs::remove_dir_all(dir);
    }
}
