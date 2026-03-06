use std::path::Path;

use tracing::debug;

/// User-overridable task prompts loaded from `.arcana/prompts/`.
///
/// Each file replaces the compiled-in default for a specific pipeline phase.
/// Files are plain markdown. Missing files fall back to the built-in prompt.
///
/// ```text
/// .arcana/prompts/
/// ├── explore.md          # ingest: codebase exploration
/// ├── ingest-plan.md      # ingest: note planning
/// ├── ingest-generate.md  # ingest: note generation
/// ├── tidy-plan.md        # tidy: reorganization planning
/// ├── tidy-generate.md    # tidy: note rewriting
/// ├── tidy-audit.md       # tidy --audit: structural audit
/// └── chat.md             # interactive chat / librarian
/// ```
#[derive(Debug, Clone, Default)]
pub struct UserPrompts {
    pub explore: Option<String>,
    pub ingest_plan: Option<String>,
    pub ingest_generate: Option<String>,
    pub tidy_plan: Option<String>,
    pub tidy_generate: Option<String>,
    pub tidy_audit: Option<String>,
    pub chat: Option<String>,
}

impl UserPrompts {
    /// Load prompt overrides from `.arcana/prompts/` in the vault root.
    pub fn load(vault_root: &Path) -> Self {
        let dir = vault_root.join(".arcana").join("prompts");
        if !dir.is_dir() {
            return Self::default();
        }

        let prompts = Self {
            explore: read_optional(&dir.join("explore.md")),
            ingest_plan: read_optional(&dir.join("ingest-plan.md")),
            ingest_generate: read_optional(&dir.join("ingest-generate.md")),
            tidy_plan: read_optional(&dir.join("tidy-plan.md")),
            tidy_generate: read_optional(&dir.join("tidy-generate.md")),
            tidy_audit: read_optional(&dir.join("tidy-audit.md")),
            chat: read_optional(&dir.join("chat.md")),
        };

        let count = [
            &prompts.explore,
            &prompts.ingest_plan,
            &prompts.ingest_generate,
            &prompts.tidy_plan,
            &prompts.tidy_generate,
            &prompts.tidy_audit,
            &prompts.chat,
        ]
        .iter()
        .filter(|p| p.is_some())
        .count();

        if count > 0 {
            debug!("loaded {count} custom prompt override(s) from .arcana/prompts/");
        }

        prompts
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
    fn load_no_dir() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = UserPrompts::load(dir.path());
        assert!(prompts.explore.is_none());
        assert!(prompts.chat.is_none());
    }

    #[test]
    fn load_partial() {
        let dir = tempfile::tempdir().unwrap();
        let prompts_dir = dir.path().join(".arcana").join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(prompts_dir.join("explore.md"), "Custom explore prompt").unwrap();
        std::fs::write(prompts_dir.join("chat.md"), "Custom chat prompt").unwrap();

        let prompts = UserPrompts::load(dir.path());
        assert_eq!(prompts.explore.as_deref(), Some("Custom explore prompt"));
        assert_eq!(prompts.chat.as_deref(), Some("Custom chat prompt"));
        assert!(prompts.ingest_plan.is_none());
    }

    #[test]
    fn ignores_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        let prompts_dir = dir.path().join(".arcana").join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(prompts_dir.join("explore.md"), "  \n  ").unwrap();

        let prompts = UserPrompts::load(dir.path());
        assert!(prompts.explore.is_none());
    }
}
