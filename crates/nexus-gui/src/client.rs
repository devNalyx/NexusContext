use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

/// Blocking round-trip to nexusd's control socket. Local Unix-socket calls
/// are sub-millisecond, so doing this synchronously on GTK's main thread in
/// a signal handler is acceptable for this version - moving to an async
/// client is a future refinement, not a v1 requirement.
pub fn call(method: &str, params: Value) -> Result<Value> {
    let socket_path = nexus_core::Paths::resolve().control_socket();
    let stream = UnixStream::connect(&socket_path).map_err(|err| {
        anyhow!(
            "can't reach nexusd control socket at {} ({err}) - is `nexusd serve` running?",
            socket_path.display()
        )
    })?;

    let mut writer = stream.try_clone()?;
    let request = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    writeln!(writer, "{}", serde_json::to_string(&request)?)?;
    writer.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let response: Value = serde_json::from_str(line.trim())?;
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown control API error");
        bail!("{message}");
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("control API response missing 'result'"))
}
