use rusqlite::Connection;

use crate::errors::Result;

pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(PRAGMAS)?;
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

const PRAGMAS: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA foreign_keys = ON;
    PRAGMA busy_timeout = 5000;
";

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS notes (
        id            INTEGER PRIMARY KEY,
        path          TEXT NOT NULL UNIQUE,
        title         TEXT,
        content_hash  INTEGER NOT NULL,
        frontmatter   TEXT,
        body          TEXT NOT NULL,
        tags          TEXT NOT NULL DEFAULT '',
        created_at    TEXT,
        modified_at   TEXT,
        is_ai         BOOLEAN NOT NULL DEFAULT 0,
        ai_model      TEXT,
        ai_session    TEXT,
        ai_reviewed   BOOLEAN DEFAULT 0,
        indexed_at    TEXT NOT NULL DEFAULT (datetime('now'))
    );

    CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
        title,
        body,
        tags,
        content = 'notes',
        content_rowid = 'id',
        tokenize = 'porter unicode61 remove_diacritics 2'
    );

    CREATE TRIGGER IF NOT EXISTS notes_ai AFTER INSERT ON notes BEGIN
        INSERT INTO notes_fts(rowid, title, body, tags)
        VALUES (new.id, new.title, new.body, new.tags);
    END;

    CREATE TRIGGER IF NOT EXISTS notes_ad AFTER DELETE ON notes BEGIN
        INSERT INTO notes_fts(notes_fts, rowid, title, body, tags)
        VALUES ('delete', old.id, old.title, old.body, old.tags);
    END;

    CREATE TRIGGER IF NOT EXISTS notes_au AFTER UPDATE ON notes BEGIN
        INSERT INTO notes_fts(notes_fts, rowid, title, body, tags)
        VALUES ('delete', old.id, old.title, old.body, old.tags);
        INSERT INTO notes_fts(rowid, title, body, tags)
        VALUES (new.id, new.title, new.body, new.tags);
    END;

    CREATE TABLE IF NOT EXISTS tags (
        note_id  INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
        tag      TEXT NOT NULL,
        PRIMARY KEY (note_id, tag)
    );
    CREATE INDEX IF NOT EXISTS idx_tags_tag ON tags(tag);

    CREATE TABLE IF NOT EXISTS links (
        source_id  INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
        target     TEXT NOT NULL,
        PRIMARY KEY (source_id, target)
    );
    CREATE INDEX IF NOT EXISTS idx_links_target ON links(target);

    CREATE TABLE IF NOT EXISTS index_stats (
        run_id        TEXT PRIMARY KEY,
        started_at    TEXT NOT NULL,
        finished_at   TEXT,
        notes_scanned INTEGER,
        notes_updated INTEGER,
        notes_added   INTEGER,
        notes_removed INTEGER,
        duration_ms   INTEGER,
        errors        TEXT
    );
";

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn schema_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        // Running again should not fail
        init_schema(&conn).unwrap();
    }

    #[test]
    fn wal_mode_set() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = Connection::open(&db_path).unwrap();
        init_schema(&conn).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn fts5_available() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        // FTS5 table should exist and be queryable
        let count: i64 = conn
            .query_row("SELECT count(*) FROM notes_fts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
