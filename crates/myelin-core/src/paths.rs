use directories::{BaseDirs, ProjectDirs};
use std::path::PathBuf;

/// Resolved filesystem locations. Config lives at `~/.config/myelin`
/// (overridable via `MYELIN_CONFIG_DIR`, e.g. for isolating tests from a
/// real `config.toml`), data at `~/.local/share/myelin` unless overridden
/// via `MYELIN_DATA_DIR` — same override convention NexusContext used for
/// `NEXUS_CACHE_DIR`.
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    /// Unix domain socket paths are capped at ~108 bytes (`SUN_LEN`), so the
    /// control socket lives under the platform runtime dir, not `data_dir`.
    pub runtime_dir: PathBuf,
}

impl Paths {
    pub fn resolve() -> Self {
        let dirs = ProjectDirs::from("", "", "myelin")
            .expect("could not determine a home directory for the current user");

        let config_dir = std::env::var_os("MYELIN_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs.config_dir().to_path_buf());
        let data_dir = std::env::var_os("MYELIN_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs.data_dir().to_path_buf());
        let runtime_dir = dirs
            .runtime_dir()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| data_dir.clone());

        Self {
            config_dir,
            data_dir,
            runtime_dir,
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn control_socket(&self) -> PathBuf {
        self.runtime_dir.join("myelin.sock")
    }

    pub fn log_file(&self) -> PathBuf {
        self.data_dir.join("myelind.log")
    }

    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("myelin.db")
    }

    /// Where promoted skills get written. Deliberately the *personal*
    /// skills directory (not a project-scoped `.claude/skills`), since the
    /// whole point is patterns that generalize across repos, not one.
    /// Overridable via `MYELIN_SKILLS_DIR` for testing.
    pub fn skills_dir(&self) -> PathBuf {
        if let Some(dir) = std::env::var_os("MYELIN_SKILLS_DIR") {
            return PathBuf::from(dir);
        }
        BaseDirs::new()
            .expect("could not determine a home directory for the current user")
            .home_dir()
            .join(".claude")
            .join("skills")
    }

    /// `~/.claude/settings.json` - Claude Code's own shared config, not
    /// Myelin's. Overridable via `MYELIN_CLAUDE_SETTINGS_FILE` for testing.
    pub fn claude_settings_file(&self) -> PathBuf {
        if let Some(path) = std::env::var_os("MYELIN_CLAUDE_SETTINGS_FILE") {
            return PathBuf::from(path);
        }
        BaseDirs::new()
            .expect("could not determine a home directory for the current user")
            .home_dir()
            .join(".claude")
            .join("settings.json")
    }
}
