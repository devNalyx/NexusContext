use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub embeddings: EmbeddingsConfig,
}

/// Embeddings are an optional layer: the knowledge graph covers structural
/// queries with no endpoint configured at all, per the proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingsConfig {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_timeout_secs() -> u64 {
    30
}

impl Default for EmbeddingsConfig {
    fn default() -> Self {
        Self {
            endpoint: None,
            model: None,
            api_key: None,
            timeout_secs: default_timeout_secs(),
        }
    }
}

impl Config {
    /// Missing config file is not an error - defaults apply, matching the
    /// "useful with zero config" goal from the proposal.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Config::default());
        }

        let raw = std::fs::read_to_string(path).map_err(|source| Error::ConfigRead {
            path: path.to_path_buf(),
            source,
        })?;

        Ok(toml::from_str(&raw)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::ConfigRead {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let raw = toml::to_string_pretty(self)?;
        std::fs::write(path, raw).map_err(|source| Error::ConfigRead {
            path: path.to_path_buf(),
            source,
        })
    }
}
