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
  - `trace_call_path` and the GUI's call-graph visualization both take a
    caller-supplied `depth` for how far the BFS fans out across the
    `CALLS`/`CALLS_RESOLVED` graph — traversal cost grows combinatorially
    with it on a densely-connected graph, so it's capped independently of
    `limit` via `clamp_depth()` at `SERVER_MAX_DEPTH` (10).
- **Indexing skips (doesn't error or crash on) any single file over
  `MAX_INDEXABLE_FILE_BYTES` (5MB)**, checked via `stat` before the file is
  ever read into memory — closes an unbounded-memory path for a huge
  generated/vendored/minified file landing in an indexed tree (part of the
  same class of issue as the #17 OOM investigation).
- **`run_cypher_query` (the `search_graph` MCP tool's underlying query
  engine) is bounded to a few seconds of wall-clock execution time**,
  enforced via a `rusqlite` progress handler that cooperatively interrupts
  the statement (`GraphStore::set_query_timeout`) rather than trying to
  kill a thread — a pathological or unselective query can't hang the
  daemon indefinitely.
- **The file-watcher's internal event channel is bounded**
  (`sync_channel`, 256 batches) rather than unbounded — a debounced batch
  of filesystem events piles up in memory if the receiving loop falls
  behind (e.g. a slow reindex in progress); the bound turns that into a
  brief backpressure stall on notify's internal thread instead of
  unbounded growth.
- **The systemd unit is hardened**: `NoNewPrivileges=true`,
  `ProtectSystem=strict`, `ProtectHome=read-only`, with explicit
  `ReadWritePaths` for just the config/data dirs it actually needs.

## Observability (issue #58 close-out)

Bounds only help if pressure against them is actually visible - the
control API's `status.get` (`crates/nexusd/src/control.rs`) reports:

- **`rss_kb`** - this process's resident set size, read from
  `/proc/self/status`'s `VmRSS` line. Linux-only (`None` on a non-Linux
  Unix where `/proc` doesn't exist); the control API itself is already
  `#[cfg(unix)]`-only (see [[ADRs/README|ADR 0009]]), so there's no
  Windows gap to document separately here.
- **`watch_budget.queue_depth`/`queue_bound`/`queue_full_events`** - live
  depth of the bounded watcher channel (`WATCHER_CHANNEL_BOUND`, 256,
  see above), plus a lifetime count of times it was observed at that
  bound. Zero/low in the overwhelming majority of setups; a
  `queue_full_events` that keeps climbing is the loud signal that
  backpressure is real on this machine, not just a theoretical ceiling.
- **`indexing.active`/`completed_count`/`superseded_count`** - whether a
  full-rebuild reindex (MCP `index_repository`, watcher auto-reindex, or
  `projects.reindex`) is running right now, a lifetime completed-job count,
  and a lifetime count of in-flight passes that bailed out early because a
  newer request for the *same* project superseded them mid-run (issue #58,
  see [[ADRs/README|ADR 0014]]'s amendment). `REINDEX_LOCK`
  (`crates/nexus-index/src/project.rs`) already serializes every caller
  process-wide, so `active` is a 0/1 flag, not a real queue depth - there is
  never more than one indexing job in flight in a given `nexusd` process.
  `superseded_count` is zero in the common case: every change made during
  an in-flight reindex is already guaranteed to be picked up by a follow-up
  pass (the watcher's debounce thread keeps queuing events even while the
  main loop is blocked indexing, and drains/re-triggers on resumption) -
  the supersession checkpoint
  (`REINDEX_GENERATION`/`note_possible_supersession`, checked every 25
  files in `nexus_index::ingest::index_directory_inner`'s per-file loop)
  exists purely to stop that in-flight pass from wastefully finishing a
  rebuild that's already known to be stale, not to prevent any data loss.
  A nonzero count means that's actually happening on a project whose edit
  bursts outlast a single reindex pass.

## What's opt-in (off unless you turn it on)

- **`allowed_roots`** — empty by default (unrestricted), matching the
  "useful with zero config" goal. When set, gates every tool that takes a
  caller-supplied `repo_path`: `index_repository`, `export_project`,
  `import_project`, `get_file_context`, `detect_changes`, `search_code`,
  `get_architecture`, `detect_dead_code`, `trace_call_path`
  (`call_graph_dot`), and `search_graph` (`run_cypher_query`) — the full
  `repo_path`-accepting MCP surface, not a subset of it. (It didn't always
  gate all of these — see below.) The check itself canonicalizes both the
  path being checked and each configured root before comparing, closing a
  `..`-traversal bypass a raw prefix check would miss — see below.
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

## What a third review pass found and fixed (2026-08-26, issue #61)

The #29 fix (canonicalize-before-check, see above) only ever landed on
`get_file_context` and `detect_changes` — the two functions a prior audit
happened to be looking at. A follow-up audit (filed as issue #61) found
five more `repo_path`-accepting query functions that skipped the
`allowed_roots` check entirely and went straight to opening whatever graph
DB sat under the given path: `search_code`, `get_architecture`,
`detect_dead_code`, `call_graph_dot` (backs `trace_call_path`), and
`run_query`/`run_cypher_query` (backs `search_graph`). With `allowed_roots`
set, a caller could pass any `repo_path` on disk that happened to have a
graph DB under it — not just a registered/allowed project — and these five
tools would read from it regardless. `allowed_roots` empty (unrestricted)
was always working as designed; the gap was that setting it didn't
actually cover the whole tool surface.

Fixed by giving every one of those five the same canonicalize-then-check
call (`require_path_allowed`, same ordering as the #29 fix) before opening
a store, via a shared `canonicalize_and_authorize` helper in `queries.rs`
for the four functions that live there. See
[[ADRs/README|ADR 0012]] and issue #61 for the consolidated adversarial
test suite (`crates/nexus-index/tests/path_security.rs`) added alongside
this, covering all seven `repo_path`-accepting query/read functions
against: a path outside `allowed_roots`, a `..`-traversal path resolving
outside it, and a genuine subdirectory of an allowed root.

Issue #61's broader scope (symlink TOCTOU races, MCP-prompt-injection
scenario coverage) is intentionally out of scope for this pass — see the
issue for what's still open.

Full detail, including every finding (including the ones judged
low-severity) is in the GitHub issues from this pass — all closed as of
this writing.

## Symlink escape vs. TOCTOU vs. confused-deputy (2026-08-27, closing out #61)

Three related but distinct questions, worth being explicit about since
they're easy to conflate:

- **Symlink escape (defended today).** `Path::canonicalize()` resolves
  every symlink component in a path before `allowed_roots` is checked —
  the same canonicalize-before-check ordering from issue #29, above — so
  a symlink inside an allowed root pointing outside it resolves to that
  outside location and is rejected exactly like a raw `../../etc` path
  would be. Holds for both a `repo_path` argument that is itself a
  symlink and a `file` argument that resolves through one. Proven
  directly by adversarial tests in
  `crates/nexus-index/tests/path_security.rs`: a symlink inside an
  allowed root pointing outside it is rejected (as both a `repo_path` and
  a `file` argument), and — the reverse case — a symlink pointing
  elsewhere inside the same allowed root is correctly still allowed, not
  falsely rejected.
- **TOCTOU races (partially defended, 2026-08-27; not fully closed).** A
  fundamentally different scenario from symlink escape above: an attacker
  with *concurrent filesystem write access* swaps a real file/directory
  for a symlink *during* a request, between the canonicalize+check step
  and the actual `fs::read`/DB open that follows it. That needs a
  co-resident malicious process racing the daemon's own check-then-use
  window — a much higher bar than "repository content steered an agent's
  request." [Issue #72](https://github.com/devNalyx/NexusContext/issues/72)
  originally scoped a full fix here (`openat2`/`O_NOFOLLOW` + inode
  comparison on Linux, per-platform elsewhere) but that was deliberately
  re-scoped down: NexusContext now opens both filesystem-read hot paths
  (`get_file_context`'s file read and the indexer's `read_source_capped`)
  with `O_NOFOLLOW` on Unix, via `nexus-index::secure_fs`. That closes the
  *cheapest* TOCTOU shape — the path being swapped for a symlink between
  check and read — with a clean error instead of a raw errno. It does
  **not** close the race where the path is swapped for a *different
  regular file or directory* at the same name — `O_NOFOLLOW` only rejects
  symlinks, so that substitution still isn't caught. Fully closing that
  needs atomic check-and-open (`openat2(RESOLVE_NO_SYMLINKS)` and
  path resolution relative to an already-open directory fd throughout),
  which remains out of scope. Unix-only: Windows and macOS are unchanged,
  with no equivalent guard attempted on either. See
  [[ADRs/README|ADR 0015]] for the full before/after and the remaining
  gap.
- **Prompt-injection / confused-deputy (defended by design).** Issue
  #61's actual framing: repository content might try to manipulate the
  calling agent into requesting a path outside the project it was invoked
  on — e.g. "read `~/.ssh/id_rsa`" smuggled into a comment the agent
  naively follows. This requires no race window at all — it's a single
  synchronous request, and `require_path_allowed` enforces
  `allowed_roots` server-side regardless of what convinced the agent to
  ask. Tested explicitly under this framing (not just implicitly via the
  generic "outside allowed_roots" cases already in the suite) using the
  issue's own `$HOME/.ssh/id_rsa`-shaped example.

See [[ADRs/README|ADR 0012]]'s 2026-08-27 update for the full reasoning.

## Adversarial security test coverage: Unix-only today, and why (2026-08-27, #83)

`crates/nexus-index/tests/path_security.rs` - the adversarial suite above
covering outside-root rejection, `..`-traversal, symlink escape, and the
confused-deputy scenario - is entirely `#[cfg(unix)]`-gated, so none of it
runs on the `test-windows` CI job. Investigated directly rather than left
as an unexplained gap:

- **Symlink creation on Windows CI is not the blocker.** A throwaway probe
  (`std::os::windows::fs::symlink_file`) was run on this repo's own
  `windows-latest` `test-windows` job and succeeded without elevation or a
  Developer Mode step - the assumption that symlinks need admin rights on
  CI doesn't hold, at least not on GitHub Actions' current Windows runners.
- **The actual blocker is config injection, and it affects every test in
  the file, not just the symlink ones.** Every case - including the
  non-symlink outside-root/`..`-traversal/confused-deputy ones - depends on
  redirecting `$HOME`/`$XDG_CONFIG_HOME` so `nexus_core::Paths::resolve()`
  (via `directories::ProjectDirs`) picks up a scratch `config.toml` with a
  controlled `allowed_roots`. That only works on Unix: `directories`'
  Windows backend resolves through the Win32 known-folder API
  (`SHGetKnownFolderPath`), not any environment variable, and there is no
  other test-only override hook for `config_dir` in the codebase today.
  Splitting the file by "does this case use a symlink" wouldn't have
  produced a runnable Windows subset - the whole file is blocked on the
  same missing piece.
- **Not fixed in this pass.** A proper cross-platform config-injection test
  harness is a real, separately-scoped change (touches every
  `Paths::resolve()` call site in `nexus-index`), not a test tweak - filed
  as [issue #85](https://github.com/devNalyx/NexusContext/issues/85). Once
  it exists, the symlink-creation finding above means the symlink-specific
  cases can extend to Windows immediately alongside the rest, with no
  further platform investigation needed.

The general CI matrix (`test-windows`) does exercise the rest of the
workspace's test suite on Windows already - this gap is specific to this
one security-focused adversarial file, not a general Windows-support gap.

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
