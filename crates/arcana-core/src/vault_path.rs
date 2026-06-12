//! Validated vault-relative paths.
//!
//! Untrusted callers (MCP tool arguments, REST request paths, AI-chosen draft
//! targets) supply note paths as strings. Joining such a string onto the vault
//! root with [`Path::join`] is unsafe: an absolute argument discards the root
//! (`root.join("/etc/passwd") == "/etc/passwd"`) and `..` components escape it.
//!
//! [`VaultPath`] is the single validated chokepoint. The only way to construct
//! one is [`VaultPath::resolve`], which rejects absolute paths and `..`
//! traversal lexically and then confirms (via canonicalization of the deepest
//! existing ancestor) that the target lies within the vault root, defeating
//! symlink escapes. Every filesystem operation in this crate takes a
//! `&VaultPath`, so an unvalidated path cannot reach the disk.

use std::path::{Component, Path, PathBuf};

use crate::errors::{ArcanaError, Result};

/// A filesystem path proven to lie within a vault root.
#[derive(Debug, Clone)]
pub struct VaultPath {
    /// Absolute path = `root.join(rel)` (not further canonicalized, so the
    /// original spelling survives for index/DB keys and `strip_prefix`).
    full: PathBuf,
    /// The original validated relative path.
    rel: String,
}

impl VaultPath {
    /// Validate `rel` against `root` and return a `VaultPath`, or
    /// [`ArcanaError::PathEscape`] if it is absolute, contains `..`, or resolves
    /// outside the vault (including through a symlink).
    pub fn resolve(root: &Path, rel: &str) -> Result<Self> {
        validate_rel(rel)?;
        let full = root.join(rel);
        if !is_contained(root, &full)? {
            return Err(ArcanaError::PathEscape(rel.to_string()));
        }
        Ok(VaultPath {
            full,
            rel: rel.to_string(),
        })
    }

    /// The absolute path to operate on.
    pub fn as_path(&self) -> &Path {
        &self.full
    }

    /// The original vault-relative path (useful as a DB key).
    pub fn rel(&self) -> &str {
        &self.rel
    }
}

/// Lexical validation with no filesystem access: reject empty paths, absolute
/// paths, and any parent-dir/root/prefix component. Sufficient to block the
/// string-only traversal vectors (`../…`, `/abs`); [`is_contained`] additionally
/// defends against symlink escapes for paths that touch the filesystem.
pub fn validate_rel(rel: &str) -> Result<()> {
    if rel.is_empty() {
        return Err(ArcanaError::PathEscape(rel.to_string()));
    }
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ArcanaError::PathEscape(rel.to_string()));
            }
        }
    }
    Ok(())
}

/// Confirm `full` resolves within `root`. `full` need not exist yet: we
/// canonicalize the deepest *existing* ancestor (resolving any symlinks in the
/// existing prefix) and check containment. Because the caller has already
/// rejected `..` components, the not-yet-existing tail cannot escape.
fn is_contained(root: &Path, full: &Path) -> Result<bool> {
    let canonical_root = root.canonicalize().map_err(ArcanaError::Io)?;
    let mut probe = full;
    loop {
        match probe.canonicalize() {
            Ok(canonical) => return Ok(canonical.starts_with(&canonical_root)),
            Err(_) => match probe.parent() {
                Some(parent) => probe = parent,
                None => return Ok(false),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_normal_path() {
        let dir = tempfile::tempdir().unwrap();
        let vp = VaultPath::resolve(dir.path(), "notes/foo.md").unwrap();
        assert!(vp.as_path().ends_with("notes/foo.md"));
        assert_eq!(vp.rel(), "notes/foo.md");
    }

    #[test]
    fn resolves_nested_nonexistent_path() {
        // The target and its parent dirs do not exist yet — must still validate.
        let dir = tempfile::tempdir().unwrap();
        let vp = VaultPath::resolve(dir.path(), "a/b/c/new.md").unwrap();
        assert!(vp.as_path().ends_with("a/b/c/new.md"));
    }

    #[test]
    fn rejects_parent_dir_traversal() {
        let dir = tempfile::tempdir().unwrap();
        assert!(VaultPath::resolve(dir.path(), "../escape.md").is_err());
        assert!(VaultPath::resolve(dir.path(), "notes/../../escape.md").is_err());
    }

    #[test]
    fn rejects_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(VaultPath::resolve(dir.path(), "/etc/passwd").is_err());
    }

    #[test]
    fn rejects_empty_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(VaultPath::resolve(dir.path(), "").is_err());
    }

    #[test]
    fn validate_rel_accepts_curdir() {
        assert!(validate_rel("./notes/foo.md").is_ok());
        assert!(validate_rel("notes/foo.md").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.md"), "secret").unwrap();

        let vault = tempfile::tempdir().unwrap();
        // A symlink inside the vault pointing at an external directory.
        symlink(outside.path(), vault.path().join("escape")).unwrap();

        // No `..`, not absolute — only canonicalization catches this.
        let result = VaultPath::resolve(vault.path(), "escape/secret.md");
        assert!(result.is_err(), "symlink escape must be rejected");
    }
}
