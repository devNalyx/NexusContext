use anyhow::{anyhow, bail, Result};
use nexus_core::{Config, Paths, Registry};
use nexus_index::{graph_db_path, index_project, GraphStore};
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
    match method {
        "status.get" => status_get(),
        "projects.list" => projects_list(),
        "projects.reindex" => projects_reindex(params),
        "config.get" => config_get(),
        "config.set" => config_set(params),
        "search.adhoc" => search_adhoc(params),
        _ => bail!("unknown control method: {method}"),
    }
}

fn status_get() -> Result<Value> {
    let paths = Paths::resolve();
    let registry = Registry::load(&paths.registry_file());
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "data_dir": paths.data_dir.display().to_string(),
        "projects_indexed": registry.projects.len()
    }))
}

fn projects_list() -> Result<Value> {
    let paths = Paths::resolve();
    let registry = Registry::load(&paths.registry_file());
    Ok(json!(registry.projects))
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
        "edges": stats.edges
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
    }

    config.save(&paths.config_file())?;
    Ok(serde_json::to_value(&config)?)
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
