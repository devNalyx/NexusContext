use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

#[derive(Debug)]
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

        // `catch_unwind` isolates a panic to this one request instead of
        // unwinding straight through `main` and killing the whole
        // `nexusd mcp` process - i.e. that agent's entire MCP session -
        // over a single bad tool call. `control.rs::serve` already gets
        // this for free (one thread per connection); this loop is the one
        // place in the daemon that didn't, since it's a single synchronous
        // loop with no thread boundary of its own. See issue #36.
        let response = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dispatch(&method, params)
        })) {
            Ok(Ok(result)) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Ok(Err(err)) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": err.code, "message": err.message }
            }),
            Err(panic_payload) => {
                let panic_message = panic_message(&panic_payload);
                tracing::error!(
                    method = %method,
                    panic = %panic_message,
                    "tool call panicked - isolated to this one request, session stays alive"
                );
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32000,
                        "message": format!("internal error: tool call panicked ({panic_message})")
                    }
                })
            }
        };

        writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
        stdout.flush()?;
    }

    Ok(())
}

/// A panic payload is `Box<dyn Any + Send>` - almost always either a `&str`
/// (a `panic!("literal")`) or a `String` (a `panic!("{}", formatted)`), but
/// not guaranteed to be either, so this falls back to a fixed message
/// rather than unwrapping.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
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

/// Regression tests for issue #36: `serve_stdio`'s dispatch loop had no
/// panic isolation - a panic anywhere in `dispatch`/`tools::call` used to
/// unwind straight through `main` and kill the whole `nexusd mcp` process.
#[cfg(test)]
mod tests {
    use super::panic_message;

    #[test]
    fn extracts_a_str_literal_panic_message() {
        let payload = std::panic::catch_unwind(|| {
            panic!("boom");
        })
        .unwrap_err();
        assert_eq!(panic_message(payload.as_ref()), "boom");
    }

    #[test]
    fn extracts_a_formatted_string_panic_message() {
        let payload = std::panic::catch_unwind(|| {
            panic!("boom {}", 42);
        })
        .unwrap_err();
        assert_eq!(panic_message(payload.as_ref()), "boom 42");
    }

    /// Proves the exact `catch_unwind` + `AssertUnwindSafe` shape
    /// `serve_stdio` wraps `dispatch` in actually isolates a panic rather
    /// than propagating it - the whole point of #36, not just that
    /// `panic_message` parses correctly once something else already caught
    /// it.
    #[test]
    fn a_panicking_dispatch_call_is_caught_not_propagated() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> Result<(), super::RpcError> { panic!("simulated tool-call panic") },
        ));
        assert!(result.is_err(), "the panic must be caught, not propagate");
        assert_eq!(
            panic_message(result.unwrap_err().as_ref()),
            "simulated tool-call panic"
        );
    }
}
