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

    pub fn usage_stats_file(&self) -> PathBuf {
        self.data_dir.join("usage_stats.json")
    }

    pub fn control_socket(&self) -> PathBuf {
        self.runtime_dir.join("nexuscontext.sock")
    }

    pub fn log_file(&self) -> PathBuf {
        self.data_dir.join("nexusd.log")
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

/// Writes `contents` to `path`, owner-only (0600), same reasoning as
/// `config.toml`'s own fix: on a shared/multi-user box, anything else this
/// daemon writes under the data dir - `registry.json`/`usage_stats.json`
/// (plain `fs::write` previously inherited the process umask, commonly
/// 0644) and `graph.db` (via `GraphStore::open`'s `Connection::open`, same
/// story) - is readable by any other local user otherwise. `graph.db` is
/// the most sensitive of the three: it holds the full indexed source text
/// (FTS5) and embedding vectors for every project ever indexed. See issue
/// #32.
///
/// `.mode(0o600)` on `OpenOptions` is applied atomically by the OS at
/// creation time, so a freshly-created file is never briefly
/// world-readable the way a write-then-chmod sequence would leave it;
/// `set_permissions` afterward additionally normalizes a *pre-existing*
/// file that predates this fix, since `mode()` only affects files the
/// `open()` call actually creates.
#[cfg(unix)]
pub fn write_owner_only(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
pub fn write_owner_only(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, contents)
}
