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
[[Indexing-Pipeline]] for the one real exception: embeddings reuse across
reindexes). Full per-file incremental diffing — a persistent
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

## No dedicated vector store

Embeddings are BLOB rows in the same SQLite `graph.db`, ranked by
brute-force cosine similarity at query time — see
[[Embeddings-and-Semantic-Search]]. Appropriate at this project's actual
scale (thousands of chunks per project); would need revisiting only well
past that scale.

## GNOME extension version churn

Shell extensions frequently break across major GNOME releases. Treated as
low-priority/optional by design and kept thin specifically to stay cheap
to fix — see [[GUI-and-Extension]].

## Platform scope

Linux x86_64 has full parity (daemon/CLI/GUI). macOS Apple Silicon gets
CLI + daemon only (`nexus-gui` is Linux/GNOME-first, not portable).
Windows isn't published — the control API's `std::os::unix::net` usage has
zero platform gating yet, a real prerequisite fix tracked as a distinct
follow-up rather than attempted.

## Related

[[Indexing-Pipeline]] · [[MCP-Tools]] · [[Watcher-and-Freshness]] ·
[[Embeddings-and-Semantic-Search]]
