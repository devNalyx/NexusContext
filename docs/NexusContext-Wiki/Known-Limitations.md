# Known Limitations

Stated plainly, not smoothed over — matching how this project's own
`README.md` and MCP tool descriptions already talk about themselves.

## Call resolution is name-based, not import-aware

No general `use`/`import` parsing, no module-path resolution (see
[[Indexing-Pipeline]]). Same-file matches win; a cross-file call resolves
only when the callee name is unique project-wide. Two files each defining a
same-named function, with no local match in the caller's own file, stays
**unresolved rather than guessed wrong** — a deliberate choice, not an
oversight. This is the single biggest accuracy ceiling on
`trace_call_path`/`search_graph`/`detect_dead_code`.

Concretely: `module_a.rs` and `module_b.rs` each define `pub fn foo()`, and
`caller.rs` (which defines no `foo` of its own) calls `foo()`. No `CALLS`
edge is created for that call site at all — not to `module_a::foo`, not to
`module_b::foo`, not to both. Both candidates then read as dead code if
nothing else calls them, even though one of them genuinely is called by
`caller.rs`; the graph just can't say which. Pinned down by
`ambiguous_resolution_tests` in `crates/nexus-index/src/ingest.rs` (issue
#59). Nothing in `trace_call_path`/`search_graph`/`detect_dead_code` today
distinguishes "ambiguous call site, silently dropped" from "no call site
found here" — a richer confidence marker for this case is future work, not
yet built.

One narrow exception (issue #67): a Rust `pub use path::name as alias;` (or
brace-list `pub use path::{name as alias, ...};`) re-export is recognized
via a lightweight regex scan — not real `use`-declaration parsing — and the
alias is linked back to the original definition, so a call site written
against the alias resolves the same way a call to the original name would.
This fixed a real false positive: `nexus_index::run_cypher_query(...)` (the
re-exported alias for `cypher::run_query`) previously left `run_query`
looking dead. Fixing that case also required teaching the Rust tags query
to recognize path-qualified calls (`module::function()`) as call sites at
all — the upstream `tree-sitter-rust` tags query only captures bare-
identifier and `self.method()`-style calls; see `RUST_SCOPED_CALL_QUERY` in
`crates/nexus-index/src/language.rs`. Only Rust re-exports are covered;
other languages' equivalent constructs (JS/TS `export { x as y }`, Python
`from foo import bar as baz`, etc.) aren't handled by this fix.

There's an open, explicitly-not-yet-pursued proposal to close part of this
gap by optionally enriching the graph with LSP-provided resolved symbols
(real cross-file references, real type resolution) — kept deliberately
speculative rather than started, since it's a genuinely large subsystem
(spawning and lifecycle-managing per-language language servers) and
doesn't fix anything currently broken, just raises accuracy on an already
honestly-scoped limitation.

## `detect_dead_code` has a real, expected false-positive rate

Name-based call resolution means a function invoked only via reflection,
routing tables, or dependency injection — never a direct call site tree-
sitter can see — reads as dead when it isn't. Stated in the tool's own
description; treat hits as leads, not conclusions.

## Reindexing is a full rebuild, not incremental

Every reindex clears and re-walks the whole project (see
[[Indexing-Pipeline]]). Full per-file incremental diffing — a persistent
`file_signatures` table plus a durable `call_sites` table so a rename in
one file correctly invalidates a resolved call edge from an *unchanged*
caller elsewhere — is deliberately not attempted yet. It's genuinely
harder than it looks and deserves the same iterate-against-a-real-
dogfooded-project rigor the watcher's reindex-loop bugs needed before
their fixes actually held (see [[Watcher-and-Freshness]]), not a
first-shot implementation.

## `query_graph`'s Cypher-lite is exactly that — lite

One pattern shape only: `MATCH (a:Kind)-[:EDGE]->(b:Kind) [WHERE ...]
RETURN a|b`. Not a general query language. Fails clearly outside that
shape.

## Symlink defense is not full TOCTOU-proofing

`O_NOFOLLOW` on the two filesystem-read hot paths (Unix only) rejects a
path that's been swapped for a symlink between the `allowed_roots` check
and the actual read/open — the cheapest TOCTOU shape. It does **not**
close the race where an attacker with concurrent filesystem write access
swaps the path for a *different regular file or directory* at the same
name in that same window; that needs atomic check-and-open
(`openat2(RESOLVE_NO_SYMLINKS)` plus path resolution relative to an
already-open directory fd throughout), which is out of scope today. This
needs a co-resident malicious process racing the daemon's own
check-then-use window, a materially higher bar than the ordinary
prompt-injection/confused-deputy threat model the rest of the security
posture targets. See [[Security-Model]] and [[ADRs/README|ADR 0015]].

## GNOME extension version churn

Shell extensions frequently break across major GNOME releases. Treated as
low-priority/optional by design and kept thin specifically to stay cheap
to fix — see [[GUI-and-Extension]].

## Platform scope

Released as of Phase 33: Linux and Windows on both x86_64 and arm64,
macOS on Apple Silicon (arm64) - a native macOS x86_64 build was attempted
but dropped when GitHub's own runner class for it never assigned a
runner (a real capacity constraint, not a workflow bug); Intel Mac users
run the arm64 build under Rosetta 2 instead.
Linux has full parity (daemon `mcp` + `serve`, CLI, GUI). macOS gets CLI +
full daemon (`serve`/the control API works too — Unix domain sockets are
native there) but no GUI (`nexus-gui` is Linux/GNOME-first, not portable).
Windows gets `nexus` CLI + `nexusd mcp` only — `serve` (the control API,
background watcher, GUI target) isn't supported there yet, since it's
built entirely on Unix domain sockets with no cross-platform abstraction
(`crates/nexusd/src/control.rs`) — see [issue #16](https://github.com/devNalyx/NexusContext/issues/16)
for the real blocker and what a future port would need. Every MCP tool
works fully on Windows regardless; the only thing missing is the
persistent background daemon that keeps a project warm automatically.

## Related

[[Indexing-Pipeline]] · [[MCP-Tools]] · [[Watcher-and-Freshness]]
