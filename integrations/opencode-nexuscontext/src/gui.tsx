import { For, Show, createEffect, createSignal } from "solid-js"
import {
  Command,
  MenuItem,
  Panel,
  Plugin,
  usePlugin,
  type PanelTab,
} from "@opencode/plugin/gui"
import { Nexus } from "./rpc.js"
import { TAB_HELP, formatError, formatTab } from "./panel-data.js"

/**
 * Experimental OpenCode Desktop panel. It is a thin view over the same typed
 * RPC contract as the TUI panel: no socket, no recall logic, no approval
 * logic. A daemon-less host renders an explicit error instead of empty data.
 */
type SessionRef = { id: string; directory: string }

const TABS = [
  "status",
  "sources",
  "projects",
  "search",
  "review",
  "architecture",
  "graph",
  "stats",
  "logs",
] as const
type Tab = (typeof TABS)[number]

// Panel tabs are per session, so switching sessions cannot silently switch
// what the user is looking at.
const [activeBySession, setActiveBySession] = createSignal<Record<string, string>>({})
const tabOf = (sessionID: string): Tab => {
  const value = activeBySession()[sessionID]
  return TABS.includes(value as Tab) ? (value as Tab) : "status"
}
const selectTab = (sessionID: string, tab: Tab) =>
  setActiveBySession((previous) => ({ ...previous, [sessionID]: tab }))

function NexusView(props: { session: () => SessionRef }) {
  const ctx = usePlugin()
  const [lines, setLines] = createSignal<string[]>(["Loading…"])
  const [message, setMessage] = createSignal("")
  const [query, setQuery] = createSignal("")
  const [confirmation, setConfirmation] = createSignal("")
  const tab = () => tabOf(props.session().id)
  const repo = () => props.session().directory

  const load = async () => {
    const current = tab()
    if ((current === "search" || current === "graph") && !query().trim()) {
      setLines([
        current === "search"
          ? "Type a pattern above, then press Refresh (or change the tab)."
          : "Type a function name above, then press Refresh (or change the tab).",
      ])
      return
    }
    setLines(["Loading…"])
    setMessage("")
    try {
      // Read ctx.client at call time: it changes after a server restart.
      const client = ctx.client.rpc(Nexus)
      let data: unknown
      switch (current) {
        case "status":
          data = await client.status({})
          break
        case "sources":
          data = await client.sources({})
          break
        case "projects":
          data = await client.projects({})
          break
        case "search":
          data = await client.search({ repo_path: repo(), pattern: query(), limit: 50 })
          break
        case "review":
          data = await client.review({ repo_path: repo(), limit: 50 })
          break
        case "architecture":
          data = await client.architecture({ repo_path: repo() })
          break
        case "graph":
          data = await client.graph({
            repo_path: repo(),
            function_name: query(),
            direction: "outbound" as const,
            depth: 3,
          })
          break
        case "stats":
          data = await client.stats({})
          break
        case "logs":
          data = await client.logs({ limit: 200 })
          break
      }
      setLines(
        data && typeof data === "object" && "error_type" in data
          ? formatError(data)
          : formatTab(current, data),
      )
    } catch (error) {
      setLines(
        formatError({ message: error instanceof Error ? error.message : String(error) }),
      )
    }
  }

  createEffect(() => {
    tab()
    query()
    repo()
    void load()
  })

  const act = async (name: "reindex" | "import" | "export" | "remove") => {
    const target = repo().trim()
    if (!target) {
      setMessage("This session has no project directory.")
      return
    }
    if (name === "remove" && confirmation() !== "delete") {
      setMessage('Removal requires typing "delete" in the confirmation field.')
      return
    }
    try {
      const client = ctx.client.rpc(Nexus)
      let result: unknown
      switch (name) {
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
          result = await client.removeProject({ repo_path: target, confirm: "delete" as const })
          break
      }
      if (result && typeof result === "object" && "error_type" in result) {
        setMessage(formatError(result).slice(-2).join(" · "))
        return
      }
      setConfirmation("")
      setMessage(`${name}: ${(result as { status?: string })?.status ?? "ok"}`)
      selectTab(props.session().id, "projects")
    } catch (error) {
      setMessage(`action failed: ${error instanceof Error ? error.message : String(error)}`)
    }
  }

  return (
    <div style={{ display: "flex", "flex-direction": "column", gap: "8px", padding: "8px" }}>
      <div role="tablist" style={{ display: "flex", "flex-wrap": "wrap", gap: "4px" }}>
        <For each={TABS}>
          {(item) => (
            <button
              type="button"
              role="tab"
              aria-selected={tab() === item}
              onClick={() => selectTab(props.session().id, item)}
            >
              {item}
            </button>
          )}
        </For>
      </div>
      <Show when={tab() === "search" || tab() === "graph"}>
        <input
          aria-label="query"
          placeholder={tab() === "search" ? "search pattern" : "function name"}
          value={query()}
          onInput={(event) => setQuery(event.currentTarget.value)}
        />
      </Show>
      <pre style={{ margin: "0", "white-space": "pre-wrap" }}>
        <For each={lines()}>{(line) => <div>{line}</div>}</For>
      </pre>
      <Show when={message() !== ""}>
        <div role="status">{message()}</div>
      </Show>
      <div style={{ display: "flex", "flex-wrap": "wrap", gap: "4px" }}>
        <button type="button" onClick={() => void act("reindex")}>Reindex</button>
        <button type="button" onClick={() => void act("import")}>Import</button>
        <button type="button" onClick={() => void act("export")}>Export</button>
        <input
          aria-label='type "delete" to confirm removal'
          placeholder='type "delete" to remove'
          value={confirmation()}
          onInput={(event) => setConfirmation(event.currentTarget.value)}
        />
        <button type="button" onClick={() => void act("remove")}>Remove project</button>
      </div>
      <div>{TAB_HELP}</div>
    </div>
  )
}

export default Plugin.define({
  i18n: {
    en: {
      title: "NexusContext",
      "command.open": "Open NexusContext",
      "command.group": "NexusContext",
      "panel.title": "NexusContext",
      "menu.open": "Open NexusContext",
    },
  },
  setup(ctx) {
    const key = `${ctx.id}:context`
    const contextTab: PanelTab = {
      id: "context",
      get title() {
        return ctx.t("panel.title")
      },
      closable: "compact",
      fallback: true,
    }

    const contributions = [
      ctx.add(Command, (): Command => ({
        id: "open",
        title: ctx.t("command.open"),
        keywords: "nexus context sources projects search graph logs",
        group: ctx.t("command.group"),
        slash: { name: "nexus", arguments: true },
        run: (input) => {
          const session = ctx.sessions.current()
          if (!session) return
          const requested = (input ?? "").trim().split(/\s+/)[0]?.toLowerCase() ?? ""
          if (TABS.includes(requested as Tab)) selectTab(session.id, requested as Tab)
          ctx.layout.open(key, session)
        },
      })),
      ctx.add(MenuItem, (): MenuItem => {
        const session = ctx.sessions.current()
        return {
          menu: "session.panel",
          id: "context",
          title: ctx.t("menu.open"),
          icon: "server",
          opened: !!session && ctx.layout.state(key, session) !== "closed",
          run: () => {
            if (session) ctx.layout.open(key, session)
          },
        }
      }),
      ctx.add(Panel, {
        id: "context",
        region: "side",
        list: (input) => (input.open.includes("context") ? [contextTab] : []),
        render: (props) => <NexusView session={() => props.session} />,
      }),
    ]

    return () => {
      for (const dispose of contributions) dispose()
    }
  },
})
