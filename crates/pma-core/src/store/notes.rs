use super::*;

impl Store {
    pub fn notes(&self) -> Result<Vec<Note>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, created_at, updated_at, text FROM notes ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok(Note {
                id: r.get(0)?,
                created_at: r.get(1)?,
                updated_at: r.get(2)?,
                text: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_note(&self, text: &str, now: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO notes (created_at, updated_at, text) VALUES (?1, ?1, ?2)",
            params![now, text],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Returns false when no note has the id.
    pub fn edit_note(&self, id: i64, text: &str, now: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE notes SET text = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, text, now],
        )? > 0)
    }

    /// Returns false when no note has the id.
    pub fn remove_note(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM notes WHERE id = ?1", [id])? > 0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub id: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::*;

    #[test]
    fn notes_add_edit_and_remove() {
        let dir = scratch("notes");
        let store = Store::open(&dir.join("p.db")).unwrap();
        let a = store
            .add_note("move CI to one reusable workflow", 10)
            .unwrap();
        let b = store.add_note("second", 20).unwrap();
        assert!(
            store
                .edit_note(a, "move CI to reusable workflows", 30)
                .unwrap()
        );
        assert!(!store.edit_note(99, "x", 30).unwrap());
        assert!(store.remove_note(b).unwrap());
        assert!(!store.remove_note(b).unwrap());
        assert_eq!(
            store.notes().unwrap(),
            [Note {
                id: a,
                created_at: 10,
                updated_at: 30,
                text: "move CI to reusable workflows".into()
            }]
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
