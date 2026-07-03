use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// In-process cache for deterministic, index-derived responses (e.g.
/// `get_architecture`) - keyed on the project's `last_indexed_unix` from the
/// registry, so a reindex naturally busts the cache instead of needing
/// explicit invalidation logic. This is the concrete piece of "prefix
/// caching for codebase metadata" from the proposal that's actually ours to
/// implement: we don't control the calling agent's LLM-side prompt cache,
/// but we can make sure repeated calls for an unchanged index don't re-hit
/// SQLite, and that our JSON output is byte-stable so a provider-side
/// prefix cache on the agent's end has something stable to key on.
struct Entry {
    last_indexed_unix: u64,
    value: Value,
}

static CACHE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();

pub fn get_or_compute(
    key: &str,
    last_indexed_unix: u64,
    compute: impl FnOnce() -> anyhow::Result<Value>,
) -> anyhow::Result<Value> {
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Some(entry) = cache.lock().unwrap().get(key) {
        if entry.last_indexed_unix == last_indexed_unix {
            tracing::debug!(key, "cache hit");
            return Ok(entry.value.clone());
        }
    }

    tracing::debug!(key, "cache miss");
    let value = compute()?;
    cache.lock().unwrap().insert(
        key.to_string(),
        Entry {
            last_indexed_unix,
            value: value.clone(),
        },
    );
    Ok(value)
}
