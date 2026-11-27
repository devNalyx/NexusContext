import { Plugin, usePlugin } from "@opencode/plugin/tui"
import { For, Show, createEffect, createSignal } from "solid-js"
import { Nexus } from "./rpc.js"
import { PANEL_NAME, TAB_HELP, TAB_METHODS, formatTab } from "./panel-data.js"

/**
 * TUI contributions: a read-only session panel over the typed server RPC,
 * plus the existing footer ledger status. Neither part opens the daemon
 * socket itself — `context.client` follows the server across restarts.
 */
function inputFor(tab: string, repoPath: string, query: string) {
  switch (tab) {
    case "search":
      return { repo_path: repoPath, pattern: query, limit: 20 }
    case "architecture":
      return { repo_path: repoPath }
    case "graph":
      return { repo_path: repoPath, function_name: query, direction: "outbound", depth: 3 }
    case "review":
      return { repo_path: repoPath, limit: 50 }
    default:
      return {}
  }
}

function NexusPanel(props: {
  tab: () => string
  query: () => string
  repoPath: () => string
}) {
  const context = usePlugin()
  const [lines, setLines] = createSignal<string[]>(["Loading…"])
  const [message, setMessage] = createSignal("")

  const load = async () => {
    const tab = props.tab()
    if ((tab === "search" || tab === "graph") && !props.query().trim()) {
      setLines([
        tab === "search"
          ? "Usage: /nexus search <pattern>"
          : "Usage: /nexus graph <function_name>",
      ])
      return
    }
    setLines(["Loading…"])
    try {
      const client = context.client.rpc(Nexus)
      const data = await client[TAB_METHODS[tab] ?? "status"](
        inputFor(tab, props.repoPath(), props.query()),
      )
      setLines(formatTab(tab, data))
      setMessage("")
    } catch (error) {
      setLines([])
      setMessage(`nexusd unavailable: ${error?.message ?? String(error)}`)
    }
  }

  createEffect(() => {
    props.tab()
    props.query()
    props.repoPath()
    void load()
  })

  return (
    <box>
      <text fg={context.theme.text.base}>{`NexusContext · ${props.tab()}`}</text>
      <text fg={context.theme.text.base}>{`repo: ${props.repoPath()}`}</text>
      <For each={lines()}>{(line) => <text fg={context.theme.text.base}>{line}</text>}</For>
      <Show when={message()}>{message()}</Show>
      <text fg={context.theme.text.base}>{`/nexus ${TAB_HELP}`}</text>
    </box>
  )
}

export default Plugin.define({
  id: "nexuscontext.status",
  setup(context) {
    const [tab, setTab] = createSignal("status")
    const [query, setQuery] = createSignal("")
    const [repoPath, setRepoPath] = createSignal(
      context.location?.directory ?? process.cwd(),
    )
    const [label, setLabel] = createSignal("NC · connecting…")
    let disposed = false

    const toast = (message: string) => context.ui.toast.show({ message })

    const projectAction = async (action: string, input: string) => {
      const parts = input.split(/\s+/).filter(Boolean)
      const confirmIndex = parts.indexOf("--confirm")
      const target = parts
        .filter(
          (part, index) =>
            !part.startsWith("--") &&
            !(confirmIndex >= 0 && index === confirmIndex + 1),
        )
        .join(" ")
      if (!target) {
        toast(`usage: /nexus ${action} <path>`)
        return
      }
      try {
        const client = context.client.rpc(Nexus)
        let result
        switch (action) {
          case "reindex":
            result = await client.reindex({ repo_path: target })
            break
          case "import":
            result = await client.importProject({ repo_path: target })
            break
          case "export":
            result = await client.exportProject({ repo_path: target })
            break
          case "remove":
            if (parts[confirmIndex + 1] !== "delete") {
              toast("removal requires: /nexus remove <path> --confirm delete")
              return
            }
            result = await client.removeProject({ repo_path: target, confirm: "delete" })
            break
          default:
            return
        }
        toast(`${action}: ${result?.status ?? "ok"}`)
        setTab("projects")
      } catch (error) {
        toast(`${action} failed: ${error?.message ?? String(error)}`)
      }
    }

    const unlayer = context.keymap.layer(() => ({
      mode: "global",
      commands: [
        {
          id: "nexuscontext.panel",
          title: "Open NexusContext panel",
          group: "NexusContext",
          slash: { name: "nexus", aliases: ["nc"], arguments: true },
          run: async (input: unknown) => {
            const parts = (
              typeof input === "string" ? input.trim() : ""
            )
              .split(/\s+/)
              .filter(Boolean)
            const action = (parts.shift() ?? "").toLowerCase()
            const rest = parts.join(" ")

            if (TAB_METHODS[action]) {
              setTab(action)
              context.ui.panel.open(PANEL_NAME)
            } else if (action === "path") {
              if (rest) setRepoPath(rest)
              context.ui.panel.open(PANEL_NAME)
            } else if (action === "search" || action === "graph") {
              setTab(action)
              setQuery(rest)
              context.ui.panel.open(PANEL_NAME)
            } else if (["reindex", "import", "export", "remove"].includes(action)) {
              await projectAction(action, rest)
            } else if (!action) {
              context.ui.panel.open(PANEL_NAME)
            } else {
              toast(`unknown /nexus action: ${action} · ${TAB_HELP}`)
            }
          },
        },
      ],
    }))

    const unslot = context.ui.slot({
      append: "session.panel",
      render: (panel) => (
        <Show when={panel.name === PANEL_NAME}>
          <NexusPanel
            tab={() => tab()}
            query={() => query()}
            repoPath={() => repoPath()}
          />
        </Show>
      ),
    })

    const poll = async () => {
      if (disposed) return
      try {
        // Read context.client at call time: it changes after a server restart.
        const summary = await context.client.rpc(Nexus).ledger({})
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
    const unfooter = context.ui.slot({
      append: "home.footer.status",
      render: () => <text fg={context.theme.text.base}>{label()}</text>,
    })

    return () => {
      disposed = true
      clearInterval(timer)
      unslot()
      unfooter()
      if (typeof unlayer === "function") unlayer()
    }
  },
})
