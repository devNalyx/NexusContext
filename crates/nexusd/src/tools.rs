use anyhow::{anyhow, bail, Result};
use nexus_core::{project_hash, Config, Paths, Registry};
use nexus_index::{graph_db_path, index_project, Direction, GraphStore, NodeRecord};
use serde_json::{json, Value};
use std::collections::HashSet;
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
        "search_graph" => search_graph(args),
        "trace_call_path" => trace_call_path(args),
        "get_file_context" => get_file_context(args),
        "get_architecture" => get_architecture(args),
        "detect_changes" => detect_changes(args),
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
    let stats = index_project(&repo_path)?;
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

fn last_indexed_unix(repo_path: &Path) -> u64 {
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
        let store = open_store(&repo_path)?;
        let (nodes, edges) = store.stats()?;
        let busiest = store.busiest_files(10)?;
        Ok(json!({
            "total_nodes": nodes,
            "total_edges": edges,
            "busiest_files": busiest.into_iter()
                .map(|(file, count)| json!({ "file": file, "definitions": count }))
                .collect::<Vec<_>>()
        }))
    })?;

    Ok(serde_json::to_string_pretty(&value)?)
}

/// Rule-based dispatcher, not an LLM-backed one - the daemon deliberately
/// has no embedded reasoning model (the calling agent is the intelligence
/// layer). This just picks the cheapest of the strategies that already
/// exist instead of making the agent guess: a named file wins outright, a
/// single identifier-like token goes straight to the graph, and anything
/// more descriptive would go to semantic search once that pipeline exists -
/// for now it falls back to a naive per-word graph search instead of
/// erroring out.
fn query_planner(args: Value) -> Result<String> {
    let repo_path = repo_path_arg(&args)?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'query' argument"))?
        .to_string();

    if args.get("file").and_then(|v| v.as_str()).is_some() {
        let text = get_file_context(args.clone())?;
        return Ok(serde_json::to_string_pretty(
            &json!({ "strategy": "file_read", "result": text }),
        )?);
    }

    let is_identifier = !query.trim().is_empty()
        && query
            .chars()
            .next()
            .map(|c| c.is_alphabetic() || c == '_')
            .unwrap_or(false)
        && query.chars().all(|c| c.is_alphanumeric() || c == '_');

    if is_identifier {
        let store = open_store(&repo_path)?;
        let results = store.search_by_name(&query, 20)?;
        return Ok(serde_json::to_string_pretty(&json!({
            "strategy": "graph_search",
            "result": records_to_json(&results)
        }))?);
    }

    let config = Config::load(&Paths::resolve().config_file())?;
    let store = open_store(&repo_path)?;

    const STOPWORDS: &[&str] = &[
        "the", "a", "an", "is", "are", "of", "to", "in", "for", "and", "or", "find", "get",
        "where", "how", "what", "does", "do",
    ];
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for word in query.split_whitespace() {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if word.len() < 3 || STOPWORDS.contains(&word.to_lowercase().as_str()) {
            continue;
        }
        for record in store.search_by_name(word, 10)? {
            if seen.insert(record.qualified_name.clone()) {
                merged.push(record);
            }
        }
    }

    let note = match config.embeddings_policy() {
        nexus_core::EmbeddingsPolicy::NotConfigured => {
            "no embeddings endpoint configured - falling back to keyword search over the graph"
        }
        nexus_core::EmbeddingsPolicy::RemoteBlocked => {
            "an embeddings endpoint is configured but blocked (remote host, allow_remote not \
             set) - falling back to keyword search over the graph"
        }
        nexus_core::EmbeddingsPolicy::Allowed => {
            "an embeddings endpoint is configured and allowed, but semantic search isn't \
             implemented yet - falling back to keyword search over the graph"
        }
    };

    Ok(serde_json::to_string_pretty(&json!({
        "strategy": "keyword_fallback_graph_search",
        "embeddings_policy": format!("{:?}", config.embeddings_policy()),
        "note": note,
        "result": records_to_json(&merged)
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
