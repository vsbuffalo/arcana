use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::errors::{ArcanaError, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArcanaConfig {
    pub vault: VaultConfig,
    pub index: IndexConfig,
    pub search: SearchConfig,
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
}
