use anyhow::{anyhow, bail, Result};
use nexus_core::{project_hash, Config, Paths, Registry};
use nexus_index::{self as index, index_project, Direction, NodeRecord};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Hard ceiling on any caller-supplied `limit`, independent of each tool's
/// own default - a single bad call from a coding agent can't blow up a
/// response regardless of what limit it asked for. See change_proposal.md.
const SERVER_MAX_LIMIT: u32 = 200;

fn clamp_limit(requested: u32) -> u32 {
    requested.min(SERVER_MAX_LIMIT)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether `repo_path` is registered and has gone cold (not warm per
/// `ProjectEntry::is_warm`) - i.e. the background watcher has already
/// stopped watching it, per the same warm-window config it uses. An
/// unregistered project (never indexed) isn't "cold", it's just untouched -
/// nothing to catch up here, `index_repository` is the entry point for that.
fn is_cold(repo_path: &std::path::Path) -> bool {
    let paths = Paths::resolve();
    let hash = project_hash(repo_path);
    let registry = Registry::load(&paths.registry_file());
    let Some(entry) = registry.projects.iter().find(|p| p.hash == hash) else {
        return false;
    };
    let warm_window_secs = Config::load(&paths.config_file())
        .map(|c| c.watcher.warm_window_secs)
        .unwrap_or_else(|_| nexus_core::WatcherConfig::default().warm_window_secs);
    !entry.is_warm(now_unix(), warm_window_secs)
}

pub fn tool_definitions() -> Value {
    json!([
        {
            "name": "index_repository",
            "description": "Build or rebuild the knowledge graph for a directory. Run this before other tools on a project you haven't indexed yet.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo_path": { "type": "string" } },
                "required": ["repo_path"]
            }
        },
        {
            "name": "search_graph",
            "description": "Structural search over indexed symbols by name substring - functions/types and, for markdown docs, heading sections. No embeddings required.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "pattern": { "type": "string" },
                    "limit": { "type": "integer", "default": 20 }
                },
                "required": ["repo_path", "pattern"]
            }
        },
        {
            "name": "trace_call_path",
            "description": "BFS over the CALLS graph to find callers/callees of a function. Resolution is name-based, not import-aware - see README.md for per-language call-graph quality and resolution caveats. Response is capped; check `total_nodes` vs `shown` for truncation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "name": { "type": "string" },
                    "direction": { "type": "string", "enum": ["inbound", "outbound"], "default": "outbound" },
                    "depth": { "type": "integer", "default": 3 },
                    "limit": { "type": "integer", "default": 100 }
                },
                "required": ["repo_path", "name"]
            }
        },
        {
            "name": "get_file_context",
            "description": "Read a file, optionally a specific line range, from an indexed project. With no range and full=false (default), returns only the first 300 lines with a truncation note - pass an explicit range or full=true for the rest.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "file": { "type": "string" },
                    "start_line": { "type": "integer" },
                    "end_line": { "type": "integer" },
                    "full": { "type": "boolean", "default": false }
                },
                "required": ["repo_path", "file"]
            }
        },
        {
            "name": "get_architecture",
            "description": "Summarize an indexed project: total node/edge counts and the busiest files by definition count (code functions/types and markdown heading sections counted together).",
            "inputSchema": {
                "type": "object",
                "properties": { "repo_path": { "type": "string" } },
                "required": ["repo_path"]
            }
        },
        {
            "name": "detect_changes",
            "description": "Map uncommitted git changes to affected graph symbols (functions/types whose line range overlaps a diff hunk).",
            "inputSchema": {
                "type": "object",
                "properties": { "repo_path": { "type": "string" } },
                "required": ["repo_path"]
            }
        },
        {
            "name": "delete_project",
            "description": "Remove a project's indexed data (graph + registry entry). Does not touch the source directory.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo_path": { "type": "string" } },
                "required": ["repo_path"]
            }
        },
        {
            "name": "detect_dead_code",
            "description": "Functions with no inbound CALLS edge (excluding main). High false-positive rate expected - see README.md. Response is capped at `limit` (default 50) with a `total_flagged` count.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "limit": { "type": "integer", "default": 50 }
                },
                "required": ["repo_path"]
            }
        },
        {
            "name": "search_code",
            "description": "Grep-like full-text search over indexed file content (not symbol names) via SQLite FTS5. Covers files tree-sitter parses (one of the 11 supported languages) plus markdown docs (.md/.markdown) - other file types aren't indexed yet. Query is matched as a literal phrase.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "default": 20 }
                },
                "required": ["repo_path", "query"]
            }
        },
        {
            "name": "query_planner",
            "description": "Picks the cheapest retrieval strategy for a query (file read, symbol search, or semantic/keyword fallback) instead of the agent guessing. Returns which strategy was used alongside the results - see README.md for the exact routing rules.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "query": { "type": "string" },
                    "file": { "type": "string" },
                    "start_line": { "type": "integer" },
                    "end_line": { "type": "integer" }
                },
                "required": ["repo_path", "query"]
            }
        },
        {
            "name": "query_graph",
            "description": "Minimal ad-hoc graph query - one pattern shape only: MATCH (a:Kind)-[:EDGE_KIND]->(b:Kind) [WHERE ...] RETURN a|b. See README.md for the full Kind/edge vocabulary.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "default": 20 }
                },
                "required": ["repo_path", "query"]
            }
        },
        {
            "name": "search_codebase",
            "description": "Semantic search via cosine similarity over embedded Function/Type nodes and markdown sections. Requires `embeddings.enabled = true` and a reachable endpoint/model - errors with an actionable reason otherwise.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo_path": { "type": "string" }, "query": { "type": "string" }, "limit": { "type": "integer" } },
                "required": ["repo_path", "query"]
            }
        },
        {
            "name": "query_memory",
            "description": "RAG-style retrieval over indexed content - currently the same ranked semantic search as search_codebase (richer retrieval, e.g. pulling full surrounding context per hit, is a future enhancement). Same requirements and fallback guidance as search_codebase.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo_path": { "type": "string" }, "query": { "type": "string" }, "limit": { "type": "integer" } },
                "required": ["repo_path", "query"]
            }
        }
    ])
}

/// Source of truth for every tool `tool_definitions()` can return - kept in
/// sync via `full_preset_matches_all_tool_names` below, so a 14th tool added
/// to `tool_definitions()` without also being added to a preset fails a test
/// instead of silently vanishing from every preset.
const ALL_TOOL_NAMES: &[&str] = &[
    "index_repository",
    "search_graph",
    "trace_call_path",
    "get_file_context",
    "get_architecture",
    "detect_changes",
    "delete_project",
    "detect_dead_code",
    "search_code",
    "query_planner",
    "query_graph",
    "search_codebase",
    "query_memory",
];

/// A read-heavy coding session's core loop: bootstrap the index, then read
/// and trace code. Every other tool needs one of these to have run first.
const MINIMAL_TOOLS: &[&str] = &[
    "index_repository",
    "search_code",
    "get_file_context",
    "get_architecture",
    "trace_call_path",
];

/// Rounds out `MINIMAL_TOOLS` with the rest of the everyday-useful,
/// non-destructive, non-embeddings-gated tools.
const STANDARD_EXTRA_TOOLS: &[&str] = &[
    "search_graph",
    "detect_changes",
    "detect_dead_code",
    "query_planner",
];

/// Admin/destructive (`delete_project`) or embeddings-gated
/// (`search_codebase`, `query_memory`) tools, plus the niche ad-hoc
/// `query_graph` DSL - opt-in via `preset = "full"` or an explicit
/// `enabled` list, not advertised by default.
const FULL_EXTRA_TOOLS: &[&str] = &[
    "delete_project",
    "query_graph",
    "search_codebase",
    "query_memory",
];

fn resolved_tool_names(config: &Config) -> std::collections::HashSet<&'static str> {
    if let Some(explicit) = &config.tools.enabled {
        return ALL_TOOL_NAMES
            .iter()
            .copied()
            .filter(|name| explicit.iter().any(|e| e == name))
            .collect();
    }
    match config.tools.preset {
        nexus_core::ToolsPreset::Minimal => MINIMAL_TOOLS.iter().copied().collect(),
        nexus_core::ToolsPreset::Standard => MINIMAL_TOOLS
            .iter()
            .chain(STANDARD_EXTRA_TOOLS)
            .copied()
            .collect(),
        nexus_core::ToolsPreset::Full => MINIMAL_TOOLS
            .iter()
            .chain(STANDARD_EXTRA_TOOLS)
            .chain(FULL_EXTRA_TOOLS)
            .copied()
            .collect(),
    }
}

/// `tools/list`'s entry point - filters `tool_definitions()` down to the
/// resolved enabled-set so a session only pays the schema-token cost for
/// tools it can actually use. See change_proposal.md.
pub fn enabled_tool_definitions(config: &Config) -> Value {
    let enabled = resolved_tool_names(config);
    let all = tool_definitions();
    Value::Array(
        all.as_array()
            .expect("tool_definitions() always returns a JSON array")
            .iter()
            .filter(|t| t["name"].as_str().is_some_and(|n| enabled.contains(n)))
            .cloned()
            .collect(),
    )
}

pub fn call(params: Value) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or_else(|| anyhow!("missing tool name"))?;
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);

    // Best-effort "this project is actually being used" signal, distinct
    // from last_indexed_unix which only moves on a reindex - lets the
    // registry answer "which of these have I actually touched lately" for
    // someone who's indexed many projects over time. delete_project makes
    // this moot (the entry is gone right after) so it's skipped there.
    if name != "delete_project" {
        if let Some(repo_path) = args.get("repo_path").and_then(|v| v.as_str()) {
            let repo_path = std::path::Path::new(repo_path);
            // The watcher (watcher::sync_watches) stops actively watching a
            // project once it's gone cold - see ProjectEntry::is_warm - so a
            // query returning to one after a gap could otherwise silently
            // answer from a stale index. Catch it up here, synchronously,
            // before touch_queried below marks it warm again. Checked
            // *before* touch_queried, since that call is about to overwrite
            // the very timestamp this staleness check reads.
            // index_repository is excluded: it already unconditionally
            // reindexes, so catching up first here would just double the work.
            if name != "index_repository" && is_cold(repo_path) {
                let reindex_start = std::time::Instant::now();
                let success = match index_project(repo_path) {
                    Ok(_) => true,
                    Err(err) => {
                        tracing::warn!(project = %repo_path.display(), error = %err, "catch-up reindex of a cold project failed");
                        false
                    }
                };
                index::record_auto_reindex(
                    repo_path,
                    reindex_start.elapsed().as_millis() as u64,
                    success,
                );
            }
            index::touch_queried(repo_path);
        }
    }

    let call_start = std::time::Instant::now();
    let result = match name {
        "index_repository" => index_repository(args),
        "delete_project" => delete_project(args),
        "search_graph" => search_graph(args),
        "trace_call_path" => trace_call_path(args),
        "get_file_context" => get_file_context(args),
        "get_architecture" => get_architecture(args),
        "detect_changes" => detect_changes(args),
        "detect_dead_code" => detect_dead_code(args),
        "search_code" => search_code(args),
        "query_graph" => query_graph(args),
        "query_planner" => query_planner(args),
        "search_codebase" | "query_memory" => semantic_search_tool(args),
        _ => bail!("unknown tool: {name}"),
    };

    // Phase 1 usage observability: lifetime aggregate counters only (calls,
    // errors, latency, output size), no per-call log - see
    // nexus_core::stats for why. Best-effort, never fails the call itself.
    {
        let latency_ms = call_start.elapsed().as_millis() as u64;
        let (is_error, output_bytes) = match &result {
            Ok(text) => (false, text.len() as u64),
            Err(err) => (true, err.to_string().len() as u64),
        };
        nexus_core::stats::record_mcp_call(
            &Paths::resolve().usage_stats_file(),
            name,
            latency_ms,
            output_bytes,
            is_error,
        );
    }

    match result {
        Ok(text) => {
            Ok(json!({ "content": [ { "type": "text", "text": text } ], "isError": false }))
        }
        Err(err) => Ok(
            json!({ "content": [ { "type": "text", "text": err.to_string() } ], "isError": true }),
        ),
    }
}

/// `search_codebase`/`query_memory` share this: check the policy, and
/// either return a specific, actionable reason it can't run right now, or
/// actually attempt the semantic search and translate a live failure into
/// an equally actionable message. Every message points back at the
/// structural tools that work regardless, so the calling agent has
/// somewhere to go rather than just hitting a dead end.
fn semantic_search_tool(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'query' argument"))?;
    let limit = clamp_limit(args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as u32);

    let config = Config::load(&Paths::resolve().config_file())?;
    match config.embeddings_policy() {
        nexus_core::EmbeddingsPolicy::NotConfigured => bail!(
            "embeddings backend not configured - structural tools (search_graph, trace_call_path, \
             get_architecture, search_code, query_planner) work without one."
        ),
        nexus_core::EmbeddingsPolicy::Disabled => bail!(
            "an embeddings endpoint and model are configured but embeddings.enabled = false - set \
             it to true in config.toml (or via the GUI's Config tab) to turn semantic search on. \
             Structural tools (search_graph, trace_call_path, get_architecture, search_code, \
             query_planner) work without it."
        ),
        nexus_core::EmbeddingsPolicy::RemoteBlocked => bail!(
            "embeddings endpoint {} is not loopback/private, and allow_remote isn't set - \
             refusing to send code to it. Set embeddings.allow_remote = true in config.toml \
             if this is intentional.",
            config.embeddings.endpoint.as_deref().unwrap_or("?")
        ),
        nexus_core::EmbeddingsPolicy::Allowed => {}
    }

    match index::semantic_search(&repo_path, &config.embeddings, query, limit) {
        Ok(hits) => Ok(serde_json::to_string_pretty(&semantic_hits_to_json(&hits))?),
        Err(err) => Err(anyhow!(
            "embeddings endpoint is configured and enabled, but this request just failed: {err}. \
             This isn't retried automatically - try search_graph, search_code, or query_planner \
             instead while the endpoint is unavailable."
        )),
    }
}

fn semantic_hits_to_json(hits: &[index::SemanticHit]) -> Value {
    json!(hits
        .iter()
        .map(|hit| json!({
            "kind": format!("{:?}", hit.node.kind),
            "name": hit.node.name,
            "qualified_name": hit.node.qualified_name,
            "file": hit.node.file_path,
            "start_line": hit.node.start_line,
            "end_line": hit.node.end_line,
            "score": hit.score,
            "chunk_text": hit.chunk_text,
        }))
        .collect::<Vec<_>>())
}

fn repo_path_arg(args: &Value) -> Result<PathBuf> {
    let raw = args
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    Ok(PathBuf::from(raw))
}

fn records_to_json(records: &[NodeRecord]) -> Value {
    json!(records
        .iter()
        .map(|n| json!({
            "kind": format!("{:?}", n.kind),
            "name": n.name,
            "qualified_name": n.qualified_name,
            "file": n.file_path,
            "start_line": n.start_line,
            "end_line": n.end_line,
        }))
        .collect::<Vec<_>>())
}

fn index_repository(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let stats = index_project(&repo_path)?;
    Ok(serde_json::to_string_pretty(&json!({
        "status": "indexed",
        "files_indexed": stats.files_indexed,
        "nodes": stats.nodes,
        "edges": stats.edges,
        "embeddings_status": stats.embeddings_status
    }))?)
}

fn delete_project(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    index::delete_project(&repo_path)?;
    Ok(serde_json::to_string_pretty(
        &json!({ "status": "deleted" }),
    )?)
}

fn search_graph(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'pattern' argument"))?;
    let limit = clamp_limit(args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32);

    let store = index::open_store(&repo_path)?;
    let results = store.search_by_name(pattern, limit)?;
    Ok(serde_json::to_string_pretty(&records_to_json(&results))?)
}

fn trace_call_path(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'name' argument"))?;
    let direction = match args.get("direction").and_then(|v| v.as_str()) {
        Some("inbound") => Direction::Inbound,
        _ => Direction::Outbound,
    };
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as u32;
    let limit =
        clamp_limit(args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as u32) as usize;

    let store = index::open_store(&repo_path)?;
    let results = store.trace_calls(name, direction, depth)?;
    // Unbounded BFS output on a high-fan-out function can return an
    // arbitrarily large node set - same total/shown truncation pattern as
    // detect_dead_code, so the response stays honest about what's hidden.
    let total = results.len();
    let shown: Vec<_> = results.into_iter().take(limit).collect();
    Ok(serde_json::to_string_pretty(&json!({
        "total_nodes": total,
        "shown": shown.len(),
        "nodes": records_to_json(&shown)
    }))?)
}

fn get_file_context(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let file = args
        .get("file")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'file' argument"))?;
    let start = args
        .get("start_line")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let end = args
        .get("end_line")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
    index::get_file_context(&repo_path, file, start, end, full)
}

fn last_indexed_unix(repo_path: &std::path::Path) -> u64 {
    let paths = Paths::resolve();
    let hash = project_hash(repo_path);
    Registry::load(&paths.registry_file())
        .projects
        .into_iter()
        .find(|p| p.hash == hash)
        .map(|p| p.last_indexed_unix)
        .unwrap_or(0)
}

fn get_architecture(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let cache_key = format!("get_architecture:{}", project_hash(&repo_path));

    let value = crate::cache::get_or_compute(&cache_key, last_indexed_unix(&repo_path), || {
        let summary = index::get_architecture(&repo_path)?;
        Ok(json!({
            "total_nodes": summary.total_nodes,
            "total_edges": summary.total_edges,
            "busiest_files": summary.busiest_files.into_iter()
                .map(|(file, count)| json!({ "file": file, "definitions": count }))
                .collect::<Vec<_>>(),
            "language_breakdown": summary.language_breakdown.into_iter()
                .map(|(ext, count)| json!({ "extension": ext, "files": count }))
                .collect::<Vec<_>>()
        }))
    })?;

    Ok(serde_json::to_string_pretty(&value)?)
}

fn query_planner(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'query' argument"))?;
    let file = args.get("file").and_then(|v| v.as_str());
    let start = args
        .get("start_line")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let end = args
        .get("end_line")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);

    let plan = index::plan_query(&repo_path, query, file, start, end)?;

    let result = if let Some(text) = plan.file_content {
        json!(text)
    } else {
        records_to_json(&plan.records)
    };

    Ok(serde_json::to_string_pretty(&json!({
        "strategy": plan.strategy,
        "note": plan.note,
        "embeddings_policy": plan.embeddings_policy.map(|p| format!("{p:?}")),
        "result": result
    }))?)
}

fn detect_dead_code(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let limit =
        clamp_limit(args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as u32) as usize;
    let dead = index::detect_dead_code(&repo_path)?;
    // Unbounded on a real project this size flagged ~40% of all indexed
    // symbols as "dead" (mostly false positives - see the tool description's
    // name-resolution caveat) and blew past 99K chars in one response,
    // costing more tokens than the caller would have spent just grepping.
    // Truncating with an explicit total keeps the response honest about
    // what's being hidden rather than silently dropping it.
    let total = dead.len();
    let shown: Vec<_> = dead.into_iter().take(limit).collect();
    Ok(serde_json::to_string_pretty(&json!({
        "total_flagged": total,
        "shown": shown.len(),
        "note": "high false-positive rate is expected here - see this tool's description",
        "functions": records_to_json(&shown)
    }))?)
}

fn search_code(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'query' argument"))?;
    let limit = clamp_limit(args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32);

    let hits = index::search_code(&repo_path, query, limit)?;
    Ok(serde_json::to_string_pretty(&json!(hits
        .iter()
        .map(|h| json!({ "file": h.file_path, "snippet": h.snippet }))
        .collect::<Vec<_>>()))?)
}

fn query_graph(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'query' argument"))?;
    let limit = clamp_limit(args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32);

    let results = nexus_index::run_cypher_query(&repo_path, query, limit)?;
    Ok(serde_json::to_string_pretty(&records_to_json(&results))?)
}

fn detect_changes(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let affected = index::detect_changes(&repo_path)?;
    Ok(serde_json::to_string_pretty(&records_to_json(&affected))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::ToolsPreset;

    fn tool_names(defs: &Value) -> std::collections::HashSet<String> {
        defs.as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn clamp_limit_passes_through_requests_at_or_below_the_max() {
        assert_eq!(clamp_limit(1), 1);
        assert_eq!(clamp_limit(SERVER_MAX_LIMIT), SERVER_MAX_LIMIT);
    }

    #[test]
    fn clamp_limit_caps_requests_above_the_max() {
        assert_eq!(clamp_limit(SERVER_MAX_LIMIT + 1), SERVER_MAX_LIMIT);
        assert_eq!(clamp_limit(100_000), SERVER_MAX_LIMIT);
    }

    #[test]
    fn full_preset_matches_all_tool_definitions() {
        let config = Config {
            tools: nexus_core::ToolsConfig {
                preset: ToolsPreset::Full,
                enabled: None,
            },
            ..Default::default()
        };
        let filtered = tool_names(&enabled_tool_definitions(&config));
        let all = tool_names(&tool_definitions());
        assert_eq!(filtered, all);
        assert_eq!(all.len(), ALL_TOOL_NAMES.len());
    }

    #[test]
    fn minimal_and_standard_presets_are_subsets_of_full() {
        let all: std::collections::HashSet<_> = ALL_TOOL_NAMES.iter().copied().collect();
        let minimal: std::collections::HashSet<_> = MINIMAL_TOOLS.iter().copied().collect();
        let standard: std::collections::HashSet<_> = MINIMAL_TOOLS
            .iter()
            .chain(STANDARD_EXTRA_TOOLS)
            .copied()
            .collect();
        assert!(minimal.is_subset(&standard));
        assert!(standard.is_subset(&all));
    }

    #[test]
    fn minimal_standard_extra_and_full_extra_partition_all_tool_names_exactly() {
        let reconstructed: std::collections::HashSet<_> = MINIMAL_TOOLS
            .iter()
            .chain(STANDARD_EXTRA_TOOLS)
            .chain(FULL_EXTRA_TOOLS)
            .copied()
            .collect();
        let all: std::collections::HashSet<_> = ALL_TOOL_NAMES.iter().copied().collect();
        assert_eq!(
            reconstructed, all,
            "a tool was added to tool_definitions() without being added to a preset, or vice versa"
        );
    }

    #[test]
    fn default_config_resolves_to_standard_nine_tools() {
        let config = Config::default();
        let filtered = tool_names(&enabled_tool_definitions(&config));
        assert_eq!(filtered.len(), 9);
        assert!(filtered.contains("search_code"));
        assert!(!filtered.contains("delete_project"));
        assert!(!filtered.contains("search_codebase"));
    }

    #[test]
    fn minimal_preset_resolves_to_exactly_five_tools() {
        let config = Config {
            tools: nexus_core::ToolsConfig {
                preset: ToolsPreset::Minimal,
                enabled: None,
            },
            ..Default::default()
        };
        let filtered = tool_names(&enabled_tool_definitions(&config));
        assert_eq!(filtered.len(), 5);
        assert_eq!(
            filtered,
            MINIMAL_TOOLS
                .iter()
                .map(|s| s.to_string())
                .collect::<std::collections::HashSet<_>>()
        );
    }

    #[test]
    fn explicit_enabled_list_overrides_preset() {
        let config = Config {
            tools: nexus_core::ToolsConfig {
                preset: ToolsPreset::Standard,
                enabled: Some(vec![
                    "search_codebase".to_string(),
                    "query_graph".to_string(),
                ]),
            },
            ..Default::default()
        };
        let filtered = tool_names(&enabled_tool_definitions(&config));
        assert_eq!(filtered.len(), 2);
        assert!(filtered.contains("search_codebase"));
        assert!(filtered.contains("query_graph"));
        assert!(!filtered.contains("search_code"));
    }

    #[test]
    fn unknown_name_in_enabled_list_is_silently_dropped() {
        let config = Config {
            tools: nexus_core::ToolsConfig {
                preset: ToolsPreset::Standard,
                enabled: Some(vec![
                    "search_code".to_string(),
                    "not_a_real_tool".to_string(),
                ]),
            },
            ..Default::default()
        };
        let filtered = tool_names(&enabled_tool_definitions(&config));
        assert_eq!(filtered.len(), 1);
        assert!(filtered.contains("search_code"));
    }
}
