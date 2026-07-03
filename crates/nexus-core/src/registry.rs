use serde::{Deserialize, Serialize};
use std::path::Path;

/// Tracks which projects have been indexed, so the control API / GUI can
/// list them by human-readable path without reverse-engineering the hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub root_path: String,
    pub hash: String,
    pub last_indexed_unix: u64,
    pub nodes: i64,
    pub edges: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    pub projects: Vec<ProjectEntry>,
}

impl Registry {
    /// A missing or corrupt registry file just means "no projects yet" -
    /// not a hard error, consistent with Config::load's zero-config stance.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn upsert(&mut self, entry: ProjectEntry) {
        match self.projects.iter_mut().find(|p| p.hash == entry.hash) {
            Some(existing) => *existing = entry,
            None => self.projects.push(entry),
        }
    }
}
