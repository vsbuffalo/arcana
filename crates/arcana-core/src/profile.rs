use std::path::Path;

use tracing::debug;

/// Brain profile: user-defined style guide and knowledge taxonomy.
///
/// Loaded from `.arcana/style.md` and `.arcana/taxonomy.md` in the vault root.
/// Both files are optional — the profile degrades gracefully when either is missing.
#[derive(Debug, Clone, Default)]
pub struct BrainProfile {
    style: Option<String>,
    taxonomy: Option<String>,
}

impl BrainProfile {
    /// Load brain profile from the vault root.
    /// Reads `.arcana/style.md` and `.arcana/taxonomy.md` if they exist.
    pub fn load(vault_root: &Path) -> Self {
        let arcana_dir = vault_root.join(".arcana");

        let style = read_optional(&arcana_dir.join("style.md"));
        let taxonomy = read_optional(&arcana_dir.join("taxonomy.md"));

        if style.is_some() || taxonomy.is_some() {
            debug!(
                "loaded brain profile: style={}, taxonomy={}",
                if style.is_some() { "yes" } else { "no" },
                if taxonomy.is_some() { "yes" } else { "no" },
            );
        }

        Self { style, taxonomy }
    }

    pub fn style(&self) -> Option<&str> {
        self.style.as_deref()
    }

    pub fn taxonomy(&self) -> Option<&str> {
        self.taxonomy.as_deref()
    }

    /// Whether any profile files are present.
    pub fn is_empty(&self) -> bool {
        self.style.is_none() && self.taxonomy.is_none()
    }
}

fn read_optional(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(content) if !content.trim().is_empty() => Some(content),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_with_both_files() {
        let dir = tempfile::tempdir().unwrap();
        let arcana = dir.path().join(".arcana");
        std::fs::create_dir_all(&arcana).unwrap();
        std::fs::write(arcana.join("style.md"), "# My Style\nDense but clear.").unwrap();
        std::fs::write(arcana.join("taxonomy.md"), "# Taxonomy\n## Zones").unwrap();

        let profile = BrainProfile::load(dir.path());
        assert!(profile.style().unwrap().contains("My Style"));
        assert!(profile.taxonomy().unwrap().contains("Zones"));
        assert!(!profile.is_empty());
    }

    #[test]
    fn load_with_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let profile = BrainProfile::load(dir.path());
        assert!(profile.style().is_none());
        assert!(profile.taxonomy().is_none());
        assert!(profile.is_empty());
    }

    #[test]
    fn load_with_only_style() {
        let dir = tempfile::tempdir().unwrap();
        let arcana = dir.path().join(".arcana");
        std::fs::create_dir_all(&arcana).unwrap();
        std::fs::write(arcana.join("style.md"), "# Style").unwrap();

        let profile = BrainProfile::load(dir.path());
        assert!(profile.style().is_some());
        assert!(profile.taxonomy().is_none());
        assert!(!profile.is_empty());
    }

    #[test]
    fn load_ignores_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        let arcana = dir.path().join(".arcana");
        std::fs::create_dir_all(&arcana).unwrap();
        std::fs::write(arcana.join("style.md"), "   \n  ").unwrap();

        let profile = BrainProfile::load(dir.path());
        assert!(profile.style().is_none());
    }
}
