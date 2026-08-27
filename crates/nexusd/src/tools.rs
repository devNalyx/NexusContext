use anyhow::{anyhow, bail, Result};
use nexus_core::{project_hash, Config, Paths, Registry, WatcherConfig};
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

/// Hard ceiling on any caller-supplied graph traversal `depth`. Unlike
/// `limit` (a result-count cap), `depth` controls how far a call-graph walk
/// fans out - cost grows combinatorially with it on a densely-connected
/// graph, so an unbounded value from a bad/adversarial agent call can blow
/// up latency and memory even with `limit` already capped. See issue #58.
pub(crate) const SERVER_MAX_DEPTH: u32 = 10;

pub(crate) fn clamp_depth(requested: u32) -> u32 {
    requested.min(SERVER_MAX_DEPTH)
}

/// Per-tool call/error/byte counters for `get_session_usage`, scoped to
/// *this process* rather than persisted like `nexus_core::stats`'s
/// lifetime-aggregate `usage_stats.json` - `nexusd mcp` is spawned fresh
/// per agent session, so in-memory-only naturally gives "this session"
/// instead of "ever" without needing an explicit session id or a reset
/// call anywhere.
struct SessionToolStats {
    call_count: u64,
    error_count: u64,
    output_bytes: u64,
}

/// Static allow-list backing `get_session_usage`'s "reads avoided"
/// counterfactual (issues #11, #40 follow-up 1) - a tool counts here only
/// if a successful call plausibly substituted for a manual file
/// read/grep the calling agent would otherwise have done. Deliberately
/// conservative and name-based rather than inspecting each call's
/// args/result for a finer-grained split (e.g. `get_file_context` ranged
/// vs full) - a full-file `get_file_context` still substitutes a manual
/// read, so it counts too; the false-positive-prone or scan-shaped tools
/// below are excluded outright instead of trying to weight them.
///
/// Deliberately excluded, with the caller left to judge each themselves:
/// `search_code` (a scan's hits still typically need a real read afterward -
/// the hit isn't the substitute), `detect_dead_code` (own tool description
/// documents a high false-positive
/// rate - not a confident "avoided" signal), `query_graph` (an ad-hoc power
/// query, not a stand-in for a plain read), `index_repository` (setup cost,
/// not a saving), `delete_project`/`get_session_usage` (admin/meta, not
/// reads at all).
const READS_AVOIDED_TOOLS: &[&str] = &[
    "get_file_context",
    "trace_call_path",
    "search_graph",
    "get_architecture",
    "detect_changes",
    "query_planner",
];

static SESSION_USAGE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, SessionToolStats>>,
> = std::sync::OnceLock::new();
static SESSION_STARTED_UNIX: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// First call in this process wins and is cheap to be a second or two off -
/// this is "roughly when did this session start," not an audit timestamp.
fn session_started_unix() -> u64 {
    *SESSION_STARTED_UNIX.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    })
}

fn record_session_call(name: &str, output_bytes: u64, is_error: bool) {
    let map = SESSION_USAGE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut map = map.lock().unwrap();
    let entry = map.entry(name.to_string()).or_insert(SessionToolStats {
        call_count: 0,
        error_count: 0,
        output_bytes: 0,
    });
    entry.call_count += 1;
    if is_error {
        entry.error_count += 1;
    }
    entry.output_bytes += output_bytes;
}

/// Pure summary over an already-collected stats map, factored out of
/// `get_session_usage` so it's testable against a small local map instead of
/// the process-wide `SESSION_USAGE` singleton (which real tool dispatch also
/// writes to, making exact-count assertions against it flaky across the rest
/// of this module's tests). See `READS_AVOIDED_TOOLS` for the allow-list and
/// `get_session_usage` for why only successful calls count.
fn reads_avoided_summary(map: &std::collections::HashMap<String, SessionToolStats>) -> (u64, u64) {
    let qualifying = || {
        map.iter()
            .filter(|(name, _)| READS_AVOIDED_TOOLS.contains(&name.as_str()))
    };
    let count = qualifying()
        .map(|(_, s)| s.call_count - s.error_count)
        .sum();
    let bytes = qualifying().map(|(_, s)| s.output_bytes).sum();
    (count, bytes)
}

/// A plain character-count heuristic (roughly 4 bytes/token for English-ish
/// code and prose), not a real tokenizer - nexusd has no reason to carry a
/// tokenizer dependency just for a ballpark. Good enough for "roughly how
/// much of my budget did this cost," not for anything that needs to match a
/// specific model's actual count.
fn estimate_tokens(bytes: u64) -> u64 {
    bytes / 4
}

pub fn tool_definitions() -> Value {
    json!([
        {
            "name": "index_repository",
            "description": "Build or rebuild the knowledge graph for a directory. Run this before other tools on a project you haven't indexed yet. `deep: true` also runs LSP-resolved-symbol enrichment if configured - see README.md.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "deep": { "type": "boolean", "default": false }
                },
                "required": ["repo_path"]
            }
        },
        {
            "name": "search_graph",
            "description": "Structural search over indexed symbols by name substring - functions/types and, for markdown docs, heading sections.",
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
            "description": "BFS over the CALLS graph to find callers/callees of a function. Resolution is name-based, not import-aware - see README.md for per-language call-graph quality and resolution caveats. Each returned node carries `provenance`/`resolution`/`confidence` fields: `tree-sitter`/`name-match`/`heuristic` for a plain name-based CALLS edge, or `lsp`/`semantic-symbol`/`exact` where LSP enrichment (rust-analyzer) verified the reference. Response is capped; check `total_nodes` vs `shown` for truncation.",
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
            "description": "Map uncommitted git changes to affected graph symbols (functions/types whose line range overlaps a diff hunk). Optional `blast_radius=true` additionally walks inbound callers (direct and transitive) of each changed function via the same BFS `trace_call_path` uses, up to `depth` hops - each transitive node carries the same `provenance`/`resolution`/`confidence` fields `trace_call_path` returns. Default (`blast_radius=false`) response and cost are unchanged from the plain symbol list.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "blast_radius": { "type": "boolean", "default": false },
                    "depth": { "type": "integer", "default": 3 },
                    "limit": { "type": "integer", "default": 100 }
                },
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
            "description": "Functions with no inbound CALLS edge (excluding main). High false-positive rate expected - see README.md. Response is capped at `limit` (default 50) with a `total_flagged` count. Optional `path_prefix` restricts results to a subdirectory (or exact file) of the repo, e.g. \"pkg/events\" - useful on monorepos to exclude vendored/generated directories from the candidate set.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "limit": { "type": "integer", "default": 50 },
                    "path_prefix": { "type": "string" }
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
            "name": "get_session_usage",
            "description": "How much data NexusContext has sent back in this session so far, per tool, with a rough token estimate - useful for keeping an eye on your own context budget. Scoped to this session only (since this MCP connection started, not lifetime totals across every session), and counts only NexusContext's own MCP responses - not your system prompt, conversation history, or any other tool's output.",
            "inputSchema": {
                "type": "object",
                "properties": {}
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
    "get_session_usage",
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
/// non-destructive tools.
const STANDARD_EXTRA_TOOLS: &[&str] = &[
    "search_graph",
    "detect_changes",
    "detect_dead_code",
    "query_planner",
    "get_session_usage",
];

/// Admin/destructive (`delete_project`) tools, plus the niche ad-hoc
/// `query_graph` DSL - opt-in via `preset = "full"` or an explicit
/// `enabled` list, not advertised by default.
const FULL_EXTRA_TOOLS: &[&str] = &["delete_project", "query_graph"];

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
    // someone who's indexed many projects over time, and catches a cold
    // project up with a synchronous reindex first if the watcher had
    // stopped watching it - see nexus_index::touch_and_catchup, the one
    // shared entry point this, control.rs, and the CLI all call.
    // delete_project is excluded entirely: the entry is gone right after,
    // so there's nothing to mark warm. index_repository still marks the
    // project warm (it's real usage too) but skips the catch-up check -
    // it's about to unconditionally reindex itself, so checking staleness
    // first would just double the work.
    if name != "delete_project" {
        if let Some(repo_path) = args.get("repo_path").and_then(|v| v.as_str()) {
            let repo_path = std::path::Path::new(repo_path);
            if name == "index_repository" {
                index::touch_queried(repo_path);
            } else {
                index::touch_and_catchup(repo_path);
            }
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
        "get_session_usage" => get_session_usage(args),
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
        // Session-scoped counterpart to the lifetime file above - lives only
        // in this process's memory, so it naturally resets every time
        // `nexusd mcp` is spawned fresh for a new session. Backs
        // `get_session_usage`: "how much has NexusContext sent me this
        // session" is a different, agent-relevant question from "how much
        // has it ever sent anyone," which is all the persisted file answers.
        record_session_call(name, output_bytes, is_error);
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

/// Per-node provenance/confidence schema for issue #59: distinguishes a
/// `CALLS` edge (tree-sitter's static, name-based pass - ambiguous across
/// files/overloads) from a `CALLS_RESOLVED` edge (issue #10's LSP
/// enrichment, backed by rust-analyzer's semantic symbol resolution) so an
/// MCP caller doesn't have to treat every hop in a `trace_call_path` result
/// as equally certain. See `EdgeKind::CallsResolved` in `graph.rs` and
/// `GraphStore::trace_calls`'s doc comment for how a node is tagged when it
/// could in principle be reached via both kinds.
fn edge_kind_provenance(kind: index::EdgeKind) -> Value {
    match kind {
        index::EdgeKind::CallsResolved => json!({
            "provenance": "lsp",
            "resolution": "semantic-symbol",
            "confidence": "exact",
        }),
        // Calls, and any other non-resolved kind reaching this path -
        // conservative default rather than a panic on a future EdgeKind
        // variant this function hasn't been told about yet.
        _ => json!({
            "provenance": "tree-sitter",
            "resolution": "name-match",
            "confidence": "heuristic",
        }),
    }
}

fn traced_nodes_to_json(traced: &[index::TracedNode]) -> Value {
    json!(traced
        .iter()
        .map(|t| {
            let mut obj = json!({
                "kind": format!("{:?}", t.node.kind),
                "name": t.node.name,
                "qualified_name": t.node.qualified_name,
                "file": t.node.file_path,
                "start_line": t.node.start_line,
                "end_line": t.node.end_line,
            });
            if let Value::Object(ref mut map) = obj {
                if let Value::Object(prov) = edge_kind_provenance(t.edge_kind) {
                    map.extend(prov);
                }
            }
            obj
        })
        .collect::<Vec<_>>())
}

fn index_repository(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    // #10: opt-in only, and only via this explicit argument - the
    // watcher's ordinary auto-reindex-on-file-change loop always calls the
    // plain (non-deep) path regardless of this config, so enabling
    // `[lsp]` never adds latency there.
    let deep = args.get("deep").and_then(|v| v.as_bool()).unwrap_or(false);
    let stats = if deep {
        index::index_project_deep(&repo_path)?
    } else {
        index_project(&repo_path)?
    };
    Ok(serde_json::to_string_pretty(&json!({
        "status": "indexed",
        "files_indexed": stats.files_indexed,
        "nodes": stats.nodes,
        "edges": stats.edges,
        "lsp_enrichment": stats.lsp_enrichment,
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
    let depth = clamp_depth(args.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as u32);
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
        "nodes": traced_nodes_to_json(&shown)
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

/// Takes no arguments deliberately - this reports on the session itself,
/// not on any one project, so there's nothing to scope it by.
fn get_session_usage(_args: Value) -> Result<String> {
    let map = SESSION_USAGE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let map = map.lock().unwrap();

    let mut by_tool: Vec<Value> = map
        .iter()
        .map(|(name, s)| {
            json!({
                "name": name,
                "call_count": s.call_count,
                "error_count": s.error_count,
                "output_bytes": s.output_bytes,
                "estimated_tokens": estimate_tokens(s.output_bytes),
            })
        })
        .collect();
    // Biggest contributor first - that's the one worth knowing about.
    by_tool.sort_by(|a, b| {
        b["output_bytes"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&a["output_bytes"].as_u64().unwrap_or(0))
    });

    let total_calls: u64 = map.values().map(|s| s.call_count).sum();
    let total_output_bytes: u64 = map.values().map(|s| s.output_bytes).sum();

    let (reads_avoided, bytes_avoided) = reads_avoided_summary(&map);

    // #40 follow-up 1: the fixed per-session cost every tool schema (name +
    // description + params) adds to the context regardless of whether any
    // tool is ever called - previously acknowledged in README's Phase 21/22
    // notes but never actually measured. `enabled_tool_definitions` isn't
    // reachable from here without the resolved config/preset this call
    // doesn't have, so this reports the unfiltered `tool_definitions()` size
    // (the ceiling every preset is a subset of), noted as such below.
    let schema_tax_bytes = tool_definitions().to_string().len() as u64;

    Ok(serde_json::to_string_pretty(&json!({
        "session_started_unix": session_started_unix(),
        "total_calls": total_calls,
        "total_output_bytes": total_output_bytes,
        "total_estimated_tokens": estimate_tokens(total_output_bytes),
        "by_tool": by_tool,
        "schema_tax": {
            "bytes": schema_tax_bytes,
            "estimated_tokens": estimate_tokens(schema_tax_bytes),
            "note": "Fixed cost of every tool's name+description+params schema, paid once per session regardless of tool usage - this is the full unfiltered tool_definitions() size (config.tools.preset/enabled trims what's actually sent, so a Minimal/Standard session's real tax is <= this number, not exactly it).",
        },
        "reads_avoided": {
            "count": reads_avoided,
            "bytes": bytes_avoided,
            "estimated_tokens": estimate_tokens(bytes_avoided),
            "counted_tools": READS_AVOIDED_TOOLS,
            "note": "Successful calls to tools that plausibly substituted a manual file read/grep, per an explicit, conservative allow-list (see READS_AVOIDED_TOOLS in source) - excludes raw scans (search_code, whose hits still typically need a real read afterward), detect_dead_code (documented high false-positive rate), and admin/meta tools. 'bytes'/'count' are measured facts about what NexusContext returned, not a token or dollar estimate of what reading the files by hand would have cost - treat this as a floor on savings, not a precise counterfactual.",
        },
        "note": "Scoped to this session only (this MCP connection, since it started) - not lifetime totals. Counts only NexusContext's own MCP response bytes, not your system prompt, conversation history, or any other tool's output, and NexusContext doesn't know your model's actual context limit or real tokenizer - estimated_tokens is a bytes/4 approximation, not an exact count.",
    }))?)
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

/// #40 follow-up 3: a cheap, in-band staleness signal so an agent can
/// decide reindex-vs-trust without a separate call - reuses the same
/// warm/cold gate the background watcher already computes
/// (`ProjectEntry::is_warm`), rather than inventing a second notion of
/// freshness. Does the real `Paths::resolve()`/`Registry::load` I/O; the
/// actual shaping logic is factored into `index_freshness_json_from` below
/// so it's testable without a real registry file.
fn index_freshness_json(repo_path: &std::path::Path, now_unix: u64) -> Value {
    let paths = Paths::resolve();
    let hash = project_hash(repo_path);
    let entry = Registry::load(&paths.registry_file())
        .projects
        .into_iter()
        .find(|p| p.hash == hash);
    let warm_window_secs = Config::load(&paths.config_file())
        .map(|c| c.watcher.warm_window_secs)
        .unwrap_or_else(|_| WatcherConfig::default().warm_window_secs);
    index_freshness_json_from(entry.as_ref(), now_unix, warm_window_secs)
}

fn index_freshness_json_from(
    entry: Option<&nexus_core::ProjectEntry>,
    now_unix: u64,
    warm_window_secs: u64,
) -> Value {
    let Some(entry) = entry else {
        return json!({ "indexed": false });
    };
    json!({
        "indexed": true,
        "last_indexed_unix": entry.last_indexed_unix,
        "seconds_since_indexed": now_unix.saturating_sub(entry.last_indexed_unix),
        "warm": entry.is_warm(now_unix, warm_window_secs),
        "note": "warm means the background watcher is actively keeping this project's graph in sync with the working tree; cold means it isn't (yet) and the graph may be behind - a tool result under a cold project is worth verifying, not treating as ground truth",
    })
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
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

    // Freshness is merged in *after* the cache lookup, not inside the cached
    // closure above - `warm`/`seconds_since_indexed` both move with wall-clock
    // time independent of a reindex, and the cache is keyed (and only
    // invalidated) on `last_indexed_unix`, so baking either into the cached
    // value would freeze them at whatever they were on the first cache miss.
    let mut value = value;
    if let Value::Object(ref mut map) = value {
        map.insert(
            "index_freshness".to_string(),
            index_freshness_json(&repo_path, now_unix()),
        );
    }

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
        "result": result,
        "index_freshness": index_freshness_json(&repo_path, now_unix()),
    }))?)
}

fn detect_dead_code(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let limit =
        clamp_limit(args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as u32) as usize;
    let path_prefix = args.get("path_prefix").and_then(|v| v.as_str());
    let dead = index::detect_dead_code(&repo_path, path_prefix)?;
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
    let blast_radius = args
        .get("blast_radius")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // blast_radius=false (the default) must stay byte-for-byte identical to
    // the tool's pre-#89 response and cost - this branch calls the same
    // plain `detect_changes` it always has, never touching the BFS below.
    if !blast_radius {
        let affected = index::detect_changes(&repo_path)?;
        return Ok(serde_json::to_string_pretty(&records_to_json(&affected))?);
    }

    let depth = clamp_depth(args.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as u32);
    let limit =
        clamp_limit(args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as u32) as usize;

    let result = index::detect_changes_blast_radius(&repo_path, depth)?;
    let direct_count = result.direct.len();
    let transitive_total = result.transitive.len();
    let transitive_shown: Vec<_> = result.transitive.into_iter().take(limit).collect();
    Ok(serde_json::to_string_pretty(&json!({
        "direct": records_to_json(&result.direct),
        "transitive_total": transitive_total,
        "transitive_shown": transitive_shown.len(),
        "transitive": traced_nodes_to_json(&transitive_shown),
        "summary": {
            "direct_count": direct_count,
            "transitive_count": transitive_total,
            "files_touched": result.files_touched,
        }
    }))?)
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
    fn estimate_tokens_is_bytes_over_four() {
        assert_eq!(estimate_tokens(4000), 1000);
        assert_eq!(estimate_tokens(3), 0);
    }

    #[test]
    fn session_usage_accumulates_calls_errors_and_bytes_per_tool() {
        // A name no real tool ever uses, so this stays isolated from
        // whatever other tests do to the same process-wide SESSION_USAGE
        // map (it's deliberately a singleton - see its own doc comment).
        record_session_call("__test_only_tool_a__", 100, false);
        record_session_call("__test_only_tool_a__", 300, true);

        let map = SESSION_USAGE.get().unwrap().lock().unwrap();
        let entry = map.get("__test_only_tool_a__").unwrap();
        assert_eq!(entry.call_count, 2);
        assert_eq!(entry.error_count, 1);
        assert_eq!(entry.output_bytes, 400);
    }

    #[test]
    fn get_session_usage_reports_a_recorded_call_with_its_token_estimate() {
        record_session_call("__test_only_tool_b__", 40, false);

        let output = get_session_usage(Value::Null).unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        let by_tool = parsed["by_tool"].as_array().unwrap();
        let entry = by_tool
            .iter()
            .find(|t| t["name"] == "__test_only_tool_b__")
            .expect("just-recorded tool missing from get_session_usage output");
        assert_eq!(entry["output_bytes"], 40);
        assert_eq!(entry["estimated_tokens"], 10);
    }

    #[test]
    fn reads_avoided_summary_counts_only_qualifying_tools_and_only_successful_calls() {
        let mut map = std::collections::HashMap::new();
        // Qualifies: 3 calls, 1 error - only the 2 successful ones count.
        map.insert(
            "trace_call_path".to_string(),
            SessionToolStats {
                call_count: 3,
                error_count: 1,
                output_bytes: 900,
            },
        );
        // Doesn't qualify (a raw scan, per READS_AVOIDED_TOOLS's doc
        // comment) - must not contribute even though it has real calls.
        map.insert(
            "search_code".to_string(),
            SessionToolStats {
                call_count: 5,
                error_count: 0,
                output_bytes: 5000,
            },
        );

        let (count, bytes) = reads_avoided_summary(&map);
        assert_eq!(count, 2);
        assert_eq!(bytes, 900);
    }

    #[test]
    fn get_session_usage_reports_a_nonzero_schema_tax() {
        // Not asserting an exact byte count - that would just re-encode
        // tool_definitions()'s current size and break on every schema edit.
        // The property that matters here (#40 follow-up 1) is that the tax
        // is actually measured and reported, not left implied.
        let output = get_session_usage(Value::Null).unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert!(parsed["schema_tax"]["bytes"].as_u64().unwrap() > 0);
        assert!(parsed["reads_avoided"]["counted_tools"].is_array());
    }

    #[test]
    fn index_freshness_reports_unindexed_when_no_registry_entry() {
        let value = index_freshness_json_from(None, 1000, 3600);
        assert_eq!(value["indexed"], false);
    }

    #[test]
    fn index_freshness_reports_warm_within_the_window_and_cold_beyond_it() {
        let entry = nexus_core::ProjectEntry {
            root_path: "/tmp/whatever".to_string(),
            hash: "deadbeef".to_string(),
            last_indexed_unix: 1000,
            nodes: 0,
            edges: 0,
            last_queried_unix: 1000,
            auto_reindex_count: 0,
            auto_reindex_fail_count: 0,
            auto_reindex_total_ms: 0,
            last_auto_reindex_ms: 0,
            last_auto_reindex_unix: 0,
        };

        let warm = index_freshness_json_from(Some(&entry), 1000 + 3600, 3600);
        assert_eq!(warm["warm"], true);
        assert_eq!(warm["seconds_since_indexed"], 3600);

        let cold = index_freshness_json_from(Some(&entry), 1000 + 3601, 3600);
        assert_eq!(cold["warm"], false);
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
    fn clamp_depth_passes_through_requests_at_or_below_the_max() {
        assert_eq!(clamp_depth(1), 1);
        assert_eq!(clamp_depth(SERVER_MAX_DEPTH), SERVER_MAX_DEPTH);
    }

    #[test]
    fn clamp_depth_caps_requests_above_the_max() {
        assert_eq!(clamp_depth(SERVER_MAX_DEPTH + 1), SERVER_MAX_DEPTH);
        assert_eq!(clamp_depth(100_000), SERVER_MAX_DEPTH);
    }

    /// Regression tests for issue #59: `trace_call_path`'s JSON response
    /// must distinguish a name-based (`CALLS`) hop from an LSP-verified
    /// (`CALLS_RESOLVED`) one. These exercise `edge_kind_provenance` and
    /// `traced_nodes_to_json` directly against hand-built `TracedNode`s
    /// rather than through a real indexed project + LSP server - the same
    /// "don't require a real backend for a response-shape test" approach
    /// `enrich.rs`'s own tests use for anything that isn't specifically
    /// testing real rust-analyzer output (those are `#[ignore]`d and gated
    /// on `NEXUS_TEST_RUST_ANALYZER`/PATH detection).
    fn traced_function(name: &str, edge_kind: index::EdgeKind) -> index::TracedNode {
        index::TracedNode {
            node: NodeRecord {
                id: 1,
                kind: index::NodeKind::Function,
                name: name.to_string(),
                qualified_name: format!("a.rs::{name}#1"),
                file_path: "a.rs".to_string(),
                start_line: 1,
                end_line: 2,
            },
            edge_kind,
        }
    }

    #[test]
    fn a_heuristic_calls_edge_reports_tree_sitter_name_match_confidence() {
        let provenance = edge_kind_provenance(index::EdgeKind::Calls);
        assert_eq!(provenance["provenance"], "tree-sitter");
        assert_eq!(provenance["resolution"], "name-match");
        assert_eq!(provenance["confidence"], "heuristic");
    }

    #[test]
    fn an_lsp_resolved_edge_reports_exact_semantic_confidence() {
        let provenance = edge_kind_provenance(index::EdgeKind::CallsResolved);
        assert_eq!(provenance["provenance"], "lsp");
        assert_eq!(provenance["resolution"], "semantic-symbol");
        assert_eq!(provenance["confidence"], "exact");
    }

    #[test]
    fn traced_nodes_to_json_carries_per_node_provenance_through_to_the_response() {
        let traced = vec![
            traced_function("heuristic_callee", index::EdgeKind::Calls),
            traced_function("resolved_callee", index::EdgeKind::CallsResolved),
        ];
        let json = traced_nodes_to_json(&traced);
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);

        assert_eq!(arr[0]["name"], "heuristic_callee");
        assert_eq!(arr[0]["confidence"], "heuristic");
        assert_eq!(arr[0]["provenance"], "tree-sitter");

        assert_eq!(arr[1]["name"], "resolved_callee");
        assert_eq!(arr[1]["confidence"], "exact");
        assert_eq!(arr[1]["provenance"], "lsp");
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
    fn default_config_resolves_to_standard_ten_tools() {
        let config = Config::default();
        let filtered = tool_names(&enabled_tool_definitions(&config));
        assert_eq!(filtered.len(), 10);
        assert!(filtered.contains("search_code"));
        assert!(filtered.contains("get_session_usage"));
        assert!(!filtered.contains("delete_project"));
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
                    "delete_project".to_string(),
                    "query_graph".to_string(),
                ]),
            },
            ..Default::default()
        };
        let filtered = tool_names(&enabled_tool_definitions(&config));
        assert_eq!(filtered.len(), 2);
        assert!(filtered.contains("delete_project"));
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

    /// Guards against exactly the drift that cost two review rounds this
    /// week: `get_session_usage` shipped in PR #19 without any doc update
    /// (caught in PR #21), and the landing page was then missed by that very
    /// fix and had to be caught in a follow-up commit to #21. See issue #24.
    ///
    /// Deliberately scoped to "live reference" docs only - `README.md` (and
    /// `change_proposal.md`) are phase-by-phase historical logs by design
    /// and legitimately contain frozen old counts (Phase 21's "13 total"
    /// stays 13 forever, recording what was true at that phase; Phase 29's
    /// "9 → 10" likewise). A generic check there would false-positive on
    /// intentional history rather than catch real drift - that's reviewed
    /// by a human PR reviewer instead (as it already was, for #21).
    #[test]
    fn doc_prose_tool_counts_match_the_real_tool_set() {
        let total = ALL_TOOL_NAMES.len();
        let minimal = MINIMAL_TOOLS.len();
        let standard = minimal + STANDARD_EXTRA_TOOLS.len();
        assert_eq!(total, standard + FULL_EXTRA_TOOLS.len());

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let read = |rel: &str| -> String {
            std::fs::read_to_string(root.join(rel))
                .unwrap_or_else(|e| panic!("failed to read {rel} for doc-drift check: {e}"))
        };

        let mcp_tools = read("docs/NexusContext-Wiki/MCP-Tools.md");
        assert!(
            mcp_tools.contains(&format!("{total} tools total")),
            "MCP-Tools.md's tool-count intro is stale - expected \"{total} tools total\""
        );
        assert!(
            mcp_tools.contains(&format!("all {total} are advertised")),
            "MCP-Tools.md's preset-rationale heading is stale - expected \"all {total} are advertised\""
        );

        let configuration = read("docs/NexusContext-Wiki/Configuration.md");
        assert!(
            configuration.contains(&format!(
                "\"minimal\" ({minimal}) | \"standard\" (default, {standard}) | \"full\" ({total})"
            )),
            "Configuration.md's preset comment line is stale"
        );
        assert!(
            configuration.contains(&format!("which of the {total} MCP tools")),
            "Configuration.md's [tools] field description is stale"
        );

        let home = read("docs/NexusContext-Wiki/Home.md");
        assert!(
            home.contains(&format!("the {total} tools an agent can call")),
            "Home.md's tool count is stale"
        );

        let install = read("INSTALL.md");
        assert!(
            install.contains(&format!(
                "only {standard} of these {total} are actually advertised"
            )),
            "INSTALL.md's tool count is stale"
        );
        assert!(
            install.contains(&format!(
                "\"minimal\" ({minimal} core read tools) | \"standard\" (default, {standard}) | \"full\" (all {total})"
            )),
            "INSTALL.md's preset comment line is stale"
        );

        let landing_page = read("docs/index.html");
        assert!(
            landing_page.contains(&format!("\"n\">{total}</div><div class=\"l\">MCP tools")),
            "docs/index.html's hero stat is stale"
        );
        assert!(
            landing_page.contains(&format!("stdio server with {total} tools")),
            "docs/index.html's subhead paragraph is stale"
        );
        assert!(
            landing_page.contains(&format!("{total} MCP tools, grouped by what they answer")),
            "docs/index.html's capabilities heading is stale"
        );
    }
}
