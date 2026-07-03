use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub embeddings: EmbeddingsConfig,
    /// If non-empty, `index_repository`/reindex only accepts paths under one
    /// of these roots. Empty (the default) means unrestricted, matching the
    /// "useful with zero config" goal - this is an opt-in safety rail for
    /// anyone who wants it, not a default restriction.
    #[serde(default)]
    pub allowed_roots: Vec<String>,
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
    /// Explicit opt-in required before the daemon will send code to a
    /// non-loopback/non-private endpoint - see the "self-contained, no
    /// cloud calls" claim in the proposal. Defaults to false so a remote
    /// endpoint in config.toml doesn't silently start exfiltrating code
    /// after a config change.
    #[serde(default)]
    pub allow_remote: bool,
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
            allow_remote: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingsPolicy {
    NotConfigured,
    Allowed,
    /// Configured, but points off-box and `allow_remote` isn't set.
    RemoteBlocked,
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

    pub fn embeddings_policy(&self) -> EmbeddingsPolicy {
        let Some(endpoint) = &self.embeddings.endpoint else {
            return EmbeddingsPolicy::NotConfigured;
        };
        if self.embeddings.allow_remote || is_loopback_or_private(endpoint) {
            EmbeddingsPolicy::Allowed
        } else {
            EmbeddingsPolicy::RemoteBlocked
        }
    }

    pub fn is_path_allowed(&self, path: &Path) -> bool {
        self.allowed_roots.is_empty()
            || self
                .allowed_roots
                .iter()
                .any(|root| path.starts_with(root))
    }
}

fn extract_host(endpoint: &str) -> Option<&str> {
    let without_scheme = endpoint.split("://").nth(1).unwrap_or(endpoint);
    let host_port = without_scheme.split('/').next().unwrap_or(without_scheme);
    let host = host_port.split(':').next().unwrap_or(host_port);
    (!host.is_empty()).then_some(host)
}

fn is_loopback_or_private(endpoint: &str) -> bool {
    let Some(host) = extract_host(endpoint) else {
        return false;
    };
    if host == "localhost" {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_loopback() || v4.is_private(),
        Ok(IpAddr::V6(v6)) => v6.is_loopback(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_and_private_hosts_are_recognized() {
        assert!(is_loopback_or_private("http://localhost:11434/v1"));
        assert!(is_loopback_or_private("http://127.0.0.1:11434/v1"));
        assert!(is_loopback_or_private("http://192.168.1.50:11434/v1"));
        assert!(is_loopback_or_private("http://10.0.0.5:11434/v1"));
    }

    #[test]
    fn public_hosts_are_not_loopback_or_private() {
        assert!(!is_loopback_or_private("https://api.example.com/v1"));
        assert!(!is_loopback_or_private("http://8.8.8.8/v1"));
    }
}
