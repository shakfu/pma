use super::*;

impl Store {
    /// The seconds the last measured base verify took in `project`.
    pub fn last_base_seconds(&self, project: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT seconds FROM verify_base WHERE project = ?1 ORDER BY rowid DESC LIMIT 1",
                [project],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Attempts consumed against a task revision, across every run of it.
    pub fn consumed_attempts(&self, project: &str, revision: &str) -> Result<i64> {
        Ok(self
            .conn
            .query_row(
                "SELECT attempts FROM exhaustion WHERE project = ?1 AND revision = ?2",
                params![project, revision],
                |r| r.get(0),
            )
            // No row is no attempt; any other error is an error, since the
            // limit it guards would otherwise be lifted.
            .optional()?
            .unwrap_or(0))
    }

    /// Records one consumed attempt and returns the new total.
    pub fn consume_attempt(&self, project: &str, revision: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO exhaustion (project, revision, attempts, updated_at)
             VALUES (?1, ?2, 1, ?3)
             ON CONFLICT (project, revision)
             DO UPDATE SET attempts = attempts + 1, updated_at = ?3",
            params![project, revision, crate::dates::now()],
        )?;
        self.consumed_attempts(project, revision)
    }

    /// Takes back `n` consumed attempts, never below none.
    pub fn refund_attempts(&self, project: &str, revision: &str, n: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE exhaustion SET attempts = max(attempts - ?3, 0), updated_at = ?4
             WHERE project = ?1 AND revision = ?2",
            params![project, revision, n, crate::dates::now()],
        )?;
        Ok(())
    }

    /// Returns false when the revision had consumed none.
    pub fn reset_attempts(&self, project: &str, revision: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM exhaustion WHERE project = ?1 AND revision = ?2",
            params![project, revision],
        )? > 0)
    }

    /// A base verification of this exact repository, commit, command,
    /// timeout and sandbox, or `None` when it has not been measured.
    pub fn verify_base(
        &self,
        project: &str,
        base: &str,
        command: &str,
        timeout_minutes: i64,
        sandbox: bool,
    ) -> Result<Option<(Option<bool>, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT ok, seconds FROM verify_base
             WHERE project = ?1 AND base = ?2 AND command = ?3 AND timeout_minutes = ?4
               AND sandbox = ?5",
        )?;
        let mut rows = stmt.query_map(
            params![project, base, command, timeout_minutes, sandbox],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(rows.next().transpose()?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_verify_base(
        &self,
        project: &str,
        base: &str,
        command: &str,
        timeout_minutes: i64,
        sandbox: bool,
        ok: Option<bool>,
        seconds: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO verify_base
                (project, base, command, timeout_minutes, sandbox, ok, seconds, measured_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                project,
                base,
                command,
                timeout_minutes,
                sandbox,
                ok,
                seconds,
                crate::dates::now()
            ],
        )?;
        Ok(())
    }

    /// Appends an attempt. `n` is assigned here, so two reworks cannot race
    /// to the same number.
    pub fn insert_attempt(&self, a: &Attempt) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO attempts (run_id, n, agent, prompt, feedback, started_at, seconds,
                                   cost_usd, summary, verify, verify_ok, error, outcome, model)
             VALUES (?1, (SELECT COALESCE(MAX(n), 0) + 1 FROM attempts WHERE run_id = ?1),
                     ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                a.run_id,
                a.agent,
                a.prompt,
                a.feedback,
                a.started_at,
                a.seconds,
                a.cost_usd,
                a.summary,
                a.verify,
                a.verify_ok,
                a.error,
                a.outcome,
                a.model,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Every attempt, oldest first. `run` limits it to one run.
    pub fn attempts(&self, run: Option<i64>) -> Result<Vec<Attempt>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, n, agent, prompt, feedback, started_at, seconds, cost_usd,
                    summary, verify, verify_ok, error, outcome, model
             FROM attempts WHERE (?1 IS NULL OR run_id = ?1) ORDER BY run_id, n",
        )?;
        let rows = stmt.query_map([run], |r| {
            Ok(Attempt {
                id: r.get(0)?,
                run_id: r.get(1)?,
                n: r.get(2)?,
                agent: r.get(3)?,
                prompt: r.get(4)?,
                feedback: r.get(5)?,
                started_at: r.get(6)?,
                seconds: r.get(7)?,
                cost_usd: r.get(8)?,
                summary: r.get(9)?,
                verify: r.get(10)?,
                verify_ok: r.get(11)?,
                error: r.get(12)?,
                outcome: r.get(13)?,
                model: r.get(14)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// One agent invocation and the verification that followed it. Rows are
/// appended and never updated, so a rework cannot overwrite what the previous
/// attempt did.
#[derive(Debug, Clone, PartialEq)]
pub struct Attempt {
    pub id: i64,
    pub run_id: i64,
    /// 1 for the first attempt, then one per rework.
    pub n: i64,
    pub agent: String,
    pub prompt: String,
    /// The reviewer feedback this attempt was given, if any.
    pub feedback: Option<String>,
    pub started_at: i64,
    /// Agent and verify time for this attempt alone.
    pub seconds: Option<i64>,
    /// `None` when the agent reports no cost. Never zero for unknown.
    pub cost_usd: Option<f64>,
    pub summary: Option<String>,
    pub verify: Option<String>,
    pub verify_ok: Option<bool>,
    pub error: Option<String>,
    /// The state the run reached: `ready` or `failed`. Null for the attempt
    /// reconstructed from a run that predates this table.
    pub outcome: Option<String>,
    /// The model this attempt ran, or `None` for the worker's own default.
    pub model: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::*;

    /// The counter survives the run that raised it, so rejecting a task and
    /// dispatching it again does not start from zero. Rewording it does.
    #[test]
    fn consumed_attempts_are_kept_per_task_revision() {
        let dir = scratch("exhaustion");
        let store = Store::open(&dir.join("p.db")).unwrap();
        assert_eq!(store.consumed_attempts("p", "fix the parser").unwrap(), 0);

        assert_eq!(store.consume_attempt("p", "fix the parser").unwrap(), 1);
        assert_eq!(store.consume_attempt("p", "fix the parser").unwrap(), 2);
        assert_eq!(store.consumed_attempts("p", "fix the parser").unwrap(), 2);
        // A reworded task, another project, and a `ci` incident naming other
        // workflows are each their own revision.
        assert_eq!(
            store
                .consumed_attempts("p", "fix the parser on empty input")
                .unwrap(),
            0
        );
        assert_eq!(store.consumed_attempts("q", "fix the parser").unwrap(), 0);

        // A refund never takes the counter below none.
        store.refund_attempts("p", "fix the parser", 1).unwrap();
        assert_eq!(store.consumed_attempts("p", "fix the parser").unwrap(), 1);
        store.refund_attempts("p", "fix the parser", 3).unwrap();
        assert_eq!(store.consumed_attempts("p", "fix the parser").unwrap(), 0);
        store.consume_attempt("p", "fix the parser").unwrap();

        assert!(store.reset_attempts("p", "fix the parser").unwrap());
        assert_eq!(store.consumed_attempts("p", "fix the parser").unwrap(), 0);
        assert!(!store.reset_attempts("p", "fix the parser").unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The cache answers only for the exact repository, commit, command,
    /// timeout and sandbox it measured. A pass under a longer limit says
    /// nothing about a shorter one, a confined result nothing about an
    /// unconfined one, and a changed command nothing at all.
    #[test]
    fn base_verification_is_cached_per_command_and_timeout() {
        let dir = scratch("verify-base");
        let store = Store::open(&dir.join("p.db")).unwrap();
        assert_eq!(
            store
                .verify_base("p", "abc", "make test", 30, true)
                .unwrap(),
            None
        );

        store
            .set_verify_base("p", "abc", "make test", 30, true, Some(false), 12)
            .unwrap();
        assert_eq!(
            store
                .verify_base("p", "abc", "make test", 30, true)
                .unwrap(),
            Some((Some(false), 12))
        );
        for (base, command, timeout, sandbox) in [
            ("def", "make test", 30, true),
            ("abc", "cargo test", 30, true),
            ("abc", "make test", 10, true),
            ("abc", "make test", 30, false),
        ] {
            assert_eq!(
                store
                    .verify_base("p", base, command, timeout, sandbox)
                    .unwrap(),
                None,
                "{base} {command} {timeout} {sandbox}"
            );
        }
        // A base that could not be measured is unknown, not failing.
        store
            .set_verify_base("p", "def", "make test", 30, true, None, 0)
            .unwrap();
        assert_eq!(
            store
                .verify_base("p", "def", "make test", 30, true)
                .unwrap(),
            Some((None, 0))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A rework must not overwrite what the previous attempt did.
    #[test]
    fn attempts_are_appended_never_replaced() {
        let dir = scratch("attempts");
        let store = Store::open(&dir.join("p.db")).unwrap();
        let mut run = Run::blank();
        store.insert_run(&mut run).unwrap();
        assert!(run.dispatched_at.is_some(), "stamped on insert");

        let mut a = Attempt {
            id: 0,
            run_id: run.id,
            n: 0,
            agent: "claude".into(),
            prompt: "do it".into(),
            feedback: None,
            started_at: 10,
            seconds: Some(30),
            cost_usd: Some(0.2),
            summary: Some("first".into()),
            verify: Some("make test".into()),
            verify_ok: Some(false),
            error: None,
            outcome: Some("ready".into()),
            model: Some("haiku".into()),
        };
        store.insert_attempt(&a).unwrap();
        a.feedback = Some("try again".into());
        a.summary = Some("second".into());
        a.verify_ok = Some(true);
        store.insert_attempt(&a).unwrap();

        let rows = store.attempts(Some(run.id)).unwrap();
        assert_eq!(rows.iter().map(|r| r.n).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(rows[0].summary.as_deref(), Some("first"));
        assert_eq!(rows[0].verify_ok, Some(false));
        assert_eq!(rows[1].feedback.as_deref(), Some("try again"));
        assert!(store.attempts(None).unwrap().len() == 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
