# Change Proposal: reduce MCP tool-schema token footprint

**Status:** proposal, not yet implemented
**Context:** NexusContext's stated goal is to *reduce* the token cost of agentic coding
sessions (code search / architecture / call-graph tools instead of the agent grepping
blind). In practice, connecting the `nexusd mcp` server currently costs a fixed
**~2.5k tokens of tool schemas**, loaded eagerly and unconditionally on every session
start, with no way to opt into a smaller tool set. On a machine that restarts Claude
Code sessions often (94 recorded session starts for one user), that's a recurring tax
that works against the project's own goal. This doc lists concrete, file-anchored
issues and proposed fixes, ordered by impact.

## 1. No mechanism to trim the tool set (biggest lever)

`tool_definitions()` in `crates/nexusd/src/tools.rs:32` unconditionally returns all 13
tools every time, wired straight through at `crates/nexusd/src/mcp.rs:65`. There is no
"core" vs "extended" split and no per-tool disable flag. `config.toml` (documented in
`INSTALL.md:120-136`) only exposes `[embeddings]`, `allowed_roots`, and
`[watcher].warm_window_secs` — nothing about tool selection.

Not every consumer needs `delete_project`, `detect_changes`, `render`-style
administrative tools on every single session — a typical read-heavy coding session
mostly uses `search_code` / `get_file_context` / `get_architecture` /
`trace_call_path`.

**Proposal:** add a `[tools]` section to `config.toml`:

```toml
[tools]
# "minimal" | "standard" (default) | "full"
preset = "standard"
# optional explicit override, takes precedence over preset
# enabled = ["search_code", "get_architecture", "get_file_context", "trace_call_path"]
```

`tool_definitions()` should filter its static list against the resolved enabled-set
before returning it from `tools/list`. This is the single highest-leverage change —
a user who only needs 5-6 tools for a given project could cut the fixed session-start
cost by more than half.

## 2. Five tool descriptions are doc-comments, not tool-use instructions

Token sizes observed roughly track description length. The worst offenders embed
caveats/rationale that belong in `README.md`, not in a schema a model re-reads every
session:

- **`detect_dead_code`** (`tools.rs:113`, ~297 tok) — includes a full paragraph on
  name-based call resolution false positives and why results are capped.
- **`trace_call_path`** (`tools.rs:58`, ~285 tok) — includes a per-language quality
  breakdown ("solid for Rust/Python/JS/TS/Go/Java/Ruby; structural-only for
  C/C++/C#/PHP — see language.rs for why").
- **`query_planner`** (`tools.rs:138`, ~246 tok)
- **`search_codebase`** (`tools.rs:166`, ~241 tok)
- **`query_graph`** (`tools.rs:153`, ~230 tok)

**Proposal:** trim each to a one- or two-sentence behavioral contract (what it does,
what params mean) and move the "why" — false-positive caveats, per-language quality
notes, resolution-algorithm rationale — into `README.md` / `docs/`, linked from a short
in-schema note if needed. Target: cut these five from ~1,300 tokens combined to
roughly 400-500.

## 3. Unbounded tool *results* work against the token-reduction goal

Schema size isn't the only cost — response size matters more once a tool is actually
called:

- **`trace_call_path`** (`tools.rs:389-404`) has no `limit` parameter at all, only
  `depth` (default 3). A deep or high-fan-out call graph can return an unbounded
  number of nodes with no truncation or pagination.
- **`get_file_context`** (`tools.rs:406-421`) returns the entire file verbatim when
  `start_line`/`end_line` are omitted — no size cap, no pagination. On a large file
  this alone can outweigh the tool-schema savings from #1 and #2 combined.

Contrast with `detect_dead_code` (`tools.rs:487-505`), which already learned this
lesson — it added a `limit`/`total_flagged` cap after a real incident ("99K chars in
one response", noted at `tools.rs:491-496` and `README.md:204`).

**Proposal:** apply the same pattern project-wide:
- Add a node-count cap to `trace_call_path` (independent of `depth`) with a
  `total_nodes`-style truncation indicator, mirroring `detect_dead_code`.
- Make `get_file_context` default to a bounded window (e.g. first N lines) when no
  range is given, rather than the whole file, and require an explicit range (or an
  explicit `full=true`) to return everything.

## 4. No server-side hard cap on caller-supplied `limit`

`search_code`, `search_graph`, `query_graph`, `search_codebase`, and `query_memory`
all accept a caller-supplied `limit` (defaults 10-20) but nothing clamps it server-side
if a caller passes a very large value.

**Proposal:** clamp every accepted `limit` to `min(requested, server_max)` (e.g.
server_max = 200) so a single bad call from a coding agent can't blow up a response
regardless of intent.

## 5. Cache-invalidation — ruled out, worth documenting

Investigated whether tool-schema content varies between calls (which would break
Anthropic prompt caching and force expensive cache rewrites every turn instead of
once per session). **It doesn't** — `tool_definitions()` (`tools.rs:32`) is a pure
static literal, and `initialize` (`mcp.rs:60-64`) only embeds
`env!("CARGO_PKG_VERSION")`, a compile-time constant. The ~2.5k-token cost is a fixed,
cache-stable, one-time-per-session tax, not a caching bug in this server. Worth a line
in `README.md` so this isn't re-investigated as a phantom bug later — the real lever
is reducing the fixed size (#1, #2), not chasing non-determinism that isn't there.

## Priority order

1. `[tools]` preset/enable-list config (#1) — biggest win, purely additive, no
   behavior change for existing users on the default preset.
2. Description trims (#2) — mechanical, low risk.
3. Result-size caps on `trace_call_path` and `get_file_context` (#3) — protects
   against the largest single-call blowups.
4. Global `limit` clamp (#4) — small defensive addition.
