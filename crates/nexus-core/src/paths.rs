use directories::ProjectDirs;
use std::path::PathBuf;

/// Resolved filesystem locations, honoring the `NEXUS_CACHE_DIR` env override
/// documented in the proposal (config lives at ~/.config, data at ~/.local/share
/// unless overridden).
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn resolve() -> Self {
        let dirs = ProjectDirs::from("", "", "nexuscontext")
            .expect("could not determine a home directory for the current user");

        let config_dir = dirs.config_dir().to_path_buf();
        let data_dir = std::env::var_os("NEXUS_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs.data_dir().to_path_buf());

        Self {
            config_dir,
            data_dir,
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn project_data_dir(&self, project_hash: &str) -> PathBuf {
        self.data_dir.join(project_hash)
    }
}
