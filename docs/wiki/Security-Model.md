# Security Model

NexusContext is an MCP server driven directly by an LLM agent, and a daemon
that reads arbitrary parts of the filesystem on request — worth being
explicit about what's protected, what's opt-in, and what was found and
fixed by a dedicated review pass rather than designed in from the start.

## What's blocked by default

- **Remote embeddings endpoints.** `Config::embeddings_policy()` refuses to
  send code to a non-loopback/non-private endpoint unless
  `allow_remote = true` is set explicitly. Filling in an endpoint doesn't
  silently start sending code to it either — `enabled` is a separate
  switch. See [[Embeddings-and-Semantic-Search]].
- **`config.toml` is owner-only on disk (`0600`)**, written atomically at
  creation time (not write-then-chmod, which would leave a brief
  world-readable window). It can hold `embeddings.api_key` in plaintext, so
  this matters on any shared/multi-user machine, not just in theory.
- **`embeddings.api_key` is never echoed back over the control API.**
  `config.get`/`config.set` both return `has_api_key: bool` instead of the
  raw value — the key never leaves the daemon process once set.
- **Per-response size caps everywhere.** No MCP tool can return an
  unbounded payload:
  - `get_file_context` — a byte ceiling (`MAX_RETURNED_BYTES`, 300KB) *and*
    a line ceiling (`MAX_RETURNED_LINES`, 4000), independently enforced —
    a line-count cap alone doesn't bound a response whose lines are
    individually enormous (a minified bundle, a generated one-line blob).
  - `search_codebase`/`query_memory` — a tighter row limit
    (`SEMANTIC_MAX_LIMIT`, 30, vs. the general 200) plus a separate,
    smaller per-hit `chunk_text` cap (1000 bytes, UTF-8-safe — cuts at the
    last whole codepoint, not a raw byte slice that could land
    mid-character), flagged via `chunk_text_truncated`.
  - Every other tool's `limit` passes through a shared `clamp_limit()`
    capped at `SERVER_MAX_LIMIT` (200) regardless of what a caller asks
    for.
- **The systemd unit is hardened**: `NoNewPrivileges=true`,
  `ProtectSystem=strict`, `ProtectHome=read-only`, with explicit
  `ReadWritePaths` for just the config/data dirs it actually needs.

## What's opt-in (off unless you turn it on)

- **`allowed_roots`** — empty by default (unrestricted), matching the
  "useful with zero config" goal. When set, gates `index_repository`,
  `export_project`, `import_project`, `get_file_context`, and
  `detect_changes` — every tool that takes a caller-supplied `repo_path`.
  (It didn't always gate the last two — see below.)
- **Semantic search** — see [[Embeddings-and-Semantic-Search]].

## What a dedicated review pass found and fixed (v0.1.13/v0.1.14)

Run explicitly against two questions — where can an agent burn unnecessary
tokens through this daemon, and where can data leak that shouldn't — each
filed as a GitHub issue before being fixed:

- Two tool calls racing against the same freshly-cold, embeddings-enabled
  project could both trigger a full reindex — the reindex lock serialized
  them but didn't dedupe the work, meaning real duplicate embeddings-API
  spend. Fixed with a double-checked-locking recheck.
- `allowed_roots` only gated indexing, not `get_file_context`/
  `detect_changes` — a real confused-deputy gap for an LLM-driven tool,
  where content the agent is reading could itself suggest reading
  somewhere it shouldn't. Fixed — see above.
- `config.toml` was saved with whatever the process umask produced (`664`
  observed on a real shared box). Fixed — see above.
- `api_key` was echoed back in cleartext over the (unauthenticated) control
  socket. Fixed — see above.
- `get_file_context(full=true)` and `search_codebase`/`query_memory` had no
  server-side response-size ceiling at all. Fixed — see above.

Full detail, including the exact code paths and PR review rounds, is in
`README.md`'s Phase 25/26 entries — this note is the current-state summary,
not the incident log.

## Trust boundary, stated plainly

The MCP tools trust the calling agent, not arbitrary network input — there
is no authentication on the control socket beyond filesystem permissions
on the runtime dir. This daemon is designed to run as the user's own
process, talked to by the user's own agent and the user's own GUI, not
exposed to any other principal.

## Related

[[MCP-Tools]] · [[Configuration]] · [[Watcher-and-Freshness]] (the inotify
budget incident, a different class of "one project can affect everything
else on the machine" problem) · [[Known-Limitations]]
