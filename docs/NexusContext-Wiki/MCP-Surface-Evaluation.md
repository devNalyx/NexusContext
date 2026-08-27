# MCP Tool Surface Evaluation

Addresses [#60](https://github.com/devNalyx/NexusContext/issues/60) — "Reduce tool/schema
complexity and evolve query routing into an internal planner."

This is an evaluation only: an inventory, a classification, and a recommendation. No tools are
removed, hidden, deprecated, or renamed by this document or its accompanying PR. That is
explicitly out of scope here — any actual surface change is a separate, future, product-approved
change.

## Method

Tool list and schemas taken directly from `crates/nexusd/src/tools.rs`, `tool_definitions()`
(12 tools, confirmed by grepping `"name":` in that function — the issue text's older examples
undercount by omitting `delete_project`, `query_planner`, and `get_session_usage`).

Token estimate reuses the exact convention `get_session_usage` already uses internally
(`estimate_tokens`, `crates/nexusd/src/tools.rs`): **bytes / 4**, a plain character-count
heuristic, not a real tokenizer. Applied here to `name` + `description` + `inputSchema` (as
serialized JSON), per tool, since that's the actual bytes an MCP client's context pays for once
per session when the tool list is advertised.

## 1. Full tool inventory

| Tool | Purpose (one line) | desc bytes | schema bytes | total bytes | ~tokens |
|---|---|---:|---:|---:|---:|
| `index_repository` | Build/rebuild the knowledge graph for a directory | 204 | 141 | 361 | 90 |
| `search_graph` | Structural search over indexed symbol names / doc headings | 116 | 181 | 309 | 77 |
| `trace_call_path` | BFS over the CALLS graph for callers/callees of a function | 493 | 309 | 817 | 204 |
| `get_file_context` | Read a file (or line range) from an indexed project | 219 | 245 | 480 | 120 |
| `get_architecture` | Summarize an indexed project: node/edge counts, busiest files | 165 | 94 | 275 | 68 |
| `detect_changes` | Map uncommitted git changes to affected graph symbols | 110 | 94 | 218 | 54 |
| `delete_project` | Remove a project's indexed data (admin op) | 94 | 94 | 202 | 50 |
| `detect_dead_code` | Functions with no inbound CALLS edge (high false-positive rate) | 178 | 139 | 333 | 83 |
| `search_code` | Full-text (FTS5) search over indexed file content | 268 | 177 | 456 | 114 |
| `query_planner` | Picks cheapest retrieval strategy for a query automatically | 236 | 228 | 477 | 119 |
| `query_graph` | Raw ad-hoc Cypher-lite graph query (one pattern shape) | 163 | 177 | 351 | 87 |
| `get_session_usage` | Reports this session's own MCP response bytes/tokens per tool | 390 | 36 | 443 | 110 |

**Total schema overhead of the full 12-tool surface: ~1,176 tokens** (4,522 bytes), paid once per
MCP session just for the tool list an agent sees before making any call — before any actual
query/response tokens.

`trace_call_path` (204 tok) and `get_file_context` (120 tok) are the two heaviest single
schemas — both carry substantial description text documenting real caveats (name-based
resolution, provenance/confidence fields, truncation behavior, the 300-line default cap), so
their weight is earned documentation, not bloat.

## 2. Core vs. advanced/internal classification

Using the issue's own proposed distinction (core = matches direct agent intent; advanced/internal
= implementation mechanism):

| Tool | Classification | Rationale |
|---|---|---|
| `get_file_context` | **Core** | Direct "show me this file" intent |
| `search_code` | **Core** | Direct "find this text" intent; #57-validated |
| `get_architecture` | **Core** | Direct "orient me in this codebase" intent |
| `trace_call_path` | **Core** (by intent) | Matches "who calls X" intent even though #57 found it doesn't yet beat grep on a small repo |
| `detect_changes` | **Core** | Direct "what did I just touch" intent, changes-equivalent per the issue's own example list |
| `detect_dead_code` | **Core** | Direct intent (impact/reachability), and #57's clearest measured win |
| `query_planner` | **Advanced/internal** | Meta-tool *about* routing, not itself a retrieval intent — see §4 |
| `search_graph` | **Advanced/internal** | Symbol-name-substring search; `query_planner`'s `graph_search` strategy and `search_code` both substantially overlap this |
| `query_graph` | **Advanced/internal** | Raw Cypher-lite escape hatch — exactly the issue's own example of an implementation mechanism |
| `index_repository` | **Advanced/internal (setup)** | Necessary but a one-time/maintenance action, not a query intent |
| `delete_project` | **Advanced/internal (admin)** | Admin op, exactly the issue's own example |
| `get_session_usage` | **Advanced/internal (meta)** | Self-monitoring, not a retrieval intent |

That's **6 core, 6 advanced/internal** — a materially different split than the issue's own
5-example "core" sketch (context/search/architecture/trace/changes), because `detect_dead_code`
earns core status on #57's actual evidence even though the issue didn't name it, while
`query_planner` itself — the issue's proposed *mechanism* for reducing surface — is properly
advanced/internal, not core, once it exists as a tool: it's infrastructure an agent might use
directly today, but conceptually it's "how do I retrieve," not "what do I want."

## 3. Cross-reference with #57's real benchmark evidence

From `Product-Thesis-Validation.md` (6 tasks, baseline-grep vs. NexusContext, this repo,
70 files):

- **`detect_dead_code`**: clear, measured win. Fewer tool calls than baseline, caught 8 real
  candidates the grep-only baseline agent explicitly hedged as non-exhaustive. One real
  correctness gap found (re-export blind spot on `run_query`/`run_cypher_query`), but the
  capability itself is validated.
- **`search_code`**: real, measured win via a different mechanism than expected — Task 5's
  ~24% token savings came from *finding an existing doc* via full-text search, not from graph
  traversal. Validates `search_code` as core, but the win is "search discovers curated docs,"
  worth remembering when tuning it.
- **`trace_call_path` / `search_graph`**: **no measured advantage over baseline grep** on 3 of 6
  tasks (Tasks 1, 3, 6) at this repo's size (70 files). Both matched baseline correctness but used
  *more* tool calls and *more* wall-clock time in every head-to-head. The benchmark explicitly
  could not confirm or deny value at larger scale or higher symbol-collision rates — this is a
  real open gap, not evidence they're worthless, but it is also not evidence they currently pull
  their weight as a "core, always-present" tool on typical-sized repos.
- **`query_planner`**: not exercised at all in #57 — the benchmark's NexusContext-condition
  sub-agents were told to "prefer NexusContext tools where useful" but were never specifically
  directed at `query_planner`, and none of the 6 task write-ups mention it being called. This
  is itself informative: even when explicitly available, ad-hoc agents defaulted to the
  specialized tools whose names described their task, not the router. That's weak but real
  evidence that a router tool doesn't organically get chosen by agents over direct-intent tools
  — it has to be either the *only* entry point (implicit routing, not an explicit extra tool) or
  it doesn't get used.
- **Cross-session persistence and large/unfamiliar-repo behavior** — the two categories where the
  issue's own thesis (and NexusContext's differentiation from grep generally) would most plausibly
  show — were **not tested** in #57. Any recommendation here about `trace_call_path`/`search_graph`
  is bounded by that gap.

## 4. Does `query_planner` already make specialized tools redundant?

**No, not yet, and the gap is specific, not vague.** `query_planner` (`crates/nexus-index/src/queries.rs`,
`plan_query`) currently routes a single `query` string among exactly three strategies:

1. `file_read` — if a `file` argument is given, delegates to `get_file_context`.
2. `graph_search` — if the query looks like a bare identifier, delegates to `search_by_name`
   (the same lookup `search_graph` exposes).
3. keyword/FTS fallback — otherwise, a per-word FTS merge (the same underlying search `search_code`
   exposes).

That is real coverage of three of the twelve tools' *retrieval mechanisms* — but it does not
cover, and would need new routing logic to cover, at minimum:

- **`trace_call_path`** — no notion of direction/depth/callers-vs-callees exists in `plan_query`'s
  three branches; this is a structurally different query shape (graph BFS with parameters), not a
  string-lookup strategy choice.
- **`detect_dead_code`** — a whole-graph reachability computation, not a per-query lookup at all.
- **`get_architecture`** — a project-level summary, not answering a specific query.
- **`detect_changes`** — git-diff-to-symbol mapping, no query string involved.
- **`query_graph`**'s arbitrary Cypher-lite pattern space — by design not reducible to an implicit
  router without becoming the router itself.

So the honest answer to "does `query_planner` already cover enough of what a core agent-facing
surface needs": **it covers the low-hanging, genuinely fungible middle of the surface (plain
lookup: name search vs. text search vs. file read) but none of the higher-value, #57-validated
capabilities** (`detect_dead_code`'s reachability analysis, `trace_call_path`'s directional BFS).
Those two are exactly the tools #57 found to have real, distinct value — collapsing them into
`query_planner` would require adding intent classification for "who calls this" and "what's dead
code here" as new routable strategies, which is a real feature addition, not a surface-reduction
refactor.

## 5. Recommendation

**The evidence does not support aggressive reduction. It supports one narrow, low-risk move and
otherwise counsels leaving the surface alone until the two named gaps (large-repo, cross-session)
are actually benchmarked.**

Specifically:

1. **Do not deprecate or hide `trace_call_path`, `search_graph`, `detect_dead_code`,
   `get_architecture`, or `detect_changes` now.** Three of these (`trace_call_path`,
   `search_graph`) showed no advantage over grep *on a 70-file repo* — but the benchmark itself
   says this may not generalize to larger/unfamiliar repos, and no follow-up benchmark has run yet.
   Hiding a tool on unconfirmed-negative evidence is a bigger risk than the ~200-token schema cost
   of keeping it visible.
2. **Correction (2026-08-27): this is already done, not a pending recommendation.**
   `FULL_EXTRA_TOOLS` in `crates/nexusd/src/tools.rs` already gates both `query_graph` and
   `delete_project` behind `preset = "full"` (or an explicit `enabled` list) — neither ships in
   the default `Standard` preset's 10 tools. This predates this evaluation entirely (introduced in
   the `[tools]` config/MCP schema filtering work well before issue #60 was filed). The paragraph
   below originally recommended this as a future move without checking whether the existing
   preset-filtering mechanism already covered it; it did. No code change was needed here — this
   entry is left in place, corrected, rather than deleted, so the reasoning for why these two
   tools are the right opt-in candidates stays on record.
3. **`query_planner` is not ready to replace or absorb the specialized tools.** It currently
   routes ~3 of 12 tools' worth of capability. Before any "collapse specialized tools behind the
   planner" change is viable, `plan_query` would need new routable strategies for at minimum
   directional call-tracing (`trace_call_path`) and reachability/dead-code detection
   (`detect_dead_code`) — the two capabilities #57 actually validated as differentiated. Absorbing
   the *un*-validated ones first would be backwards.
4. **The router-as-extra-hop concern in the issue is only weakly supported.** #57's NexusContext
   sub-agents, even when told to prefer NexusContext tools, never chose `query_planner` over the
   named specialized tool for a task that matched a specialized tool's name — suggesting the real
   value of routing, if pursued, is *implicit* (folding routing logic into the specialized tools'
   own dispatch, or making `query_planner` the sole exposed entry point rather than one tool among
   twelve) rather than *explicit* (an extra visible tool an agent must decide to call). That's a
   direction worth a follow-up design, not something this evaluation should decide unilaterally.
5. **Total schema overhead (~1,176 tokens for all 12 tools) is not, by itself, a strong case for
   reduction.** For comparison, a single full-file `get_file_context` read or one `detect_dead_code`
   response can individually cost several times that in response tokens (#57's Task 5 baseline used
   ~51k tokens on four ~1000-line file reads). The tool-list tax is real but small relative to
   response-token costs; the stronger argument for trimming `query_graph`/`delete_project` is
   agent decision complexity and rarity-of-use, not raw token count.

**Concrete next steps this evaluation recommends, in order:**
- File a follow-up issue to benchmark `trace_call_path`/`search_graph`/cross-session persistence
  on a large, unfamiliar repository — the two gaps #57 explicitly left open and that this
  evaluation's recommendation depends on.
- ~~If/when product sign-off wants an actual surface change, start with `query_graph` and
  `delete_project` behind an opt-in preset~~ — already done, see the correction above.
- Do not attempt to fold `trace_call_path` or `detect_dead_code` into `query_planner` until
  `plan_query` gains explicit routing logic for directional graph BFS and reachability queries —
  treat that as a feature addition to scope and benchmark on its own, not a byproduct of a
  surface-reduction PR.

## 6. Closing #60

With the correction above, every actionable item #60 asked for is now satisfied:
`query_planner` already exists as the internal-routing mechanism the issue proposed evolving
toward; tools are inventoried and classified core vs. advanced/internal (§3); and the one
concrete low-risk surface-reduction move (`query_graph`/`delete_project` opt-in-only) was already
shipped independently of this issue. What remains — folding more capability into `query_planner`,
and benchmarking a large/unfamiliar repo before considering hiding `trace_call_path`/
`search_graph` — is real future work, but it's speculative until that benchmark exists, not a
known gap this issue can meaningfully stay open to track. Closing #60; the large-repo benchmark
follow-up is tracked under #57 instead, where the rest of that evidence-gathering lives.
