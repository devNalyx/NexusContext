/**
 * Pure formatters shared by the OpenCode TUI/GUI panels and their tests.
 * They turn control/RPC results into plain lines, so the view layer stays a
 * thin renderer and formatting can be verified without an OpenCode runtime.
 */

const text = (value) =>
  value === null || value === undefined || value === "" ? "—" : String(value)

const heading = (title) => [title, "─".repeat(Math.max(12, title.length))]

const jsonLines = (value) => JSON.stringify(value ?? {}, null, 2).split("\n")

const count = (value) => (Array.isArray(value) ? value.length : 0)

function formatStatus(data) {
  const d = data ?? {}
  const watch = d.watch_budget ?? {}
  const indexing = d.indexing ?? {}
  return [
    ...heading("NexusContext · daemon status"),
    `version: ${text(d.version)}`,
    `data_dir: ${text(d.data_dir)}`,
    `log_file: ${text(d.log_file)}`,
    `projects: ${text(d.projects_indexed)} indexed · ${text(d.projects_watched)} watched`,
    `watch budget: ${text(watch.estimated_watches_used)}/${text(watch.estimated_watches_budget)} · pressure ${text(watch.pressure_events)} · queue ${text(watch.queue_depth)}/${text(watch.queue_bound)} (full ${text(watch.queue_full_events)})`,
    `indexing: active=${text(indexing.active)} completed=${text(indexing.completed_count)} superseded=${text(indexing.superseded_count)}`,
    `rss_kb: ${text(d.rss_kb)}`,
  ]
}

function formatSources(data) {
  const d = data ?? {}
  const sources = Array.isArray(d.sources) ? d.sources : []
  const lines = [...heading("Registered sources · provenance only, not fact approval")]
  if (sources.length === 0) {
    lines.push("(none) — register with: nexus source add <id> <root> --kind <kind>")
  } else {
    for (const source of sources) {
      lines.push(
        `${text(source.id)} · ${text(source.kind)} · ${text(source.root_path)}`,
        `    ${text(source.resource)} — ${text(source.title)}`,
      )
    }
  }
  lines.push(text(d.note))
  return lines
}

function formatProjects(projects) {
  const rows = Array.isArray(projects) ? projects : []
  const lines = [...heading(`Indexed projects · ${rows.length}`)]
  if (rows.length === 0) lines.push("(none) — index with: nexus index <path>")
  for (const p of rows) {
    lines.push(
      `${text(p.root_path)}`,
      `    nodes=${text(p.nodes)} edges=${text(p.edges)} disk_bytes=${text(p.disk_bytes)} hash=${text(p.hash)}`,
      `    last_indexed=${text(p.last_indexed_unix)} last_queried=${text(p.last_queried_unix)}${p.source_id ? ` source=${text(p.source_id)} (${text(p.source_kind)})` : ""}`,
    )
  }
  lines.push(
    "actions: /nexus reindex <path> · import <path> · export <path> · remove <path> --confirm delete",
  )
  return lines
}

function formatSearch(results) {
  const rows = Array.isArray(results) ? results : []
  const lines = [...heading(`Search · ${rows.length} result(s)`)]
  if (rows.length === 0) lines.push("(none) — /nexus search <pattern>")
  for (const n of rows) {
    lines.push(
      `[${text(n.kind)}] ${text(n.name)} — ${text(n.file)}:${text(n.start_line)}-${text(n.end_line)}`,
    )
  }
  return lines
}

function formatReview(data) {
  const d = data ?? {}
  const candidates = count(d.skill_candidates)
  const promoted = Array.isArray(d.promoted_skills) ? d.promoted_skills : []
  return [
    ...heading("Review queue · read-only"),
    `stale facts: ${text(d.fact_stale_count)}`,
    `skill candidates: ${candidates}`,
    `promoted skills: ${promoted.length}`,
    ...promoted.slice(0, 20).map((s) => `    ${text(s.name)} — ${text(s.freshness_label)}`),
    text(d.note),
    text(d.labels?.skill),
  ]
}

function formatArchitecture(data) {
  const d = data ?? {}
  const busiest = Array.isArray(d.busiest_files) ? d.busiest_files : []
  const languages = Array.isArray(d.language_breakdown) ? d.language_breakdown : []
  return [
    ...heading("Architecture"),
    `nodes=${text(d.total_nodes)} edges=${text(d.total_edges)} last_indexed=${text(d.last_indexed_unix)}`,
    "busiest files:",
    ...busiest.slice(0, 10).map((f) => `    ${text(f.file)} — ${text(f.definitions)} definitions`),
    "languages:",
    ...languages.slice(0, 10).map((l) => `    ${text(l.extension)} — ${text(l.files)} files`),
  ]
}

const statsRows = (rows) =>
  (Array.isArray(rows) ? rows : [])
    .slice()
    .sort((a, b) => (b.call_count ?? 0) - (a.call_count ?? 0))
    .slice(0, 10)
    .map(
      (s) =>
        `    ${text(s.name)} — calls=${text(s.call_count)} errors=${text(s.error_count)} avg=${text(s.avg_latency_ms)}ms bytes=${text(s.total_output_bytes)}`,
    )

function formatStats(data) {
  const d = data ?? {}
  const reindex = d.reindex ?? {}
  return [
    ...heading("Usage · lifetime aggregates"),
    "MCP tools:",
    ...statsRows(d.mcp_tools),
    "control methods:",
    ...statsRows(d.control_methods),
    `auto-reindex: ${text(reindex.total_auto_reindex_count)} runs · ${text(reindex.total_auto_reindex_fail_count)} failed · avg ${text(reindex.avg_auto_reindex_ms)}ms`,
    `collecting_since=${text(d.collecting_since_unix)}`,
  ]
}

function formatGraph(data) {
  const dot = typeof data?.dot === "string" ? data.dot : ""
  const lines = dot.split("\n")
  const head = [...heading("Call graph · DOT (render with Graphviz if needed)")]
  if (lines.length === 0 || dot === "") return [...head, "(empty graph)"]
  head.push(...lines.slice(0, 80))
  if (lines.length > 80) head.push(`… ${lines.length - 80} more line(s) omitted`)
  return head
}

function formatLogs(data) {
  const d = data ?? {}
  const lines = Array.isArray(d.lines) ? d.lines : []
  return [
    ...heading(`Logs · ${lines.length} line(s)`),
    `path: ${text(d.path)}${d.truncated ? " (tail)" : ""}`,
    ...lines,
  ]
}

function formatLedger(data) {
  const d = data ?? {}
  const estimate = d.estimate ?? {}
  const harness = d.harness ?? {}
  return [
    ...heading("Session context ledger"),
    `session: ${text(d.session_id)}`,
    `NC estimate: ${text(estimate.injected_tokens)} tokens (${text(estimate.token_source)})`,
    `harness: ${text(harness.token_source)} available=${text(harness.available)}`,
    `provider input tokens: ${text(harness.tokens?.input)} · cost_usd: ${text(harness.cost_usd)}`,
  ]
}

export const TAB_METHODS = {
  status: "status",
  sources: "sources",
  projects: "projects",
  search: "search",
  review: "review",
  architecture: "architecture",
  graph: "graph",
  stats: "stats",
  logs: "logs",
}

const FORMATTERS = {
  status: formatStatus,
  sources: formatSources,
  projects: formatProjects,
  search: formatSearch,
  review: formatReview,
  architecture: formatArchitecture,
  graph: formatGraph,
  stats: formatStats,
  logs: formatLogs,
}

export const PANEL_NAME = "nexuscontext.context"

export const TAB_HELP = Object.keys(TAB_METHODS).join(" · ")

/** Format an RPC result for a tab. Unknown data falls back to JSON. */
export function formatTab(tab, data) {
  const formatter = FORMATTERS[tab]
  return formatter ? formatter(data) : jsonLines(data)
}
