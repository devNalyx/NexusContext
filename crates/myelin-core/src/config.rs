use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Ingestion sources and the capture-worthiness filter land here once the
/// extraction pipeline design settles — see README.md.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub promotion: PromotionConfig,
    #[serde(default)]
    pub atrophy: AtrophyConfig,
    #[serde(default)]
    pub pruning: PruningConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
}

/// Tunable knobs for the warmup-queue -> skill promotion logic. Defaults
/// are guesses (see README's open risks), not measured values — this
/// exists so retuning them doesn't require a rebuild.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionConfig {
    /// Reps a candidate needs (with no high-stakes signal) before it
    /// auto-promotes.
    #[serde(default = "default_reps")]
    pub reps: i64,
    /// Token-overlap (Jaccard) threshold for "this observation is the same
    /// procedure as that candidate". 0.0-1.0.
    #[serde(default = "default_similarity_threshold")]
    pub similarity_threshold: f64,
}

fn default_reps() -> i64 {
    3
}

fn default_similarity_threshold() -> f64 {
    0.4
}

impl Default for PromotionConfig {
    fn default() -> Self {
        Self {
            reps: default_reps(),
            similarity_threshold: default_similarity_threshold(),
        }
    }
}

/// A skill nobody's invoked (or used since) in this long is flagged
/// `stale` by `list_skills` — informational only, nothing acts on it yet.
/// The default (30 days) is a guess, same caveat as `PromotionConfig`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtrophyConfig {
    #[serde(default = "default_stale_after_secs")]
    pub stale_after_secs: i64,
}

fn default_stale_after_secs() -> i64 {
    30 * 24 * 3600
}

impl Default for AtrophyConfig {
    fn default() -> Self {
        Self {
            stale_after_secs: default_stale_after_secs(),
        }
    }
}

/// Soft cap on live (non-archived) skills, enforced at promotion time by
/// auto-archiving the least-recently-used active skill(s) - never by
/// blocking a promotion. Every live skill's frontmatter loads into every
/// future session, so an unbounded skill count is an unbounded, compounding
/// token cost - see change_proposal.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PruningConfig {
    /// `<= 0` disables auto-eviction entirely.
    #[serde(default = "default_max_active_skills")]
    pub max_active_skills: i64,
}

fn default_max_active_skills() -> i64 {
    25
}

impl Default for PruningConfig {
    fn default() -> Self {
        Self {
            max_active_skills: default_max_active_skills(),
        }
    }
}

/// Which MCP tools `tools/list` advertises. Every session start pays a
/// fixed token cost for the schema of each tool returned here - see
/// change_proposal.md. `enabled`, when set, takes precedence over `preset`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolsConfig {
    #[serde(default)]
    pub preset: ToolsPreset,
    #[serde(default)]
    pub enabled: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolsPreset {
    Minimal,
    #[default]
    Standard,
    Full,
}

impl Config {
    /// Missing config file is not an error — defaults apply.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pruning_config_defaults_to_25_when_absent() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.pruning.max_active_skills, 25);
    }

    #[test]
    fn pruning_config_round_trips_explicit_value() {
        let config: Config = toml::from_str("[pruning]\nmax_active_skills = 10\n").unwrap();
        assert_eq!(config.pruning.max_active_skills, 10);
    }

    #[test]
    fn tools_config_defaults_to_standard_preset_with_no_enabled_override() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.tools.preset, ToolsPreset::Standard);
        assert_eq!(config.tools.enabled, None);
    }

    #[test]
    fn tools_config_round_trips_preset_only() {
        let config: Config = toml::from_str("[tools]\npreset = \"minimal\"\n").unwrap();
        assert_eq!(config.tools.preset, ToolsPreset::Minimal);
        assert_eq!(config.tools.enabled, None);
    }

    #[test]
    fn tools_config_round_trips_enabled_only() {
        let config: Config =
            toml::from_str("[tools]\nenabled = [\"list_skills\", \"mark_skill_used\"]\n").unwrap();
        assert_eq!(config.tools.preset, ToolsPreset::Standard);
        assert_eq!(
            config.tools.enabled,
            Some(vec![
                "list_skills".to_string(),
                "mark_skill_used".to_string()
            ])
        );
    }

    #[test]
    fn tools_config_round_trips_full_preset() {
        let config: Config = toml::from_str("[tools]\npreset = \"full\"\n").unwrap();
        assert_eq!(config.tools.preset, ToolsPreset::Full);
    }
}
