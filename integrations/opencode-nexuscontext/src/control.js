import net from "node:net"
import os from "node:os"
import path from "node:path"

/**
 * Resolve the daemon's Unix control socket without importing Rust's `Paths`
 * into the plugin. Same precedence as the daemon uses: explicit override,
 * XDG_RUNTIME_DIR, then the XDG data dir fallback.
 */
export function resolveSocketPath(env = process.env) {
  if (env.NEXUS_CONTROL_SOCKET) return env.NEXUS_CONTROL_SOCKET
  // ProjectDirs::runtime_dir() appends the app name; then Paths appends
  // "nexuscontext.sock". Must match crates/nexus-core/src/paths.rs exactly.
  if (env.XDG_RUNTIME_DIR) {
    return path.join(env.XDG_RUNTIME_DIR, "nexuscontext", "nexuscontext.sock")
  }
  // No runtime dir: the daemon falls back to its data_dir. NEXUS_CACHE_DIR
  // overrides that; otherwise it is $XDG_DATA_HOME/nexuscontext.
  const dataDir =
    env.NEXUS_CACHE_DIR ||
    path.join(env.XDG_DATA_HOME || path.join(os.homedir(), ".local", "share"), "nexuscontext")
  return path.join(dataDir, "nexuscontext.sock")
}

/**
 * Minimal NDJSON JSON-RPC client for nexusd's control socket (same framing as
 * `crates/nexusd/src/control.rs`). No account, no HTTP, no cloud.
 */
export class NexusControlClient {
  constructor({ socketPath = resolveSocketPath(), timeoutMs = 2000 } = {}) {
    this.socketPath = socketPath
    this.timeoutMs = timeoutMs
    this.nextId = 0
  }

  call(method, params = {}) {
    const id = ++this.nextId
    const payload = JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n"

    return new Promise((resolve, reject) => {
      const socket = net.createConnection(this.socketPath)
      let buffer = ""
      let settled = false

      const finish = (fn, value) => {
        if (settled) return
        settled = true
        clearTimeout(timer)
        socket.destroy()
        fn(value)
      }

      const timer = setTimeout(() => {
        finish(reject, new Error(`nexusd control call timed out: ${method}`))
      }, this.timeoutMs)

      socket.setEncoding("utf8")
      socket.on("connect", () => socket.write(payload))
      socket.on("data", (chunk) => {
        buffer += chunk
        const newline = buffer.indexOf("\n")
        if (newline === -1) return

        let response
        try {
          response = JSON.parse(buffer.slice(0, newline))
        } catch (error) {
          finish(reject, new Error(`invalid control response: ${error.message}`))
          return
        }
        if (response.error) {
          finish(reject, new Error(response.error.message || "control error"))
          return
        }
        finish(resolve, response.result)
      })
      socket.on("error", (error) => finish(reject, error))
      socket.on("close", () => {
        if (!settled) finish(reject, new Error(`nexusd control socket closed: ${method}`))
      })
    })
  }
}
