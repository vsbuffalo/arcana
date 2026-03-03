use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

use crate::errors::Result;
use crate::index::Database;

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub text: String,
    pub limit: Option<usize>,
    pub filters: SearchFilters,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchFilters {
    pub tags: Vec<String>,
    pub path_prefix: Option<String>,
    pub ai_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub note_id: i64,
    pub path: String,
    pub title: Option<String>,
    pub snippet: String,
    pub score: f64,
}

static RESERVED_CHARS: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"[*"()+]"#).unwrap());

static RESERVED_WORDS: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\b(AND|OR|NOT|NEAR)\b").unwrap());

pub fn sanitize_fts_query(input: &str) -> String {
    if input.trim().is_empty() {
        return String::new();
    }

    // Remove reserved characters
    let cleaned = RESERVED_CHARS.replace_all(input, " ");
    // Remove reserved words
    let cleaned = RESERVED_WORDS.replace_all(&cleaned, " ");
    // Handle hyphens: turn them into spaces
    let cleaned = cleaned.replace('-', " ");

    // Split into terms, quote each, join with implicit AND (space)
    let terms: Vec<String> = cleaned
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t))
        .collect();

    terms.join(" ")
}

pub fn execute_search(db: &Database, query: &SearchQuery) -> Result<Vec<SearchResult>> {
    let sanitized = sanitize_fts_query(&query.text);
    if sanitized.is_empty() {
        return Ok(vec![]);
    }

    let limit = query.limit.unwrap_or(20);
    let has_tag_filter = !query.filters.tags.is_empty();
    let has_path_filter = query.filters.path_prefix.is_some();

    if has_tag_filter && has_path_filter {
        let tag = &query.filters.tags[0];
        let path_pat = format!("{}%", query.filters.path_prefix.as_ref().unwrap());
        let mut stmt = db.conn.prepare(
            "SELECT n.id, n.path, n.title,
                    snippet(notes_fts, 1, '<mark>', '</mark>', '...', 32) as snippet,
                    bm25(notes_fts, 5.0, 1.0, 2.0) as score
             FROM notes_fts
             JOIN notes n ON n.id = notes_fts.rowid
             JOIN tags t ON t.note_id = n.id
             WHERE notes_fts MATCH ?1
               AND n.path LIKE ?2
               AND t.tag = ?3
             ORDER BY score
             LIMIT ?4",
        )?;
        let results = stmt
            .query_map(
                params![sanitized, path_pat, tag, limit as i64],
                map_search_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(results)
    } else if has_tag_filter {
        let tag = &query.filters.tags[0];
        let mut stmt = db.conn.prepare(
            "SELECT n.id, n.path, n.title,
                    snippet(notes_fts, 1, '<mark>', '</mark>', '...', 32) as snippet,
                    bm25(notes_fts, 5.0, 1.0, 2.0) as score
             FROM notes_fts
             JOIN notes n ON n.id = notes_fts.rowid
             JOIN tags t ON t.note_id = n.id
             WHERE notes_fts MATCH ?1
               AND t.tag = ?2
             ORDER BY score
             LIMIT ?3",
        )?;
        let results = stmt
            .query_map(params![sanitized, tag, limit as i64], map_search_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(results)
    } else if has_path_filter {
        let path_pat = format!("{}%", query.filters.path_prefix.as_ref().unwrap());
        let mut stmt = db.conn.prepare(
            "SELECT n.id, n.path, n.title,
                    snippet(notes_fts, 1, '<mark>', '</mark>', '...', 32) as snippet,
                    bm25(notes_fts, 5.0, 1.0, 2.0) as score
             FROM notes_fts
             JOIN notes n ON n.id = notes_fts.rowid
             WHERE notes_fts MATCH ?1
               AND n.path LIKE ?2
             ORDER BY score
             LIMIT ?3",
        )?;
        let results = stmt
            .query_map(params![sanitized, path_pat, limit as i64], map_search_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(results)
    } else {
        let ai_filter = if query.filters.ai_only {
            " AND n.is_ai = 1"
        } else {
            ""
        };
        let sql = format!(
            "SELECT n.id, n.path, n.title,
                    snippet(notes_fts, 1, '<mark>', '</mark>', '...', 32) as snippet,
                    bm25(notes_fts, 5.0, 1.0, 2.0) as score
             FROM notes_fts
             JOIN notes n ON n.id = notes_fts.rowid
             WHERE notes_fts MATCH ?1{}
             ORDER BY score
             LIMIT ?2",
            ai_filter
        );
        let mut stmt = db.conn.prepare(&sql)?;
        let results = stmt
            .query_map(params![sanitized, limit as i64], map_search_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(results)
    }
}

fn map_search_row(row: &rusqlite::Row) -> rusqlite::Result<SearchResult> {
    Ok(SearchResult {
        note_id: row.get(0)?,
        path: row.get(1)?,
        title: row.get(2)?,
        snippet: row.get(3)?,
        score: row.get(4)?,
    })
}

pub fn execute_list(
    db: &Database,
    filters: &SearchFilters,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    let has_tag_filter = !filters.tags.is_empty();
    let has_path_filter = filters.path_prefix.is_some();

    if has_tag_filter {
        let tag = &filters.tags[0];
        let mut stmt = db.conn.prepare(
            "SELECT n.id, n.path, n.title, '' as snippet, 0.0 as score
             FROM notes n
             JOIN tags t ON t.note_id = n.id
             WHERE t.tag = ?1
             ORDER BY n.path
             LIMIT ?2",
        )?;
        let results = stmt
            .query_map(params![tag, limit as i64], map_search_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(results)
    } else if has_path_filter {
        let path_pat = format!("{}%", filters.path_prefix.as_ref().unwrap());
        let mut stmt = db.conn.prepare(
            "SELECT n.id, n.path, n.title, '' as snippet, 0.0 as score
             FROM notes n
             WHERE n.path LIKE ?1
             ORDER BY n.path
             LIMIT ?2",
        )?;
        let results = stmt
            .query_map(params![path_pat, limit as i64], map_search_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(results)
    } else {
        let mut stmt = db.conn.prepare(
            "SELECT n.id, n.path, n.title, '' as snippet, 0.0 as score
             FROM notes n
             ORDER BY n.path
             LIMIT ?1",
        )?;
        let results = stmt
            .query_map(params![limit as i64], map_search_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_plain_query() {
        assert_eq!(sanitize_fts_query("hello world"), "\"hello\" \"world\"");
    }

    #[test]
    fn sanitize_special_chars() {
        assert_eq!(sanitize_fts_query("C++"), "\"C\"");
        assert_eq!(sanitize_fts_query("what's new"), "\"what's\" \"new\"");
    }

    #[test]
    fn sanitize_hyphenated() {
        assert_eq!(
            sanitize_fts_query("self-attention"),
            "\"self\" \"attention\""
        );
    }

    #[test]
    fn sanitize_reserved_words() {
        assert_eq!(sanitize_fts_query("NOT this AND that"), "\"this\" \"that\"");
    }

    #[test]
    fn sanitize_quoted() {
        assert_eq!(sanitize_fts_query("\"quoted\""), "\"quoted\"");
    }

    #[test]
    fn sanitize_empty() {
        assert_eq!(sanitize_fts_query(""), "");
        assert_eq!(sanitize_fts_query("   "), "");
    }

    #[test]
    fn sanitize_wildcard() {
        assert_eq!(sanitize_fts_query("term*"), "\"term\"");
    }
}
