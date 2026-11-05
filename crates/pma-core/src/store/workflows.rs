use super::*;

impl Store {
    /// Stores a document as a draft. Refused here, so an unreadable one never
    /// reaches the table, and stored normalised, so two revisions diff by what
    /// they mean rather than by how they were typed -- or by whether a script
    /// or a person wrote them. `source` keeps the script for provenance.
    pub fn add_workflow_revision(
        &self,
        doc: &crate::workflow::Document,
        source: Option<&str>,
        by: &str,
        worst_case_runs: i64,
        worst_case_cost: f64,
    ) -> Result<i64> {
        let next: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(revision), 0) + 1 FROM workflows",
            [],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO workflows
                (revision, document, source, proposed_by, created_at,
                 worst_case_runs, worst_case_cost)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                next,
                doc.to_json(),
                source,
                by,
                crate::dates::now(),
                worst_case_runs,
                worst_case_cost
            ],
        )?;
        Ok(next)
    }

    /// Puts a revision into effect and retires any other. `by` is recorded:
    /// what a workflow may spend is somebody's decision.
    pub fn activate_workflow(&self, revision: i64, by: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let found = tx.execute(
            "UPDATE workflows SET activated_at = ?2, activated_by = ?3 WHERE revision = ?1",
            params![revision, crate::dates::now(), by],
        )?;
        if found == 0 {
            return Err(format!("no workflow revision {revision}").into());
        }
        tx.execute(
            "UPDATE workflows SET activated_at = NULL, activated_by = NULL WHERE revision <> ?1",
            [revision],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every revision, oldest first.
    pub fn workflow_revisions(&self) -> Result<Vec<WorkflowRevision>> {
        let mut stmt = self.conn.prepare(
            "SELECT revision, document, source, proposed_by, created_at, activated_at,
                    activated_by, worst_case_runs, worst_case_cost
             FROM workflows ORDER BY revision",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(WorkflowRevision {
                revision: r.get(0)?,
                document: r.get(1)?,
                source: r.get(2)?,
                proposed_by: r.get(3)?,
                created_at: r.get(4)?,
                activated_at: r.get(5)?,
                activated_by: r.get(6)?,
                worst_case_runs: r.get(7)?,
                worst_case_cost: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The revision in effect, if any.
    pub fn active_workflow(&self) -> Result<Option<WorkflowRevision>> {
        Ok(self
            .workflow_revisions()?
            .into_iter()
            .find(|r| r.activated_at.is_some()))
    }

    /// One revision by number. What an instance resumes under, which is the
    /// revision it recorded rather than whichever is in effect now.
    pub fn workflow_revision(&self, revision: i64) -> Result<Option<WorkflowRevision>> {
        Ok(self
            .workflow_revisions()?
            .into_iter()
            .find(|r| r.revision == revision))
    }

    /// Starts an instance: the frozen argument bag and what it was run over.
    pub fn add_workflow_instance(
        &self,
        workflow: &str,
        revision: i64,
        args: &str,
        target: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO workflow_instances (workflow, revision, args, target, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![workflow, revision, args, target, crate::dates::now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn workflow_instances(&self) -> Result<Vec<WorkflowInstance>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workflow, revision, args, target, started_at, finished_at, outcome
             FROM workflow_instances ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(WorkflowInstance {
                id: r.get(0)?,
                workflow: r.get(1)?,
                revision: r.get(2)?,
                args: r.get(3)?,
                target: r.get(4)?,
                started_at: r.get(5)?,
                finished_at: r.get(6)?,
                outcome: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One instance by id.
    pub fn workflow_instance(&self, instance: i64) -> Result<Option<WorkflowInstance>> {
        Ok(self
            .workflow_instances()?
            .into_iter()
            .find(|i| i.id == instance))
    }

    /// Records why an instance cannot go on, without closing it. A pass that
    /// hit a cap has work recorded and a reason it stopped, and neither is a
    /// finished workflow.
    pub fn note_instance_outcome(&self, instance: i64, outcome: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_instances SET outcome = ?2 WHERE id = ?1",
            params![instance, outcome],
        )?;
        Ok(())
    }

    /// Records new arguments on an instance and clears the outcome that
    /// stopped it, so a raised cap resumes it.
    pub fn reopen_instance(&self, instance: i64, args: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_instances SET args = ?2, outcome = NULL WHERE id = ?1",
            params![instance, args],
        )?;
        Ok(())
    }

    pub fn finish_instance(&self, instance: i64, outcome: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_instances SET finished_at = ?2, outcome = ?3 WHERE id = ?1",
            params![instance, crate::dates::now(), outcome],
        )?;
        Ok(())
    }

    /// Units are immutable, so this only ever inserts.
    pub fn add_workflow_unit(&self, instance: i64, u: &WorkflowUnit) -> Result<()> {
        self.conn.execute(
            "INSERT INTO workflow_units
                (instance, id, type, node, parent, root, depth, lap, project, data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                instance,
                u.id,
                u.ty,
                u.node,
                u.parent,
                u.root,
                u.depth,
                u.lap,
                u.project,
                u.data,
                crate::dates::now()
            ],
        )?;
        Ok(())
    }

    pub fn workflow_units(&self, instance: i64) -> Result<Vec<WorkflowUnit>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, type, node, parent, root, depth, lap, project, data
             FROM workflow_units WHERE instance = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map([instance], |r| {
            Ok(WorkflowUnit {
                id: r.get(0)?,
                ty: r.get(1)?,
                node: r.get(2)?,
                parent: r.get(3)?,
                root: r.get(4)?,
                depth: r.get(5)?,
                lap: r.get(6)?,
                project: r.get(7)?,
                data: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One row per unit and edge: which way it went, or the guard that
    /// refused it. A unit that stopped is therefore answerable for.
    pub fn add_workflow_move(
        &self,
        instance: i64,
        unit: &str,
        edge: usize,
        taken: bool,
        reason: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO workflow_moves (instance, unit, edge, taken, reason, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                instance,
                unit,
                edge as i64,
                taken,
                reason,
                crate::dates::now()
            ],
        )?;
        Ok(())
    }

    /// Every move recorded, as `(unit, edge, taken)`.
    pub fn workflow_moves(&self, instance: i64) -> Result<Vec<(String, usize, bool)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT unit, edge, taken FROM workflow_moves WHERE instance = ?1")?;
        let rows = stmt.query_map([instance], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)? as usize,
                r.get::<_, bool>(2)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_workflow_verdict(
        &self,
        instance: i64,
        unit: &str,
        check: &str,
        verdict: &str,
        detail: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO workflow_verdicts
                (instance, unit, check_name, verdict, detail, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![instance, unit, check, verdict, detail, crate::dates::now()],
        )?;
        Ok(())
    }

    /// Verdicts by unit and check name.
    pub fn workflow_verdicts(&self, instance: i64) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT unit, check_name, verdict FROM workflow_verdicts WHERE instance = ?1",
        )?;
        let rows = stmt.query_map([instance], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// What a workflow instance's runs have cost, a run that reported no
    /// cost counted at `unknown`.
    pub fn instance_spend(&self, instance: i64, unknown: f64) -> Result<f64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(COALESCE(cost_usd, ?2)), 0) FROM runs
             WHERE workflow_instance = ?1",
            params![instance, unknown],
            |r| r.get(0),
        )?)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowInstance {
    pub id: i64,
    pub workflow: String,
    pub revision: i64,
    /// The arguments the pass was given, as JSON, so a replay reads the same
    /// instantiation rather than the document's defaults.
    pub args: String,
    /// What the argument bag was taken from.
    pub target: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub outcome: Option<String>,
}

/// A unit as stored: immutable, carrying its lineage and its data.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowUnit {
    pub id: String,
    pub ty: String,
    /// The node that wrote it; `@input` for a root unit.
    pub node: String,
    pub parent: Option<String>,
    pub root: String,
    pub depth: i64,
    pub lap: i64,
    pub project: Option<String>,
    /// The declared fields, as JSON.
    pub data: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowRevision {
    pub revision: i64,
    /// The document as `pma` read it.
    pub document: String,
    /// The script that built it, where one did.
    pub source: Option<String>,
    pub proposed_by: String,
    pub created_at: i64,
    pub activated_at: Option<i64>,
    pub activated_by: Option<String>,
    /// Over one unit of input, so what a pass may spend is this times the
    /// argument bag.
    pub worst_case_runs: Option<i64>,
    pub worst_case_cost: Option<f64>,
}

impl WorkflowRevision {
    pub fn document(&self) -> Result<crate::workflow::Document> {
        Ok(crate::workflow::Document::parse(&self.document)?)
    }
}
