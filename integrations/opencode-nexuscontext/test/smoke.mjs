import assert from "node:assert/strict"
import fs from "node:fs"
import net from "node:net"
import os from "node:os"
import path from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..")
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "nexus-opencode-"))
const socketPath = path.join(tmp, "runtime", "nexuscontext", "nexuscontext.sock")
fs.mkdirSync(path.dirname(socketPath), { recursive: true })
process.env.NEXUS_CONTROL_SOCKET = socketPath

let server
try {
  // Mimic a local plugin under .opencode/plugins/ with a stubbed SDK. No
  // OpenCode install, no npm install, no network: this exercises the real
  // handlers against a fake nexusd control socket.
  const pluginDir = path.join(tmp, ".opencode", "plugins", "opencode-nexuscontext")
  const sdkDir = path.join(pluginDir, "node_modules", "@opencode", "plugin")
  fs.mkdirSync(sdkDir, { recursive: true })
  fs.copyFileSync(path.join(root, "package.json"), path.join(pluginDir, "package.json"))
  fs.cpSync(path.join(root, "src"), path.join(pluginDir, "src"), { recursive: true })
  fs.writeFileSync(
    path.join(sdkDir, "package.json"),
    JSON.stringify({
      name: "@opencode/plugin",
      type: "module",
      exports: { ".": "./index.js", "./rpc": "./rpc.js" },
    }),
  )
  fs.writeFileSync(
    path.join(sdkDir, "index.js"),
    "export const Plugin = { define: (definition) => definition }\n",
  )
  fs.writeFileSync(
    path.join(sdkDir, "rpc.js"),
    "export const Rpc = { define: (definition) => ({ ...definition, contract: true }) }\n",
  )

  const received = []
  let rpcRegistration
  const INJECTED_TEXT = "## NexusContext verified memory\n- [decision] owner: alice (src/lib.rs)\n### Approved skills (procedural memory, not repo-anchored)\n- [skill] rotate a key — revoke first (load with list_skills)\n"
  const responses = {
    "ledger.inject": {
      text: INJECTED_TEXT,
      bytes: Buffer.byteLength(INJECTED_TEXT),
      estimated_tokens: 30,
      budget_tokens: 800,
      fact_ids: ["f1"],
      skill_ids: ["s1"],
    },
    "ledger.tool": { event_id: 1, source: "estimate" },
    "ledger.usage": { event_id: 2, source: "harness" },
  }
  server = net.createServer((socket) => {
    socket.setEncoding("utf8")
    socket.on("data", (chunk) => {
      const request = JSON.parse(chunk.toString().trim())
      received.push(request)
      socket.write(
        JSON.stringify({
          jsonrpc: "2.0",
          id: request.id,
          result: responses[request.method] ?? { echoed: request.method },
        }) + "\n",
      )
    })
  })
  await new Promise((resolve) => server.listen(socketPath, resolve))

  const plugin = await import(
    pathToFileURL(path.join(pluginDir, "src", "index.js")).href + `?t=${Date.now()}`
  ).then((module) => module.default)

  assert.equal(plugin.id, "nexuscontext.brain")
  assert.equal(typeof plugin.setup, "function")

  const sessionHooks = new Map()
  const toolHooks = new Map()
  const stored = {}
  const entries = []
  let disposed = 0
  const usageEvents = [
    {
      type: "session.usage.recorded",
      data: {
        sessionID: "s1",
        source: "generate",
        tokens: { input: 1200, output: 80, reasoning: 0, cache: { read: 20, write: 5 } },
        cost: 0.0125,
      },
    },
  ]
  const ctx = {
    options: { budgetTokens: 800 },
    location: { directory: tmp },
    storage: {
      set: async (key, value) => {
        stored[key] = value
      },
    },
    rpc: {
      register: async (contract, implementations) => {
        rpcRegistration = { contract, implementations }
        return { dispose: async () => void disposed++ }
      },
    },
    session: {
      hook: async (name, callback) => {
        sessionHooks.set(name, callback)
        return { dispose: async () => void disposed++ }
      },
      instructions: {
        entry: {
          put: async (input) => entries.push(input),
          list: async () => [],
          remove: async () => {},
        },
      },
    },
    tool: {
      hook: async (name, callback) => {
        toolHooks.set(name, callback)
        return { dispose: async () => void disposed++ }
      },
    },
    event: {
      subscribe: function* ({ signal }) {
        usageAborted = signal
        for (const event of usageEvents) yield event
      },
    },
  }
  let usageAborted

  const cleanup = await plugin.setup(ctx)
  assert.deepEqual([...sessionHooks.keys()], ["prompt", "context", "compaction"])
  assert.deepEqual([...toolHooks.keys()], ["execute.before", "execute.after"])
  assert.equal(stored["nexuscontrol.budgetTokens"], 800)
  assert.equal(stored["nexuscontrol.socket"], socketPath)
  assert.ok(usageAborted, "usage stream should be subscribed")

  // The panel-facing RPC is registered on the server part, not opened
  // directly by the views. Every method maps to one control method, and the
  // destructive one requires an explicit human confirmation token.
  assert.ok(rpcRegistration, "RPC contract should be registered")
  assert.equal(rpcRegistration.contract.id, "nexuscontext")
  assert.deepEqual(Object.keys(rpcRegistration.implementations), [
    "status",
    "sources",
    "projects",
    "search",
    "architecture",
    "graph",
    "review",
    "stats",
    "logs",
    "ledger",
    "reindex",
    "importProject",
    "exportProject",
    "removeProject",
  ])
  const rpcContext = {
    error: (type, message, data) => ({ error_type: type, message, data }),
  }
  assert.equal(
    (await rpcRegistration.implementations.sources({}, rpcContext)).echoed,
    "sources.list",
  )
  assert.equal(
    (await rpcRegistration.implementations.logs({ limit: 10 }, rpcContext)).echoed,
    "logs.tail",
  )
  assert.equal(
    (
      await rpcRegistration.implementations.removeProject(
        { repo_path: "/tmp/project" },
        rpcContext,
      )
    ).error_type,
    "confirmation_required",
  )
  assert.equal(
    (
      await rpcRegistration.implementations.removeProject(
        { repo_path: "/tmp/project", confirm: "delete" },
        rpcContext,
      )
    ).echoed,
    "projects.delete",
  )

  // The prompt hook puts the entry before the system snapshot is assembled.
  // A real OpenCode v2.4.1 run showed that doing this only in `context`
  // produced a ledger entry the agent never received, so this ordering is
  // part of the contract, not an implementation detail.
  await sessionHooks.get("prompt")({ sessionID: "s1", prompt: { text: "ledger owner" } })
  assert.equal(entries.length, 1)
  assert.equal(entries[0].key, "nexuscontext.memory")
  assert.equal(entries[0].value, INJECTED_TEXT)
  const injection = received.find((r) => r.method === "ledger.inject")
  assert.ok(injection, "ledger.inject was called from the prompt hook")
  assert.equal(injection.params.query, "ledger owner")
  assert.equal(injection.params.budget_tokens, 800)
  assert.equal(injection.params.repo_path, tmp)

  // context must not re-inject a turn the prompt hook already handled, or
  // the same text would be ledger-recorded twice for one model call.
  await sessionHooks.get("context")({ sessionID: "s1" })
  assert.equal(entries.length, 1)
  assert.equal(received.filter((r) => r.method === "ledger.inject").length, 1)

  // compaction refreshes the same key; it must not append or re-run recall.
  await sessionHooks.get("compaction")({ sessionID: "s1" })
  assert.equal(entries.length, 2)
  assert.equal(entries[1].key, "nexuscontext.memory")
  assert.equal(entries[1].value, INJECTED_TEXT)
  assert.equal(received.filter((r) => r.method === "ledger.inject").length, 1)

  // Tool telemetry is bytes-only and labelled by the daemon as an estimate.
  toolHooks.get("execute.before")({ tool: "read", input: {} })
  await toolHooks.get("execute.after")({
    sessionID: "s1",
    tool: "read",
    status: "completed",
    result: { content: "hello" },
  })
  const toolCall = received.find((r) => r.method === "ledger.tool")
  assert.ok(toolCall, "ledger.tool was called")
  assert.equal(toolCall.params.status, "completed")
  assert.equal(toolCall.params.tool, "read")
  assert.ok(toolCall.params.bytes > 0)

  // Harness-reported tokens/cost arrive verbatim, separate from estimates.
  await new Promise((resolve) => setImmediate(resolve))
  const usage = received.find((r) => r.method === "ledger.usage")
  assert.ok(usage, "ledger.usage was called from the event stream")
  assert.equal(usage.params.tokens.input, 1200)
  assert.equal(usage.params.cost_usd, 0.0125)

  await cleanup()
  assert.equal(disposed, 6)
  assert.ok(usageAborted.aborted, "usage stream should be aborted on cleanup")

  // Socket resolution must match crates/nexus-core/src/paths.rs exactly.
  const { resolveSocketPath } = await import(
    pathToFileURL(path.join(root, "src", "control.js")).href
  )
  assert.equal(
    resolveSocketPath({ XDG_RUNTIME_DIR: "/run/user/1000" }),
    path.join("/run/user/1000", "nexuscontext", "nexuscontext.sock"),
  )
  assert.equal(
    resolveSocketPath({ NEXUS_CACHE_DIR: "/tmp/nc-data" }),
    path.join("/tmp/nc-data", "nexuscontext.sock"),
  )

  // Panel formatters are pure and testable without an OpenCode runtime.
  const { formatTab, TAB_METHODS } = await import(
    pathToFileURL(path.join(root, "src", "panel-data.js")).href
  )
  assert.ok(TAB_METHODS.graph === "graph" && TAB_METHODS.logs === "logs")
  const sourceLines = formatTab("sources", {
    sources: [{ id: "secondbrain", kind: "obsidian-vault", root_path: "/vault" }],
    note: "provenance only",
  })
  assert.ok(sourceLines.some((line) => line.includes("secondbrain")))
  assert.ok(sourceLines.some((line) => line.includes("provenance only")))
  assert.deepEqual(formatTab("logs", { lines: ["a", "b"], truncated: true }).slice(-2), [
    "a",
    "b",
  ])

  console.log("opencode-nexuscontext hook + ledger + RPC panel smoke test: ok")
} finally {
  if (server) await new Promise((resolve) => server.close(resolve))
  fs.rmSync(tmp, { recursive: true, force: true })
}
