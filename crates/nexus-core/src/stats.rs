use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Lifetime aggregate for one MCP tool or control-API method - no per-call
/// event log, since this is a Phase 1 "get real signal before deciding on
/// any limits" pass, not an audit trail. Averages/percentiles are derived
/// from these sums at read time rather than stored.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCallStats {
    pub call_count: u64,
    pub error_count: u64,
    pub total_latency_ms: u64,
    pub max_latency_ms: u64,
    /// Sum of response payload sizes (the text/JSON actually returned to the
    /// caller) - this is the token-cost-relevant signal, not on-disk size.
    pub total_output_bytes: u64,
    pub last_called_unix: u64,
}

impl ToolCallStats {
    fn record(&mut self, latency_ms: u64, output_bytes: u64, is_error: bool, unix_time: u64) {
        self.call_count += 1;
        if is_error {
            self.error_count += 1;
        }
        self.total_latency_ms += latency_ms;
        self.max_latency_ms = self.max_latency_ms.max(latency_ms);
        self.total_output_bytes += output_bytes;
        self.last_called_unix = unix_time;
    }
}

/// Two separate maps so MCP-driven usage (external agents) and control-API
/// usage (the GUI itself) never get mixed into one signal - they answer
/// different questions ("how are agents using this" vs "how is the GUI
/// being used").
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct UsageStats {
    #[serde(default)]
    pub mcp_tools: HashMap<String, ToolCallStats>,
    #[serde(default)]
    pub control_methods: HashMap<String, ToolCallStats>,
    #[serde(default)]
    pub collecting_since_unix: u64,
}

impl UsageStats {
    /// A missing or corrupt stats file just means "no data yet" - consistent
    /// with Registry::load's zero-config stance.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// Temp file + rename, same reasoning as Registry::save: a stats write
    /// racing another writer must never leave the file truncated or
    /// unparseable.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Best-effort, like `touch_queried`: a stats-write failure must never fail
/// the tool call it's instrumenting, so errors are swallowed rather than
/// propagated. Known limitation: concurrent writers (e.g. two simultaneous
/// `nexusd mcp` sessions) can lose an increment to a last-writer-wins race,
/// same as `projects.json` today - acceptable for insight-gathering.
fn record(
    path: &Path,
    bucket: impl FnOnce(&mut UsageStats) -> &mut HashMap<String, ToolCallStats>,
    name: &str,
    latency_ms: u64,
    output_bytes: u64,
    is_error: bool,
) {
    let mut stats = UsageStats::load(path);
    if stats.collecting_since_unix == 0 {
        stats.collecting_since_unix = now_unix();
    }
    let unix_time = now_unix();
    bucket(&mut stats)
        .entry(name.to_string())
        .or_default()
        .record(latency_ms, output_bytes, is_error, unix_time);
    let _ = stats.save(path);
}

pub fn record_mcp_call(
    path: &Path,
    name: &str,
    latency_ms: u64,
    output_bytes: u64,
    is_error: bool,
) {
    record(
        path,
        |s| &mut s.mcp_tools,
        name,
        latency_ms,
        output_bytes,
        is_error,
    );
}

pub fn record_control_call(
    path: &Path,
    name: &str,
    latency_ms: u64,
    output_bytes: u64,
    is_error: bool,
) {
    record(
        path,
        |s| &mut s.control_methods,
        name,
        latency_ms,
        output_bytes,
        is_error,
    );
}
