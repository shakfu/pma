use super::*;

impl Store {
    /// Creates a campaign with a fixed membership. The set does not change
    /// afterwards: a rescan must not move work under a campaign in flight.
    pub fn add_campaign(&self, c: &Campaign) -> Result<()> {
        if c.members.is_empty() {
            return Err(format!("campaign `{}` selects no project", c.name).into());
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO campaigns (name, text, description, class, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![c.name, c.text, c.description, c.class.name(), c.created_at],
        )
        .map_err(|e| format!("campaign `{}`: {e}", c.name))?;
        for (project, _) in &c.members {
            tx.execute(
                "INSERT INTO campaign_members (campaign, project) VALUES (?1, ?2)",
                params![c.name, project],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn campaigns(&self) -> Result<Vec<Campaign>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, text, description, class, created_at FROM campaigns ORDER BY name",
        )?;
        let rows: Vec<Campaign> = stmt
            .query_map([], |r| {
                let class: String = r.get(3)?;
                Ok(Campaign {
                    name: r.get(0)?,
                    text: r.get(1)?,
                    description: r.get(2)?,
                    class: Class::parse(&class).unwrap_or(Class::Specified),
                    created_at: r.get(4)?,
                    members: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut stmt = self
            .conn
            .prepare("SELECT campaign, project, run_id FROM campaign_members ORDER BY project")?;
        let members: Vec<(String, String, Option<i64>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .map(|mut c| {
                c.members = members
                    .iter()
                    .filter(|(name, ..)| *name == c.name)
                    .map(|(_, p, run)| (p.clone(), *run))
                    .collect();
                c
            })
            .collect())
    }

    pub fn campaign(&self, name: &str) -> Result<Campaign> {
        self.campaigns()?
            .into_iter()
            .find(|c| c.name == name)
            .ok_or_else(|| format!("unknown campaign `{name}`; see `pma campaign`").into())
    }

    /// Records which run took a member, so a restart does not dispatch it
    /// again and open a second pull request.
    pub fn set_campaign_run(&self, campaign: &str, project: &str, run: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE campaign_members SET run_id = ?3 WHERE campaign = ?1 AND project = ?2",
            params![campaign, project, run],
        )?;
        Ok(())
    }

    /// Returns false when no campaign has the name.
    pub fn remove_campaign(&self, name: &str) -> Result<bool> {
        self.conn
            .execute("DELETE FROM campaign_members WHERE campaign = ?1", [name])?;
        Ok(self
            .conn
            .execute("DELETE FROM campaigns WHERE name = ?1", [name])?
            > 0)
    }
}

/// One task definition applied across a fixed set of repositories.
#[derive(Debug, Clone, PartialEq)]
pub struct Campaign {
    pub name: String,
    pub text: String,
    pub description: String,
    pub class: Class,
    pub created_at: i64,
    /// Project, and the run that took it, if any.
    pub members: Vec<(String, Option<i64>)>,
}
