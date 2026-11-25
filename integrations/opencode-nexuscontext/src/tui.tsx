import { Plugin } from "@opencode/plugin/tui"
import { createSignal } from "solid-js"
import { NexusControlClient, resolveSocketPath } from "./control.js"

/**
 * TUI status contribution. Purely read-only: it polls the daemon's ledger and
 * shows how much bounded NC context is in play alongside the harness's own
 * provider-reported tokens. No writes, no account, no network.
 */
export default Plugin.define({
  id: "nexuscontext.status",
  setup(context) {
    const control = new NexusControlClient({ socketPath: resolveSocketPath() })
    const [label, setLabel] = createSignal("NC · connecting…")
    let disposed = false

    const poll = async () => {
      if (disposed) return
      try {
        const summary = await control.call("ledger.latest", {})
        const estimate = summary?.estimate?.injected_tokens ?? 0
        const harness = summary?.harness
        const input = harness?.tokens?.input
        const cost = harness?.cost_usd
        if (typeof input === "number" && input > 0) {
          const usd = typeof cost === "number" ? ` · $${cost.toFixed(4)}` : ""
          setLabel(`NC · ~${estimate}/800 est · ${input} in${usd}`)
        } else {
          setLabel(`NC · ~${estimate}/800 est`)
        }
      } catch {
        setLabel("NC · offline")
      }
    }

    void poll()
    const timer = setInterval(() => void poll(), 5000)
    const unslot = context.ui.slot({
      append: "home.footer.status",
      render: () => <text fg={context.theme.text.base}>{label()}</text>,
    })

    return () => {
      disposed = true
      clearInterval(timer)
      unslot()
    }
  },
})
