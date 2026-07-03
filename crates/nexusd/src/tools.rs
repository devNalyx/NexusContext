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
            "description": "Structural search over indexed symbols by name substring. No embeddings required.",
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
            "description": "BFS over the CALLS graph to find callers/callees of a function. Same-file call resolution only in this version.",
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
            "description": "Summarize an indexed project: total node/edge counts and the busiest files by definition count.",
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
            "description": "Functions with no inbound CALLS edge (excluding main). Caveat: call resolution is same-file only, so a function only ever called from a different file will show up as a false positive - treat results as worth a second look, not a guarantee.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo_path": { "type": "string" } },
                "required": ["repo_path"]
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
            "name": "search_codebase",
            "description": "Semantic search over code. Requires an [embeddings] endpoint configured in config.toml; returns an error otherwise.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo_path": { "type": "string" }, "query": { "type": "string" } },
                "required": ["repo_path", "query"]
            }
        },
        {
            "name": "query_memory",
            "description": "RAG-style retrieval over indexed content. Requires an [embeddings] endpoint configured in config.toml; returns an error otherwise.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo_path": { "type": "string" }, "query": { "type": "string" } },
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

    let result = match name {
        "index_repository" => index_repository(args),
        "delete_project" => delete_project(args),
        "search_graph" => search_graph(args),
        "trace_call_path" => trace_call_path(args),
        "get_file_context" => get_file_context(args),
        "get_architecture" => get_architecture(args),
        "detect_changes" => detect_changes(args),
        "detect_dead_code" => detect_dead_code(args),
        "query_planner" => query_planner(args),
        "search_codebase" | "query_memory" => Err(embeddings_unavailable_error()),
        _ => bail!("unknown tool: {name}"),
    };

    match result {
        Ok(text) => Ok(json!({ "content": [ { "type": "text", "text": text } ], "isError": false })),
        Err(err) => Ok(json!({ "content": [ { "type": "text", "text": err.to_string() } ], "isError": true })),
    }
}

fn embeddings_unavailable_error() -> anyhow::Error {
    let config = match Config::load(&Paths::resolve().config_file()) {
        Ok(c) => c,
        Err(_) => return anyhow!("embeddings backend not configured"),
    };
    match config.embeddings_policy() {
        nexus_core::EmbeddingsPolicy::RemoteBlocked => anyhow!(
            "embeddings endpoint {} is not loopback/private, and allow_remote isn't set - \
             refusing to send code to it. Set embeddings.allow_remote = true in config.toml \
             if this is intentional.",
            config.embeddings.endpoint.as_deref().unwrap_or("?")
        ),
        nexus_core::EmbeddingsPolicy::Allowed => anyhow!(
            "embeddings endpoint is configured and allowed, but the embedding HTTP client isn't \
             implemented yet - structural tools (search_graph, trace_call_path, get_architecture, \
             detect_changes) work without one."
        ),
        nexus_core::EmbeddingsPolicy::NotConfigured => anyhow!(
            "embeddings backend not configured - structural tools (search_graph, trace_call_path, \
             get_architecture, detect_changes) work without one."
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

fn index_repository(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let stats = index_project(&repo_path)?;
    Ok(serde_json::to_string_pretty(&json!({
        "status": "indexed",
        "files_indexed": stats.files_indexed,
        "nodes": stats.nodes,
        "edges": stats.edges
    }))?)
}

fn delete_project(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    index::delete_project(&repo_path)?;
    Ok(serde_json::to_string_pretty(&json!({ "status": "deleted" }))?)
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
    let start = args.get("start_line").and_then(|v| v.as_u64()).map(|n| n as usize);
    let end = args.get("end_line").and_then(|v| v.as_u64()).map(|n| n as usize);
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
    let start = args.get("start_line").and_then(|v| v.as_u64()).map(|n| n as usize);
    let end = args.get("end_line").and_then(|v| v.as_u64()).map(|n| n as usize);

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
    let dead = index::detect_dead_code(&repo_path)?;
    Ok(serde_json::to_string_pretty(&records_to_json(&dead))?)
}

fn detect_changes(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let affected = index::detect_changes(&repo_path)?;
    Ok(serde_json::to_string_pretty(&records_to_json(&affected))?)
}
