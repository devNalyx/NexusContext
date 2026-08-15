# Indexing Pipeline

How a directory becomes a queryable graph.

## Parsing: `tree-sitter-tags`, not hand-written queries

Early on, each supported language needed a hand-written tree-sitter query —
a ceiling of about 2 languages before it stopped being worth the effort per
addition. The project migrated to consuming the `TAGS_QUERY` that nearly
every actively-maintained tree-sitter grammar crate already bundles — a
community-maintained query using conventional capture names
(`@definition.function`, `@reference.call`, ...), the same mechanism GitHub
code navigation and Neovim's `nvim-treesitter` rely on. Adding a language
now costs "add the grammar crate + map its extensions," not "write and
debug a new query language." See [[Language-Support]] for the current
11-language list and their call-graph quality tiers.

## Two passes, so cross-file calls actually resolve

1. **Per-file pass**: every file is parsed and its own `File`/`Function`/
   `Type` nodes inserted. Call sites are collected but not resolved yet.
2. **Project-wide resolution pass**: once every file's functions are known,
   each pending call site is resolved against a project-wide name registry.

This is what lets a function called only from a *different* file still show
up in `trace_call_path` — it used to be invisible entirely. It's still
**name-based, not import-aware**: no `use`/`import` parsing, no module-path
resolution. Same-file matches win; a cross-file match resolves only when
the callee name is unique project-wide. Two files each defining a
same-named function, with no local match in the caller's own file, is left
**unresolved rather than guessed wrong** — see [[Known-Limitations]].

## Markdown docs get their own structural model

`.md`/`.markdown` files aren't forced into the code model. A dedicated
heading parser extracts `NodeKind::Section` nodes (a heading + its body,
down to the next equal-or-shallower heading) linked by `EdgeKind::Contains`
(parent heading → nested child heading). Handles level-skips without a
phantom intermediate node, multiple independent top-level trees per file,
and correctly ignores heading-like text inside fenced code blocks. This
feeds full-text search, `search_graph`, and embeddings identically to code
— the embeddings pipeline just consumes `(node_id, chunk_text)` pairs
regardless of source.

## Full-text search

SQLite FTS5 over every file tree-sitter parses (one of the 11 supported
languages) plus markdown. Query matched as a literal phrase via
`search_code`. Other file types (plain `.txt`, config files, etc.) aren't
indexed for full-text search yet.

## Minified/bundled files are handled specially, so they can't OOM the daemon

A single dense, single-line file (a vendored/minified JS bundle is the
common case) used to be able to take the daemon down: every function's
range spanned the same giant line, so per-function chunk text went
untruncated and the cross-file call-resolution pass deep-cloned the
whole same-file function map onto every pending call site — O(functions
× calls) memory, growing superlinearly with file size (observed: 3x the
bytes cost 11x the memory). Fixed on the ingest path itself, not by
excluding such files: chunk text is truncated at build time, same-file
calls resolve immediately per-file instead of cloning the map per call,
and any line over 2000 characters skips call-site resolution entirely
for that file (structural nodes/full-text search still cover it; only
the call graph loses fidelity for that one file). See README Phase 28
for the incident this was root-caused from.

## Reindexing is a full rebuild, not incremental — with one real exception

Every reindex is `GraphStore::clear()` + a full re-walk. There's no
per-file incremental diffing (see [[Known-Limitations]]). The one place
this *is* optimized: **embeddings reuse**. Before `clear()` wipes the
`embeddings` table, a snapshot is taken keyed by `qualified_name` (stable
across a rebuild, unlike `node_id`, which resets every `clear()`). A chunk
whose text is byte-identical to its previous snapshot entry gets its old
vector reinserted under the new node id — zero network cost. Only
genuinely new or changed chunks go to the real embeddings endpoint.
`embeddings_status` reports both counts, e.g. `"ok: 12 chunks embedded,
340 reused unchanged"`. This is what keeps a routine catch-up reindex on an
embeddings-enabled project cheap.

Concurrent reindex safety: `graph.db` runs in WAL mode, and the full
rebuild happens inside `BEGIN IMMEDIATE`/`COMMIT` with a 30s busy timeout,
so two overlapping rebuild attempts (the watcher firing during a manual
reindex, say) serialize instead of corrupting the graph. A process-wide
`REINDEX_LOCK` mutex enforces this at the Rust level too — see
[[Watcher-and-Freshness]] for the race this specifically closes.

## Related

[[Language-Support]] · [[Storage-and-Data-Model]] ·
[[Embeddings-and-Semantic-Search]] · [[Watcher-and-Freshness]] ·
[[Known-Limitations]]
