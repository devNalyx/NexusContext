//! Read-only control-socket client for `nexus status` (issue #117).
//!
//! The CLI never starts or stops a daemon: it only asks the running one for
//! `status.get` so "installed version" and "running version" can be shown
//! side by side. A mismatch means a restart is required, which was previously
//! invisible unless you hand-crafted a JSON-RPC call to the socket.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonStatus {
    /// `status.get` answered with the version of the running binary.
    Running(String),
    /// No daemon is listening on the control socket.
    NotRunning,
    /// The socket answered, but the reply could not be used.
    Error(String),
}

/// Queries the daemon's own compile-time version. Connection failures mean
/// "not running" rather than an error, because `nexus status` is expected to
/// work with no daemon at all; protocol failures are surfaced explicitly.
#[cfg(unix)]
pub fn query(socket: &Path) -> DaemonStatus {
    use std::os::unix::net::UnixStream;

    let stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(_) => return DaemonStatus::NotRunning,
    };
    // A wedged daemon must not hang a status command forever.
    if let Err(err) = stream.set_read_timeout(Some(Duration::from_secs(2))) {
        return DaemonStatus::Error(format!("could not set socket timeout: {err}"));
    }
    match call(stream) {
        Ok(result) => match result.get("version").and_then(Value::as_str) {
            Some(version) => DaemonStatus::Running(version.to_string()),
            None => DaemonStatus::Error("status.get returned no version".to_string()),
        },
        Err(err) => DaemonStatus::Error(err.to_string()),
    }
}

#[cfg(not(unix))]
pub fn query(_socket: &Path) -> DaemonStatus {
    // `serve` (and therefore the control socket) is Unix-only today - issue #16.
    DaemonStatus::Error("control socket is not available on this platform".to_string())
}

#[cfg(unix)]
fn call(stream: std::os::unix::net::UnixStream) -> Result<Value> {
    use std::io::{BufRead, BufReader, Write};

    let mut writer = stream.try_clone()?;
    let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "status.get", "params": {} });
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

/// Pure formatter so all three states can be tested without a live daemon.
pub fn status_lines(
    installed: &str,
    config_file: &Path,
    data_dir: &Path,
    daemon: &DaemonStatus,
) -> Vec<String> {
    vec![
        format!("installed   : {installed}"),
        format!("config file : {}", config_file.display()),
        format!("data dir    : {}", data_dir.display()),
        daemon_line(installed, daemon),
    ]
}

pub fn daemon_line(installed: &str, daemon: &DaemonStatus) -> String {
    match daemon {
        DaemonStatus::Running(version) if version == installed => {
            format!("daemon      : running {version}")
        }
        DaemonStatus::Running(version) => {
            format!("daemon      : running {version} — RESTART REQUIRED (installed {installed})")
        }
        DaemonStatus::NotRunning => "daemon      : not running (start `nexusd serve`)".to_string(),
        DaemonStatus::Error(err) => format!("daemon      : unknown ({err})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn matching_daemon_version_has_no_restart_warning() {
        let line = daemon_line("0.1.28", &DaemonStatus::Running("0.1.28".to_string()));
        assert_eq!(line, "daemon      : running 0.1.28");
        assert!(!line.contains("RESTART"));
    }

    #[test]
    fn stale_daemon_version_is_explicitly_actionable() {
        let line = daemon_line("0.1.28", &DaemonStatus::Running("0.1.26".to_string()));
        assert_eq!(
            line,
            "daemon      : running 0.1.26 — RESTART REQUIRED (installed 0.1.28)"
        );
    }

    #[test]
    fn absent_and_error_states_are_distinguishable() {
        assert_eq!(
            daemon_line("0.1.28", &DaemonStatus::NotRunning),
            "daemon      : not running (start `nexusd serve`)"
        );
        let error = daemon_line("0.1.28", &DaemonStatus::Error("bad reply".to_string()));
        assert!(error.starts_with("daemon      : unknown ("));
        assert!(error.contains("bad reply"));
        assert!(!error.contains("RESTART"));
    }

    #[test]
    fn status_lines_put_installed_version_before_paths_and_daemon() {
        let lines = status_lines(
            "0.1.28",
            Path::new("/cfg/config.toml"),
            Path::new("/data"),
            &DaemonStatus::Running("0.1.27".to_string()),
        );
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "installed   : 0.1.28");
        assert_eq!(lines[1], "config file : /cfg/config.toml");
        assert_eq!(lines[2], "data dir    : /data");
        assert!(lines[3].contains("RESTART REQUIRED"));
    }
}
