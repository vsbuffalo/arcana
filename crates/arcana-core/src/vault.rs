use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::config::ArcanaConfig;
use crate::drafts::DraftManager;
use crate::errors::{ArcanaError, Result};
use crate::git::{InitInfo, VaultGit};
use crate::index::fts::{IndexEntry, IndexStats};
use crate::index::Database;
use crate::note::{extract_inline_tags, extract_wikilinks, FileMeta, Frontmatter, Note};
use crate::profile::BrainProfile;
use crate::search::{SearchFilters, SearchQuery, SearchResult};
use crate::writer::NoteWriter;

pub struct Vault {
    pub(crate) db: Database,
    pub(crate) config: ArcanaConfig,
    pub(crate) root: PathBuf,
    git: Option<VaultGit>,
    init_info: Option<InitInfo>,
    drafts: DraftManager,
    profile: BrainProfile,
}

#[derive(Debug)]
pub struct VaultStats {
    pub total_notes: usize,
    pub total_tags: usize,
    pub total_links: usize,
}

/// Intermediate result from parallel file processing (no DB access needed).
struct ParsedFile {
    rel_path: String,
    content_hash: u64,
    entry: IndexEntry,
}

impl Vault {
    pub fn open(config: ArcanaConfig) -> Result<Self> {
        let root = config.vault.path.canonicalize().map_err(|e| {
            ArcanaError::Config(format!(
                "vault path '{}' not found: {}",
                config.vault.path.display(),
                e
            ))
        })?;

        let db_path = config.db_path();
        debug!("opening database at {}", db_path.display());
        let db = Database::open(&db_path)?;

        let (git, init_info) = if config.git.enabled {
            match VaultGit::open_or_init(&root, &config.git) {
                Ok((g, info)) => (Some(g), Some(info)),
                Err(e) => {
                    warn!("git init failed, continuing without git: {e}");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        let profile = BrainProfile::load(&root);
        let drafts = DraftManager::with_zones(&root, profile.zones(), profile.projects());

        Ok(Vault {
            db,
            config,
            root,
            git,
            init_info,
            drafts,
            profile,
        })
    }

    pub fn open_in_memory(config: ArcanaConfig) -> Result<Self> {
        let root = config.vault.path.canonicalize().map_err(|e| {
            ArcanaError::Config(format!(
                "vault path '{}' not found: {}",
                config.vault.path.display(),
                e
            ))
        })?;
        let db = Database::open_in_memory()?;
        let profile = BrainProfile::load(&root);
        let drafts = DraftManager::with_zones(&root, profile.zones(), profile.projects());
        Ok(Vault {
            db,
            config,
            root,
            git: None,
            init_info: None,
            drafts,
            profile,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn git_init_info(&self) -> Option<&InitInfo> {
        self.init_info.as_ref()
    }

    pub fn index(&self) -> Result<IndexStats> {
        let start = Instant::now();
        info!("starting vault index at {}", self.root.display());

        // Phase 1: Walk filesystem and collect markdown file paths
        let md_files = self.collect_md_files()?;
        let num_scanned = md_files.len();
        debug!("found {} markdown files", num_scanned);

        // Phase 2: Parallel read + hash + parse
        // All rayon work completes before any DB access.
        // parse_file_no_db does NO database access — only filesystem + CPU.
        let root = &self.root;
        let parsed: Vec<ParsedFile> = md_files
            .par_iter()
            .filter_map(|path| match parse_file_no_db(path, root) {
                Ok(parsed) => Some(parsed),
                Err(e) => {
                    warn!("failed to process {}: {}", path.display(), e);
                    None
                }
            })
            .collect();

        // Phase 3: Single transaction on main thread — all DB access here
        let tx = self.db.conn.unchecked_transaction()?;

        let current_paths: Vec<String> = md_files
            .iter()
            .filter_map(|p| {
                p.strip_prefix(&self.root)
                    .ok()
                    .and_then(|r| r.to_str())
                    .map(|s| s.to_string())
            })
            .collect();

        let removed = self.db.delete_missing_notes(&current_paths)?;

        let mut added = 0usize;
        let mut updated = 0usize;
        let mut unchanged = 0usize;

        for parsed_file in &parsed {
            // Check hash against DB on main thread
            let existing_hash = self.db.get_content_hash(&parsed_file.rel_path)?;
            if existing_hash == Some(parsed_file.content_hash) {
                unchanged += 1;
                continue;
            }
            let is_new = existing_hash.is_none();

            let note_id = self.db.upsert_note(&parsed_file.entry)?;
            self.db.upsert_tags(note_id, &parsed_file.entry.tags)?;
            self.db.upsert_links(note_id, &parsed_file.entry.links)?;

            if is_new {
                added += 1;
            } else {
                updated += 1;
            }
        }

        tx.commit()?;

        let stats = IndexStats {
            notes_scanned: num_scanned,
            notes_added: added,
            notes_updated: updated,
            notes_removed: removed,
            notes_unchanged: unchanged,
        };

        let duration = start.elapsed();
        info!(
            "index complete: {} scanned, {} added, {} updated, {} removed, {} unchanged in {:?}",
            stats.notes_scanned,
            stats.notes_added,
            stats.notes_updated,
            stats.notes_removed,
            stats.notes_unchanged,
            duration
        );

        // Save stats
        let run_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        self.db
            .save_index_stats(&run_id, &now, &now, &stats, duration.as_millis() as i64)?;

        Ok(stats)
    }

    pub fn reindex_paths(&self, paths: &[PathBuf]) -> Result<IndexStats> {
        let start = Instant::now();
        let mut added = 0usize;
        let mut updated = 0usize;
        let mut removed = 0usize;

        let tx = self.db.conn.unchecked_transaction()?;

        for path in paths {
            let rel_path = path
                .strip_prefix(&self.root)
                .map_err(|_| ArcanaError::PathEscape(path.display().to_string()))?;

            if !path.exists() {
                let rel_str = rel_path.to_str().unwrap_or("");
                let r = self.db.conn.execute(
                    "DELETE FROM notes WHERE path = ?1",
                    rusqlite::params![rel_str],
                )?;
                if r > 0 {
                    removed += 1;
                }
                continue;
            }

            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }

            match parse_file_no_db(path, &self.root) {
                Ok(parsed) => {
                    let existing_hash = self.db.get_content_hash(&parsed.rel_path)?;
                    if existing_hash == Some(parsed.content_hash) {
                        continue;
                    }
                    let is_new = existing_hash.is_none();

                    let note_id = self.db.upsert_note(&parsed.entry)?;
                    self.db.upsert_tags(note_id, &parsed.entry.tags)?;
                    self.db.upsert_links(note_id, &parsed.entry.links)?;
                    if is_new {
                        added += 1;
                    } else {
                        updated += 1;
                    }
                }
                Err(e) => {
                    warn!("failed to reindex {}: {}", path.display(), e);
                }
            }
        }

        tx.commit()?;

        let stats = IndexStats {
            notes_scanned: paths.len(),
            notes_added: added,
            notes_updated: updated,
            notes_removed: removed,
            notes_unchanged: paths.len() - added - updated - removed,
        };

        debug!("reindex complete in {:?}", start.elapsed());
        Ok(stats)
    }

    fn collect_md_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for entry in WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                let path = e.path();
                if let Ok(rel) = path.strip_prefix(&self.root) {
                    !self.config.is_excluded(rel)
                } else {
                    true
                }
            })
        {
            let entry = entry?;
            if entry.file_type().is_file() {
                if let Some(ext) = entry.path().extension() {
                    if ext == "md" {
                        files.push(entry.into_path());
                    }
                }
            }
        }
        Ok(files)
    }

    pub fn read_note(&self, rel_path: &str) -> Result<Note> {
        let full_path = self.root.join(rel_path);
        if !full_path.exists() {
            return Err(ArcanaError::NoteNotFound(rel_path.to_string()));
        }

        let content = std::fs::read_to_string(&full_path)?;
        let metadata = std::fs::metadata(&full_path)?;
        let hash = xxhash_rust::xxh3::xxh3_64(content.as_bytes());

        let file_meta = FileMeta {
            size_bytes: metadata.len(),
            modified_on_disk: metadata
                .modified()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            content_hash: hash,
        };

        Note::parse(PathBuf::from(rel_path), &content, file_meta)
    }

    pub fn stats(&self) -> Result<VaultStats> {
        self.db.set_query_only(true)?;
        let result = (|| {
            Ok(VaultStats {
                total_notes: self.db.note_count()?,
                total_tags: self.db.tag_count()?,
                total_links: self.db.link_count()?,
            })
        })();
        self.db.set_query_only(false)?;
        result
    }

    pub fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        self.db.set_query_only(true)?;
        let result = crate::search::execute_search(&self.db, query);
        self.db.set_query_only(false)?;
        result
    }

    pub fn list(&self, filters: &SearchFilters, limit: usize) -> Result<Vec<SearchResult>> {
        self.db.set_query_only(true)?;
        let result = crate::search::execute_list(&self.db, filters, limit);
        self.db.set_query_only(false)?;
        result
    }

    pub fn create_note(
        &self,
        rel_path: &str,
        body: &str,
        frontmatter: Option<Frontmatter>,
    ) -> Result<()> {
        let writer =
            NoteWriter::with_zones(&self.root, self.profile.zones(), self.profile.projects());
        writer.create(rel_path, body, frontmatter)?;
        self.reindex_paths(&[self.root.join(rel_path)])?;

        if let Some(ref git) = self.git {
            if self.config.git.auto_commit {
                let msg = format!("arcana: create {rel_path}");
                if let Err(e) = git.commit_ai_write(&[Path::new(rel_path)], &msg) {
                    warn!("git commit failed for create: {e}");
                }
            }
        }

        Ok(())
    }

    pub fn update_note(
        &self,
        rel_path: &str,
        body: Option<&str>,
        append: Option<&str>,
        frontmatter_patch: Option<Frontmatter>,
    ) -> Result<()> {
        let writer = NoteWriter::new(&self.root);
        writer.update(rel_path, body, append, frontmatter_patch)?;
        self.reindex_paths(&[self.root.join(rel_path)])?;

        if let Some(ref git) = self.git {
            if self.config.git.auto_commit {
                let msg = format!("arcana: update {rel_path}");
                if let Err(e) = git.commit_ai_write(&[Path::new(rel_path)], &msg) {
                    warn!("git commit failed for update: {e}");
                }
            }
        }

        Ok(())
    }

    pub fn git(&self) -> Option<&VaultGit> {
        self.git.as_ref()
    }

    pub fn drafts(&self) -> &DraftManager {
        &self.drafts
    }

    pub fn profile(&self) -> &BrainProfile {
        &self.profile
    }

    /// Condensed directory tree (top 2 levels + note counts) from the index.
    pub fn vault_tree(&self) -> Result<String> {
        self.db.set_query_only(true)?;
        let result = (|| {
            let paths = self.db.get_all_paths()?;
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();

            for path in &paths {
                let parts: Vec<&str> = path.split('/').collect();
                let prefix = match parts.len() {
                    0 | 1 => continue, // root-level files, skip
                    2 => format!("{}/", parts[0]),
                    _ => format!("{}/{}/", parts[0], parts[1]),
                };
                *counts.entry(prefix).or_insert(0) += 1;
            }

            let mut out = String::new();
            for (prefix, count) in &counts {
                let label = if *count == 1 { "note" } else { "notes" };
                out.push_str(&format!("{prefix} ({count} {label})\n"));
            }
            Ok(out)
        })();
        self.db.set_query_only(false)?;
        result
    }
}

/// Parse a file without any DB access. Safe for rayon parallel execution.
fn parse_file_no_db(path: &Path, root: &Path) -> Result<ParsedFile> {
    let content = std::fs::read_to_string(path)?;
    let hash = xxhash_rust::xxh3::xxh3_64(content.as_bytes());

    let rel_path = path
        .strip_prefix(root)
        .map_err(|_| ArcanaError::PathEscape(path.display().to_string()))?;
    let rel_str = rel_path.to_str().unwrap_or("").to_string();

    let metadata = std::fs::metadata(path)?;
    let file_meta = FileMeta {
        size_bytes: metadata.len(),
        modified_on_disk: metadata
            .modified()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        content_hash: hash,
    };

    let note = Note::parse(rel_path.to_path_buf(), &content, file_meta)?;

    let mut all_tags = note.frontmatter.tags.clone();
    let inline_tags = extract_inline_tags(&note.body);
    for tag in inline_tags {
        if !all_tags.contains(&tag) {
            all_tags.push(tag);
        }
    }

    let links = extract_wikilinks(&note.body);

    let fm_yaml = if has_frontmatter_content(&note.frontmatter) {
        serde_yaml::to_string(&note.frontmatter).ok()
    } else {
        None
    };

    let created_at = note.frontmatter.created.map(|d| d.to_rfc3339());
    let modified_at = note.frontmatter.modified.map(|d| d.to_rfc3339());

    let entry = IndexEntry {
        path: rel_str.clone(),
        title: note.frontmatter.title.clone(),
        content_hash: hash,
        frontmatter_yaml: fm_yaml,
        body: note.body,
        created_at,
        modified_at,
        is_ai: note.frontmatter.ai.is_some(),
        ai_model: note.frontmatter.ai.as_ref().map(|a| a.model.clone()),
        ai_session: note
            .frontmatter
            .ai
            .as_ref()
            .map(|a| a.agent_session.clone()),
        ai_reviewed: note.frontmatter.ai.as_ref().is_some_and(|a| a.reviewed),
        tags: all_tags,
        links,
    };

    Ok(ParsedFile {
        rel_path: rel_str,
        content_hash: hash,
        entry,
    })
}

fn has_frontmatter_content(fm: &Frontmatter) -> bool {
    fm.title.is_some()
        || fm.created.is_some()
        || fm.modified.is_some()
        || !fm.tags.is_empty()
        || !fm.aliases.is_empty()
        || fm.ai.is_some()
        || !fm.extra.is_empty()
}
