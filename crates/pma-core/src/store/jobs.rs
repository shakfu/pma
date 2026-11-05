use super::*;

impl Store {
    /// For each run a dispatch job about one project started: seconds from the
    /// job being queued to the agent starting.
    pub fn key_to_agent(&self) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.dispatched_at - (SELECT MAX(j.created_at) FROM jobs j
                 WHERE j.project = r.project AND j.args LIKE '[\"dispatch\"%'
                   AND j.created_at <= r.dispatched_at AND j.finished_at >= r.dispatched_at)
             FROM runs r",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, Option<i64>>(0))?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect())
    }

    /// Refuses a command already queued or running from the same directory,
    /// in the same statement, so two clicks cannot both queue it.
    pub fn add_job(&self, args: &[String], cwd: &Path, project: Option<&str>) -> Result<i64> {
        let json = serde_json::to_string(args)?;
        let cwd = cwd.to_string_lossy();
        let added = self.conn.execute(
            "INSERT INTO jobs (args, cwd, state, created_at, project)
             SELECT ?1, ?2, 'queued', ?3, ?4
             WHERE NOT EXISTS (SELECT 1 FROM jobs WHERE args = ?1 AND cwd = ?2
                               AND state IN ('queued', 'running'))",
            params![json, cwd, crate::dates::now(), project],
        )?;
        if added == 0 {
            let (id, state): (i64, String) = self.conn.query_row(
                "SELECT id, state FROM jobs WHERE args = ?1 AND cwd = ?2
                 AND state IN ('queued', 'running') ORDER BY id LIMIT 1",
                params![json, cwd],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            return Err(format!("job {id} is {state} already: pma {}", args.join(" ")).into());
        }
        Ok(self.conn.last_insert_rowid())
    }

    /// The newest `limit` jobs, newest first.
    pub fn jobs(&self, limit: i64) -> Result<Vec<Job>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {JOB_COLUMNS} FROM jobs ORDER BY id DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map([limit], job_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn job(&self, id: i64) -> Result<Job> {
        self.conn
            .query_row(
                &format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = ?1"),
                [id],
                job_row,
            )
            .optional()?
            .ok_or_else(|| format!("no job {id}; `pma job` lists them").into())
    }

    /// Marks the oldest queued job running and returns it.
    /// Queued jobs, oldest first.
    pub fn queued_jobs(&self) -> Result<Vec<Job>> {
        let mut jobs = self.jobs(i64::MAX)?;
        jobs.retain(|j| j.state == "queued");
        jobs.sort_by_key(|j| j.id);
        Ok(jobs)
    }

    /// Takes a queued job; `None` if it was stopped first.
    pub fn claim_job(&self, id: i64) -> Result<Option<Job>> {
        let claimed = self.conn.execute(
            "UPDATE jobs SET state = 'running', started_at = ?2 WHERE id = ?1 AND state = 'queued'",
            params![id, crate::dates::now()],
        )?;
        match claimed {
            0 => Ok(None),
            _ => self.job(id).map(Some),
        }
    }

    /// Records what would approve the plan a job stopped at.
    pub fn set_job_awaiting(&self, id: i64, args: &[String]) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET awaiting = ?2 WHERE id = ?1",
            params![id, serde_json::to_string(args)?],
        )?;
        Ok(())
    }

    /// Marks an awaiting job approved. False if it was not awaiting.
    pub fn approve_job(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE jobs SET state = 'approved' WHERE id = ?1 AND state = 'awaiting-approval'",
            [id],
        )? > 0)
    }

    /// Marks every job awaiting `plan` approved, once that plan has run by
    /// any route. Plan ids are hex, so the quoted match is exact.
    pub fn settle_jobs_awaiting(&self, plan: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET state = 'approved'
             WHERE state = 'awaiting-approval' AND instr(awaiting, '\"' || ?1 || '\"') > 0",
            [plan],
        )?;
        Ok(())
    }

    pub fn set_job_pid(&self, id: i64, pid: u32) -> Result<()> {
        self.conn
            .execute("UPDATE jobs SET pid = ?2 WHERE id = ?1", params![id, pid])?;
        Ok(())
    }

    /// Records how a job ended. A job stopped while it ran stays `stopped`.
    pub fn finish_job(&self, id: i64, exit_code: Option<i32>) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET finished_at = ?2, exit_code = ?3,
                 state = CASE WHEN state = 'stopped' THEN 'stopped'
                              WHEN ?3 = 0 AND awaiting IS NOT NULL THEN 'awaiting-approval'
                              WHEN ?3 = 0 THEN 'done' ELSE 'failed' END
             WHERE id = ?1",
            params![id, crate::dates::now(), exit_code],
        )?;
        Ok(())
    }

    /// Returns the state the job was in. Only a queued or running job stops.
    pub fn stop_job(&self, id: i64) -> Result<String> {
        let job = self.job(id)?;
        if matches!(job.state.as_str(), "queued" | "running") {
            self.conn.execute(
                "UPDATE jobs SET state = 'stopped', finished_at = ?2 WHERE id = ?1",
                params![id, crate::dates::now()],
            )?;
        }
        Ok(job.state)
    }

    /// Jobs left `running` by a runner that exited. Called under the runner
    /// lock, so no live runner holds them.
    pub fn fail_running_jobs(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE jobs SET state = 'failed', finished_at = ?1 WHERE state = 'running'",
            [crate::dates::now()],
        )?)
    }

    pub fn has_queued_jobs(&self) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM jobs WHERE state = 'queued'",
            [],
            |r| r.get::<_, i64>(0),
        )? > 0)
    }
}

/// A `pma` command queued to run detached.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: i64,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// `queued`, `running`, `done`, `failed` or `stopped`.
    pub state: String,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    /// The process group leader of a running job.
    pub pid: Option<i64>,
    pub exit_code: Option<i64>,
    /// For a job that stopped at a workflow plan: the arguments that approve
    /// it, which `pma job approve` queues.
    pub awaiting: Option<Vec<String>>,
    /// The project the command is about, when it names one.
    pub project: Option<String>,
}

const JOB_COLUMNS: &str = "id, args, cwd, state, created_at, started_at, finished_at, pid, \
                           exit_code, awaiting, project";

fn job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let args: String = r.get(1)?;
    Ok(Job {
        id: r.get(0)?,
        args: serde_json::from_str(&args).unwrap_or_default(),
        cwd: PathBuf::from(r.get::<_, String>(2)?),
        state: r.get(3)?,
        created_at: r.get(4)?,
        started_at: r.get(5)?,
        finished_at: r.get(6)?,
        pid: r.get(7)?,
        exit_code: r.get(8)?,
        awaiting: r
            .get::<_, Option<String>>(9)?
            .and_then(|a| serde_json::from_str(&a).ok()),
        project: r.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::*;

    /// A command not yet finished is not queued twice from one directory.
    #[test]
    fn a_job_is_not_queued_twice() {
        let dir = scratch("job-twice");
        let store = Store::open(&dir.join("p.db")).unwrap();
        let args = ["verify".to_string(), "a".into()];
        let first = store.add_job(&args, &dir, Some("a")).unwrap();
        let err = store.add_job(&args, &dir, Some("a")).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("job {first} is queued already: pma verify a")
        );
        store.add_job(&args, &dir.join("x"), Some("a")).unwrap();
        store.finish_job(first, Some(0)).unwrap();
        store.add_job(&args, &dir, Some("a")).unwrap();
    }
}
