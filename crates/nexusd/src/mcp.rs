use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

struct RpcError {
    code: i64,
    message: String,
}

/// MCP stdio transport: newline-delimited JSON-RPC 2.0 messages on stdin/stdout.
/// Notifications (no "id") get no response, per spec.
pub fn serve_stdio() -> anyhow::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(error = %err, "failed to parse JSON-RPC message, ignoring");
                continue;
            }
        };

        let method = request
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        let Some(id) = request.get("id").cloned() else {
            tracing::debug!(method = %method, "received notification");
            continue;
        };

        let response = match dispatch(&method, params) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(err) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": err.code, "message": err.message }
            }),
        };

        writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
        stdout.flush()?;
    }

    Ok(())
}

fn dispatch(method: &str, params: Value) -> Result<Value, RpcError> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "nexuscontext", "version": env!("CARGO_PKG_VERSION") }
        })),
        "tools/list" => {
            let config = nexus_core::Config::load(&nexus_core::Paths::resolve().config_file())
                .unwrap_or_default();
            Ok(json!({ "tools": crate::tools::enabled_tool_definitions(&config) }))
        }
        "tools/call" => crate::tools::call(params).map_err(|err| RpcError {
            code: -32000,
            message: err.to_string(),
        }),
        _ => Err(RpcError {
            code: -32601,
            message: format!("method not found: {method}"),
        }),
    }
}
