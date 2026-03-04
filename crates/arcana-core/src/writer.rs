use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::errors::{ArcanaError, Result};
use crate::note::{FileMeta, Frontmatter, Note};

pub struct NoteWriter {
    root: PathBuf,
    zones: Vec<String>,
    projects: Vec<String>,
}

impl NoteWriter {
    pub fn new(root: &Path) -> Self {
        NoteWriter {
            root: root.to_path_buf(),
            zones: Vec::new(),
            projects: Vec::new(),
        }
    }

    pub fn with_zones(root: &Path, zones: Vec<String>, projects: Vec<String>) -> Self {
        NoteWriter {
            root: root.to_path_buf(),
            zones,
            projects,
        }
    }

    pub fn create(
        &self,
        rel_path: &str,
        body: &str,
        frontmatter: Option<Frontmatter>,
    ) -> Result<PathBuf> {
        validate_zone(rel_path, &self.zones, &self.projects)?;
        let full_path = self.safe_path(rel_path)?;

        if full_path.exists() {
            return Err(ArcanaError::NoteAlreadyExists(rel_path.to_string()));
        }

        let note = Note {
            path: PathBuf::from(rel_path),
            frontmatter: frontmatter.unwrap_or_default(),
            body: body.to_string(),
            file_meta: FileMeta {
                size_bytes: 0,
                modified_on_disk: std::time::SystemTime::now(),
                content_hash: 0,
            },
        };

        let content = note.to_string();
        atomic_write(&full_path, content.as_bytes())?;
        Ok(full_path)
    }

    pub fn update(
        &self,
        rel_path: &str,
        body: Option<&str>,
        append: Option<&str>,
        frontmatter_patch: Option<Frontmatter>,
    ) -> Result<PathBuf> {
        let full_path = self.safe_path(rel_path)?;

        if !full_path.exists() {
            return Err(ArcanaError::NoteNotFound(rel_path.to_string()));
        }

        let content = fs::read_to_string(&full_path)?;
        let hash = xxhash_rust::xxh3::xxh3_64(content.as_bytes());
        let metadata = fs::metadata(&full_path)?;
        let file_meta = FileMeta {
            size_bytes: metadata.len(),
            modified_on_disk: metadata
                .modified()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            content_hash: hash,
        };

        let mut note = Note::parse(PathBuf::from(rel_path), &content, file_meta)?;

        if let Some(patch) = frontmatter_patch {
            merge_frontmatter(&mut note.frontmatter, &patch);
        }

        if let Some(new_body) = body {
            note.body = new_body.to_string();
        }

        if let Some(text) = append {
            note.body.push_str(text);
        }

        let output = note.to_string();
        atomic_write(&full_path, output.as_bytes())?;
        Ok(full_path)
    }

    fn safe_path(&self, rel_path: &str) -> Result<PathBuf> {
        // Reject path traversal
        if rel_path.contains("..") {
            return Err(ArcanaError::PathEscape(rel_path.to_string()));
        }

        let full_path = self.root.join(rel_path);

        // Canonicalize parent to check prefix
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)?;
            let canonical_parent = parent.canonicalize()?;
            let canonical_root = self.root.canonicalize()?;
            if !canonical_parent.starts_with(&canonical_root) {
                return Err(ArcanaError::PathEscape(rel_path.to_string()));
            }
        }

        Ok(full_path)
    }
}

pub fn validate_zone(rel_path: &str, zones: &[String], projects: &[String]) -> Result<()> {
    if zones.is_empty() {
        return Ok(());
    }
    if zones.iter().any(|z| rel_path.starts_with(z.as_str())) {
        return Ok(());
    }
    let zones_str = zones.join(", ");
    if let Some(suggestion) = suggest_zone(rel_path, projects) {
        return Err(ArcanaError::InvalidZone {
            message: format!(
                "'{rel_path}' is outside allowed zones. Did you mean '{suggestion}'? Allowed: {zones_str}"
            ),
        });
    }
    Err(ArcanaError::InvalidZone {
        message: format!("'{rel_path}' is outside allowed zones. Allowed: {zones_str}"),
    })
}

fn suggest_zone(rel_path: &str, projects: &[String]) -> Option<String> {
    let first = rel_path.split('/').next()?;
    if projects.iter().any(|p| p == first) {
        return Some(format!("projects/{rel_path}"));
    }
    None
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| ArcanaError::Io(std::io::Error::other("no parent directory")))?;

    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(data)?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| ArcanaError::Io(e.error))?;
    Ok(())
}

fn merge_frontmatter(base: &mut Frontmatter, patch: &Frontmatter) {
    if patch.title.is_some() {
        base.title.clone_from(&patch.title);
    }
    if patch.created.is_some() {
        base.created = patch.created;
    }
    if patch.modified.is_some() {
        base.modified = patch.modified;
    }
    if !patch.tags.is_empty() {
        for tag in &patch.tags {
            if !base.tags.contains(tag) {
                base.tags.push(tag.clone());
            }
        }
    }
    if !patch.aliases.is_empty() {
        for alias in &patch.aliases {
            if !base.aliases.contains(alias) {
                base.aliases.push(alias.clone());
            }
        }
    }
    if patch.ai.is_some() {
        base.ai.clone_from(&patch.ai);
    }
    for (key, value) in &patch.extra {
        base.extra.insert(key.clone(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let writer = NoteWriter::new(dir.path());

        let fm = Frontmatter {
            title: Some("Test".to_string()),
            tags: vec!["rust".to_string()],
            ..Default::default()
        };

        writer.create("test.md", "Hello world\n", Some(fm)).unwrap();

        let content = fs::read_to_string(dir.path().join("test.md")).unwrap();
        assert!(content.contains("title: Test"));
        assert!(content.contains("Hello world"));
    }

    #[test]
    fn update_preserves_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let writer = NoteWriter::new(dir.path());

        let fm = Frontmatter {
            title: Some("Original".to_string()),
            tags: vec!["original".to_string()],
            ..Default::default()
        };

        writer
            .create("note.md", "Original body\n", Some(fm))
            .unwrap();

        let patch = Frontmatter {
            tags: vec!["added".to_string()],
            ..Default::default()
        };

        writer
            .update("note.md", None, Some("\nAppended text\n"), Some(patch))
            .unwrap();

        let content = fs::read_to_string(dir.path().join("note.md")).unwrap();
        assert!(content.contains("title: Original"));
        assert!(content.contains("original"));
        assert!(content.contains("added"));
        assert!(content.contains("Appended text"));
    }

    #[test]
    fn path_escape_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let writer = NoteWriter::new(dir.path());

        let result = writer.create("../escape.md", "bad", None);
        assert!(result.is_err());
    }

    #[test]
    fn create_with_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let writer = NoteWriter::new(dir.path());

        writer.create("sub/dir/note.md", "Nested\n", None).unwrap();
        assert!(dir.path().join("sub/dir/note.md").exists());
    }

    #[test]
    fn duplicate_create_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let writer = NoteWriter::new(dir.path());

        writer.create("dup.md", "First", None).unwrap();
        let result = writer.create("dup.md", "Second", None);
        assert!(result.is_err());
    }

    #[test]
    fn validate_zone_valid() {
        let zones = vec!["concepts/".into(), "projects/".into()];
        assert!(validate_zone("projects/arcana/foo.md", &zones, &[]).is_ok());
        assert!(validate_zone("concepts/rust.md", &zones, &[]).is_ok());
    }

    #[test]
    fn validate_zone_invalid() {
        let zones = vec!["concepts/".into(), "projects/".into()];
        let result = validate_zone("random/foo.md", &zones, &[]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("outside allowed zones") || err.contains("Allowed"));
    }

    #[test]
    fn validate_zone_empty_means_no_restriction() {
        assert!(validate_zone("anything/goes.md", &[], &[]).is_ok());
    }

    #[test]
    fn validate_zone_suggests_project() {
        let zones = vec!["projects/".into(), "notes/".into()];
        let projects = vec!["clasp".into(), "arcana".into()];
        let result = validate_zone("clasp/foo.md", &zones, &projects);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("projects/clasp/foo.md"));
    }

    #[test]
    fn with_zones_rejects_bad_path() {
        let dir = tempfile::tempdir().unwrap();
        let writer =
            NoteWriter::with_zones(dir.path(), vec!["projects/".into()], vec!["clasp".into()]);
        let result = writer.create("random/note.md", "bad", None);
        assert!(result.is_err());
    }
}
