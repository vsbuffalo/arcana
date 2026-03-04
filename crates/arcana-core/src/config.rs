use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::errors::{ArcanaError, Result};

// ---------------------------------------------------------------------------
// Global config path
// ---------------------------------------------------------------------------

/// Returns the global config path: `$XDG_CONFIG_HOME/arcana/config.toml`
/// (falls back to `~/.config/arcana/config.toml`).
pub fn global_config_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        Some(PathBuf::from(xdg).join("arcana").join("config.toml"))
    } else if let Ok(home) = std::env::var("HOME") {
        Some(
            PathBuf::from(home)
                .join(".config")
                .join("arcana")
                .join("config.toml"),
        )
    } else {
        None
    }
}

/// Load config with full merge hierarchy:
/// compiled defaults → global config → vault-local config.
pub fn load_merged(
    global_path: Option<&Path>,
    vault_local_path: Option<&Path>,
) -> Result<ArcanaConfig> {
    let mut base = toml::Value::Table(toml::value::Table::new());

    // Serialize compiled defaults as the base
    let defaults = ArcanaConfig::default();
    let defaults_str = toml::to_string(&defaults).map_err(|e| ArcanaError::Config(e.to_string()))?;
    let defaults_val: toml::Value =
        toml::from_str(&defaults_str).map_err(|e| ArcanaError::Config(e.to_string()))?;
    if let toml::Value::Table(t) = defaults_val {
        base = toml::Value::Table(t);
    }

    // Overlay global config
    if let Some(gp) = global_path {
        if gp.is_file() {
            let content = std::fs::read_to_string(gp).map_err(ArcanaError::Io)?;
            let global_val: toml::Value =
                toml::from_str(&content).map_err(|e| ArcanaError::Config(e.to_string()))?;
            merge_toml_values(&mut base, global_val);
        }
    }

    // Overlay vault-local config
    if let Some(vp) = vault_local_path {
        if vp.is_file() {
            let content = std::fs::read_to_string(vp).map_err(ArcanaError::Io)?;
            let local_val: toml::Value =
                toml::from_str(&content).map_err(|e| ArcanaError::Config(e.to_string()))?;
            merge_toml_values(&mut base, local_val);
        }
    }

    let merged_str = toml::to_string(&base).map_err(|e| ArcanaError::Config(e.to_string()))?;
    toml::from_str(&merged_str).map_err(|e| ArcanaError::Config(e.to_string()))
}

/// Deep-merge `overlay` into `base`. Tables are merged recursively;
/// non-table values in overlay replace base values.
fn merge_toml_values(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base_table), toml::Value::Table(overlay_table)) => {
            for (key, overlay_val) in overlay_table {
                let entry = base_table
                    .entry(key)
                    .or_insert(toml::Value::Table(toml::value::Table::new()));
                merge_toml_values(entry, overlay_val);
            }
        }
        (base, overlay) => {
            *base = overlay;
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArcanaConfig {
    pub vault: VaultConfig,
    pub index: IndexConfig,
    pub search: SearchConfig,
    pub llm: LlmConfig,
    pub agent: AgentSettings,
    pub git: GitConfig,
    pub drafts: DraftsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultConfig {
    pub path: PathBuf,
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexConfig {
    pub db_location: DbLocation,
    pub db_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DbLocation {
    #[default]
    Colocated,
    Xdg,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    pub default_limit: usize,
    pub snippet_length: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub provider: String,
    pub model: String,
    pub api_key_env: String,
    pub endpoint: Option<String>,
    pub max_output_tokens: Option<u32>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            endpoint: None,
            max_output_tokens: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSettings {
    pub max_iterations: usize,
    pub max_tokens: usize,
    pub max_output_tokens: usize,
    pub default_tags: Vec<String>,
    pub ingest: OperationOverrides,
    pub tidy: OperationOverrides,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            max_iterations: 20,
            max_tokens: 200_000,
            max_output_tokens: 8192,
            default_tags: vec!["ai-generated".to_string()],
            ingest: OperationOverrides::default(),
            tidy: OperationOverrides::default(),
        }
    }
}

/// Per-operation overrides. `Option` fields inherit from parent `[agent]` when unset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OperationOverrides {
    pub max_iterations: Option<usize>,
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GitConfig {
    pub enabled: bool,
    pub auto_commit: bool,
    pub user_name: String,
    pub user_email: String,
    pub ai_name: String,
    pub ai_email: String,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_commit: true,
            user_name: String::new(),
            user_email: String::new(),
            ai_name: "arcana-ai".to_string(),
            ai_email: "ai@arcana.local".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DraftsConfig {
    pub retention_days: u32,
}

impl Default for DraftsConfig {
    fn default() -> Self {
        Self { retention_days: 30 }
    }
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("."),
            exclude: vec![
                ".obsidian".to_string(),
                ".trash".to_string(),
                ".arcana".to_string(),
            ],
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            default_limit: 20,
            snippet_length: 150,
        }
    }
}

impl ArcanaConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(ArcanaError::Io)?;
        toml::from_str(&content).map_err(|e| ArcanaError::Config(e.to_string()))
    }

    pub fn with_vault_path(mut self, path: PathBuf) -> Self {
        self.vault.path = path;
        self
    }

    pub fn db_path(&self) -> PathBuf {
        if let Some(ref explicit) = self.index.db_path {
            return explicit.clone();
        }

        match self.index.db_location {
            DbLocation::Colocated => self.vault.path.join(".arcana").join("index.db"),
            DbLocation::Xdg => {
                let vault_name = self
                    .vault
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("default");
                let hash = simple_hash(self.vault.path.to_str().unwrap_or(""));
                let dir_name = format!("{}-{:x}", vault_name, hash);

                dirs_db_path().join(dir_name).join("index.db")
            }
        }
    }

    pub fn is_excluded(&self, path: &Path) -> bool {
        for component in path.components() {
            let s = component.as_os_str().to_string_lossy();
            if s.starts_with('.') && s != "." && s != ".." {
                return true;
            }
            for excl in &self.vault.exclude {
                if s == excl.as_str() {
                    return true;
                }
            }
        }
        false
    }
}

fn dirs_db_path() -> PathBuf {
    if let Ok(data) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(data).join("arcana")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("arcana")
    } else {
        PathBuf::from(".arcana")
    }
}

fn simple_hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = ArcanaConfig::default();
        assert!(matches!(config.index.db_location, DbLocation::Colocated));
        assert_eq!(config.search.default_limit, 20);
        assert_eq!(config.agent.max_tokens, 200_000);
        assert_eq!(config.agent.max_output_tokens, 8192);
    }

    #[test]
    fn colocated_db_path() {
        let config = ArcanaConfig::default().with_vault_path("/tmp/vault".into());
        let path = config.db_path();
        assert_eq!(path, PathBuf::from("/tmp/vault/.arcana/index.db"));
    }

    #[test]
    fn exclusion_filter() {
        let config = ArcanaConfig::default();
        assert!(config.is_excluded(Path::new(".obsidian/plugins/foo")));
        assert!(config.is_excluded(Path::new(".trash/old-note.md")));
        assert!(config.is_excluded(Path::new(".hidden/file.md")));
        assert!(!config.is_excluded(Path::new("notes/my-note.md")));
    }

    #[test]
    fn merge_toml_deep() {
        let mut base = toml::from_str::<toml::Value>(
            r#"
            [llm]
            model = "claude-sonnet"
            provider = "anthropic"

            [agent]
            max_tokens = 200000
            "#,
        )
        .unwrap();

        let overlay = toml::from_str::<toml::Value>(
            r#"
            [llm]
            model = "claude-haiku"

            [agent.ingest]
            max_tokens = 300000
            "#,
        )
        .unwrap();

        merge_toml_values(&mut base, overlay);

        let table = base.as_table().unwrap();
        // llm.model overridden, llm.provider preserved
        assert_eq!(
            table["llm"]["model"].as_str().unwrap(),
            "claude-haiku"
        );
        assert_eq!(
            table["llm"]["provider"].as_str().unwrap(),
            "anthropic"
        );
        // agent.max_tokens preserved, agent.ingest.max_tokens added
        assert_eq!(
            table["agent"]["max_tokens"].as_integer().unwrap(),
            200000
        );
        assert_eq!(
            table["agent"]["ingest"]["max_tokens"].as_integer().unwrap(),
            300000
        );
    }

    #[test]
    fn load_merged_no_files() {
        let config = load_merged(None, None).unwrap();
        assert_eq!(config.agent.max_tokens, 200_000);
        assert_eq!(config.llm.provider, "anthropic");
    }

    #[test]
    fn load_merged_global_only() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("config.toml");
        std::fs::write(
            &global,
            r#"
            [llm]
            model = "claude-haiku-4-5-20251001"
            "#,
        )
        .unwrap();

        let config = load_merged(Some(&global), None).unwrap();
        assert_eq!(config.llm.model, "claude-haiku-4-5-20251001");
        // Defaults preserved
        assert_eq!(config.llm.provider, "anthropic");
        assert_eq!(config.agent.max_tokens, 200_000);
    }

    #[test]
    fn load_merged_vault_overrides_global() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.toml");
        std::fs::write(
            &global,
            r#"
            [llm]
            model = "claude-haiku-4-5-20251001"

            [agent]
            max_tokens = 300000
            "#,
        )
        .unwrap();

        let vault = dir.path().join("vault.toml");
        std::fs::write(
            &vault,
            r#"
            [agent.ingest]
            max_tokens = 500000
            "#,
        )
        .unwrap();

        let config = load_merged(Some(&global), Some(&vault)).unwrap();
        // Global: model override
        assert_eq!(config.llm.model, "claude-haiku-4-5-20251001");
        // Global: agent.max_tokens
        assert_eq!(config.agent.max_tokens, 300000);
        // Vault-local: ingest override
        assert_eq!(config.agent.ingest.max_tokens, Some(500000));
        // Tidy unset
        assert_eq!(config.agent.tidy.max_tokens, None);
    }

    #[test]
    fn operation_overrides_inherit() {
        let config = ArcanaConfig::default();
        // When override is None, callers should use parent
        let ingest_max = config
            .agent
            .ingest
            .max_tokens
            .unwrap_or(config.agent.max_tokens);
        assert_eq!(ingest_max, 200_000);

        let ingest_iters = config
            .agent
            .ingest
            .max_iterations
            .unwrap_or(config.agent.max_iterations);
        assert_eq!(ingest_iters, 20);
    }
}
