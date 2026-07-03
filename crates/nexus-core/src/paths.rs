use directories::ProjectDirs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Resolved filesystem locations, honoring the `NEXUS_CACHE_DIR` env override
/// documented in the proposal (config lives at ~/.config, data at ~/.local/share
/// unless overridden).
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    /// Short-lived runtime files (currently just the control socket) live
    /// here, not under `data_dir`: Unix domain socket paths are capped at
    /// ~108 bytes (`SUN_LEN`), and `data_dir` has no such guarantee.
    /// Falls back to `data_dir` if the platform has no runtime dir concept.
    pub runtime_dir: PathBuf,
}

impl Paths {
    pub fn resolve() -> Self {
        let dirs = ProjectDirs::from("", "", "nexuscontext")
            .expect("could not determine a home directory for the current user");

        let config_dir = dirs.config_dir().to_path_buf();
        let data_dir = std::env::var_os("NEXUS_CACHE_DIR")
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

    pub fn project_data_dir(&self, project_hash: &str) -> PathBuf {
        self.data_dir.join(project_hash)
    }

    pub fn registry_file(&self) -> PathBuf {
        self.data_dir.join("projects.json")
    }

    pub fn control_socket(&self) -> PathBuf {
        self.runtime_dir.join("nexuscontext.sock")
    }
}

/// Stable, dependency-free identifier for a project root, used to namespace
/// its graph/vector store under the shared data dir.
pub fn project_hash(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
