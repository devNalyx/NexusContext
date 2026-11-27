use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// If non-empty, `index_repository`/reindex only accepts paths under one
    /// of these roots. Empty (the default) means unrestricted, matching the
    /// "useful with zero config" goal - this is an opt-in safety rail for
    /// anyone who wants it, not a default restriction.
    #[serde(default)]
    pub allowed_roots: Vec<String>,
    #[serde(default)]
    pub watcher: WatcherConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub lsp: LspConfig,
    /// Opt-in indexing of YAML/JSON configuration (issue #112). Off by default:
    /// config frequently contains secrets, so content indexing must be a
    /// deliberate choice with exclusions applied before ingestion.
    #[serde(default)]
    pub config_index: ConfigIndexConfig,
    /// Human-registered external knowledge sources (e.g. an Obsidian vault).
    /// Registration is deliberately a CLI/control-plane act, never an MCP
    /// write path - agents can discover sources via `list_sources`, not
    /// create or approve them. Empty by default.
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
}

/// A registered external source. `root_path` is stored canonically by
/// `nexus source add`; `kind` is free-form so new source flavours don't need
/// a config-schema change, and `resource` is the OKF-style provenance URI
/// used on curated facts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    pub id: String,
    pub root_path: String,
    #[serde(default = "default_source_kind")]
    pub kind: String,
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_source_enabled")]
    pub enabled: bool,
}

fn default_source_kind() -> String {
    "markdown".to_string()
}

fn default_source_enabled() -> bool {
    true
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            root_path: String::new(),
            kind: default_source_kind(),
            resource: None,
            title: None,
            description: None,
            enabled: true,
        }
    }
}

/// Governs which registered projects the background file watcher actively
/// watches/auto-reindexes. A project not queried via any MCP tool within
/// `warm_window_secs` is "cold" and stops being watched - see
/// `ProjectEntry::is_warm` - so idle repos stop costing inotify watches and
/// wasted reindex cycles nobody's looking at.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherConfig {
    #[serde(default = "default_warm_window_secs")]
    pub warm_window_secs: u64,
}

fn default_warm_window_secs() -> u64 {
    6 * 3600
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            warm_window_secs: default_warm_window_secs(),
        }
    }
}

/// Which MCP tools `tools/list` advertises. Every session start pays a fixed
/// token cost for the schema of each tool returned here, so trimming this
/// set is the highest-leverage way to reduce that per-session tax - see
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

/// Optional LSP-resolved-symbol enrichment (issue #10) - strictly
/// enrichment, never load-bearing: default-off, and a missing/failing
/// server always degrades to the static tree-sitter-only index rather than
/// failing the reindex. Rust-only pilot (`rust-analyzer`); per the issue's
/// own scope-narrowing (one language proves the shape - provenance and the
/// warm/cold split are the hard, language-agnostic decisions, not the
/// server integration itself), a multi-language matrix is explicitly not
/// this pilot's job. Never runs on a normal reindex - only on an explicit
/// `deep` request (`index_repository`'s `deep` argument / `nexus reindex
/// --deep`), so enabling this never adds latency to the watcher's ordinary
/// auto-reindex-on-file-change loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Override if `rust-analyzer` isn't on PATH under its default name.
    #[serde(default = "default_lsp_server_command")]
    pub server_command: String,
    /// Caps how many LSP server child processes `nexusd serve` keeps
    /// resident at once (reused across repeated `--deep` reindexes of the
    /// same project rather than respawned each time) - the daemon evicts
    /// the least-recently-used server once a new project would exceed this.
    /// A per-project rust-analyzer instance is the "memory-heavy, needs a
    /// cap" cost the issue's own risk section flagged; this is that cap.
    #[serde(default = "default_lsp_max_concurrent_servers")]
    pub max_concurrent_servers: usize,
    /// Per-request timeout talking to the server - a hung/slow server
    /// degrades enrichment to "whatever resolved before the timeout,"
    /// never blocks the reindex indefinitely.
    #[serde(default = "default_lsp_request_timeout_secs")]
    pub request_timeout_secs: u64,
}

fn default_lsp_server_command() -> String {
    "rust-analyzer".to_string()
}

fn default_lsp_max_concurrent_servers() -> usize {
    2
}

fn default_lsp_request_timeout_secs() -> u64 {
    10
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            server_command: default_lsp_server_command(),
            max_concurrent_servers: default_lsp_max_concurrent_servers(),
            request_timeout_secs: default_lsp_request_timeout_secs(),
        }
    }
}

/// Opt-in source-linked YAML/JSON indexing for GitOps-style repositories.
///
/// Globs are matched against paths relative to the indexed repository root,
/// and `exclude` always wins over `include`. `metadata_only` indexes key paths
/// and source locations without storing raw file content in FTS - useful when
/// values may contain secrets. This is not secret detection or redaction: it
/// is a conservative content policy, not a safety guarantee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigIndexConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_config_index_include")]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub metadata_only: bool,
}

fn default_config_index_include() -> Vec<String> {
    vec![
        "**/*.yaml".to_string(),
        "**/*.yml".to_string(),
        "**/*.json".to_string(),
    ]
}

impl Default for ConfigIndexConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            include: default_config_index_include(),
            exclude: Vec::new(),
            metadata_only: false,
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
        write_config_file(path, &raw).map_err(|source| Error::ConfigRead {
            path: path.to_path_buf(),
            source,
        })
    }

    /// `Path::starts_with` is a component-wise prefix check, not a real
    /// containment check - it does not resolve `..`, so a raw
    /// `"<root>/../../etc"` starts-with-`<root>` even though it plainly
    /// escapes it. Canonicalizing `path` here (not just trusting a caller to
    /// have done it already) closes that at the source: every call site
    /// that reaches this function is protected, not just the ones a
    /// previous review happened to check by hand. `allowed_roots` entries
    /// are canonicalized the same way for a fair comparison, in case one is
    /// configured relative or behind a symlink. Falls back to the raw form
    /// on a canonicalization failure (path/root doesn't exist yet) rather
    /// than failing open - matches `nexus_core::paths::project_hash`'s
    /// established fallback pattern. See issue #29.
    pub fn is_path_allowed(&self, path: &Path) -> bool {
        if self.allowed_roots.is_empty() {
            return true;
        }
        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.allowed_roots.iter().any(|root| {
            let canonical_root = Path::new(root)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(root));
            canonical_path.starts_with(&canonical_root)
        })
    }

    /// Finds the registered source matching a project root. Both sides are
    /// canonicalized here (not just trusted as stored) so a source written
    /// before a symlink change, or an index request using a `..`-laden path,
    /// still resolves to the same identity. No `allowed_roots` meaning here:
    /// indexing still runs its own check separately.
    pub fn source_for_root(&self, root: &Path) -> Option<&SourceConfig> {
        let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        self.sources.iter().find(|source| {
            let source_root = Path::new(&source.root_path)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(&source.root_path));
            source_root == canonical
        })
    }
}

/// Owner-only (0600) rather than left to whatever the process umask happens
/// to produce - `config.toml` can hold other secrets in the future, and on a
/// shared/multi-user box a group- or world-readable config file is a real
/// plaintext-secret leak, not a hypothetical one. `crate::paths::
/// write_owner_only` now also backs `registry.json`/`usage_stats.json` (see
/// issue #32) - this used to duplicate the same `OpenOptions`/`mode(0o600)`
/// logic locally.
fn write_config_file(path: &Path, contents: &str) -> std::io::Result<()> {
    crate::paths::write_owner_only(path, contents.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_index_is_opt_in_with_safe_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.config_index.enabled);
        assert!(!config.config_index.metadata_only);
        assert_eq!(
            config.config_index.include,
            vec![
                "**/*.yaml".to_string(),
                "**/*.yml".to_string(),
                "**/*.json".to_string()
            ]
        );
        assert!(config.config_index.exclude.is_empty());
    }

    #[test]
    fn config_index_round_trips_custom_policy() {
        let config: Config = toml::from_str(
            "[config_index]\nenabled = true\ninclude = [\"**/*.yml\"]\nexclude = [\"**/secrets/**\"]\nmetadata_only = true\n",
        )
        .unwrap();
        assert!(config.config_index.enabled);
        assert!(config.config_index.metadata_only);
        assert_eq!(config.config_index.include, vec!["**/*.yml".to_string()]);
        assert_eq!(
            config.config_index.exclude,
            vec!["**/secrets/**".to_string()]
        );
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
            toml::from_str("[tools]\nenabled = [\"search_code\", \"get_file_context\"]\n").unwrap();
        assert_eq!(config.tools.preset, ToolsPreset::Standard);
        assert_eq!(
            config.tools.enabled,
            Some(vec![
                "search_code".to_string(),
                "get_file_context".to_string()
            ])
        );
    }

    #[test]
    fn tools_config_round_trips_full_preset() {
        let config: Config = toml::from_str("[tools]\npreset = \"full\"\n").unwrap();
        assert_eq!(config.tools.preset, ToolsPreset::Full);
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_config_file_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "nexuscontext-config-test-{:?}-{}",
            std::thread::current().id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let config = Config::default();
        config.save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        // Re-saving an existing file (e.g. a pre-fix 0644 file, simulated
        // here by widening it first) must also end up owner-only - `mode()`
        // on OpenOptions only governs *creation*, so save() has to actively
        // normalize an existing file's permissions too, not just rely on it.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        config.save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        std::fs::remove_file(&path).ok();
    }

    fn scratch_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "nexuscontext-pathcheck-{label}-{}",
            std::process::id()
        ))
    }

    /// Regression test for issue #29: a `..`-laden path used to pass
    /// `is_path_allowed` outright because `Path::starts_with` never
    /// resolves `..`, only for a caller's *later* canonicalize to reveal it
    /// had actually escaped `allowed_roots` all along.
    #[test]
    fn dot_dot_traversal_outside_an_allowed_root_is_rejected() {
        let root = scratch_dir("root");
        let outside = scratch_dir("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let config = Config {
            allowed_roots: vec![root.to_string_lossy().to_string()],
            ..Default::default()
        };

        // Escapes `root` via `..` into a directory that's a sibling, not a
        // descendant - `Path::starts_with` alone would have said yes here.
        let escaping = root.join("..").join(outside.file_name().unwrap());
        assert!(
            !config.is_path_allowed(&escaping),
            "a `..`-traversal path that resolves outside allowed_roots must be rejected"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn a_real_subdirectory_of_an_allowed_root_is_still_accepted() {
        let root = scratch_dir("root-subdir");
        let nested = root.join("nested").join("project");
        std::fs::create_dir_all(&nested).unwrap();

        let config = Config {
            allowed_roots: vec![root.to_string_lossy().to_string()],
            ..Default::default()
        };

        assert!(
            config.is_path_allowed(&nested),
            "a genuine descendant of an allowed root must still be accepted"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn empty_allowed_roots_permits_everything_unrestricted() {
        let config = Config::default();
        assert!(config.allowed_roots.is_empty());
        assert!(config.is_path_allowed(std::path::Path::new("/anything/at/all")));
    }

    #[test]
    fn a_nonexistent_path_falls_back_to_raw_comparison_rather_than_panicking() {
        // Neither the checked path nor the configured root exist on disk -
        // canonicalize() fails for both, and is_path_allowed must fall back
        // to comparing the raw forms rather than erroring or panicking.
        let config = Config {
            allowed_roots: vec!["/nexuscontext-test-does-not-exist-root".to_string()],
            ..Default::default()
        };
        assert!(config.is_path_allowed(std::path::Path::new(
            "/nexuscontext-test-does-not-exist-root/child"
        )));
        assert!(!config.is_path_allowed(std::path::Path::new(
            "/nexuscontext-test-does-not-exist-elsewhere"
        )));
    }

    #[test]
    fn sources_default_to_empty_and_round_trip() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.sources.is_empty());

        let config: Config = toml::from_str(
            "[[sources]]\nid = \"vault\"\nroot_path = \"/tmp/vault\"\nkind = \"obsidian-vault\"\nresource = \"obsidian-vault://vault\"\ntitle = \"Notes\"\n",
        )
        .unwrap();
        assert_eq!(config.sources.len(), 1);
        let source = &config.sources[0];
        assert_eq!(source.id, "vault");
        assert_eq!(source.kind, "obsidian-vault");
        assert_eq!(source.resource.as_deref(), Some("obsidian-vault://vault"));
        assert_eq!(source.title.as_deref(), Some("Notes"));
        assert!(source.enabled);

        let raw = toml::to_string_pretty(&config).unwrap();
        let reparsed: Config = toml::from_str(&raw).unwrap();
        assert_eq!(reparsed.sources[0].id, "vault");
    }

    #[test]
    fn source_for_root_matches_canonical_path_only() {
        let root = scratch_dir("source-root");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let config = Config {
            sources: vec![SourceConfig {
                id: "vault".to_string(),
                root_path: root.to_string_lossy().to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            config
                .source_for_root(&nested.join(".."))
                .map(|s| s.id.as_str()),
            Some("vault")
        );
        assert!(config
            .source_for_root(std::path::Path::new("/nexuscontext-test-not-the-vault"))
            .is_none());
        std::fs::remove_dir_all(&root).ok();
    }
}
