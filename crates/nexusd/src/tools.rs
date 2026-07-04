use anyhow::{anyhow, bail, Result};
use nexus_core::{project_hash, Config, Paths, Registry};
use nexus_index::{self as index, index_project, Direction, NodeRecord};
use serde_json::{json, Value};
use std::path::PathBuf;

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
            "description": "BFS over the CALLS graph to find callers/callees of a function. Resolution is name-based, not import-aware: same-file matches win, and a cross-file call resolves only if the callee name is unique project-wide - ambiguous same-named functions across files are left unresolved. Call-graph quality varies by language: solid for Rust/Python/JS/TS/Go/Java/Ruby; structural-only (no call edges) for C/C++/C#/PHP - see language.rs for why.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "name": { "type": "string" },
                    "direction": { "type": "string", "enum": ["inbound", "outbound"], "default": "outbound" },
                    "depth": { "type": "integer", "default": 3 }
                },
                "required": ["repo_path", "name"]
            }
        },
        {
            "name": "get_file_context",
            "description": "Read a file, optionally a specific line range, from an indexed project.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "file": { "type": "string" },
                    "start_line": { "type": "integer" },
                    "end_line": { "type": "integer" }
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
            "description": "Functions with no inbound CALLS edge (excluding main). Caveat: call resolution is name-based (same-file, or cross-file only when the name is unique project-wide), so a function called only via an ambiguous same-named cross-file call, or invoked via reflection/routing/dependency injection rather than a direct call, may show up as a false positive - treat results as worth a second look, not a guarantee. Response is capped at `limit` (default 50) with a `total_flagged` count, since an untargeted sweep on a large project can flag hundreds of mostly-false-positive results.",
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
            "description": "Picks the cheapest retrieval strategy for a query instead of the agent guessing: a specific file goes straight to get_file_context, a single identifier-like token goes to search_graph, and a descriptive multi-word query goes to semantic search if configured or a keyword-over-the-graph fallback otherwise. Returns which strategy was used alongside the results.",
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
            "description": "Minimal ad-hoc graph query - not full Cypher, exactly one pattern shape: MATCH (a:Kind)-[:EDGE_KIND]->(b:Kind) [WHERE a.name = 'value' or b.name = 'value'] RETURN a|b. Kind is Function, Type, File, or Section (a markdown heading; CONTAINS edges link a heading to its nested sub-headings). Fails with a clear error for anything outside that shape rather than guessing.",
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
            "description": "Semantic search via cosine similarity against embedded Function/Type nodes and markdown heading sections alike. Requires embeddings.enabled = true and a reachable endpoint/model in config.toml (see the GUI's Config tab), and that this project was reindexed after enabling it. Errors with a specific, actionable reason otherwise - structural tools (search_graph, search_code, query_planner) work regardless.",
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
            index::touch_queried(std::path::Path::new(repo_path));
        }
    }

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
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as u32;

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
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;

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

    let store = index::open_store(&repo_path)?;
    let results = store.trace_calls(name, direction, depth)?;
    Ok(serde_json::to_string_pretty(&records_to_json(&results))?)
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
    index::get_file_context(&repo_path, file, start, end)
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
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
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
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;

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
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;

    let results = nexus_index::run_cypher_query(&repo_path, query, limit)?;
    Ok(serde_json::to_string_pretty(&records_to_json(&results))?)
}

fn detect_changes(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let affected = index::detect_changes(&repo_path)?;
    Ok(serde_json::to_string_pretty(&records_to_json(&affected))?)
}
