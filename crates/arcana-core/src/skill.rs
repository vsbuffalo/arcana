use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::errors::{ArcanaError, Result};
use crate::frontmatter::split_frontmatter;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillMeta {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Tool sets this skill expects (e.g. ["project"])
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub source_path: PathBuf,
    pub meta: SkillMeta,
    /// The markdown content (minus frontmatter), injected into <domain_skill>.
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct SkillSummary {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub tools: Vec<String>,
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Resolve a skill by name or path.
///
/// Resolution order:
/// 1. Exact file path (if the string contains a path separator or ends in .md)
/// 2. `.arcana/skills/<name>.md`
/// 3. `.arcana/skills/<name>/SKILL.md`
pub fn resolve_skill(name_or_path: &str, vault_root: &Path) -> Result<Skill> {
    // 1. Try as exact file path
    let as_path = Path::new(name_or_path);
    if as_path.is_absolute() || name_or_path.contains('/') || name_or_path.ends_with(".md") {
        let path = if as_path.is_absolute() {
            as_path.to_path_buf()
        } else {
            vault_root.join(as_path)
        };
        if path.is_file() {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(name_or_path)
                .to_string();
            return load_skill(&path, &name);
        }
        return Err(ArcanaError::Config(format!(
            "skill file not found: {}",
            path.display()
        )));
    }

    let skills_dir = vault_root.join(".arcana").join("skills");

    // 2. .arcana/skills/<name>.md
    let flat = skills_dir.join(format!("{name_or_path}.md"));
    if flat.is_file() {
        return load_skill(&flat, name_or_path);
    }

    // 3. .arcana/skills/<name>/SKILL.md
    let dir_skill = skills_dir.join(name_or_path).join("SKILL.md");
    if dir_skill.is_file() {
        return load_skill(&dir_skill, name_or_path);
    }

    Err(ArcanaError::Config(format!(
        "skill not found: '{name_or_path}' (searched {}/skills/)",
        skills_dir.parent().unwrap_or(&skills_dir).display()
    )))
}

fn load_skill(path: &Path, name: &str) -> Result<Skill> {
    let content = std::fs::read_to_string(path)?;
    let (yaml, body) = split_frontmatter(&content);

    let meta = if yaml.is_empty() {
        SkillMeta::default()
    } else {
        serde_yaml::from_str(&yaml).unwrap_or_default()
    };

    Ok(Skill {
        name: name.to_string(),
        source_path: path.to_path_buf(),
        meta,
        body,
    })
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

/// List available skills from `.arcana/skills/`.
///
/// - `.md` files: name = stem, parse frontmatter for summary
/// - Subdirectories with `SKILL.md`: name = dir name
pub fn list_skills(vault_root: &Path) -> Vec<SkillSummary> {
    let skills_dir = vault_root.join(".arcana").join("skills");
    if !skills_dir.is_dir() {
        return Vec::new();
    }

    let mut summaries = Vec::new();

    let entries = match std::fs::read_dir(&skills_dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_file() && path.extension().is_some_and(|e| e == "md") {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if let Some(summary) = skill_summary_from_file(&path, &name) {
                summaries.push(summary);
            }
        } else if path.is_dir() {
            let skill_file = path.join("SKILL.md");
            if skill_file.is_file() {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                if let Some(summary) = skill_summary_from_file(&skill_file, &name) {
                    summaries.push(summary);
                }
            }
        }
    }

    summaries.sort_by(|a, b| a.name.cmp(&b.name));
    summaries
}

fn skill_summary_from_file(path: &Path, name: &str) -> Option<SkillSummary> {
    let content = std::fs::read_to_string(path).ok()?;
    let (yaml, _body) = split_frontmatter(&content);

    let meta: SkillMeta = if yaml.is_empty() {
        SkillMeta::default()
    } else {
        serde_yaml::from_str(&yaml).unwrap_or_default()
    };

    Some(SkillSummary {
        name: name.to_string(),
        title: meta.title,
        description: meta.description,
        tools: meta.tools,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_vault() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join(".arcana").join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        dir
    }

    #[test]
    fn resolve_by_path() {
        let dir = setup_vault();
        let skill_path = dir.path().join("my-skill.md");
        std::fs::write(&skill_path, "---\ntitle: My Skill\n---\nDo the thing.\n").unwrap();

        let skill = resolve_skill(skill_path.to_str().unwrap(), dir.path()).unwrap();
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.meta.title.as_deref(), Some("My Skill"));
        assert_eq!(skill.body, "Do the thing.\n");
    }

    #[test]
    fn resolve_by_name_flat() {
        let dir = setup_vault();
        let skills_dir = dir.path().join(".arcana").join("skills");
        std::fs::write(
            skills_dir.join("extract.md"),
            "---\ntitle: Extract\ntools:\n  - project\n---\nExtract models.\n",
        )
        .unwrap();

        let skill = resolve_skill("extract", dir.path()).unwrap();
        assert_eq!(skill.name, "extract");
        assert_eq!(skill.meta.title.as_deref(), Some("Extract"));
        assert_eq!(skill.meta.tools, vec!["project"]);
        assert_eq!(skill.body, "Extract models.\n");
    }

    #[test]
    fn resolve_by_name_directory() {
        let dir = setup_vault();
        let skill_dir = dir
            .path()
            .join(".arcana")
            .join("skills")
            .join("model-extract");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ntitle: Model Extract\ndescription: Extract scientific models\ntools:\n  - project\n---\nDetailed methodology.\n",
        )
        .unwrap();

        let skill = resolve_skill("model-extract", dir.path()).unwrap();
        assert_eq!(skill.name, "model-extract");
        assert_eq!(skill.meta.title.as_deref(), Some("Model Extract"));
        assert_eq!(
            skill.meta.description.as_deref(),
            Some("Extract scientific models")
        );
        assert_eq!(skill.body, "Detailed methodology.\n");
    }

    #[test]
    fn resolve_not_found() {
        let dir = setup_vault();
        let result = resolve_skill("nonexistent", dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("skill not found"), "got: {err}");
    }

    #[test]
    fn no_frontmatter_fallback() {
        let dir = setup_vault();
        let skills_dir = dir.path().join(".arcana").join("skills");
        std::fs::write(skills_dir.join("simple.md"), "Just plain instructions.\n").unwrap();

        let skill = resolve_skill("simple", dir.path()).unwrap();
        assert_eq!(skill.name, "simple");
        assert!(skill.meta.title.is_none());
        assert_eq!(skill.body, "Just plain instructions.\n");
    }

    #[test]
    fn frontmatter_parsing() {
        let dir = setup_vault();
        let skills_dir = dir.path().join(".arcana").join("skills");
        std::fs::write(
            skills_dir.join("full.md"),
            "---\ntitle: Full Skill\ndescription: A complete skill\ntools:\n  - project\n  - vault\n---\nBody.\n",
        )
        .unwrap();

        let skill = resolve_skill("full", dir.path()).unwrap();
        assert_eq!(skill.meta.title.as_deref(), Some("Full Skill"));
        assert_eq!(skill.meta.description.as_deref(), Some("A complete skill"));
        assert_eq!(skill.meta.tools, vec!["project", "vault"]);
    }

    #[test]
    fn list_empty_dir() {
        let dir = setup_vault();
        let skills = list_skills(dir.path());
        assert!(skills.is_empty());
    }

    #[test]
    fn list_mixed_entries() {
        let dir = setup_vault();
        let skills_dir = dir.path().join(".arcana").join("skills");

        // Flat skill
        std::fs::write(
            skills_dir.join("alpha.md"),
            "---\ntitle: Alpha Skill\n---\nAlpha body.\n",
        )
        .unwrap();

        // Directory skill
        let beta_dir = skills_dir.join("beta");
        std::fs::create_dir_all(&beta_dir).unwrap();
        std::fs::write(
            beta_dir.join("SKILL.md"),
            "---\ntitle: Beta Skill\ndescription: Beta desc\n---\nBeta body.\n",
        )
        .unwrap();

        // Non-skill file (should be skipped)
        std::fs::write(skills_dir.join("README.txt"), "ignore me").unwrap();

        // Directory without SKILL.md (should be skipped)
        let empty_dir = skills_dir.join("empty");
        std::fs::create_dir_all(&empty_dir).unwrap();

        let skills = list_skills(dir.path());
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "alpha");
        assert_eq!(skills[0].title.as_deref(), Some("Alpha Skill"));
        assert_eq!(skills[1].name, "beta");
        assert_eq!(skills[1].title.as_deref(), Some("Beta Skill"));
        assert_eq!(skills[1].description.as_deref(), Some("Beta desc"));
    }

    #[test]
    fn list_no_skills_dir() {
        let dir = tempfile::tempdir().unwrap();
        let skills = list_skills(dir.path());
        assert!(skills.is_empty());
    }
}
