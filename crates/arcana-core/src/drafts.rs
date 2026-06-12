use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::errors::{ArcanaError, Result};
use crate::writer::validate_zone;

/// Per-session manifest stored as `_manifest.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionManifest {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub source: String,
    pub provider: String,
    pub model: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hash: Option<String>,
    #[serde(default)]
    pub drafts: Vec<DraftEntry>,
}

/// Entry for a single draft within a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftEntry {
    pub path: String,
    pub status: DraftStatus,
    #[serde(default)]
    pub kind: DraftKind,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub original_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DraftStatus {
    Pending,
    Approved,
    Rejected,
    Edited,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftKind {
    #[default]
    NewNote,
    SuggestEdit,
    /// Move/merge: source note(s) should be deleted on approve.
    Move,
}

/// Summary info for a session.
#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub source: String,
    pub provider: String,
    pub model: String,
    pub task: String,
    pub input_hash: Option<String>,
    pub total_drafts: usize,
    pub pending_drafts: usize,
}

/// Summary info for a single draft.
#[derive(Debug, Clone, Serialize)]
pub struct DraftInfo {
    pub path: String,
    pub status: DraftStatus,
    pub kind: DraftKind,
    pub reason: Option<String>,
    pub original_path: Option<String>,
}

/// Metadata for creating a new session.
pub struct SessionMeta {
    pub source: String,
    pub provider: String,
    pub model: String,
    pub task: String,
    pub input_hash: Option<String>,
}

/// Manages AI drafts in `.arcana/drafts/`.
pub struct DraftManager {
    drafts_dir: PathBuf,
    vault_root: PathBuf,
    zones: Vec<String>,
    projects: Vec<String>,
}

impl DraftManager {
    pub fn new(vault_root: &Path) -> Self {
        Self {
            drafts_dir: vault_root.join(".arcana").join("drafts"),
            vault_root: vault_root.to_path_buf(),
            zones: Vec::new(),
            projects: Vec::new(),
        }
    }

    pub fn with_zones(vault_root: &Path, zones: Vec<String>, projects: Vec<String>) -> Self {
        Self {
            drafts_dir: vault_root.join(".arcana").join("drafts"),
            vault_root: vault_root.to_path_buf(),
            zones,
            projects,
        }
    }

    /// Create a new session, returns the session ID.
    pub fn create_session(&self, meta: SessionMeta) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let session_dir = self.drafts_dir.join(&id);
        fs::create_dir_all(&session_dir)?;

        let manifest = SessionManifest {
            id: id.clone(),
            created_at: Utc::now(),
            source: meta.source,
            provider: meta.provider,
            model: meta.model,
            task: meta.task,
            input_hash: meta.input_hash,
            drafts: Vec::new(),
        };

        self.write_manifest(&id, &manifest)?;
        debug!("created draft session {id}");
        Ok(id)
    }

    /// Create a new draft note in a session.
    pub fn create_draft(&self, session_id: &str, rel_path: &str, content: &str) -> Result<()> {
        self.validate_path(rel_path)?;
        validate_zone(rel_path, &self.zones, &self.projects)?;

        let draft_path = self.draft_file_path(session_id, rel_path);
        if let Some(parent) = draft_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&draft_path, content)?;

        let mut manifest = self.read_manifest(session_id)?;
        manifest.drafts.push(DraftEntry {
            path: rel_path.to_string(),
            status: DraftStatus::Pending,
            kind: DraftKind::NewNote,
            reason: None,
            original_path: None,
        });
        self.write_manifest(session_id, &manifest)?;

        debug!("created draft {rel_path} in session {session_id}");
        Ok(())
    }

    /// Create a move/merge draft: on approve, source note is deleted.
    pub fn create_move_draft(
        &self,
        session_id: &str,
        rel_path: &str,
        content: &str,
        source_path: &str,
    ) -> Result<()> {
        self.validate_path(rel_path)?;
        validate_zone(rel_path, &self.zones, &self.projects)?;

        let draft_path = self.draft_file_path(session_id, rel_path);
        if let Some(parent) = draft_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&draft_path, content)?;

        let mut manifest = self.read_manifest(session_id)?;
        manifest.drafts.push(DraftEntry {
            path: rel_path.to_string(),
            status: DraftStatus::Pending,
            kind: DraftKind::Move,
            reason: None,
            original_path: Some(source_path.to_string()),
        });
        self.write_manifest(session_id, &manifest)?;

        debug!("created move draft {source_path} → {rel_path} in session {session_id}");
        Ok(())
    }

    /// Suggest an edit to an existing vault note.
    pub fn suggest_edit(
        &self,
        session_id: &str,
        original_path: &str,
        new_content: &str,
        reason: &str,
    ) -> Result<()> {
        self.validate_path(original_path)?;

        // Store the proposed content using the original path
        let draft_path = self.draft_file_path(session_id, original_path);
        if let Some(parent) = draft_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&draft_path, new_content)?;

        let mut manifest = self.read_manifest(session_id)?;
        manifest.drafts.push(DraftEntry {
            path: original_path.to_string(),
            status: DraftStatus::Pending,
            kind: DraftKind::SuggestEdit,
            reason: Some(reason.to_string()),
            original_path: Some(original_path.to_string()),
        });
        self.write_manifest(session_id, &manifest)?;

        debug!("suggested edit for {original_path} in session {session_id}");
        Ok(())
    }

    /// List all sessions.
    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let mut sessions = Vec::new();
        if !self.drafts_dir.exists() {
            return Ok(sessions);
        }

        for entry in fs::read_dir(&self.drafts_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            match self.read_manifest(&id) {
                Ok(manifest) => {
                    let pending = manifest
                        .drafts
                        .iter()
                        .filter(|d| d.status == DraftStatus::Pending)
                        .count();
                    sessions.push(SessionInfo {
                        id: manifest.id,
                        created_at: manifest.created_at,
                        source: manifest.source,
                        provider: manifest.provider,
                        model: manifest.model,
                        task: manifest.task,
                        input_hash: manifest.input_hash,
                        total_drafts: manifest.drafts.len(),
                        pending_drafts: pending,
                    });
                }
                Err(_) => continue,
            }
        }

        sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(sessions)
    }

    /// List drafts in a session.
    pub fn list_drafts(&self, session_id: &str) -> Result<Vec<DraftInfo>> {
        let manifest = self.read_manifest(session_id)?;
        Ok(manifest
            .drafts
            .into_iter()
            .map(|d| DraftInfo {
                path: d.path,
                status: d.status,
                kind: d.kind,
                reason: d.reason,
                original_path: d.original_path,
            })
            .collect())
    }

    /// Read the content of a draft.
    pub fn read_draft(&self, session_id: &str, rel_path: &str) -> Result<String> {
        let draft_path = self.draft_file_path(session_id, rel_path);
        if !draft_path.exists() {
            return Err(ArcanaError::NoteNotFound(format!(
                "draft {rel_path} in session {session_id}"
            )));
        }
        Ok(fs::read_to_string(draft_path)?)
    }

    /// Approve a draft: move it into the vault.
    /// Returns the vault path where the note was placed.
    /// For Move drafts, also deletes the source note if it differs from the output.
    /// The caller is responsible for reindexing and git committing.
    pub fn approve(&self, session_id: &str, rel_path: &str) -> Result<PathBuf> {
        let content = self.read_draft(session_id, rel_path)?;
        let target = self.vault_root.join(rel_path);

        // Ensure parent directories exist
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(&target, content)?;

        // Update manifest and extract move info before writing
        let mut manifest = self.read_manifest(session_id)?;
        let mut source_to_delete = None;
        if let Some(entry) = manifest.drafts.iter_mut().find(|d| d.path == rel_path) {
            // For Move drafts, delete the source note if it differs from the output.
            if entry.kind == DraftKind::Move {
                if let Some(ref orig) = entry.original_path {
                    if orig != rel_path {
                        source_to_delete = Some(orig.clone());
                    }
                }
            }
            entry.status = DraftStatus::Approved;
        }
        self.write_manifest(session_id, &manifest)?;

        // Delete source note for Move drafts
        if let Some(ref source) = source_to_delete {
            let source_path = self.vault_root.join(source);
            if source_path.exists() {
                fs::remove_file(&source_path)?;
                // Clean up empty parent directories
                if let Some(parent) = source_path.parent() {
                    remove_empty_parents(parent, &self.vault_root);
                }
                debug!("deleted source {source} (moved to {rel_path})");
            }
        }

        // Remove the draft file
        let draft_path = self.draft_file_path(session_id, rel_path);
        if draft_path.exists() {
            fs::remove_file(draft_path)?;
        }

        debug!("approved draft {rel_path} from session {session_id}");
        Ok(target)
    }

    /// Reject a draft.
    pub fn reject(&self, session_id: &str, rel_path: &str) -> Result<()> {
        let mut manifest = self.read_manifest(session_id)?;
        if let Some(entry) = manifest.drafts.iter_mut().find(|d| d.path == rel_path) {
            entry.status = DraftStatus::Rejected;
        }
        self.write_manifest(session_id, &manifest)?;

        // Remove the draft file
        let draft_path = self.draft_file_path(session_id, rel_path);
        if draft_path.exists() {
            fs::remove_file(draft_path)?;
        }

        debug!("rejected draft {rel_path} from session {session_id}");
        Ok(())
    }

    /// Prune old sessions (past retention days). Returns count of pruned sessions.
    pub fn prune(&self, retention_days: u32) -> Result<usize> {
        let cutoff = Utc::now() - chrono::Duration::days(retention_days as i64);
        let mut pruned = 0;

        if !self.drafts_dir.exists() {
            return Ok(0);
        }

        for entry in fs::read_dir(&self.drafts_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            match self.read_manifest(&id) {
                Ok(manifest) => {
                    // Only prune if all drafts are resolved (not pending)
                    let has_pending = manifest
                        .drafts
                        .iter()
                        .any(|d| d.status == DraftStatus::Pending);
                    if !has_pending && manifest.created_at < cutoff {
                        fs::remove_dir_all(entry.path())?;
                        pruned += 1;
                        debug!("pruned session {id}");
                    }
                }
                Err(_) => {
                    // Broken session dir, remove it
                    fs::remove_dir_all(entry.path()).ok();
                    pruned += 1;
                }
            }
        }

        Ok(pruned)
    }

    /// Find pending sessions that have drafts targeting any of the given paths.
    ///
    /// Returns `(session_id, session_info, overlapping_paths)` for each match.
    pub fn find_conflicts(
        &self,
        paths: &[&str],
    ) -> Result<Vec<(String, SessionInfo, Vec<String>)>> {
        let sessions = self.list_sessions()?;
        let mut conflicts = Vec::new();

        for session in sessions {
            if session.pending_drafts == 0 {
                continue;
            }
            let drafts = self.list_drafts(&session.id)?;
            let overlapping: Vec<String> = drafts
                .iter()
                .filter(|d| d.status == DraftStatus::Pending)
                .filter(|d| paths.contains(&d.path.as_str()))
                .map(|d| d.path.clone())
                .collect();

            if !overlapping.is_empty() {
                conflicts.push((session.id.clone(), session, overlapping));
            }
        }

        Ok(conflicts)
    }

    fn draft_file_path(&self, session_id: &str, rel_path: &str) -> PathBuf {
        self.drafts_dir.join(session_id).join(rel_path)
    }

    fn manifest_path(&self, session_id: &str) -> PathBuf {
        self.drafts_dir.join(session_id).join("_manifest.toml")
    }

    fn read_manifest(&self, session_id: &str) -> Result<SessionManifest> {
        let path = self.manifest_path(session_id);
        let content = fs::read_to_string(&path)
            .map_err(|_| ArcanaError::NoteNotFound(format!("session manifest for {session_id}")))?;
        toml::from_str(&content).map_err(|e| ArcanaError::Config(format!("manifest parse: {e}")))
    }

    fn write_manifest(&self, session_id: &str, manifest: &SessionManifest) -> Result<()> {
        let path = self.manifest_path(session_id);
        let content = toml::to_string_pretty(manifest)
            .map_err(|e| ArcanaError::Config(format!("manifest serialize: {e}")))?;
        fs::write(path, content)?;
        Ok(())
    }

    fn validate_path(&self, rel_path: &str) -> Result<()> {
        // Reject absolute paths and `..` traversal, and confirm containment
        // within the vault root. Draft targets are chosen by the AI, so an
        // absolute path here would otherwise escape both the draft staging
        // directory and (on approval) the vault.
        crate::vault_path::VaultPath::resolve(&self.vault_root, rel_path)?;
        Ok(())
    }
}

/// Walk up from `dir` toward `stop_at`, removing empty directories.
fn remove_empty_parents(dir: &Path, stop_at: &Path) {
    let mut current = dir.to_path_buf();
    while current.starts_with(stop_at) && current != stop_at {
        if fs::read_dir(&current)
            .map(|mut d| d.next().is_none())
            .unwrap_or(false)
        {
            if fs::remove_dir(&current).is_err() {
                break;
            }
        } else {
            break;
        }
        if !current.pop() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_meta() -> SessionMeta {
        SessionMeta {
            source: "chat".to_string(),
            provider: "ollama".to_string(),
            model: "qwen:35b".to_string(),
            task: "test task".to_string(),
            input_hash: None,
        }
    }

    #[test]
    fn create_session_and_draft() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DraftManager::new(dir.path());

        let session_id = mgr.create_session(test_meta()).unwrap();
        assert!(!session_id.is_empty());

        mgr.create_draft(&session_id, "research/test.md", "# Test\n\nDraft content\n")
            .unwrap();

        let drafts = mgr.list_drafts(&session_id).unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].path, "research/test.md");
        assert_eq!(drafts[0].status, DraftStatus::Pending);
        assert_eq!(drafts[0].kind, DraftKind::NewNote);
    }

    #[test]
    fn read_draft_content() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DraftManager::new(dir.path());

        let session_id = mgr.create_session(test_meta()).unwrap();
        mgr.create_draft(&session_id, "note.md", "hello world")
            .unwrap();

        let content = mgr.read_draft(&session_id, "note.md").unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn approve_moves_to_vault() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DraftManager::new(dir.path());

        let session_id = mgr.create_session(test_meta()).unwrap();
        mgr.create_draft(&session_id, "approved.md", "approved content")
            .unwrap();

        let target = mgr.approve(&session_id, "approved.md").unwrap();
        assert!(target.exists());
        assert_eq!(fs::read_to_string(&target).unwrap(), "approved content");

        let drafts = mgr.list_drafts(&session_id).unwrap();
        assert_eq!(drafts[0].status, DraftStatus::Approved);
    }

    #[test]
    fn reject_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DraftManager::new(dir.path());

        let session_id = mgr.create_session(test_meta()).unwrap();
        mgr.create_draft(&session_id, "rejected.md", "reject me")
            .unwrap();

        mgr.reject(&session_id, "rejected.md").unwrap();

        let drafts = mgr.list_drafts(&session_id).unwrap();
        assert_eq!(drafts[0].status, DraftStatus::Rejected);

        // Draft file should be removed
        let result = mgr.read_draft(&session_id, "rejected.md");
        assert!(result.is_err());
    }

    #[test]
    fn suggest_edit() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DraftManager::new(dir.path());

        let session_id = mgr.create_session(test_meta()).unwrap();
        mgr.suggest_edit(
            &session_id,
            "existing.md",
            "improved content",
            "fix typos and improve clarity",
        )
        .unwrap();

        let drafts = mgr.list_drafts(&session_id).unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].kind, DraftKind::SuggestEdit);
        assert_eq!(
            drafts[0].reason.as_deref(),
            Some("fix typos and improve clarity")
        );
    }

    #[test]
    fn list_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DraftManager::new(dir.path());

        let id1 = mgr.create_session(test_meta()).unwrap();
        mgr.create_draft(&id1, "a.md", "aaa").unwrap();

        let id2 = mgr.create_session(test_meta()).unwrap();
        mgr.create_draft(&id2, "b.md", "bbb").unwrap();

        let sessions = mgr.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn path_traversal_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DraftManager::new(dir.path());

        let session_id = mgr.create_session(test_meta()).unwrap();
        let result = mgr.create_draft(&session_id, "../escape.md", "bad");
        assert!(result.is_err());
    }

    #[test]
    fn create_draft_rejects_bad_zone() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DraftManager::with_zones(
            dir.path(),
            vec!["projects/".into(), "notes/".into()],
            vec!["clasp".into()],
        );
        let session_id = mgr.create_session(test_meta()).unwrap();
        let result = mgr.create_draft(&session_id, "random/note.md", "bad");
        assert!(result.is_err());
    }

    #[test]
    fn suggest_edit_allows_any_existing_path() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DraftManager::with_zones(dir.path(), vec!["projects/".into()], vec![]);
        let session_id = mgr.create_session(test_meta()).unwrap();
        // suggest_edit should not zone-check since it targets existing notes
        let result = mgr.suggest_edit(&session_id, "random/existing.md", "content", "fix typo");
        assert!(result.is_ok());
    }

    #[test]
    fn approve_with_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DraftManager::new(dir.path());

        let session_id = mgr.create_session(test_meta()).unwrap();
        mgr.create_draft(&session_id, "deep/nested/note.md", "nested content")
            .unwrap();

        let target = mgr.approve(&session_id, "deep/nested/note.md").unwrap();
        assert!(target.exists());
        assert_eq!(target, dir.path().join("deep/nested/note.md"));
    }
}
