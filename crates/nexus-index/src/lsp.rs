//! Minimal LSP client for issue #10's resolved-symbol enrichment pilot -
//! rust-analyzer only, just enough of the protocol to ask
//! `textDocument/references` for each indexed function's definition site.
//! Not a general-purpose LSP library: no diagnostics, no hover, no
//! completion, nothing this pilot doesn't need.
//!
//! Strictly best-effort throughout. Every public entry point returns
//! `Result`, but every *caller* in `enrich.rs` treats an `Err` here as "skip
//! enrichment for this run," never as a reason to fail the reindex - see
//! that module's doc comment.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// One resolved reference to a symbol, in the callee's own file - the
/// caller (`enrich.rs`) is responsible for mapping `(file, line)` back to
/// whichever indexed `Function` node's range contains it, if any.
pub struct ReferenceLocation {
    /// Workspace-relative or absolute path, as returned by the server after
    /// stripping the `file://` scheme - `enrich.rs` normalizes it against
    /// the project root before comparing to indexed `file_path`s.
    pub file: String,
    /// 0-based, per the LSP spec (`enrich.rs` converts to/from this
    /// project's 1-based `start_line`/`end_line` node fields).
    pub line: u32,
}

/// A live rust-analyzer child process, past the `initialize`/`initialized`
/// handshake and ready for `textDocument/references` requests. Talks
/// newline-framed... no - LSP's actual wire format: `Content-Length: N\r\n
/// \r\n<N bytes of JSON>`, distinct from NexusContext's own MCP transport
/// (plain newline-delimited JSON-RPC) - two different protocols that happen
/// to both be JSON-RPC 2.0 at the message-body level.
pub struct LspClient {
    child: Child,
    stdin: ChildStdin,
    /// Every framed message the reader thread has parsed off stdout,
    /// oldest first - `next_response`/`drain_until_response` pull from this
    /// rather than reading stdout directly, so a slow/stuck server can't
    /// block the whole enrichment pass past `overall_deadline`
    /// (`recv_timeout` on the channel returns instead of hanging forever).
    inbox: Receiver<Value>,
    next_id: i64,
}

fn write_framed(stdin: &mut ChildStdin, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len())?;
    stdin.write_all(&body)?;
    stdin.flush()?;
    Ok(())
}

/// Reads exactly one `Content-Length`-framed JSON-RPC message from `r`.
/// Blocking - the dedicated reader thread this runs on is what makes the
/// rest of this client effectively non-blocking from the caller's side.
fn read_framed(r: &mut impl BufRead) -> Result<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut header = String::new();
        let n = r.read_line(&mut header)?;
        if n == 0 {
            bail!("LSP server closed its stdout (EOF) while reading a header");
        }
        let header = header.trim_end();
        if header.is_empty() {
            break; // blank line - end of headers
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse().context("bad Content-Length header")?);
        }
        // Other headers (e.g. Content-Type) are ignored - rust-analyzer
        // doesn't send any that change how the body should be parsed.
    }
    let content_length =
        content_length.ok_or_else(|| anyhow!("LSP message had no Content-Length header"))?;
    let mut body = vec![0u8; content_length];
    r.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

impl LspClient {
    /// Spawns `server_command` in `root` and runs the `initialize`/
    /// `initialized` handshake. Does *not* wait for the server to finish
    /// its own workspace indexing - see `enrich.rs`'s settle step for that,
    /// kept as a separate concern so this constructor's own failure modes
    /// (server binary missing, handshake rejected) stay easy to reason
    /// about on their own.
    pub fn spawn(server_command: &str, root: &Path) -> Result<Self> {
        let mut child = Command::new(server_command)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to spawn LSP server '{server_command}'"))?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;

        // A dedicated reader thread, not inline blocking reads on the main
        // enrichment thread - `enrich.rs` needs to bound total time spent
        // waiting on this server with one overall deadline
        // (`Receiver::recv_timeout`), which a synchronous blocking read on
        // a pipe has no portable way to do on its own.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(message) = read_framed(&mut reader) {
                if tx.send(message).is_err() {
                    break; // receiver gone - client dropped, stop reading
                }
            }
            // Loop exits either on EOF/a framing error (server's done
            // talking) or the receiver having been dropped - both mean
            // "stop reading," nothing further to distinguish here.
        });

        let mut client = LspClient {
            child,
            stdin,
            inbox: rx,
            next_id: 1,
        };
        client.handshake(root)?;
        Ok(client)
    }

    fn handshake(&mut self, root: &Path) -> Result<()> {
        let root_uri = format!("file://{}", root.display());
        let response = self.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {},
            }),
            Duration::from_secs(30),
        )?;
        if response.get("error").is_some() {
            bail!("LSP 'initialize' failed: {response}");
        }
        self.notify("initialized", json!({}))?;
        Ok(())
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        write_framed(
            &mut self.stdin,
            &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        )
    }

    /// Sends a request and blocks (up to `timeout`) for *its* response,
    /// silently discarding any notifications received in the meantime
    /// (`$/progress` and similar - `enrich.rs`'s settle step is the one
    /// place that cares about those, and reads them directly off the same
    /// inbox before any requests are sent).
    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        write_framed(
            &mut self.stdin,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        )?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("timed out waiting for a response to '{method}'");
            }
            let message = self
                .inbox
                .recv_timeout(remaining)
                .map_err(|_| anyhow!("timed out waiting for a response to '{method}'"))?;
            if message.get("id").and_then(|v| v.as_i64()) == Some(id) {
                return Ok(message);
            }
            // Not our response (a notification, or a response to a request
            // we've since given up on) - keep waiting for ours within what
            // budget remains.
        }
    }

    /// Drains whatever's already queued or arrives before `deadline`,
    /// returning once nothing new has shown up for `quiet_for` - a cheap
    /// proxy for "the server has gone idle" without parsing rust-analyzer's
    /// specific `$/progress` token structure (which isn't part of the
    /// stable LSP spec and has shifted across its own versions). Used once,
    /// right after the handshake, to give initial workspace indexing a
    /// chance to finish before the first real request - see `enrich.rs`.
    pub fn wait_until_idle(&mut self, deadline: Instant, quiet_for: Duration) {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            match self.inbox.recv_timeout(remaining.min(quiet_for)) {
                Ok(_) => continue, // something arrived - still busy, keep waiting
                Err(mpsc::RecvTimeoutError::Timeout) => return, // quiet_for elapsed with nothing new
                Err(mpsc::RecvTimeoutError::Disconnected) => return, // server's gone
            }
        }
    }

    pub fn did_open(&mut self, file_uri: &str, language_id: &str, text: &str) -> Result<()> {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": file_uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                }
            }),
        )
    }

    /// `line`/`character` are 0-based, per the LSP spec.
    pub fn references(
        &mut self,
        file_uri: &str,
        line: u32,
        character: u32,
        timeout: Duration,
    ) -> Result<Vec<ReferenceLocation>> {
        let response = self.request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": file_uri },
                "position": { "line": line, "character": character },
                "context": { "includeDeclaration": false },
            }),
            timeout,
        )?;
        if let Some(err) = response.get("error") {
            bail!("textDocument/references failed: {err}");
        }
        let Some(locations) = response.get("result").and_then(|v| v.as_array()) else {
            return Ok(Vec::new()); // null result - no references found, not an error
        };
        Ok(locations
            .iter()
            .filter_map(|loc| {
                let uri = loc.get("uri")?.as_str()?;
                let file = uri.strip_prefix("file://").unwrap_or(uri).to_string();
                let line = loc.get("range")?.get("start")?.get("line")?.as_u64()? as u32;
                Some(ReferenceLocation { file, line })
            })
            .collect())
    }

    /// Best-effort clean shutdown: `shutdown` request, `exit` notification,
    /// then kill outright if the process hasn't actually exited within a
    /// short grace period - never lets a misbehaving server outlive the
    /// enrichment pass that spawned it.
    pub fn shutdown(mut self) {
        let _ = self.request("shutdown", Value::Null, Duration::from_secs(5));
        let _ = self.notify("exit", Value::Null);

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return, // exited on its own
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
