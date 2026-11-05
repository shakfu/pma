use super::*;

impl Store {
    /// Assigns the run's id and stamps `dispatched_at`, in the row and in
    /// `run`.
    pub fn insert_run(&self, run: &mut Run) -> Result<i64> {
        run.dispatched_at = Some(crate::dates::now());
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO runs (project, task_key, text, gh, quadrant, agent, branch, worktree,
                               default_branch, base, prompt, state, repo, dispatched_at,
                               class, scope, tier, description, agent_budget, timeout_minutes,
                               verify, verify_base_ok, verify_base_seconds, model,
                               complexity, features, estimator,
                               route_revision, route, approval,
                               workflow_instance, node, unit, lap, preset, extra_args,
                               git_snapshot, workspace, start, interactive)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                     ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                     ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40)",
            params![
                run.project,
                run.task_key,
                run.text,
                run.gh,
                run.quadrant,
                run.agent,
                run.branch,
                path_str(&run.worktree)?,
                run.default_branch,
                run.base,
                run.prompt,
                run.state.name(),
                path_str(&run.repo)?,
                run.dispatched_at,
                run.class.map(Class::name),
                run.scope.join(","),
                run.tier,
                run.description,
                run.agent_budget,
                run.timeout_minutes,
                run.verify,
                run.verify_base_ok,
                run.verify_base_seconds,
                run.model,
                run.complexity,
                run.features.map(Features::to_json),
                run.complexity.map(|_| crate::complexity::ESTIMATOR),
                run.route_revision,
                run.route,
                run.approval.map(Approval::name),
                run.workflow_instance,
                run.node,
                run.unit,
                run.lap,
                run.preset,
                serde_json::to_string(&run.extra_args).unwrap_or_else(|_| "[]".into()),
                run.git_snapshot,
                run.workspace.name(),
                run.start,
                run.interactive,
            ],
        )?;
        run.id = tx.last_insert_rowid();
        for (n, item) in run.items.iter().enumerate() {
            tx.execute(
                "INSERT INTO run_items (run, n, key, text, gh, dropped)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    run.id,
                    n as i64 + 1,
                    item.key,
                    item.text,
                    item.gh,
                    item.dropped
                ],
            )?;
        }
        tx.commit()?;
        Ok(run.id)
    }

    /// Marks bundle items 1-based `dropped` as left open, and every other
    /// item as done.
    pub fn set_dropped(&self, id: i64, dropped: &[usize]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("UPDATE run_items SET dropped = 0 WHERE run = ?1", [id])?;
        for n in dropped {
            tx.execute(
                "UPDATE run_items SET dropped = 1 WHERE run = ?1 AND n = ?2",
                params![id, *n as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Writes every mutable column of the run with `run.id`.
    pub fn update_run(&self, run: &Run) -> Result<()> {
        self.write_run(run, None)
    }

    /// `update_run`, only while the row is still in `read`, the state the copy
    /// was read in. For a command that holds no session lock, so its stale
    /// copy cannot undo a publish that ran meanwhile.
    pub fn update_run_from(&self, run: &Run, read: RunState) -> Result<()> {
        self.write_run(run, Some(read))
    }

    fn write_run(&self, run: &Run, read: Option<RunState>) -> Result<()> {
        let written = self.conn.execute(
            // `verify` is frozen at dispatch: a command re-detected at rework
            // could differ from the one the base was checked with.
            "UPDATE runs SET state = ?2, feedback = ?3, seconds = ?4, cost_usd = ?5,
                summary = ?6, commits = ?7, diffstat = ?8, verify_ok = ?9,
                error = ?10, outcome = ?11, prompt = ?12, ready_at = ?13,
                decided_at = ?14, published_at = ?15, review_seconds = ?16,
                changed_paths = ?17, scope_error = ?18, approved_tree = ?19,
                approved_head = ?20, approved_by = ?21, reviewed_tree = ?22, infra = ?23,
                base = ?25, denied = ?26, verify_base_ok = ?27, verify_base_seconds = ?28
             WHERE id = ?1 AND (?24 IS NULL OR state = ?24)",
            params![
                run.id,
                run.state.name(),
                run.feedback,
                run.seconds,
                run.cost_usd,
                run.summary,
                run.commits,
                run.diffstat,
                run.verify_ok,
                run.error,
                run.outcome,
                run.prompt,
                run.ready_at,
                run.decided_at,
                run.published_at,
                run.review_seconds,
                run.changed_paths
                    .as_ref()
                    .map(|p| serde_json::to_string(p).unwrap_or_default()),
                run.scope_error,
                run.approved_tree,
                run.approved_head,
                run.approved_by,
                run.reviewed_tree,
                run.infra,
                read.map(RunState::name),
                run.base,
                serde_json::to_string(&run.denied).ok(),
                run.verify_base_ok,
                run.verify_base_seconds,
            ],
        )?;
        if written == 0 {
            let now = self.run(run.id)?.state;
            return Err(format!(
                "run #{} is now {}; read it again with `pma review {}`",
                run.id,
                now.name(),
                run.id
            )
            .into());
        }
        Ok(())
    }

    /// Adds review time in one column, so a command that holds no session
    /// lock writes nothing else.
    pub fn add_review_seconds(&self, id: i64, seconds: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET review_seconds = coalesce(review_seconds, 0) + ?2 WHERE id = ?1",
            params![id, seconds],
        )?;
        Ok(())
    }

    pub fn set_pane(&self, id: i64, pane: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET pane = ?2, agent_state = NULL WHERE id = ?1",
            params![id, pane],
        )?;
        Ok(())
    }

    /// What routing decided for a run inserted before its base check.
    pub fn route_queued(&self, run: &Run) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET agent = ?2, model = ?3, preset = ?4, extra_args = ?5, scope = ?6,
                tier = ?7, verify_base_ok = ?8, verify_base_seconds = ?9, complexity = ?10,
                features = ?11, estimator = ?12, route_revision = ?13, route = ?14,
                approval = ?15, prompt = ?16
             WHERE id = ?1",
            params![
                run.id,
                run.agent,
                run.model,
                run.preset,
                serde_json::to_string(&run.extra_args).unwrap_or_else(|_| "[]".into()),
                run.scope.join(","),
                run.tier,
                run.verify_base_ok,
                run.verify_base_seconds,
                run.complexity,
                run.features.map(Features::to_json),
                run.estimator,
                run.route_revision,
                run.route,
                run.approval.map(Approval::name),
                run.prompt,
            ],
        )?;
        Ok(())
    }

    /// Removes a run that routing refused before it started, with its items.
    /// Only a queued one: anything later has a record to keep.
    pub fn delete_queued(&self, id: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM run_items WHERE run = ?1 AND EXISTS
                 (SELECT 1 FROM runs WHERE id = ?1 AND state = 'queued')",
            [id],
        )?;
        tx.execute("DELETE FROM runs WHERE id = ?1 AND state = 'queued'", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// `None` when the last attempt ran without a room.
    pub fn set_room(&self, id: i64, room: Option<&str>) -> Result<()> {
        self.conn
            .execute("UPDATE runs SET room = ?2 WHERE id = ?1", params![id, room])?;
        Ok(())
    }

    pub fn set_agent_state(&self, id: i64, state: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET agent_state = ?2 WHERE id = ?1",
            params![id, state],
        )?;
        Ok(())
    }

    /// Starts the review clock, unless a reading already did.
    pub fn start_review(&self, id: i64, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET review_started = coalesce(review_started, ?2) WHERE id = ?1",
            params![id, now],
        )?;
        Ok(())
    }

    /// Stops the review clock and adds at most `cap` seconds to what it has
    /// measured.
    pub fn settle_review(&self, id: i64, now: i64, cap: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET review_measured = coalesce(review_measured, 0)
                 + min(max(?2 - review_started, 0), ?3), review_started = NULL
             WHERE id = ?1 AND review_started IS NOT NULL",
            params![id, now, cap],
        )?;
        Ok(())
    }

    pub fn set_reviewed_tree(&self, id: i64, tree: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET reviewed_tree = ?2 WHERE id = ?1",
            params![id, tree],
        )?;
        Ok(())
    }

    pub fn runs(&self) -> Result<Vec<Run>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project, task_key, text, gh, quadrant, agent, branch, worktree,
                    default_branch, base, prompt, state, feedback, seconds,
                    cost_usd, summary, commits, diffstat, verify, verify_ok, error, outcome, repo,
                    dispatched_at, ready_at, decided_at, published_at, review_seconds,
                    class, scope, tier, description, agent_budget, timeout_minutes,
                    verify_base_ok, verify_base_seconds, changed_paths, scope_error, model,
                    complexity, features, estimator, route_revision, route, approval,
                    approved_tree, approved_head, approved_by,
                    workflow_instance, node, unit, lap, preset, extra_args, git_snapshot,
                    reviewed_tree, infra, workspace, start, denied,
                    review_measured, pane, interactive, agent_state, room
             FROM runs ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            let state: String = r.get(12)?;
            Ok(Run {
                id: r.get(0)?,
                project: r.get(1)?,
                task_key: r.get(2)?,
                text: r.get(3)?,
                gh: r.get(4)?,
                quadrant: r.get(5)?,
                agent: r.get(6)?,
                branch: r.get(7)?,
                worktree: PathBuf::from(r.get::<_, String>(8)?),
                default_branch: r.get(9)?,
                base: r.get(10)?,
                prompt: r.get(11)?,
                state: RunState::parse(&state).unwrap_or(RunState::Failed),
                feedback: r.get(13)?,
                seconds: r.get(14)?,
                cost_usd: r.get(15)?,
                summary: r.get(16)?,
                commits: r.get(17)?,
                diffstat: r.get(18)?,
                verify: r.get(19)?,
                verify_ok: r.get(20)?,
                error: r.get(21)?,
                outcome: r.get(22)?,
                repo: PathBuf::from(r.get::<_, String>(23)?),
                dispatched_at: r.get(24)?,
                ready_at: r.get(25)?,
                decided_at: r.get(26)?,
                published_at: r.get(27)?,
                review_seconds: r.get(28)?,
                class: r
                    .get::<_, Option<String>>(29)?
                    .as_deref()
                    .and_then(Class::parse),
                scope: r
                    .get::<_, Option<String>>(30)?
                    .unwrap_or_default()
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
                tier: r.get(31)?,
                description: r.get(32)?,
                agent_budget: r.get(33)?,
                timeout_minutes: r.get(34)?,
                verify_base_ok: r.get(35)?,
                verify_base_seconds: r.get(36)?,
                changed_paths: r
                    .get::<_, Option<String>>(37)?
                    .and_then(|s| serde_json::from_str(&s).ok()),
                scope_error: r.get(38)?,
                model: r.get(39)?,
                complexity: r.get(40)?,
                features: r
                    .get::<_, Option<String>>(41)?
                    .as_deref()
                    .and_then(Features::from_json),
                estimator: r.get(42)?,
                route_revision: r.get(43)?,
                route: r.get(44)?,
                approval: r
                    .get::<_, Option<String>>(45)?
                    .as_deref()
                    .and_then(Approval::parse),
                approved_tree: r.get(46)?,
                approved_head: r.get(47)?,
                approved_by: r.get(48)?,
                workflow_instance: r.get(49)?,
                node: r.get(50)?,
                unit: r.get(51)?,
                lap: r.get(52)?,
                preset: r.get(53)?,
                extra_args: r
                    .get::<_, Option<String>>(54)?
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                git_snapshot: r.get(55)?,
                reviewed_tree: r.get(56)?,
                infra: r.get(57)?,
                workspace: Workspace::parse(&r.get::<_, String>(58)?).unwrap_or_default(),
                start: r.get(59)?,
                denied: r
                    .get::<_, Option<String>>(60)?
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                review_measured: r.get(61)?,
                pane: r.get(62)?,
                interactive: r.get(63)?,
                agent_state: r.get(64)?,
                room: r.get(65)?,
                items: Vec::new(),
            })
        })?;
        let mut runs: Vec<Run> = rows.collect::<rusqlite::Result<_>>()?;
        let mut stmt = self
            .conn
            .prepare("SELECT run, key, text, gh, dropped FROM run_items ORDER BY run, n")?;
        let mut items: HashMap<i64, Vec<RunItem>> = HashMap::new();
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                RunItem {
                    key: r.get(1)?,
                    text: r.get(2)?,
                    gh: r.get(3)?,
                    dropped: r.get(4)?,
                },
            ))
        })? {
            let (run, item) = row?;
            items.entry(run).or_default().push(item);
        }
        for run in &mut runs {
            run.items = items.remove(&run.id).unwrap_or_default();
        }
        Ok(runs)
    }

    pub fn run(&self, id: i64) -> Result<Run> {
        self.runs()?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("no run #{id}").into())
    }

    /// Decided runs in a project and how many were accepted. An open pull
    /// request is in neither: no one has judged it.
    pub fn decided_runs(&self, project: &str) -> Result<(i64, i64)> {
        let mut decided = 0;
        let mut accepted = 0;
        for run in self.runs()?.iter().filter(|r| r.project == project) {
            match run.state {
                RunState::Approved | RunState::Committed | RunState::Pushed | RunState::Merged => {
                    decided += 1;
                    accepted += 1;
                }
                RunState::Rejected if run.infra => {}
                RunState::Rejected | RunState::Closed => decided += 1,
                _ => {}
            }
        }
        Ok((decided, accepted))
    }

    /// Moves the runs still holding a task onto its new key, once `pma` has
    /// written an id into its line: those keyed by its old key or by its text,
    /// and the matching items of open bundles.
    /// A finished run keeps the key it was dispatched under.
    pub fn rekey_runs(&self, project: &str, old: &str, text: &str, new: &str) -> Result<()> {
        let text = crate::todo::normal_text(text);
        for run in self.runs()? {
            if run.project == project
                && !run.state.is_final()
                && (run.task_key == old || crate::todo::normal_text(&run.text) == text)
            {
                self.conn.execute(
                    "UPDATE runs SET task_key = ?2 WHERE id = ?1",
                    params![run.id, new],
                )?;
            }
            // A bundle's own key stays `bundle`; its items move.
            if run.project == project && !run.state.is_final() {
                for (n, item) in run.items.iter().enumerate() {
                    if item.key == old || crate::todo::normal_text(&item.text) == text {
                        self.conn.execute(
                            "UPDATE run_items SET key = ?3 WHERE run = ?1 AND n = ?2",
                            params![run.id, n as i64 + 1, new],
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Marks runs left queued or running by a session that ended as failed.
    /// Only a `Session` holder runs agents, so while it is held none is live.
    pub fn fail_interrupted_runs(&self, held: &Session) -> Result<usize> {
        const FAIL: &str = "UPDATE runs SET state = 'failed',
                error = 'interrupted: pma exited during the run'
             WHERE state IN ('queued', 'running')";
        // Only what `held` covers: a run elsewhere may be live under its own lock.
        let Some(projects) = &held.projects else {
            return Ok(self.conn.execute(FAIL, [])?);
        };
        let mut failed = 0;
        for project in projects {
            failed += self
                .conn
                .execute(&format!("{FAIL} AND project = ?1"), [project])?;
        }
        Ok(failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunState {
    #[default]
    Queued,
    Running,
    Ready,
    Failed,
    Approved,
    /// `pma pr` opened a pull request that is neither merged nor closed.
    PrOpen,
    /// Rejected in `pma review`: never published.
    Rejected,
    /// `pma stage` wrote it into its clone's index. It holds its task until
    /// it is committed and pushed, or rejected.
    Staged,
    /// `pma commit` committed it in its clone.
    Committed,
    /// `pma push` pushed it to the default branch.
    Pushed,
    /// Its pull request was merged.
    Merged,
    /// Its pull request was closed without merging.
    Closed,
    /// Written into its clone as unstaged changes, for the developer to
    /// stage and commit, or discard.
    Applied,
    /// A workflow node that is not an `edit` finished; its output became
    /// units. It changed no files, so there is nothing to review.
    Done,
}

impl RunState {
    const ALL: [(&'static str, RunState); 14] = [
        ("queued", RunState::Queued),
        ("running", RunState::Running),
        ("ready", RunState::Ready),
        ("failed", RunState::Failed),
        ("approved", RunState::Approved),
        ("pr-open", RunState::PrOpen),
        ("rejected", RunState::Rejected),
        ("staged", RunState::Staged),
        ("committed", RunState::Committed),
        ("pushed", RunState::Pushed),
        ("merged", RunState::Merged),
        ("closed", RunState::Closed),
        ("applied", RunState::Applied),
        ("done", RunState::Done),
    ];

    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, s)| *s == self)
            .map_or("", |(n, _)| n)
    }

    fn parse(s: &str) -> Option<RunState> {
        Self::ALL.iter().find(|(n, _)| *n == s).map(|(_, v)| *v)
    }

    /// A run in a final state no longer holds its task or its worktree.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            RunState::Rejected
                | RunState::Pushed
                | RunState::Merged
                | RunState::Closed
                | RunState::Applied
                | RunState::Done
        )
    }

    /// Whether its change reached the default branch: pushed, or merged.
    pub fn landed(self) -> bool {
        matches!(self, RunState::Pushed | RunState::Merged)
    }
}

/// One task dispatched to an agent, from its worktree to publishing. `Default`
/// is every field empty, which a workflow node fills differently from a
/// dispatch: it has no branch to push and no item to tick.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Run {
    pub id: i64,
    pub project: String,
    /// The item's key, or a signal: `ci` or `deps`.
    pub task_key: String,
    pub text: String,
    pub gh: Option<i64>,
    /// Quadrant at dispatch; `None` for an untiered project.
    pub quadrant: Option<String>,
    pub agent: String,
    /// The user's clone the worktree belongs to.
    pub repo: PathBuf,
    pub branch: String,
    pub worktree: PathBuf,
    pub default_branch: String,
    /// Commit the worktree started from.
    pub base: String,
    pub prompt: String,
    pub state: RunState,
    /// Reviewer feedback for the latest rework.
    pub feedback: Option<String>,
    /// Agent and verify time, summed over reworks.
    pub seconds: Option<i64>,
    /// Summed over reworks; `None` when the agent reports no cost.
    pub cost_usd: Option<f64>,
    pub summary: Option<String>,
    /// Commits the agent made beyond `base`; it was told to make none.
    pub commits: Option<i64>,
    pub diffstat: Option<String>,
    /// The verify command, or `None` when there is none.
    pub verify: Option<String>,
    pub verify_ok: Option<bool>,
    pub error: Option<String>,
    /// What publishing did: a pushed commit, or a pull request URL.
    pub outcome: Option<String>,
    /// When the run row was created. Never updated, so a per-day count and a
    /// digest window have a stable date. Per-attempt start times are in
    /// `attempts`.
    pub dispatched_at: Option<i64>,
    /// When the run last became ready. A rework moves it forward.
    pub ready_at: Option<i64>,
    /// The first approval or rejection. A pull request closed later does not
    /// replace the approval that published it.
    pub decided_at: Option<i64>,
    /// When `pma` published: the push, or the pull request opening. A merge
    /// days later does not move it.
    pub published_at: Option<i64>,
    /// Review time the user reported with `pma review --minutes`, summed over
    /// reviews of this run.
    pub review_seconds: Option<i64>,
    /// The class predicted at dispatch. `None` for a run dispatched before
    /// classes existed.
    pub class: Option<Class>,
    /// Globs the class allowed, resolved at dispatch. Empty means the class
    /// stated no bound; the privileged paths still apply.
    pub scope: Vec<String>,
    /// The project's tier at dispatch. Tiers change, and routes match on them.
    pub tier: Option<u8>,
    /// The item's description lines as dispatched.
    pub description: Option<String>,
    /// `agent_budget` at dispatch, so an attempt's cost can be read against
    /// the limit that applied to it.
    pub agent_budget: Option<f64>,
    pub timeout_minutes: Option<i64>,
    /// `verify` at the base commit, before the agent ran. `None` when there
    /// is no command, or when the run predates the check; `Some(false)` also
    /// covers a base run that timed out.
    pub verify_base_ok: Option<bool>,
    pub verify_base_seconds: Option<i64>,
    /// Every path the run changed against its base, both sides of a rename
    /// included. `None` when they could not be enumerated: an empty list
    /// would read as a clean run.
    pub changed_paths: Option<Vec<String>>,
    /// Why the paths could not be enumerated.
    pub scope_error: Option<String>,
    /// The model chosen at dispatch, or `None` for the worker's own default.
    pub model: Option<String>,
    /// 1 to 5, from the rule named in `estimator`.
    pub complexity: Option<i64>,
    /// What that rule read.
    pub features: Option<Features>,
    pub estimator: Option<String>,
    /// The policy revision in effect at dispatch, and the route it matched.
    /// A later activation does not change either.
    pub route_revision: Option<i64>,
    pub route: Option<String>,
    pub approval: Option<Approval>,
    /// The tree the approver saw, as `git write-tree` names it. Publishing refuses
    /// to publish a worktree that no longer matches.
    pub approved_tree: Option<String>,
    /// The commit the worktree was on. Each publish attempt starts again from
    /// it and `approved_tree`.
    pub approved_head: Option<String>,
    pub approved_by: Option<String>,
    /// The workflow instance that dispatched this run, or `None` for a task
    /// dispatched on its own.
    pub workflow_instance: Option<i64>,
    /// The qualified node, which a route matches on and a report groups by.
    pub node: Option<String>,
    /// The unit that drove it.
    pub unit: Option<String>,
    /// Laps the unit had completed. 0 outside a workflow.
    pub lap: i64,
    /// The preset this run was dispatched under, where one named it.
    pub preset: Option<String>,
    /// Arguments the preset added, recorded so a replay reads back what ran.
    pub extra_args: Vec<String>,
    /// `dispatch::git_snapshot` at dispatch. `None` for a run from before it
    /// was taken.
    pub git_snapshot: Option<String>,
    /// The tree the reviewer was last shown: what the agent left, or what
    /// `pma review <id>` printed since. An approval must match it. `None` for
    /// a run from before it was recorded.
    pub reviewed_tree: Option<String>,
    /// Rejected for a fault of the check or the machine, not the agent.
    pub infra: bool,
    /// Where the agent edited. A worktree run's `worktree` is its own
    /// directory; a clone run's is `repo`.
    pub workspace: Workspace,
    /// A clone run's working tree at dispatch, as a commit; `HEAD` itself
    /// when nothing differed. What its diff and its revert read against.
    pub start: Option<String>,
    /// What the agent was refused in its last attempt.
    pub denied: Vec<String>,
    /// Seconds from each first reading to the decision on it.
    pub review_measured: Option<i64>,
    /// The herdr pane of the last attempt; it may since have closed.
    pub pane: Option<String>,
    /// A person drives the agent in a pane: it reports no cost or summary.
    pub interactive: bool,
    /// What herdr last said of the agent in `pane`: `blocked` needs a person.
    pub agent_state: Option<String>,
    /// The minos room the last attempt spoke in.
    pub room: Option<String>,
    /// A bundle's items, in prompt order; empty for any other run.
    pub items: Vec<RunItem>,
}

/// One TODO.md item of a bundle.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RunItem {
    pub key: String,
    pub text: String,
    pub gh: Option<i64>,
    /// The approver left it open: not ticked, not closed.
    pub dropped: bool,
}

impl Run {
    /// Review time: what a person stated, else what was measured. Stated over
    /// summed: someone who gives a figure has already counted the reading.
    pub fn review_time(&self) -> Option<i64> {
        self.review_seconds.or(self.review_measured)
    }

    /// The agent changed nothing: no path and no diff. Such a run has nothing
    /// to approve or publish.
    pub fn changed_nothing(&self) -> bool {
        self.changed_paths.as_ref().is_some_and(|p| p.is_empty())
            || self.diffstat.as_deref() == Some("no changes")
    }

    /// The open pull request a conflict fix under review will update. A
    /// publish to the clone would leave it open and untracked.
    pub fn fixing_pr(&self) -> Option<&str> {
        matches!(
            self.state,
            RunState::Ready | RunState::Failed | RunState::Approved
        )
        .then_some(self.outcome.as_deref())
        .flatten()
        .filter(|o| o.starts_with("http"))
    }

    /// A run with every field empty, for tests in this crate to override.
    #[cfg(test)]
    pub fn blank() -> Run {
        Run {
            id: 0,
            project: "p".into(),
            task_key: "fix it".into(),
            text: "Fix it".into(),
            gh: None,
            quadrant: None,
            agent: "claude".into(),
            repo: PathBuf::from("/r/p"),
            branch: "pma/fix-it".into(),
            worktree: PathBuf::from("/w/p/fix-it"),
            default_branch: "main".into(),
            base: "abc".into(),
            prompt: "do it".into(),
            state: RunState::Queued,
            feedback: None,
            seconds: None,
            cost_usd: None,
            summary: None,
            commits: None,
            diffstat: None,
            verify: None,
            verify_ok: None,
            error: None,
            outcome: None,
            dispatched_at: None,
            ready_at: None,
            decided_at: None,
            published_at: None,
            review_seconds: None,
            class: None,
            scope: Vec::new(),
            tier: None,
            description: None,
            agent_budget: None,
            timeout_minutes: None,
            verify_base_ok: None,
            verify_base_seconds: None,
            changed_paths: None,
            scope_error: None,
            model: None,
            complexity: None,
            features: None,
            estimator: None,
            route_revision: None,
            route: None,
            approval: None,
            approved_tree: None,
            approved_head: None,
            approved_by: None,
            workflow_instance: None,
            node: None,
            unit: None,
            lap: 0,
            preset: None,
            extra_args: Vec::new(),
            git_snapshot: None,
            reviewed_tree: None,
            infra: false,
            workspace: Workspace::Worktree,
            start: None,
            denied: Vec::new(),
            review_measured: None,
            pane: None,
            room: None,
            interactive: false,
            agent_state: None,
            items: Vec::new(),
        }
    }

    /// Moves the run to `state` and stamps the transition. Call it instead of
    /// assigning `state`, so no transition goes unrecorded.
    pub fn enter(&mut self, state: RunState) {
        let now = crate::dates::now();
        match state {
            RunState::Ready => self.ready_at = Some(now),
            RunState::Approved | RunState::Committed | RunState::Rejected => {
                self.decided_at.get_or_insert(now);
            }
            RunState::Pushed | RunState::PrOpen => {
                self.published_at.get_or_insert(now);
            }
            _ => {}
        }
        self.state = state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::*;

    /// An id written into an item's line moves an open bundle's item onto
    /// it, by key or by text; the bundle's own key stays.
    #[test]
    fn a_rekey_moves_an_open_bundles_items() {
        let dir = scratch("rekey-bundle");
        let store = Store::open(&dir.join("p.db")).unwrap();
        let item = |key: &str, text: &str| RunItem {
            key: key.into(),
            text: text.into(),
            ..RunItem::default()
        };
        let mut run = Run {
            task_key: crate::dispatch::BUNDLE.into(),
            text: "task a; task b".into(),
            items: vec![item("a", "task a"), item("old-b", "Task  B")],
            ..Run::blank()
        };
        store.insert_run(&mut run).unwrap();
        store.rekey_runs("p", "a", "task a", "id:1").unwrap();
        store.rekey_runs("p", "b", "task b", "id:2").unwrap();
        let got = store.run(run.id).unwrap();
        assert_eq!(got.task_key, crate::dispatch::BUNDLE);
        let keys: Vec<&str> = got.items.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, ["id:1", "id:2"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The clock starts at the first reading, stops at the decision, adds at
    /// most the cap, and leaves a run no one opened at nothing. A later write
    /// of the run keeps it, and a stated time wins over it.
    #[test]
    fn the_review_clock_runs_from_first_reading_to_decision() {
        let dir = scratch("review-clock");
        let store = Store::open(&dir.join("p.db")).unwrap();
        let mut run = Run::blank();
        store.insert_run(&mut run).unwrap();
        let time = |store: &Store| store.run(run.id).unwrap().review_time();

        store.settle_review(run.id, 500, 60).unwrap();
        assert_eq!(time(&store), None, "never opened");

        store.start_review(run.id, 100).unwrap();
        store.start_review(run.id, 130).unwrap();
        store.settle_review(run.id, 140, 60).unwrap();
        assert_eq!(time(&store), Some(40), "from the first reading");

        store.update_run(&run).unwrap();
        assert_eq!(time(&store), Some(40), "a stale copy does not undo it");

        store.start_review(run.id, 200).unwrap();
        store.settle_review(run.id, 9000, 60).unwrap();
        assert_eq!(time(&store), Some(100), "an idle window is capped");

        store.settle_review(run.id, 9500, 60).unwrap();
        assert_eq!(time(&store), Some(100), "already stopped");

        store.add_review_seconds(run.id, 420).unwrap();
        assert_eq!(time(&store), Some(420), "stated, not summed");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A run's refusals read back as they were written.
    #[test]
    fn a_runs_denials_are_kept() {
        let dir = scratch("denied");
        let store = Store::open(&dir.join("p.db")).unwrap();
        let mut run = Run::blank();
        store.insert_run(&mut run).unwrap();
        assert!(store.run(run.id).unwrap().denied.is_empty());
        run.denied = vec!["git push".into()];
        store.update_run(&run).unwrap();
        assert_eq!(store.run(run.id).unwrap().denied, ["git push"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A copy read before a publish must not write its older state over it.
    #[test]
    fn a_guarded_write_refuses_a_run_that_moved_on() {
        let dir = scratch("guarded");
        let store = Store::open(&dir.join("p.db")).unwrap();
        let mut run = Run::blank();
        run.state = RunState::Ready;
        store.insert_run(&mut run).unwrap();
        let mut stale = store.run(run.id).unwrap();

        let mut published = store.run(run.id).unwrap();
        published.state = RunState::Pushed;
        store.update_run(&published).unwrap();

        stale.state = RunState::Approved;
        let err = store
            .update_run_from(&stale, RunState::Ready)
            .unwrap_err()
            .to_string();
        assert!(err.contains("is now pushed"), "{err}");
        assert_eq!(store.run(run.id).unwrap().state, RunState::Pushed);
        store.update_run_from(&stale, RunState::Pushed).unwrap();

        store.add_review_seconds(run.id, 60).unwrap();
        store.add_review_seconds(run.id, 60).unwrap();
        assert_eq!(store.run(run.id).unwrap().review_seconds, Some(120));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A rejection put down to infrastructure says nothing about the agent, so
    /// the project's record that routing reads leaves it out.
    #[test]
    fn an_infrastructure_rejection_is_not_decided() {
        let dir = scratch("decided");
        let store = Store::open(&dir.join("p.db")).unwrap();
        for (state, infra) in [
            (RunState::Merged, false),
            (RunState::Rejected, false),
            (RunState::Rejected, true),
        ] {
            let mut run = Run::blank();
            run.project = "p".into();
            store.insert_run(&mut run).unwrap();
            run.state = state;
            run.infra = infra;
            store.update_run(&run).unwrap();
        }
        assert_eq!(store.decided_runs("p").unwrap(), (2, 1));
        assert!(store.runs().unwrap()[2].infra, "stored and read back");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Each transition stamps its own time, and neither a merge nor a closure
    /// moves the time the run was published.
    #[test]
    fn transitions_are_stamped_once() {
        let mut run = Run::blank();
        run.enter(RunState::Ready);
        let ready = run.ready_at.expect("ready stamped");
        run.enter(RunState::Approved);
        let decided = run.decided_at.expect("approval stamped");
        run.enter(RunState::PrOpen);
        let published = run.published_at.expect("publication stamped");

        run.enter(RunState::Merged);
        assert_eq!(
            run.published_at,
            Some(published),
            "a merge is not a publish"
        );
        run.enter(RunState::Rejected);
        assert_eq!(run.decided_at, Some(decided), "the approval stands");
        assert_eq!(run.ready_at, Some(ready));
    }

    #[test]
    fn runs_round_trip_and_interrupted_runs_fail() {
        let dir = scratch("runs");
        let mut store = Store::open(&dir.join("p.db")).unwrap();
        let mut run = Run {
            id: 0,
            project: "p".into(),
            task_key: "fix it".into(),
            text: "Fix it".into(),
            gh: Some(3),
            quadrant: Some("Q1".into()),
            agent: "claude".into(),
            repo: PathBuf::from("/r/p"),
            branch: "pma/fix-it".into(),
            worktree: PathBuf::from("/w/p/fix-it"),
            default_branch: "main".into(),
            base: "abc".into(),
            prompt: "do it".into(),
            state: RunState::Running,
            feedback: None,
            seconds: None,
            cost_usd: None,
            summary: None,
            commits: None,
            diffstat: None,
            verify: Some("make test".into()),
            verify_ok: None,
            error: None,
            outcome: None,
            dispatched_at: None,
            ready_at: None,
            decided_at: None,
            published_at: None,
            review_seconds: None,
            class: Some(Class::Mechanical),
            scope: vec!["Cargo.lock".into(), "Cargo.toml".into()],
            tier: Some(2),
            description: Some("within the constraints\n".into()),
            agent_budget: Some(1.0),
            timeout_minutes: Some(30),
            verify_base_ok: Some(true),
            verify_base_seconds: Some(12),
            changed_paths: None,
            scope_error: None,
            model: Some("sonnet".into()),
            complexity: Some(2),
            features: Some(Features {
                text_words: 3,
                ..Features::default()
            }),
            estimator: Some(crate::complexity::ESTIMATOR.into()),
            route_revision: Some(2),
            route: Some("chores".into()),
            approval: Some(Approval::Batch),
            approved_tree: None,
            approved_head: None,
            approved_by: None,
            workflow_instance: None,
            node: None,
            unit: None,
            lap: 0,
            preset: None,
            extra_args: Vec::new(),
            git_snapshot: Some("hook pre-commit abc executable".into()),
            reviewed_tree: None,
            infra: false,
            workspace: Workspace::Worktree,
            start: None,
            denied: Vec::new(),
            review_measured: None,
            pane: None,
            room: None,
            interactive: false,
            agent_state: None,
            items: vec![
                RunItem {
                    key: "a".into(),
                    text: "first".into(),
                    gh: Some(7),
                    dropped: false,
                },
                RunItem {
                    key: "b".into(),
                    text: "second".into(),
                    gh: None,
                    dropped: true,
                },
            ],
        };
        store.insert_run(&mut run).unwrap();
        assert_eq!(store.run(run.id).unwrap(), run);
        store.set_dropped(run.id, &[1]).unwrap();
        (run.items[0].dropped, run.items[1].dropped) = (true, false);
        assert_eq!(store.run(run.id).unwrap(), run);

        run.state = RunState::Ready;
        run.cost_usd = Some(0.25);
        run.changed_paths = Some(vec!["Cargo.lock".into(), "a b/c\nd".into()]);
        run.verify_ok = Some(false);
        run.summary = Some("done".into());
        store.update_run(&run).unwrap();
        assert_eq!(store.run(run.id).unwrap(), run);

        // The command the base was checked with cannot change under the run.
        run.verify = Some("make something-else".into());
        store.update_run(&run).unwrap();
        assert_eq!(
            store.run(run.id).unwrap().verify.as_deref(),
            Some("make test")
        );
        run.verify = Some("make test".into());

        run.state = RunState::PrOpen;
        store.update_run(&run).unwrap();
        assert_eq!(store.run(run.id).unwrap().state, RunState::PrOpen);
        assert!(!run.state.is_final(), "an open pull request holds its task");

        run.state = RunState::Running;
        store.update_run(&run).unwrap();
        let session = Session::acquire(&dir).unwrap();
        assert_eq!(store.fail_interrupted_runs(&session).unwrap(), 1);
        let failed = store.run(run.id).unwrap();
        assert_eq!(failed.state, RunState::Failed);
        assert!(failed.error.unwrap().starts_with("interrupted"));
        assert!(store.run(99).is_err());

        // A branch is leftover unless an open run of the same project owns it.
        let mut scanned = facts("p", vec![]);
        scanned.pma_branches = vec!["pma/fix-it".into(), "pma/stray".into()];
        let mut other = facts("q", vec![]);
        other.pma_branches = vec!["pma/fix-it".into()];
        let leftover = |store: &mut Store, facts: &[Facts]| {
            store.save_scan(facts, false, 1).unwrap();
            let projects = store.projects().unwrap();
            projects.iter().map(|p| p.leftover).collect::<Vec<_>>()
        };
        assert_eq!(
            leftover(&mut store, &[scanned.clone(), other.clone()]),
            [1, 1]
        );
        run.state = RunState::Pushed;
        store.update_run(&run).unwrap();
        assert_eq!(leftover(&mut store, &[scanned, other]), [2, 1]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
