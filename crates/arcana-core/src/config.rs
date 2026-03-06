use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    let defaults_str =
        toml::to_string(&defaults).map_err(|e| ArcanaError::Config(e.to_string()))?;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ArcanaConfig {
    pub vault: VaultConfig,
    pub index: IndexConfig,
    pub search: SearchConfig,
    pub agent: AgentSettings,
    pub git: GitConfig,
    pub drafts: DraftsConfig,
    /// Default LLM profile name — must exist in `[profiles]`.
    pub default_profile: String,
    /// Named LLM profiles. Each is a complete `LlmConfig`.
    pub profiles: HashMap<String, LlmConfig>,
    /// Backward compat: `[llm]` block used as fallback when no profiles are defined.
    #[serde(default)]
    llm: Option<LlmConfig>,
}

impl Default for ArcanaConfig {
    fn default() -> Self {
        Self {
            vault: VaultConfig::default(),
            index: IndexConfig::default(),
            search: SearchConfig::default(),
            agent: AgentSettings::default(),
            git: GitConfig::default(),
            drafts: DraftsConfig::default(),
            default_profile: "default".to_string(),
            profiles: HashMap::new(),
            llm: None,
        }
    }
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
    /// Env var name holding the API key. If `None`, auto-resolved from provider
    /// (e.g. `anthropic` → `ANTHROPIC_API_KEY`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

impl LlmConfig {
    /// Returns the effective `api_key_env` — explicit value or conventional
    /// default for the provider.
    pub fn effective_api_key_env(&self) -> Option<&str> {
        if let Some(ref env) = self.api_key_env {
            return Some(env);
        }
        match self.provider.as_str() {
            "anthropic" => Some("ANTHROPIC_API_KEY"),
            "openai" => Some("OPENAI_API_KEY"),
            _ => None,
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            api_key_env: None,
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
            max_tokens: 1_000_000,
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
    pub max_explore_iterations: Option<usize>,
    pub profile: Option<String>,
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

    /// Resolve the effective LLM config. Priority (later wins):
    ///
    /// 1. `default_profile` (or `[llm]` fallback for backward compat)
    /// 2. `op_profile` — per-operation default from `[agent.<op>].profile`
    /// 3. `cli_profile` — `--profile` / `ARCANA_PROFILE` CLI flag
    /// 4. `--provider` / `--model` CLI flags
    pub fn resolve_llm(
        &self,
        cli_profile: Option<&str>,
        op_profile: Option<&str>,
        cli_provider: Option<&str>,
        cli_model: Option<&str>,
    ) -> Result<LlmConfig> {
        // Pick the effective profile name: --profile > op default > default_profile
        let profile_name = cli_profile.or(op_profile).unwrap_or(&self.default_profile);

        let mut llm = if let Some(profile) = self.profiles.get(profile_name) {
            profile.clone()
        } else if let Some(ref legacy) = self.llm {
            // Backward compat: [llm] block used when profile not found in map
            legacy.clone()
        } else if self.profiles.is_empty() {
            // Zero config: no profiles, no [llm] → use compiled defaults
            LlmConfig::default()
        } else {
            let mut available: Vec<&str> = self.profiles.keys().map(|k| k.as_str()).collect();
            available.sort();
            return Err(ArcanaError::Config(format!(
                "profile '{}' not found (available: {})",
                profile_name,
                available.join(", ")
            )));
        };

        // CLI flags override everything
        if let Some(provider) = cli_provider {
            llm.provider = provider.to_string();
            // Clear api_key_env so it auto-resolves for the new provider
            llm.api_key_env = None;
        }
        if let Some(model) = cli_model {
            llm.model = model.to_string();
        }

        Ok(llm)
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
    xxhash_rust::xxh3::xxh3_64(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = ArcanaConfig::default();
        assert!(matches!(config.index.db_location, DbLocation::Colocated));
        assert_eq!(config.search.default_limit, 20);
        assert_eq!(config.agent.max_tokens, 1_000_000);
        assert_eq!(config.agent.max_output_tokens, 8192);
        assert_eq!(config.default_profile, "default");
        // Zero config works — resolve_llm falls back to compiled defaults
        let llm = config.resolve_llm(None, None, None, None).unwrap();
        assert_eq!(llm.provider, "anthropic");
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
            [profiles.sonnet]
            model = "claude-sonnet"
            provider = "anthropic"

            [agent]
            max_tokens = 200000
            "#,
        )
        .unwrap();

        let overlay = toml::from_str::<toml::Value>(
            r#"
            [profiles.sonnet]
            model = "claude-haiku"

            [agent.ingest]
            max_tokens = 300000
            "#,
        )
        .unwrap();

        merge_toml_values(&mut base, overlay);

        let table = base.as_table().unwrap();
        // profiles.sonnet.model overridden, provider preserved
        assert_eq!(
            table["profiles"]["sonnet"]["model"].as_str().unwrap(),
            "claude-haiku"
        );
        assert_eq!(
            table["profiles"]["sonnet"]["provider"].as_str().unwrap(),
            "anthropic"
        );
        // agent.max_tokens preserved, agent.ingest.max_tokens added
        assert_eq!(table["agent"]["max_tokens"].as_integer().unwrap(), 200000);
        assert_eq!(
            table["agent"]["ingest"]["max_tokens"].as_integer().unwrap(),
            300000
        );
    }

    #[test]
    fn load_merged_no_files() {
        let config = load_merged(None, None).unwrap();
        assert_eq!(config.agent.max_tokens, 1_000_000);
        let llm = config.resolve_llm(None, None, None, None).unwrap();
        assert_eq!(llm.provider, "anthropic");
    }

    #[test]
    fn load_merged_global_with_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("config.toml");
        std::fs::write(
            &global,
            r#"
            default_profile = "sonnet"

            [profiles.sonnet]
            provider = "anthropic"
            model = "claude-sonnet-4-5-20250929"

            [profiles.haiku]
            provider = "anthropic"
            model = "claude-haiku-4-5-20251001"
            "#,
        )
        .unwrap();

        let config = load_merged(Some(&global), None).unwrap();
        let llm = config.resolve_llm(None, None, None, None).unwrap();
        assert_eq!(llm.model, "claude-sonnet-4-5-20250929");
        let llm = config.resolve_llm(Some("haiku"), None, None, None).unwrap();
        assert_eq!(llm.model, "claude-haiku-4-5-20251001");
        assert_eq!(config.agent.max_tokens, 1_000_000);
    }

    #[test]
    fn load_merged_backward_compat_llm_block() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("config.toml");
        std::fs::write(
            &global,
            r#"
            [llm]
            provider = "anthropic"
            model = "claude-haiku-4-5-20251001"
            "#,
        )
        .unwrap();

        // No profiles defined — [llm] block is used as fallback
        let config = load_merged(Some(&global), None).unwrap();
        let llm = config.resolve_llm(None, None, None, None).unwrap();
        assert_eq!(llm.model, "claude-haiku-4-5-20251001");
        assert_eq!(llm.provider, "anthropic");
    }

    #[test]
    fn load_merged_vault_overrides_global() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.toml");
        std::fs::write(
            &global,
            r#"
            default_profile = "sonnet"

            [profiles.sonnet]
            provider = "anthropic"
            model = "claude-sonnet-4-5-20250929"

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
        let llm = config.resolve_llm(None, None, None, None).unwrap();
        assert_eq!(llm.model, "claude-sonnet-4-5-20250929");
        assert_eq!(config.agent.max_tokens, 300000);
        assert_eq!(config.agent.ingest.max_tokens, Some(500000));
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
        assert_eq!(ingest_max, 1_000_000);

        let ingest_iters = config
            .agent
            .ingest
            .max_iterations
            .unwrap_or(config.agent.max_iterations);
        assert_eq!(ingest_iters, 20);
    }

    fn config_with_profiles() -> ArcanaConfig {
        let mut config = ArcanaConfig {
            default_profile: "sonnet".into(),
            ..Default::default()
        };
        config.profiles.clear();
        config.profiles.insert(
            "sonnet".into(),
            LlmConfig {
                provider: "anthropic".into(),
                model: "claude-sonnet-4-5-20250929".into(),
                ..Default::default()
            },
        );
        config.profiles.insert(
            "opus".into(),
            LlmConfig {
                provider: "anthropic".into(),
                model: "claude-opus-4-6-20250918".into(),
                ..Default::default()
            },
        );
        config.profiles.insert(
            "local".into(),
            LlmConfig {
                provider: "ollama".into(),
                model: "llama3.1".into(),
                endpoint: Some("http://localhost:11434/v1".into()),
                ..Default::default()
            },
        );
        config.profiles.insert(
            "openai".into(),
            LlmConfig {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                api_key_env: Some("OPENAI_API_KEY".into()),
                ..Default::default()
            },
        );
        config
    }

    #[test]
    fn resolve_llm_default_profile() {
        let config = config_with_profiles();
        let llm = config.resolve_llm(None, None, None, None).unwrap();
        assert_eq!(llm.provider, "anthropic");
        assert_eq!(llm.model, "claude-sonnet-4-5-20250929");
        assert_eq!(llm.effective_api_key_env(), Some("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn resolve_llm_named_profile() {
        let config = config_with_profiles();
        let llm = config.resolve_llm(Some("opus"), None, None, None).unwrap();
        assert_eq!(llm.provider, "anthropic");
        assert_eq!(llm.model, "claude-opus-4-6-20250918");
    }

    #[test]
    fn resolve_llm_cli_overrides_profile() {
        let config = config_with_profiles();
        let llm = config
            .resolve_llm(Some("local"), None, None, Some("phi3"))
            .unwrap();
        assert_eq!(llm.provider, "ollama");
        assert_eq!(llm.model, "phi3"); // --model overrides
    }

    #[test]
    fn resolve_llm_profile_not_found() {
        let config = config_with_profiles();
        let err = config
            .resolve_llm(Some("nope"), None, None, None)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nope"), "error should mention the name");
        assert!(msg.contains("opus"), "error should list available profiles");
    }

    #[test]
    fn resolve_llm_cli_profile_beats_op_default() {
        let config = config_with_profiles();
        // --profile "local" beats op default "opus"
        let llm = config
            .resolve_llm(Some("local"), Some("opus"), None, None)
            .unwrap();
        assert_eq!(llm.provider, "ollama");
        assert_eq!(llm.model, "llama3.1");
    }

    #[test]
    fn resolve_llm_op_default_used_when_no_cli_profile() {
        let config = config_with_profiles();
        let llm = config.resolve_llm(None, Some("opus"), None, None).unwrap();
        assert_eq!(llm.model, "claude-opus-4-6-20250918");
    }

    #[test]
    fn effective_api_key_env_auto_resolves() {
        // anthropic auto-resolves
        let llm = LlmConfig {
            provider: "anthropic".into(),
            ..Default::default()
        };
        assert_eq!(llm.effective_api_key_env(), Some("ANTHROPIC_API_KEY"));

        // openai auto-resolves
        let llm = LlmConfig {
            provider: "openai".into(),
            ..Default::default()
        };
        assert_eq!(llm.effective_api_key_env(), Some("OPENAI_API_KEY"));

        // ollama has no key
        let llm = LlmConfig {
            provider: "ollama".into(),
            ..Default::default()
        };
        assert_eq!(llm.effective_api_key_env(), None);

        // explicit api_key_env overrides auto-resolve
        let llm = LlmConfig {
            provider: "anthropic".into(),
            api_key_env: Some("MY_CUSTOM_KEY".into()),
            ..Default::default()
        };
        assert_eq!(llm.effective_api_key_env(), Some("MY_CUSTOM_KEY"));
    }

    #[test]
    fn resolve_llm_cli_provider_clears_api_key_env() {
        let config = config_with_profiles();
        // --provider openai on a profile that had explicit api_key_env
        let llm = config
            .resolve_llm(Some("openai"), None, Some("anthropic"), None)
            .unwrap();
        // --provider cleared api_key_env, so it auto-resolves for anthropic
        assert_eq!(llm.provider, "anthropic");
        assert_eq!(llm.effective_api_key_env(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(llm.api_key_env, None);
    }
}
