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

    /// Extract allowed zone prefixes from `### zone/` headers under `## Zones` in taxonomy.
    /// Returns empty vec when no taxonomy or no Zones section (= no restrictions).
    pub fn zones(&self) -> Vec<String> {
        self.taxonomy
            .as_deref()
            .map(parse_zones)
            .unwrap_or_default()
    }

    /// Extract known project names from the `### projects/` zone section in taxonomy.
    /// Looks for `- name/ — description` lines. Returns names like `["clasp", "arcana"]`.
    pub fn projects(&self) -> Vec<String> {
        self.taxonomy
            .as_deref()
            .map(parse_projects)
            .unwrap_or_default()
    }
}

/// Parse zone prefixes from `### name/` headers under `## Zones`.
fn parse_zones(taxonomy: &str) -> Vec<String> {
    let mut zones = Vec::new();
    let mut in_zones_section = false;

    for line in taxonomy.lines() {
        let trimmed = line.trim();
        if trimmed == "## Zones" {
            in_zones_section = true;
            continue;
        }
        // Exit on next h2
        if in_zones_section && trimmed.starts_with("## ") {
            break;
        }
        if in_zones_section && trimmed.starts_with("### ") {
            let header = trimmed.trim_start_matches("### ").trim();
            if header.ends_with('/') {
                zones.push(header.to_string());
            }
        }
    }

    zones
}

/// Parse project names from the `### projects/` section.
/// Looks for lines like `- name/ — description` or `- name/`.
fn parse_projects(taxonomy: &str) -> Vec<String> {
    let mut projects = Vec::new();
    let mut in_projects = false;

    for line in taxonomy.lines() {
        let trimmed = line.trim();
        if trimmed == "### projects/" {
            in_projects = true;
            continue;
        }
        // Exit on next h3 or h2
        if in_projects && (trimmed.starts_with("### ") || trimmed.starts_with("## ")) {
            break;
        }
        if in_projects && trimmed.starts_with("- ") {
            let item = trimmed.trim_start_matches("- ");
            // Extract the name before `/` or ` `
            if let Some(name) = item.split('/').next() {
                let name = name.trim();
                if !name.is_empty() {
                    projects.push(name.to_string());
                }
            }
        }
    }

    projects
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
    fn parse_zones_from_taxonomy() {
        let taxonomy = "\
# Taxonomy

## Zones

### concepts/
General concepts and ideas.

### notes/
Daily notes and logs.

### projects/
- clasp/ — CLASP synthesizer
- arcana/ — vault indexer

## Other section
Not zones.
";
        let zones = parse_zones(taxonomy);
        assert_eq!(zones, vec!["concepts/", "notes/", "projects/"]);
    }

    #[test]
    fn parse_zones_empty_taxonomy() {
        assert!(parse_zones("").is_empty());
    }

    #[test]
    fn parse_zones_no_zones_section() {
        let taxonomy = "# Taxonomy\n\n## Tags\nSome tags.\n";
        assert!(parse_zones(taxonomy).is_empty());
    }

    #[test]
    fn parse_projects_from_taxonomy() {
        let taxonomy = "\
## Zones

### projects/
- clasp/ — CLASP synthesizer
- arcana/ — vault indexer
- weather-station/ — outdoor sensors

### notes/
Daily notes.
";
        let projects = parse_projects(taxonomy);
        assert_eq!(projects, vec!["clasp", "arcana", "weather-station"]);
    }

    #[test]
    fn parse_projects_empty_when_no_section() {
        assert!(parse_projects("## Zones\n### notes/\n").is_empty());
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
