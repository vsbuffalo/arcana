use rusqlite::params;

use crate::errors::Result;
use crate::index::Database;

pub struct IndexEntry {
    pub path: String,
    pub title: Option<String>,
    pub content_hash: u64,
    pub frontmatter_yaml: Option<String>,
    pub body: String,
    pub created_at: Option<String>,
    pub modified_at: Option<String>,
    pub is_ai: bool,
    pub ai_model: Option<String>,
    pub ai_session: Option<String>,
    pub ai_reviewed: bool,
    pub tags: Vec<String>,
    pub links: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub notes_scanned: usize,
    pub notes_added: usize,
    pub notes_updated: usize,
    pub notes_removed: usize,
    pub notes_unchanged: usize,
}

impl Database {
    pub fn get_content_hash(&self, path: &str) -> Result<Option<u64>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT content_hash FROM notes WHERE path = ?1")?;
        let result = stmt
            .query_row(params![path], |row| {
                let hash: i64 = row.get(0)?;
                Ok(hash as u64)
            })
            .ok();
        Ok(result)
    }

    pub fn get_all_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached("SELECT path FROM notes")?;
        let paths = stmt
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?;
        Ok(paths)
    }

    pub fn upsert_note(&self, entry: &IndexEntry) -> Result<i64> {
        let tags_str = entry.tags.join(" ");

        // The triggers on notes automatically sync FTS5.
        self.conn.execute(
            "INSERT INTO notes (path, title, content_hash, frontmatter, body, tags, created_at, modified_at, is_ai, ai_model, ai_session, ai_reviewed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(path) DO UPDATE SET
                title = excluded.title,
                content_hash = excluded.content_hash,
                frontmatter = excluded.frontmatter,
                body = excluded.body,
                tags = excluded.tags,
                created_at = excluded.created_at,
                modified_at = excluded.modified_at,
                is_ai = excluded.is_ai,
                ai_model = excluded.ai_model,
                ai_session = excluded.ai_session,
                ai_reviewed = excluded.ai_reviewed,
                indexed_at = datetime('now')",
            params![
                entry.path,
                entry.title,
                entry.content_hash as i64,
                entry.frontmatter_yaml,
                entry.body,
                tags_str,
                entry.created_at,
                entry.modified_at,
                entry.is_ai,
                entry.ai_model,
                entry.ai_session,
                entry.ai_reviewed,
            ],
        )?;

        let note_id: i64 = self.conn.query_row(
            "SELECT id FROM notes WHERE path = ?1",
            params![entry.path],
            |row| row.get(0),
        )?;

        Ok(note_id)
    }

    pub fn upsert_tags(&self, note_id: i64, tags: &[String]) -> Result<()> {
        self.conn
            .execute("DELETE FROM tags WHERE note_id = ?1", params![note_id])?;
        let mut stmt = self
            .conn
            .prepare_cached("INSERT INTO tags (note_id, tag) VALUES (?1, ?2)")?;
        for tag in tags {
            stmt.execute(params![note_id, tag])?;
        }
        Ok(())
    }

    pub fn upsert_links(&self, note_id: i64, links: &[String]) -> Result<()> {
        self.conn
            .execute("DELETE FROM links WHERE source_id = ?1", params![note_id])?;
        let mut stmt = self
            .conn
            .prepare_cached("INSERT OR IGNORE INTO links (source_id, target) VALUES (?1, ?2)")?;
        for link in links {
            stmt.execute(params![note_id, link])?;
        }
        Ok(())
    }

    pub fn delete_missing_notes(&self, current_paths: &[String]) -> Result<usize> {
        if current_paths.is_empty() {
            let removed = self.conn.execute("DELETE FROM notes", [])?;
            return Ok(removed);
        }

        // Use a temp table to avoid exceeding SQLite's 999 variable limit
        self.conn
            .execute_batch("CREATE TEMP TABLE IF NOT EXISTS _keep_paths (path TEXT PRIMARY KEY)")?;
        self.conn.execute("DELETE FROM _keep_paths", [])?;

        {
            let mut stmt = self
                .conn
                .prepare_cached("INSERT OR IGNORE INTO _keep_paths (path) VALUES (?1)")?;
            for path in current_paths {
                stmt.execute(params![path])?;
            }
        }

        let removed = self.conn.execute(
            "DELETE FROM notes WHERE path NOT IN (SELECT path FROM _keep_paths)",
            [],
        )?;

        self.conn.execute("DELETE FROM _keep_paths", [])?;
        Ok(removed)
    }

    pub fn note_count(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT count(*) FROM notes", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn tag_count(&self) -> Result<usize> {
        let count: i64 =
            self.conn
                .query_row("SELECT count(DISTINCT tag) FROM tags", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn link_count(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT count(*) FROM links", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn save_index_stats(
        &self,
        run_id: &str,
        started_at: &str,
        finished_at: &str,
        stats: &IndexStats,
        duration_ms: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO index_stats (run_id, started_at, finished_at, notes_scanned, notes_updated, notes_added, notes_removed, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run_id,
                started_at,
                finished_at,
                stats.notes_scanned as i64,
                stats.notes_updated as i64,
                stats.notes_added as i64,
                stats.notes_removed as i64,
                duration_ms,
            ],
        )?;
        Ok(())
    }
}
