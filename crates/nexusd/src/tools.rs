use anyhow::{anyhow, bail, Result};
use nexus_core::{project_hash, Paths};
use nexus_index::{index_directory, Direction, GraphStore, NodeRecord};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

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
        "search_graph" => search_graph(args),
        "trace_call_path" => trace_call_path(args),
        "get_file_context" => get_file_context(args),
        "get_architecture" => get_architecture(args),
        "detect_changes" => detect_changes(args),
        "search_codebase" | "query_memory" => Err(anyhow!(
            "embeddings backend not configured - this tool needs an [embeddings] endpoint \
             in config.toml. Structural tools (search_graph, trace_call_path, get_architecture, \
             detect_changes) work without one."
        )),
        _ => bail!("unknown tool: {name}"),
    };

    match result {
        Ok(text) => Ok(json!({ "content": [ { "type": "text", "text": text } ], "isError": false })),
        Err(err) => Ok(json!({ "content": [ { "type": "text", "text": err.to_string() } ], "isError": true })),
    }
}

fn repo_path_arg(args: &Value) -> Result<PathBuf> {
    let raw = args
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    Ok(PathBuf::from(raw))
}

fn graph_db_path(repo_path: &Path) -> PathBuf {
    let paths = Paths::resolve();
    let hash = project_hash(repo_path);
    paths.project_data_dir(&hash).join("graph.db")
}

fn open_store(repo_path: &Path) -> Result<GraphStore> {
    let db_path = graph_db_path(repo_path);
    if !db_path.exists() {
        bail!(
            "no index found for {} - call index_repository first",
            repo_path.display()
        );
    }
    Ok(GraphStore::open(&db_path)?)
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
    let store = GraphStore::open(&graph_db_path(&repo_path))?;
    let stats = index_directory(&repo_path, &store)?;
    Ok(serde_json::to_string_pretty(&json!({
        "status": "indexed",
        "files_indexed": stats.files_indexed,
        "nodes": stats.nodes,
        "edges": stats.edges
    }))?)
}

fn search_graph(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'pattern' argument"))?;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;

    let store = open_store(&repo_path)?;
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

    let store = open_store(&repo_path)?;
    let results = store.trace_calls(name, direction, depth)?;
    Ok(serde_json::to_string_pretty(&records_to_json(&results))?)
}

fn get_file_context(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let file = args
        .get("file")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'file' argument"))?;

    let canonical_root = repo_path
        .canonicalize()
        .map_err(|_| anyhow!("repo_path does not exist: {}", repo_path.display()))?;
    let canonical_file = canonical_root
        .join(file)
        .canonicalize()
        .map_err(|_| anyhow!("file not found: {file}"))?;
    if !canonical_file.starts_with(&canonical_root) {
        bail!("file path escapes project root: {file}");
    }

    let content = std::fs::read_to_string(&canonical_file)?;
    let start = args.get("start_line").and_then(|v| v.as_u64()).map(|n| n as usize);
    let end = args.get("end_line").and_then(|v| v.as_u64()).map(|n| n as usize);

    match (start, end) {
        (Some(s), Some(e)) => {
            let lines: Vec<&str> = content.lines().collect();
            let s = s.saturating_sub(1).min(lines.len());
            let e = e.min(lines.len());
            Ok(lines[s..e].join("\n"))
        }
        _ => Ok(content),
    }
}

fn get_architecture(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let store = open_store(&repo_path)?;
    let (nodes, edges) = store.stats()?;
    let busiest = store.busiest_files(10)?;

    Ok(serde_json::to_string_pretty(&json!({
        "total_nodes": nodes,
        "total_edges": edges,
        "busiest_files": busiest.into_iter()
            .map(|(file, count)| json!({ "file": file, "definitions": count }))
            .collect::<Vec<_>>()
    }))?)
}

fn detect_changes(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let store = open_store(&repo_path)?;

    let output = std::process::Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "diff", "--unified=0"])
        .output()?;
    if !output.status.success() {
        bail!(
            "git diff failed - is {} a git repository?",
            repo_path.display()
        );
    }

    let diff_text = String::from_utf8_lossy(&output.stdout);
    let mut affected = Vec::new();
    for (file, ranges) in parse_diff_hunks(&diff_text) {
        for (start, end) in ranges {
            affected.extend(store.nodes_overlapping(&file, start, end)?);
        }
    }
    Ok(serde_json::to_string_pretty(&records_to_json(&affected))?)
}

/// Minimal unified-diff hunk parser: pulls (file, [(start_line, end_line)])
/// out of `git diff --unified=0` output. Doesn't handle renames/binary
/// files specially - good enough for mapping changes to symbol ranges.
fn parse_diff_hunks(diff: &str) -> Vec<(String, Vec<(u32, u32)>)> {
    let mut result: Vec<(String, Vec<(u32, u32)>)> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut current_ranges: Vec<(u32, u32)> = Vec::new();

    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            if let Some(f) = current_file.take() {
                result.push((f, std::mem::take(&mut current_ranges)));
            }
            current_file = Some(path.to_string());
        } else if let Some(rest) = line.strip_prefix("@@ ") {
            // rest looks like: "-old_start,old_count +new_start,new_count @@ ..."
            if let Some(plus_part) = rest.split('+').nth(1) {
                let range_str = plus_part.split(' ').next().unwrap_or("");
                let mut parts = range_str.splitn(2, ',');
                if let Some(Ok(start)) = parts.next().map(|s| s.parse::<u32>()) {
                    let count: u32 = parts.next().and_then(|c| c.parse().ok()).unwrap_or(1);
                    let end = if count == 0 { start } else { start + count - 1 };
                    current_ranges.push((start, end));
                }
            }
        }
    }
    if let Some(f) = current_file.take() {
        result.push((f, current_ranges));
    }
    result
}
