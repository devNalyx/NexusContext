import { Plugin } from "@opencode/plugin"
import { NexusControlClient, resolveSocketPath } from "./control.js"

const DEFAULTS = {
  budgetTokens: 800,
  autoInject: true,
}

const ENTRY_KEY = "nexuscontext.memory"
const MAX_QUERY_CHARS = 2000

/**
 * OpenCode V2 server part of the flagship plug.
 *
 * There is no LLM anywhere here and no decision logic about what is worth
 * remembering: the daemon runs the deterministic recall gate, renders the
 * exact bounded text, and records the bytes/4 estimate in its ledger. This
 * adapter only transports that text into keyed instruction entries and feeds
 * back the token/cost numbers the harness itself reported.
 *
 * All daemon calls are best-effort. A missing or slow daemon must never break
 * an agent session — it just means no NC context on that step.
 */
export default Plugin.define({
  id: "nexuscontext.brain",
  async setup(ctx) {
    const options = { ...DEFAULTS, ...(ctx.options ?? {}) }
    const socketPath = options.socket ?? resolveSocketPath()
    const control = new NexusControlClient({ socketPath })
    const repoPath = options.repoPath ?? ctx.location?.directory ?? process.cwd()

    // Latest user text per session, used only to rank curated facts. Never
    // stored in memory, never sent anywhere except the local daemon.
    const queries = new Map()
    // Last text the daemon rendered, so compaction refreshes one keyed entry
    // instead of appending another block to context.
    const injected = new Map()
    // Bounded tool-name set for local status/debugging.
    const observedTools = new Set()

    await ctx.storage.set("nexuscontrol.socket", socketPath)
    await ctx.storage.set("nexuscontrol.budgetTokens", options.budgetTokens)

    const remember = (sessionID, kind, value) => {
      if (!sessionID) return
      const map = kind === "query" ? queries : injected
      if (map.size > 200) {
        const oldest = map.keys().next().value
        if (oldest !== undefined) map.delete(oldest)
      }
      map.set(sessionID, value)
    }

    const call = async (method, params) => {
      try {
        return await control.call(method, params)
      } catch (error) {
        console.warn(`[nexuscontext] ${method} skipped: ${error.message}`)
        return null
      }
    }

    const putEntry = async (sessionID, text) => {
      if (!sessionID || !text) return
      try {
        await ctx.session.instructions.entry.put({
          sessionID,
          key: ENTRY_KEY,
          value: text,
        })
      } catch (error) {
        console.warn(`[nexuscontext] instruction put skipped: ${error.message}`)
      }
    }

    const refresh = async (sessionID) => {
      if (!options.autoInject || !sessionID) return
      const result = await call("ledger.inject", {
        session_id: sessionID,
        repo_path: repoPath,
        query: queries.get(sessionID) ?? "",
        budget_tokens: options.budgetTokens,
      })
      if (!result?.text) return
      remember(sessionID, "injection", result.text)
      await putEntry(sessionID, result.text)
    }

    const registrations = []

    registrations.push(
      await ctx.session.hook("prompt", (event) => {
        const sessionID = event.sessionID
        const text = event.prompt?.text
        if (typeof text === "string" && text.length > 0) {
          remember(sessionID, "query", text.slice(0, MAX_QUERY_CHARS))
        }
      }),
    )

    registrations.push(await ctx.session.hook("context", (event) => refresh(event.sessionID)))

    // Compaction replaces the old transcript with a summary. Re-putting the
    // same keyed entry is idempotent, so NC context cannot accumulate across
    // compactions the way appended system messages would.
    registrations.push(
      await ctx.session.hook("compaction", async (event) => {
        const cached = injected.get(event.sessionID)
        if (cached) {
          await putEntry(event.sessionID, cached)
        } else {
          await refresh(event.sessionID)
        }
      }),
    )

    registrations.push(
      await ctx.tool.hook("execute.before", (event) => {
        const tool = event.tool ?? event.toolName ?? event.name
        if (typeof tool === "string") {
          if (observedTools.size > 100) observedTools.clear()
          observedTools.add(tool)
        }
      }),
    )

    registrations.push(
      await ctx.tool.hook("execute.after", async (event) => {
        const sessionID = event.sessionID ?? event.context?.sessionID
        const tool = event.tool ?? event.toolName ?? event.name ?? "unknown"
        const status = event.status === "error" ? "error" : "completed"
        let bytes = 0
        try {
          bytes = JSON.stringify(event.result ?? event.error ?? {}).length
        } catch {
          bytes = 0
        }
        await call("ledger.tool", {
          session_id: sessionID ?? "unknown",
          tool,
          status,
          bytes,
        })
      }),
    )

    // Real provider-reported numbers from the harness event stream. Only the
    // per-call `recorded` event is consumed (not the cumulative `updated`
    // event), so the ledger never double counts. Estimates stay separate.
    const controller = new AbortController()
    void (async () => {
      try {
        for await (const event of ctx.event.subscribe({
          types: ["session.usage.recorded"],
          signal: controller.signal,
        })) {
          const data = event.data ?? {}
          if (!data.sessionID) continue
          await call("ledger.usage", {
            session_id: data.sessionID,
            tokens: data.tokens ?? {},
            cost_usd: typeof data.cost === "number" ? data.cost : null,
          })
        }
      } catch (error) {
        if (error?.name !== "AbortError") {
          console.warn(`[nexuscontext] usage stream stopped: ${error.message}`)
        }
      }
    })()

    return async () => {
      controller.abort()
      await Promise.all(registrations.map((registration) => registration.dispose()))
    }
  },
})
