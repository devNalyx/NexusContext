use anyhow::{anyhow, bail, Result};
use nexus_core::{Config, Paths, Registry};
use nexus_index::{
    delete_project, export_project, get_architecture, graph_db_path, import_project, index_project,
    project_disk_usage, GraphStore,
};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

/// Control API for the GUI/GNOME extension, not for MCP clients - same
/// JSON-RPC framing as the stdio transport, but a distinct method
/// namespace (`status.*`, `config.*`, `projects.*`, `search.*`) and a
/// different transport (Unix domain socket instead of stdio), so a GUI
/// session never competes with whatever MCP client is attached to stdio.
pub fn serve(socket_path: PathBuf) -> Result<()> {
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&socket_path)?;
    tracing::info!(socket = %socket_path.display(), "control API listening");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(move || {
                    if let Err(err) = handle_connection(stream) {
                        tracing::warn!(error = %err, "control connection error");
                    }
                });
            }
            Err(err) => tracing::warn!(error = %err, "failed to accept control connection"),
        }
    }
    Ok(())
}

fn handle_connection(stream: UnixStream) -> Result<()> {
    let reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(error = %err, "failed to parse control request, ignoring");
                continue;
            }
        };

        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        let response = match dispatch(&method, params) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(err) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32000, "message": err.to_string() }
            }),
        };

        writeln!(writer, "{}", serde_json::to_string(&response)?)?;
        writer.flush()?;
    }
    Ok(())
}

fn dispatch(method: &str, params: Value) -> Result<Value> {
    tracing::debug!(method, "control request received");
    // Same "actually used" signal as the MCP tool-call path in tools.rs -
    // the GUI talks to this control API, not the MCP tool list, so without
    // this the registry would only ever see last_queried_unix move for
    // MCP-driven usage and never for someone just using the GUI directly.
    if method != "projects.delete" {
        if let Some(repo_path) = params.get("repo_path").and_then(|v| v.as_str()) {
            nexus_index::touch_queried(std::path::Path::new(repo_path));
        }
    }
    let call_start = std::time::Instant::now();
    let result = match method {
        "status.get" => status_get(),
        "projects.list" => projects_list(),
        "projects.reindex" => projects_reindex(params),
        "projects.delete" => projects_delete(params),
        "projects.export" => projects_export(params),
        "projects.import" => projects_import(params),
        "projects.architecture" => projects_architecture(params),
        "config.get" => config_get(),
        "config.set" => config_set(params),
        "embeddings.test" => embeddings_test(),
        "search.adhoc" => search_adhoc(params),
        "viz.call_graph" => viz_call_graph(params),
        "stats.get" => stats_get(),
        _ => bail!("unknown control method: {method}"),
    };

    // Same Phase 1 observability as the MCP tool-call path in tools.rs, kept
    // in a separate `control_methods` bucket so GUI-originated usage never
    // gets mixed into "how are MCP agents using this" signal.
    {
        let latency_ms = call_start.elapsed().as_millis() as u64;
        let (is_error, output_bytes) = match &result {
            Ok(v) => (
                false,
                serde_json::to_string(v).map(|s| s.len()).unwrap_or(0) as u64,
            ),
            Err(err) => (true, err.to_string().len() as u64),
        };
        nexus_core::stats::record_control_call(
            &Paths::resolve().usage_stats_file(),
            method,
            latency_ms,
            output_bytes,
            is_error,
        );
    }

    result
}

/// Backs the GUI's Usage tab: lifetime aggregate call/latency/output-size
/// counters per MCP tool and per control method, plus background-watcher
/// auto-reindex frequency/cost per project. Phase 1 observability only - no
/// enforcement, no per-call log, see nexus_core::stats.
fn stats_get() -> Result<Value> {
    let paths = Paths::resolve();
    let usage = nexus_core::UsageStats::load(&paths.usage_stats_file());
    let registry = Registry::load(&paths.registry_file());

    let total_auto_reindex_count: u64 =
        registry.projects.iter().map(|p| p.auto_reindex_count).sum();
    let total_auto_reindex_fail_count: u64 = registry
        .projects
        .iter()
        .map(|p| p.auto_reindex_fail_count)
        .sum();
    let total_auto_reindex_ms: u64 = registry
        .projects
        .iter()
        .map(|p| p.auto_reindex_total_ms)
        .sum();

    Ok(json!({
        "mcp_tools": tool_stats_json(&usage.mcp_tools),
        "control_methods": tool_stats_json(&usage.control_methods),
        "reindex": {
            "total_auto_reindex_count": total_auto_reindex_count,
            "total_auto_reindex_fail_count": total_auto_reindex_fail_count,
            "avg_auto_reindex_ms": avg(total_auto_reindex_ms, total_auto_reindex_count),
            "projects": registry.projects.iter()
                .filter(|p| p.auto_reindex_count + p.auto_reindex_fail_count > 0)
                .map(|p| json!({
                    "root_path": p.root_path,
                    "auto_reindex_count": p.auto_reindex_count,
                    "auto_reindex_fail_count": p.auto_reindex_fail_count,
                    "avg_auto_reindex_ms": avg(p.auto_reindex_total_ms, p.auto_reindex_count),
                    "last_auto_reindex_ms": p.last_auto_reindex_ms,
                    "last_auto_reindex_unix": p.last_auto_reindex_unix,
                }))
                .collect::<Vec<_>>()
        },
        "collecting_since_unix": usage.collecting_since_unix
    }))
}

fn tool_stats_json(map: &std::collections::HashMap<String, nexus_core::ToolCallStats>) -> Value {
    json!(map
        .iter()
        .map(|(name, s)| json!({
            "name": name,
            "call_count": s.call_count,
            "error_count": s.error_count,
            "avg_latency_ms": avg(s.total_latency_ms, s.call_count),
            "max_latency_ms": s.max_latency_ms,
            "total_output_bytes": s.total_output_bytes,
            "last_called_unix": s.last_called_unix,
        }))
        .collect::<Vec<_>>())
}

fn avg(total: u64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        total as f64 / count as f64
    }
}

fn status_get() -> Result<Value> {
    let paths = Paths::resolve();
    let registry = Registry::load(&paths.registry_file());
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "data_dir": paths.data_dir.display().to_string(),
        "log_file": paths.log_file().display().to_string(),
        "projects_indexed": registry.projects.len(),
        "projects_watched": crate::watcher::watched_count()
    }))
}

fn projects_list() -> Result<Value> {
    let paths = Paths::resolve();
    let registry = Registry::load(&paths.registry_file());
    // Disk usage is computed live rather than stored in the registry entry -
    // it only costs a directory listing per project, and staying live means
    // it can never drift out of sync with what's actually on disk.
    let projects: Vec<Value> = registry
        .projects
        .iter()
        .map(|p| {
            let mut entry = serde_json::to_value(p).unwrap_or(json!({}));
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("disk_bytes".to_string(), json!(project_disk_usage(&p.hash)));
            }
            entry
        })
        .collect();
    Ok(json!(projects))
}

fn projects_reindex(params: Value) -> Result<Value> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    let repo_path = PathBuf::from(repo_path);

    let stats = index_project(&repo_path)?;

    Ok(json!({
        "status": "indexed",
        "files_indexed": stats.files_indexed,
        "nodes": stats.nodes,
        "edges": stats.edges,
        "embeddings_status": stats.embeddings_status
    }))
}

fn projects_delete(params: Value) -> Result<Value> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    delete_project(std::path::Path::new(repo_path))?;
    Ok(json!({ "status": "deleted" }))
}

fn projects_export(params: Value) -> Result<Value> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    let artifact = export_project(std::path::Path::new(repo_path))?;
    Ok(json!({ "status": "exported", "artifact_path": artifact.display().to_string() }))
}

fn projects_import(params: Value) -> Result<Value> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    let stats = import_project(std::path::Path::new(repo_path))?;
    Ok(json!({ "status": "imported", "nodes": stats.nodes, "edges": stats.edges }))
}

fn projects_architecture(params: Value) -> Result<Value> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    let repo_path = std::path::Path::new(repo_path);

    let summary = get_architecture(repo_path)?;

    let hash = nexus_core::project_hash(repo_path);
    let last_indexed_unix = Registry::load(&Paths::resolve().registry_file())
        .projects
        .into_iter()
        .find(|p| p.hash == hash)
        .map(|p| p.last_indexed_unix)
        .unwrap_or(0);

    Ok(json!({
        "total_nodes": summary.total_nodes,
        "total_edges": summary.total_edges,
        "busiest_files": summary.busiest_files.into_iter()
            .map(|(file, count)| json!({ "file": file, "definitions": count }))
            .collect::<Vec<_>>(),
        "language_breakdown": summary.language_breakdown.into_iter()
            .map(|(ext, count)| json!({ "extension": ext, "files": count }))
            .collect::<Vec<_>>(),
        "last_indexed_unix": last_indexed_unix
    }))
}

fn config_get() -> Result<Value> {
    let paths = Paths::resolve();
    let config = Config::load(&paths.config_file())?;
    Ok(serde_json::to_value(config)?)
}

fn config_set(params: Value) -> Result<Value> {
    let paths = Paths::resolve();
    let mut config = Config::load(&paths.config_file())?;

    if let Some(embeddings) = params.get("embeddings") {
        if let Some(enabled) = embeddings.get("enabled").and_then(|v| v.as_bool()) {
            config.embeddings.enabled = enabled;
        }
        if let Some(endpoint) = embeddings.get("endpoint").and_then(|v| v.as_str()) {
            config.embeddings.endpoint = Some(endpoint.to_string());
        }
        if let Some(model) = embeddings.get("model").and_then(|v| v.as_str()) {
            config.embeddings.model = Some(model.to_string());
        }
        if let Some(api_key) = embeddings.get("api_key").and_then(|v| v.as_str()) {
            config.embeddings.api_key = Some(api_key.to_string());
        }
        if let Some(timeout) = embeddings.get("timeout_secs").and_then(|v| v.as_u64()) {
            config.embeddings.timeout_secs = timeout;
        }
        if let Some(allow_remote) = embeddings.get("allow_remote").and_then(|v| v.as_bool()) {
            config.embeddings.allow_remote = allow_remote;
        }
    }

    config.save(&paths.config_file())?;
    Ok(serde_json::to_value(&config)?)
}

/// Checks the currently-saved endpoint/model are actually reachable, by
/// embedding a short literal probe string and timing it - not a project-
/// scoped operation, just "is what's in config.toml right now usable at
/// all." Backs the GUI's "Test Connection" button. Errors already flow
/// through `dispatch()`'s standard `Err` -> JSON-RPC error envelope, no
/// extra plumbing needed here.
fn embeddings_test() -> Result<Value> {
    let paths = Paths::resolve();
    let config = Config::load(&paths.config_file())?;
    let result = nexus_index::embeddings::test_connection(&config.embeddings)?;
    Ok(json!({
        "model": result.model,
        "dim": result.dim,
        "latency_ms": result.latency_ms
    }))
}

fn search_adhoc(params: Value) -> Result<Value> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    let pattern = params
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'pattern' argument"))?;
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;

    let db_path = graph_db_path(std::path::Path::new(repo_path));
    if !db_path.exists() {
        bail!("no index found for {repo_path} - call projects.reindex first");
    }
    let store = GraphStore::open(&db_path)?;
    let results = store.search_by_name(pattern, limit)?;

    Ok(json!(results
        .iter()
        .map(|n| json!({
            "kind": format!("{:?}", n.kind),
            "name": n.name,
            "qualified_name": n.qualified_name,
            "file": n.file_path,
            "start_line": n.start_line,
            "end_line": n.end_line,
        }))
        .collect::<Vec<_>>()))
}

/// Renders a bounded call-graph neighborhood as Graphviz DOT source - the
/// GUI shells out to `dot` to turn this into an image. Kept as a plain DOT
/// string over the wire (not a rendered image) so the actual rendering
/// dependency (whether `dot` is installed at all) stays entirely in the
/// GUI, which is the only client that needs a picture rather than data.
fn viz_call_graph(params: Value) -> Result<Value> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    let function_name = params
        .get("function_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'function_name' argument"))?;
    let direction = match params.get("direction").and_then(|v| v.as_str()) {
        Some("inbound") => nexus_index::Direction::Inbound,
        _ => nexus_index::Direction::Outbound,
    };
    let depth = params.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as u32;

    let dot = nexus_index::call_graph_dot(
        std::path::Path::new(repo_path),
        function_name,
        direction,
        depth,
    )?;
    Ok(json!({ "dot": dot }))
}
