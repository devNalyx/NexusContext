# Known Limitations

Stated plainly, not smoothed over — matching how this project's own
`README.md` and MCP tool descriptions already talk about themselves.

## Call resolution is name-based, not import-aware

No `use`/`import` parsing, no module-path resolution (see
[[Indexing-Pipeline]]). Same-file matches win; a cross-file call resolves
only when the callee name is unique project-wide. Two files each defining a
same-named function, with no local match in the caller's own file, stays
**unresolved rather than guessed wrong** — a deliberate choice, not an
oversight. This is the single biggest accuracy ceiling on
`trace_call_path`/`search_graph`/`detect_dead_code`.

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
