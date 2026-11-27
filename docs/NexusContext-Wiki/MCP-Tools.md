# MCP Tools

31 tools total: 14 structural tools (including `vault_context`) backed by the
same SQLite graph, 11 absorbed Myelin procedural-memory tools backed by their
own local SQLite store, and 6 curated declarative-memory tools backed by a
third local store with an OKF v0.3 export. One daemon, one catalog, one ledger — no second MCP
server for the agent to pay schema tax on. Every tool caps its response somehow; see
[[Security-Model]] for why that became a deliberate, tested property rather
than an assumption.

## Index & read

| Tool | What it does |
|---|---|
| `index_repository` | Build or rebuild the knowledge graph for a directory. Run this before anything else on a project you haven't indexed yet. `deep: true` also runs LSP-resolved-symbol enrichment (Rust only, opt-in via `[lsp]`) — see [[Storage-and-Data-Model]]. |
| `list_sources` | Read-only list of human-registered external knowledge sources (`nexus source add`), with canonical root, kind and provenance URI (issue #103). Registration is CLI/control-plane only; a source's presence is provenance metadata, never fact approval. |
| `get_file_context` | Read a file, or a bounded line range, from an indexed project. No range + `full=false` (default) returns the first 300 lines with a truncation note. Every branch is now byte-capped, not just line-capped — see [[Security-Model]]. Also accepts the explicit `sources` fan-out below. |
| `vault_context` | Read-only Obsidian-style queries over a registered vault's indexed Markdown: modes `links`, `backlinks`, `tags`, `notes_with_tag` and `map` (bounded by `depth`/`limit`). Hit-level `resolution`/`confidence` labels distinguish exact path, alias, unique-basename and unresolved targets; ambiguous or external targets stay unresolved. Requires `repo_path` or an explicit `sources` selection, and requires `[vault_index] enabled = true` — it errors clearly otherwise. Metadata only: no property values, callout bodies, embeds, Canvas text, formulas or fetched targets. Opt-in (`full` preset or `tools.enabled`); see [[Obsidian-Secondbrain]] and ADR 0021. |
| `get_architecture` | Node/edge counts, busiest files by definition density, language breakdown, plus an `index_freshness` field (last-indexed time, whether the project is currently "warm"). Cached, keyed on the project's `last_indexed_unix`, so repeated calls against an unchanged index skip SQLite entirely — freshness is computed fresh on every call regardless, so it never reads stale from the cache. Default response/cost is unchanged. Optional `grouped=true` (issue #90) adds a directory-based structural breakdown built from every node's existing `file_path` — no new indexing. `depth` (default `1`) controls how many path components form each group's key (e.g. depth `1` groups by top-level directory, depth `2` by the first two components). Response then gains a `grouped` object: `groups` (per-directory `path`, `total_nodes`, and `node_counts` broken down by kind — function/type/etc.), `within_group_edges` (edges whose endpoints share a group), and `cross_group_edges` (edges crossing between two different groups, tallied per `from`/`to` pair — the "which directories actually call into which" signal). This is deliberately structural grouping by path only, not semantic subsystem/layer inference — see ADR 0017. Also accepts the explicit `sources` fan-out below. |

## Explicit `sources` fan-out

Five read tools — `search_code`, `search_graph`, `get_file_context`,
`get_architecture` and `vault_context` — take an optional `sources` argument:
an array of up to 16 registered source IDs or resource URIs (for example
`"sources": ["secondbrain", "obsidian-vault://secondbrain"]`). It is mutually
exclusive with `repo_path`, accepts only enabled registered sources, and is
opt-in per call: there is no implicit federation and no new tool name.
Unknown/disabled/duplicate entries fail rather than being silently dropped.

The result is grouped, with provenance and freshness kept per source:

```json
{
  "source_count": 1,
  "selected_by": ["id"],
  "sources": [
    {
      "id": "secondbrain",
      "kind": "obsidian-vault",
      "resource": "obsidian-vault://secondbrain",
      "root_path": "/absolute/canonical/vault",
      "index_freshness": {
        "indexed": true,
        "mode": "stdio",
        "watcher_active": false,
        "warm": true,
        "note": "warm means recently indexed, not verified-current"
      },
      "data": { "...": "the tool's normal per-source result" }
    }
  ],
  "note": "Explicit fan-out only; no implicit federation. Each result is independently authorized and freshness-labelled."
}
```

See [[Configuration]], [[Obsidian-Secondbrain]] and ADR 0022.

## Search & trace

| Tool | What it does |
|---|---|
| `search_graph` | Structural search over indexed symbols by name substring — functions/types and markdown heading `Section`s. Also accepts the explicit `sources` fan-out above. |
| `search_code` | Grep-like full-text search over indexed file content via SQLite FTS5 — code and markdown alike, matched as a literal phrase. With `[config_index] enabled = true`, YAML/JSON hits also carry `document`, `key_path`, `start_line`/`end_line`, `parse_status` (`parsed`/`partial`/`unsupported`) and a `note`; the response adds `coverage` and the shared stdio `index_freshness` envelope — `mode: stdio`, `watcher_active: false`, and an explicit note that `warm` means recently indexed, not verified-current (issue #122). Results are a page: `has_more` plus `next_offset` (null on the last page) let you continue with the existing `offset` argument; paging counts hits, so one matching config file that expands into several keys is not cut mid-file (#123). No exact total is computed. Unrendered templates are searched as literal text only — no templating language is rendered. A quoted template-like string in otherwise valid YAML/JSON keeps its static keys with a `template_syntax` `partial` label; genuinely unparseable template syntax is `unsupported` (#124). Also accepts the explicit `sources` fan-out above. |
| `trace_call_path` | BFS over the `CALLS` graph (unioned with `CALLS_RESOLVED` when a `deep` reindex has run) to find callers/callees. Name-based resolution, not import-aware — see [[Known-Limitations]]. Response is capped; check `total_nodes` vs. `shown`. `depth` is capped independently of `limit`, see [[Security-Model]]. **Each returned node also carries `provenance`/`resolution`/`confidence`** (issue #59), tagged from whichever edge first reached that node in the BFS: a plain `CALLS` hop reports `{"provenance": "tree-sitter", "resolution": "name-match", "confidence": "heuristic"}`; a `CALLS_RESOLVED` hop (only present after a `deep` reindex with LSP enrichment — see #10) reports `{"provenance": "lsp", "resolution": "semantic-symbol", "confidence": "exact"}`. Treat `heuristic` hops as plausible-but-unverified and `exact` hops as backed by rust-analyzer's semantic symbol resolution. |

## Quality & change

| Tool | What it does |
|---|---|
| `detect_dead_code` | Functions with no inbound `CALLS`/`CALLS_RESOLVED` edge (excluding `main`). High false-positive rate is expected and stated in the tool's own description — treat hits as leads, not conclusions. Optional `path_prefix` (e.g. `"pkg/events"`) scopes results to a subdirectory or exact file — a real path-prefix match, not a naive string prefix, so it won't false-match a sibling directory like `pkg/events-vendor`. Fixes issue #77: on a monorepo, an unscoped call was dominated by vendored/generated code. |
| `detect_changes` | Maps uncommitted git changes to the graph symbols whose line range overlaps a diff hunk. Default response/cost is just that list, unchanged. Optional `blast_radius=true` (issue #89) additionally walks inbound callers of each changed function — direct and transitive, via the same `trace_calls` BFS `trace_call_path` uses, with the same `depth`/`limit` clamping (see [[Security-Model]]) and the same per-node `provenance`/`resolution`/`confidence` tagging. Response then has `direct` (the changed symbols), `transitive`/`transitive_total`/`transitive_shown` (its inbound callers, capped by `limit`), and a `summary` (`direct_count`, `transitive_count`, `files_touched`). |
| `query_planner` | Picks the cheapest retrieval strategy (file read / symbol search / keyword fallback) instead of the agent guessing, and returns the actual answer in-band alongside which strategy it used — no second call needed. Also carries `index_freshness`. |

## Observability

| Tool | What it does |
|---|---|
| `get_session_usage` | Per-tool call/error/output-byte counters (plus a rough bytes/4 token estimate) for *this* MCP session only — in-memory, resets when the `nexusd mcp` process does. Also reports `schema_tax` (`bytes_ceiling`/`estimated_tokens_ceiling` for the full catalog, and `bytes_advertised`/`estimated_tokens_advertised` for the preset-filtered set this connection actually received) and `reads_avoided` (an auditable, conservative counterfactual — successful calls to an explicit tool allow-list that plausibly substituted a manual read/grep). When a harness plug has reported usage, a separate `harness` object carries provider-reported tokens/cost and `sources` explains why those numbers are never summed with the bytes/4 estimates. Not the lifetime totals `stats.get`/the GUI Usage tab show — see [[Storage-and-Data-Model]] for those. |

## Query & manage

| Tool | What it does |
|---|---|
| `query_graph` | Minimal ad-hoc Cypher-lite: exactly one pattern shape, `MATCH (a:Kind)-[:EDGE]->(b:Kind) [WHERE ...] RETURN a|b`. `Kind` is `Function`/`Type`/`File`/`Section`. Fails clearly outside that shape rather than guessing. |
| `delete_project` | Removes a project's indexed data (graph + registry entry). Never touches the source directory. Destructive — gated behind the `full` preset. |

## Procedural memory (absorbed from Myelin)

These tools operate on learned `SKILL.md` files and a local SQLite store, not
on the code graph. Agent/human judgment gates promotion; redaction happens
before storage; staleness is last-used atrophy, not code-change based. See
[[Flagship-Proposal]] and ADR 0018.

| Tool | What it does |
|---|---|
| `list_skills` | List promoted skills with provenance and an informational `stale` flag. The only procedural tool in the default `standard` preset — pure consumption, no learning side effects. |
| `record_observation` | Record a reusable procedure; similar observations accumulate reps and auto-promote, or promote immediately when `high_stakes` is set. |
| `record_skill_feedback` | Correction appends into the live `SKILL.md`; confirmation just logs. |
| `mark_skill_used` | Record use of a promoted skill, independent of feedback. |
| `list_warmup_queue` | List candidates still accumulating reps. Full preset only. |
| `list_pending_review` | List redacted excerpts staged from session transcripts, awaiting agent judgment. Full preset only. |
| `dismiss_pending_review` | Clear a staged review item. Full preset only. |
| `promote_skill` | Force-promote a candidate. Full preset only. |
| `archive_skill` | Move a skill out of the live directory without deleting it. Full preset only. |
| `restore_skill` | Reverse an archive. Full preset only. |
| `render_skill_graph` | Render one skill's bounded neighborhood as PNG/DOT. Full preset only. |

Promotion is agent/automatic judgment, **not** a human sign-off. A promoted
skill enters model context only after `nexus memory skill-approve <id> --actor
human:<name>` from the CLI; there is deliberately no MCP tool for an agent to
approve its own procedural memory. `ledger.inject` signposts at most five
active, non-atrophied, human-approved skills in a separate, clearly labelled
section under the same hard ~800-token budget. The full `SKILL.md` is loaded
only via `list_skills`; a signpost is a pointer, not a dump.

## Declarative memory (curated facts, OKF v0.3)

Decisions, incidents, lessons, dead ends and owners — never raw transcripts,
code structure or preferences. SQLite is canonical; `repo/.nexus/memory/` is
an OKF v0.3 bundle (ADR 0020) that is git-friendly and Obsidian-viewable.
Trust is a tier (`unverified` / `machine-confirmed` / `human-reviewed`) and
only a **current** `human:` approval makes a fact injectable. Staleness is
anchor/file-hash based, distinct from skill atrophy. See [[Flagship-Proposal]].

| Tool | What it does |
|---|---|
| `recall` | Budgeted, lexical fact recall with freshness in-band. Hard ~800-token cap; stale facts and anchors outside `allowed_roots` are excluded. Only tool from this group in the default `standard` preset. `include_lower_trust=true` deliberately shows machine-confirmed/unverified candidates, always labelled. |
| `list_facts` | Read-only review queue: every fact with trust tier, staleness, status and anchors. Full preset only. |
| `curate_fact` | Stage a code-anchored fact from agent judgment. Always `draft`/unverified, redacted at the store boundary, never auto-promoted. Full preset only. |
| `review_fact` | Human/CLI gate: only a `human:` actor can `approve`; `reject` clears any earlier sign-off. Full preset only. |
| `export_memory` | Write the OKF v0.3 bundle (bundle-root `index.md` routing description, standard Markdown links, no wikilinks). Full preset only. |
| `import_memory` | Read a bundle into SQLite. Unknown concepts are tolerated but not trusted; local staleness and human verification are never downgraded. Full preset only. |

## Why not all 31 are advertised by default

Every MCP session pays a fixed token cost just to load `tools/list`'s
schemas, regardless of whether that session ever calls most of them. A
`[tools]` config section controls what's actually advertised:

```toml
[tools]
preset = "standard"   # "minimal" (5) | "standard" (default, 13) | "full" (31)
# enabled = ["search_code", "get_architecture"]   # explicit list, overrides preset
```

- **`minimal`** (5): `index_repository`, `search_code`, `get_file_context`,
  `get_architecture`, `trace_call_path` — the read-heavy core loop.
- **`standard`** (13, the default): adds `list_sources`, `search_graph`,
  `detect_changes`, `detect_dead_code`, `query_planner`, `get_session_usage`,
  `list_skills` and `recall` — the only curated-memory read tool, with the
  trust gate enforced.
- **`full`** (31): adds `delete_project` (destructive), `query_graph` (niche
  DSL), the opt-in `vault_context`, the ten procedural curation/ingestion
  tools, and the five remaining curated-memory review/OKF tools. Absorbing
  memory engines must not turn every session into an 18-tool schema tax
  increase.

This was a deliberate fix, not the original design — see `README.md`
Phase 21/22 for the token-cost measurement that drove it, Phase 29
for `get_session_usage` joining `standard`, and Phase 31 for its
`schema_tax`/`reads_avoided` fields.

Every tool here works identically on Linux, macOS, and Windows — none of
them depend on `nexusd serve` (the control API, background watcher, GUI
target), which is the one piece not yet ported to Windows. See
[[Known-Limitations]].

## Related

[[Security-Model]] (response caps, `allowed_roots`) · [[Architecture]] ·
[[Configuration]]
