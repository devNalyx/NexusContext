# NexusContext

A self-hosted, local-first MCP daemon that gives AI coding agents a structural
knowledge graph over a codebase — functions, types, and call edges via
tree-sitter, plus markdown docs as a heading graph — with full-text search
on top. No LLM runs inside the daemon: it builds structure and answers
queries; the calling agent still does all the reasoning.

This is a real, working product (not a proposal), currently at **v0.1.17**,
released for Linux and Windows (x86_64 + arm64) and macOS (Apple Silicon)
— see [[Known-Limitations]] for what's tiered by platform.
This vault documents its *current state* by topic, cross-linked. For the
blow-by-blow build history (33 phases, every bug found and how it was
actually fixed, verified against a real dogfooded daemon rather than just
code review), see `README.md` at the repo root — this vault deliberately
doesn't duplicate that; it's the "what is true right now" layer on top of
"how we got here." [[ADRs/README|ADRs]] is a third layer on top of both:
the compressed "why we decided this, and what it commits future work to"
for the handful of decisions worth defending later.

## Start here

- [[Architecture]] — the daemon, its two transports, and how the pieces fit
  together.
- [[MCP-Tools]] — the 12 tools an agent can call, grouped, and why they're
  gated behind presets.
- [[Indexing-Pipeline]] — how a codebase becomes a graph: tree-sitter,
  two-pass call resolution, per-language quality tiers.

## By topic

- [[Architecture]] — daemon/transport/component overview.
- [[MCP-Tools]] — the full tool reference and the token-budget presets.
- [[Indexing-Pipeline]] — tree-sitter parsing, call resolution, markdown docs.
- [[Language-Support]] — which of the 11 languages get full call graphs vs.
  structural-only.
- [[Watcher-and-Freshness]] — auto-sync, warm/cold projects, the inotify
  watch budget.
- [[Storage-and-Data-Model]] — the SQLite graph, the registry, on-disk
  layout.
- [[Configuration]] — the full `config.toml` reference.
- [[Security-Model]] — what's opt-in, what's blocked by default, and what
  a review pass found and fixed.
- [[CLI-Reference]] — `nexus` subcommands.
- [[GUI-and-Extension]] — the GTK4 Manager app and the GNOME Shell
  indicator.
- [[Known-Limitations]] — stated plainly, not smoothed over.
- [[ADRs/README|ADRs]] — the architectural decisions that shaped the above,
  why each was made, and what it commits future work to.

## The one-sentence pitch, held to honestly

Every tool here — `search_graph`, `trace_call_path`, `get_architecture`,
`search_code`, `query_planner`, and the rest — is structural: name/graph/
full-text search over what tree-sitter actually parsed, not an embedding
model's guess at similarity. That's a deliberate, graph-first bet, not a
missing feature — see [[ADRs/README|ADRs]] 0002/0007/0010 for the history
of the (now-removed) optional embeddings layer and why it didn't earn its
keep.
