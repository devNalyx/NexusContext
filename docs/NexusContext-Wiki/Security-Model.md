# Security Model

NexusContext is an MCP server driven directly by an LLM agent, and a daemon
that reads arbitrary parts of the filesystem on request — worth being
explicit about what's protected, what's opt-in, and what was found and
fixed by a dedicated review pass rather than designed in from the start.

## What's blocked by default

- **Any outbound network call at all.** With the embeddings/semantic-search
  subsystem removed (see [[ADRs/README|ADR 0010]]), nothing in this daemon
  makes an HTTP request to anything, ever — every tool is a local
  filesystem read plus a local SQLite query. There's no endpoint,
  `allow_remote` gate, or API key to reason about.
- **Everything the daemon writes under the data/config dirs is owner-only on
  disk, directories included** — `config.toml` (`0600`, written atomically
  at creation time, not write-then-chmod, which would leave a brief
  world-readable window), `registry.json`/`usage_stats.json` (`0600`, plus
  their containing data directory itself hardened to `0700` right alongside
  them — file-level `0600` alone still left directory *listing* open,
  leaking which projects exist and that `usage_stats.json` exists at all,
  even though the file contents themselves were already protected), and
  `graph.db` plus its own per-project data directory (`0600`/`0700`) —
  `graph.db` is the most sensitive of these, since it holds the full
  indexed source text (FTS5) for every project ever indexed, not just
  metadata.
- **The control socket itself is owner-only on disk (`0600`)**, set
  explicitly right after bind — a second, explicit layer on top of (not a
  replacement for) the runtime directory already being `0700` under the
  default systemd deployment, so the protection doesn't depend solely on an
  inherited directory-permission convention holding.
- **`import_project` refuses an artifact that decompresses past a 2GiB
  cap**, streamed and checked incrementally rather than decompressed in one
  unbounded call — closes a decompression-bomb path in exactly the "import
  a teammate's shared index" workflow the feature exists for.
- **Per-response size caps everywhere.** No MCP tool can return an
  unbounded payload:
  - `get_file_context` — a byte ceiling (`MAX_RETURNED_BYTES`, 300KB) *and*
    a line ceiling (`MAX_RETURNED_LINES`, 4000), independently enforced —
    a line-count cap alone doesn't bound a response whose lines are
    individually enormous (a minified bundle, a generated one-line blob).
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
  (It didn't always gate the last two — see below.) The check itself
  canonicalizes both the path being checked and each configured root before
  comparing, closing a `..`-traversal bypass a raw prefix check would miss
  — see below.
- **LSP-resolved-symbol enrichment (`[lsp]`, issue #10)** — default
  `enabled = false`. When on, a `deep` reindex (`index_repository`'s `deep`
  argument / `nexus reindex --deep` — never the ordinary auto-reindex path)
  spawns `lsp.server_command` (default `rust-analyzer`) as a child process
  and talks LSP over its stdio to resolve cross-file references. Worth
  naming plainly: this is the one place `config.toml` controls what
  external binary the daemon executes, not just what it connects to — you're
  trusting your own config, but the failure mode is "runs a program"
  rather than a network request (there are none in this daemon at all — see
  above). Strictly enrichment: a missing/crashing/timed-out server
  degrades to the static tree-sitter-only index, never fails the reindex
  or blocks any other tool — see `crates/nexus-index/src/enrich.rs`'s own
  degrade-cleanly tests. Capped concurrency (`max_concurrent_servers`,
  default 2) bounds how many server processes can be alive at once within
  one `nexusd`/`nexus` process.

## What a dedicated review pass found and fixed (v0.1.13/v0.1.14)

Run explicitly against two questions — where can an agent burn unnecessary
tokens through this daemon, and where can data leak that shouldn't — each
filed as a GitHub issue before being fixed:

- Two tool calls racing against the same freshly-cold project could both
  trigger a full reindex — the reindex lock serialized them but didn't
  dedupe the work, meaning real duplicate rebuild cost. Fixed with a
  double-checked-locking recheck.
- `allowed_roots` only gated indexing, not `get_file_context`/
  `detect_changes` — a real confused-deputy gap for an LLM-driven tool,
  where content the agent is reading could itself suggest reading
  somewhere it shouldn't. Fixed — see above.
- `config.toml` was saved with whatever the process umask produced (`664`
  observed on a real shared box). Fixed — see above.
- `get_file_context(full=true)` had no server-side response-size ceiling
  at all. Fixed — see above.

Full detail, including the exact code paths and PR review rounds, is in
`README.md`'s Phase 25/26 entries — this note is the current-state summary,
not the incident log.

## What a second review pass found and fixed (2026-08-15)

A follow-up audit, same two questions as the first pass. The one real
MCP-reachable finding:

- **`allowed_roots` was bypassable via `..` traversal.** The check compared
  a raw, not-yet-canonicalized `repo_path` (`Path::starts_with` never
  resolves `..`), while `get_file_context` and friends only canonicalized
  *afterward* — a caller could pass `"<allowed_root>/../../etc"`, sail
  through the check, then have it resolve outside `allowed_roots` entirely.
  Fixed at the check itself (see above), not just by reordering the three
  affected call sites, so a future caller getting the ordering wrong again
  is still protected.

Plus three local-operator-scoped hardening fixes (not MCP-reachable, but
real on this project's own shared dogfooding box): the control socket and
every file the daemon writes, directories included, are now owner-only
(see above), and `import_project` caps decompressed size against a
decompression bomb, cleaning up any partial output on rejection or a
genuine mid-stream decode error alike (see above).

**One reliability fix, not strictly a security one but from the same
pass:** the MCP stdio dispatch loop (`nexusd mcp`) had no panic isolation
— unlike the control API, which already gets this for free from its
one-thread-per-connection design, a panic anywhere in tool dispatch used
to unwind straight through and kill the whole session over a single bad
tool call. Now wrapped in `catch_unwind`, isolated to one JSON-RPC error
response per request. No currently-reachable panic-from-untrusted-input
was found to pair with this during the audit — closes an architectural
gap, not a demonstrated live bug.

Full detail, including every finding (including the ones judged
low-severity) is in the GitHub issues from this pass — all closed as of
this writing.

## Trust boundary, stated plainly

The MCP tools trust the calling agent, not arbitrary network input — there
is no *authentication* on the control socket (no identity/credential
check), only filesystem permissions: the socket file itself is owner-only
(`0600`), and the runtime dir it lives in is `0700` under the default
systemd deployment. This daemon is designed to run as the user's own
process, talked to by the user's own agent and the user's own GUI, not
exposed to any other principal.

## Related

[[MCP-Tools]] · [[Configuration]] · [[Watcher-and-Freshness]] (the inotify
budget incident, a different class of "one project can affect everything
else on the machine" problem) · [[Known-Limitations]]
