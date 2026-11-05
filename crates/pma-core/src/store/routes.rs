use super::*;

impl Store {
    /// Stores a policy revision as a draft and returns its number. Storing
    /// is not activating: nothing routes by it until someone says so.
    pub fn add_route_revision(&self, document: &str, by: &str) -> Result<i64> {
        // Refused here, so an unreadable document never reaches the table.
        // Stored as `pma` read it, with every default written out, so two
        // revisions diff by what they mean rather than by how they were
        // typed, and `pma route show` states the route names a reader will
        // see in a replay.
        let document = Policy::parse(document)?.to_json();
        let next: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(revision), 0) + 1 FROM routes",
            [],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO routes (revision, document, proposed_by, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![next, document, by, crate::dates::now()],
        )?;
        Ok(next)
    }

    /// Puts a revision into effect, in shadow or for real, and retires any
    /// other. `by` is recorded: raising autonomy is somebody's decision.
    pub fn activate_route(&self, revision: i64, by: &str, shadow: bool) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let found = tx.execute(
            "UPDATE routes SET activated_at = ?2, activated_by = ?3, shadow = ?4
             WHERE revision = ?1",
            params![revision, crate::dates::now(), by, shadow],
        )?;
        if found == 0 {
            return Err(format!("no policy revision {revision}").into());
        }
        tx.execute(
            "UPDATE routes SET activated_at = NULL, activated_by = NULL WHERE revision <> ?1",
            [revision],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every revision, oldest first.
    pub fn route_revisions(&self) -> Result<Vec<RouteRevision>> {
        let mut stmt = self.conn.prepare(
            "SELECT revision, document, proposed_by, created_at, activated_at, activated_by,
                    shadow
             FROM routes ORDER BY revision",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(RouteRevision {
                revision: r.get(0)?,
                document: r.get(1)?,
                proposed_by: r.get(2)?,
                created_at: r.get(3)?,
                activated_at: r.get(4)?,
                activated_by: r.get(5)?,
                shadow: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The revision in effect, if any.
    pub fn active_route(&self) -> Result<Option<RouteRevision>> {
        Ok(self
            .route_revisions()?
            .into_iter()
            .find(|r| r.activated_at.is_some()))
    }
}

/// One stored policy document and who put it where it is.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteRevision {
    pub revision: i64,
    pub document: String,
    pub proposed_by: String,
    pub created_at: i64,
    pub activated_at: Option<i64>,
    pub activated_by: Option<String>,
    /// Computed and recorded on each run, but not applied.
    pub shadow: bool,
}

impl RouteRevision {
    pub fn policy(&self) -> Result<Policy> {
        Ok(Policy::parse(&self.document)?)
    }
}
