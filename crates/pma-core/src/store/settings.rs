use super::*;

impl Store {
    /// Fills the configuration tables from `settings.toml` when its text
    /// changed since they were last filled. With no file, writes one from the
    /// tables: the first run after they held the configuration. A migration
    /// that changes a configuration row must change the file too, or the next
    /// edit of the file undoes it.
    pub(super) fn load_settings(&self) -> Result<()> {
        if !self.settings.exists() {
            crate::settings::create(&self.settings, &self.settings_from_tables()?)?;
        }
        let (text, settings) = crate::settings::read(&self.settings)?;
        let synced: Option<String> = self
            .conn
            .query_row("SELECT text FROM settings_sync", [], |r| r.get(0))
            .optional()?;
        if synced.as_deref() != Some(text.as_str()) {
            self.apply_settings(&text, &settings)?;
        }
        Ok(())
    }

    fn settings_from_tables(&self) -> Result<crate::settings::Settings> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, tier FROM projects WHERE tier IS NOT NULL ORDER BY name")?;
        let tiers = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(crate::settings::Settings {
            config: self.config_rows()?,
            roots: self.roots()?,
            tiers,
            tags: self.project_tags()?,
            agents: self.agents()?,
            presets: self.presets()?,
        })
    }

    /// Replaces the configuration tables with what the file says. A tier for a
    /// project no scan has recorded waits for the scan.
    fn apply_settings(&self, text: &str, s: &crate::settings::Settings) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute_batch(
            "DELETE FROM config; DELETE FROM roots; DELETE FROM agents; DELETE FROM presets;
             DELETE FROM project_tags; DELETE FROM settings_sync;
             UPDATE projects SET tier = NULL;",
        )?;
        for (key, value) in &s.config {
            tx.execute(
                "INSERT OR REPLACE INTO config (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
        }
        for root in &s.roots {
            tx.execute(
                "INSERT OR IGNORE INTO roots (path) VALUES (?1)",
                [path_str(root)?],
            )?;
        }
        for (name, tier) in &s.tiers {
            tx.execute(
                "UPDATE projects SET tier = ?2 WHERE name = ?1",
                params![name, tier],
            )?;
        }
        for (project, tag) in &s.tags {
            tx.execute(
                "INSERT OR IGNORE INTO project_tags (project, tag) VALUES (?1, ?2)",
                params![project, tag],
            )?;
        }
        for w in &s.agents {
            seed_agent(&tx, w)?;
        }
        for p in &s.presets {
            tx.execute(
                "INSERT OR REPLACE INTO presets (name, agent, model, args, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    p.name,
                    p.agent,
                    p.model,
                    serde_json::to_string(&p.args).unwrap_or_else(|_| "[]".into()),
                    crate::dates::now()
                ],
            )?;
        }
        tx.execute("INSERT INTO settings_sync (text) VALUES (?1)", [text])?;
        tx.commit()?;
        Ok(())
    }

    /// Edits `settings.toml` with `f`, then refills the tables from it.
    pub(super) fn change_settings<T>(
        &self,
        f: impl FnOnce(&mut toml_edit::DocumentMut) -> T,
    ) -> Result<T> {
        let out = crate::settings::edit(&self.settings, f)?;
        let (text, settings) = crate::settings::read(&self.settings)?;
        self.apply_settings(&text, &settings)?;
        Ok(out)
    }

    /// Re-applies the file's tiers and tags, for projects a scan just added.
    pub(super) fn reapply_settings(&self) -> Result<()> {
        let (text, settings) = crate::settings::read(&self.settings)?;
        self.apply_settings(&text, &settings)
    }

    pub fn config_rows(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM config ORDER BY key")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.change_settings(|doc| crate::settings::set_config(doc, key, value))
    }

    /// Returns whether a stored value was removed.
    pub fn reset_config(&self, key: &str) -> Result<bool> {
        self.change_settings(|doc| crate::settings::reset_config(doc, key))
    }

    pub fn roots(&self) -> Result<Vec<PathBuf>> {
        let mut stmt = self.conn.prepare("SELECT path FROM roots ORDER BY path")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows
            .map(|r| r.map(PathBuf::from))
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Returns false when the root was already present.
    pub fn add_root(&self, path: &Path) -> Result<bool> {
        let path = path_str(path)?;
        self.change_settings(|doc| crate::settings::add_root(doc, path))
    }

    pub fn remove_root(&self, path: &Path) -> Result<bool> {
        let path = path_str(path)?;
        self.change_settings(|doc| crate::settings::remove_root(doc, path))
    }

    /// Records the project first if no scan has, so the tier has a row.
    pub fn set_tier(&self, name: &str, path: &Path, tier: Option<u8>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO projects (name, path, tier) VALUES (?1, ?2, ?3)
             ON CONFLICT (name) DO UPDATE SET tier = excluded.tier",
            params![name, path_str(path)?, tier],
        )?;
        self.change_settings(|doc| crate::settings::set_tier(doc, name, tier))
    }

    /// Names a worker, a model and the arguments that configure it. Replaces
    /// one of the same name.
    pub fn set_preset(&self, p: &Preset) -> Result<()> {
        if self.agent(&p.agent).is_err() {
            return Err(format!("no worker `{}`; `pma agent` lists them", p.agent).into());
        }
        self.change_settings(|doc| crate::settings::set_preset(doc, p))
    }

    pub fn presets(&self) -> Result<Vec<Preset>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, agent, model, args FROM presets ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            Ok(Preset {
                name: r.get(0)?,
                agent: r.get(1)?,
                model: r.get::<_, Option<String>>(2)?.filter(|m| !m.is_empty()),
                args: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn preset(&self, name: &str) -> Result<Preset> {
        self.presets()?
            .into_iter()
            .find(|p| p.name == name)
            .ok_or_else(|| format!("no preset `{name}`; `pma preset` lists them").into())
    }

    /// Returns false when no preset has the name.
    pub fn remove_preset(&self, name: &str) -> Result<bool> {
        self.change_settings(|doc| crate::settings::remove_preset(doc, name))
    }

    /// Every worker, by name.
    pub fn agents(&self) -> Result<Vec<Worker>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, command, args, allow, parse, reports_cost, enforces_budget,
                    sandbox, resumes, env, interactive_args, relay, relay_args
             FROM agents ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| {
            let args: String = r.get(2)?;
            let parse: String = r.get(4)?;
            Ok(Worker {
                name: r.get(0)?,
                command: r.get(1)?,
                args: serde_json::from_str(&args).unwrap_or_default(),
                interactive_args: serde_json::from_str(&r.get::<_, String>(10)?)
                    .unwrap_or_default(),
                relay: r.get::<_, Option<String>>(11)?.filter(|a| !a.is_empty()),
                relay_args: serde_json::from_str(&r.get::<_, String>(12)?).unwrap_or_default(),
                allow: r.get::<_, Option<String>>(3)?.filter(|a| !a.is_empty()),
                parse: Parser::parse(&parse).unwrap_or(Parser::TextTail),
                reports_cost: r.get(5)?,
                enforces_budget: r.get(6)?,
                sandbox: r.get(7)?,
                resumes: r.get(8)?,
                env: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn agent(&self, name: &str) -> Result<Worker> {
        self.agents()?
            .into_iter()
            .find(|w| w.name == name)
            .ok_or_else(|| format!("unknown agent `{name}`; see `pma agent`").into())
    }

    pub fn set_agent(&self, w: &Worker) -> Result<()> {
        self.change_settings(|doc| crate::settings::set_agent(doc, w))
    }

    /// Returns false when no agent has the name.
    pub fn remove_agent(&self, name: &str) -> Result<bool> {
        self.change_settings(|doc| crate::settings::remove_agent(doc, name))
    }
}

/// A named worker, model and configuration. `model` unset means the worker's
/// own default, and `args` are inserted where its record puts `{extra}`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Preset {
    pub name: String,
    pub agent: String,
    pub model: Option<String>,
    pub args: Vec<String>,
}

pub(super) fn seed_agent(conn: &Connection, w: &Worker) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO agents
            (name, command, args, allow, parse, reports_cost, enforces_budget, sandbox,
             resumes, env, interactive_args, relay, relay_args)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            w.name,
            w.command,
            serde_json::to_string(&w.args).unwrap_or_default(),
            w.allow,
            w.parse.name(),
            w.reports_cost,
            w.enforces_budget,
            w.sandbox,
            w.resumes,
            serde_json::to_string(&w.env).unwrap_or_else(|_| "{}".into()),
            serde_json::to_string(&w.interactive_args).unwrap_or_else(|_| "[]".into()),
            w.relay,
            serde_json::to_string(&w.relay_args).unwrap_or_else(|_| "[]".into()),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::*;

    /// The first open after the tables held the configuration writes them to
    /// `settings.toml`; from then on the file is what the tables say.
    #[test]
    fn settings_toml_takes_over_the_configuration_tables() {
        let dir = scratch("settings");
        let path = dir.join("p.db");
        {
            let store = Store::open(&path).unwrap();
            store
                .conn
                .execute_batch(
                    "INSERT INTO config (key, value) VALUES ('agent_budget', '3');
                 INSERT INTO roots (path) VALUES ('/r');
                 INSERT INTO project_tags (project, tag) VALUES ('a', 'py');",
                )
                .unwrap();
            store
                .conn
                .execute_batch("DELETE FROM settings_sync")
                .unwrap();
        }
        std::fs::remove_file(dir.join(crate::settings::FILE)).unwrap();
        let mut store = Store::open(&path).unwrap();
        let text = std::fs::read_to_string(dir.join(crate::settings::FILE)).unwrap();
        assert!(
            text.contains("agent_budget = 3") && text.contains("roots = [\"/r\"]"),
            "{text}"
        );
        assert!(
            text.contains("[agents.claude]"),
            "the seeded workers: {text}"
        );

        // A hand edit, applied on the next open; a tier waits for its scan.
        std::fs::write(
            dir.join(crate::settings::FILE),
            text.replace("agent_budget = 3", "agent_budget = 4") + "\n[projects.b]\ntier = 2\n",
        )
        .unwrap();
        drop(store);
        store = Store::open(&path).unwrap();
        assert!(
            store
                .config_rows()
                .unwrap()
                .contains(&("agent_budget".into(), "4".into()))
        );
        assert!(store.project("b").unwrap().is_none());
        store.save_scan(&[facts("b", vec![])], true, 1).unwrap();
        assert_eq!(store.project("b").unwrap().unwrap().tier, Some(2));

        // An invalid file stops the store from opening, naming the key.
        std::fs::write(
            dir.join(crate::settings::FILE),
            "[config]\nmax_parallel = 0\n",
        )
        .unwrap();
        drop(store);
        let e = Store::open(&path).err().expect("refused").to_string();
        assert!(e.contains("config.max_parallel"), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn roots_add_and_remove() {
        let dir = scratch("roots");
        let store = Store::open(&dir.join("p.db")).unwrap();
        assert!(store.add_root(Path::new("/a")).unwrap());
        assert!(!store.add_root(Path::new("/a")).unwrap());
        store.add_root(Path::new("/b")).unwrap();
        assert_eq!(
            store.roots().unwrap(),
            [PathBuf::from("/a"), PathBuf::from("/b")]
        );
        assert!(store.remove_root(Path::new("/a")).unwrap());
        assert_eq!(store.roots().unwrap(), [PathBuf::from("/b")]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
