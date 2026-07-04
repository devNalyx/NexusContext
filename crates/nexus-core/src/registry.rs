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
    /// Last time any tool call (search/query/trace/etc, not just a reindex)
    /// touched this project - lets a long-lived daemon distinguish "indexed
    /// once, never touched again" from active use. Missing on registry
    /// entries written before this field existed, hence the default.
    #[serde(default)]
    pub last_queried_unix: u64,
    /// Background-watcher reindex activity, distinct from a manual reindex
    /// via CLI/MCP/`projects.reindex` - lets the GUI answer "how often, and
    /// how expensively, is the auto-sync watcher rebuilding this project on
    /// its own" without conflating it with reindexes someone asked for.
    #[serde(default)]
    pub auto_reindex_count: u64,
    #[serde(default)]
    pub auto_reindex_fail_count: u64,
    #[serde(default)]
    pub auto_reindex_total_ms: u64,
    #[serde(default)]
    pub last_auto_reindex_ms: u64,
    #[serde(default)]
    pub last_auto_reindex_unix: u64,
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

    /// Writes via a temp file + rename rather than a direct write - the
    /// watcher thread and a manual reindex/tool call can both save this file
    /// around the same time, and a plain `fs::write` racing between two
    /// writers can interleave and leave `projects.json` truncated or
    /// unparseable (silently discarding every registered project, since
    /// `load` treats a parse failure the same as "no projects yet"). Rename
    /// is atomic on the same filesystem, so a reader only ever sees a
    /// complete old or complete new version, never a partial one.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    pub fn upsert(&mut self, entry: ProjectEntry) {
        match self.projects.iter_mut().find(|p| p.hash == entry.hash) {
            Some(existing) => *existing = entry,
            None => self.projects.push(entry),
        }
    }

    /// Bumps `last_queried_unix` for a project already in the registry -
    /// a no-op if it isn't registered yet (e.g. a tool call racing an
    /// in-flight first index), since there's nothing to record staleness
    /// against.
    pub fn touch_queried(&mut self, hash: &str, unix_time: u64) {
        if let Some(entry) = self.projects.iter_mut().find(|p| p.hash == hash) {
            entry.last_queried_unix = unix_time;
        }
    }

    /// Records one watcher-triggered auto-reindex attempt. A no-op if the
    /// project isn't registered yet, same reasoning as `touch_queried`.
    pub fn record_auto_reindex(
        &mut self,
        hash: &str,
        duration_ms: u64,
        unix_time: u64,
        success: bool,
    ) {
        if let Some(entry) = self.projects.iter_mut().find(|p| p.hash == hash) {
            if success {
                entry.auto_reindex_count += 1;
                entry.auto_reindex_total_ms += duration_ms;
            } else {
                entry.auto_reindex_fail_count += 1;
            }
            entry.last_auto_reindex_ms = duration_ms;
            entry.last_auto_reindex_unix = unix_time;
        }
    }
}
