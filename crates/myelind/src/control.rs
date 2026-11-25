use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

/// Control socket for a future GUI/CLI — separate from the MCP stdio
/// transport so the two never compete over the same pipe. Only `status.get`
/// exists so far; enough to prove the daemon is alive and reachable.
pub fn serve(socket_path: PathBuf) -> anyhow::Result<()> {
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
                if let Err(err) = handle_client(stream) {
                    tracing::warn!(error = %err, "control connection error");
                }
            }
            Err(err) => tracing::warn!(error = %err, "failed to accept control connection"),
        }
    }

    Ok(())
}

fn handle_client(mut stream: UnixStream) -> anyhow::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let request: Value = serde_json::from_str(line.trim())?;
    let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");

    let response = match method {
        "status.get" => json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "note": "scaffold build — no ingestion/extraction pipeline wired up yet"
        }),
        other => json!({ "error": format!("method not found: {other}") }),
    };

    writeln!(stream, "{}", serde_json::to_string(&response)?)?;
    Ok(())
}
