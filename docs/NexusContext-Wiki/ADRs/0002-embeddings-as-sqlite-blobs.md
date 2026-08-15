# 0002. Embeddings stored as SQLite BLOBs, not a dedicated vector store

Status: Accepted
Date: Phase 12 (real embeddings client)

## Context

The original design proposal specified LanceDB (embedded, disk-backed, one
table per project) as the vector store for semantic search
(`search_codebase`/`query_memory`). By the time embeddings were actually
built (Phase 12), full-text search had already landed on SQLite FTS5
rather than a separate engine, and the knowledge graph itself
([[0001-graph-storage-in-sqlite|0001]]) had already proven SQLite
sufficient for everything built so far.

## Decision

Store embedding vectors as plain BLOBs in the same `graph.db` SQLite file,
ranked by brute-force cosine similarity computed in Rust at query time -
no LanceDB, no separate vector index structure, no ANN (approximate
nearest neighbor) algorithm.

## Alternatives considered

- **LanceDB, as originally proposed.** Rejected once it came time to
  actually build it: a second embedded storage engine, alongside SQLite,
  for a workload (thousands of chunks per project, not millions) where
  brute-force cosine similarity is fast enough not to need it. Revisiting
  this was explicitly deferred, not forgotten - see the "Open Risks"
  section of `README.md`: "moot ... revisit only once the embeddings
  pipeline actually gets built" was written *before* Phase 12, and Phase
  12 confirmed the simpler path held.
- **A different dedicated vector database** (Qdrant, Chroma, etc.).
  Rejected for the same self-hosted/zero-infrastructure reasons as
  [[0001-graph-storage-in-sqlite|0001]].

## Consequences

- One storage engine for the whole daemon, not two - no dual-write
  consistency problem between a graph store and a vector store.
- Query cost is O(n) per semantic search over a project's chunk count -
  fine at this project's real scale; would need revisiting if a single
  project's embedded-chunk count grew by orders of magnitude (tens of
  thousands+), since brute-force cosine similarity doesn't scale the way
  an ANN index does.
- Embeddings reuse across reindexes (keyed by `qualified_name`, see
  [[Indexing-Pipeline]]) was straightforward to build on top of a plain
  table - would have needed reimplementing against LanceDB's own API
  otherwise.

## Related

[[0001-graph-storage-in-sqlite]] · [[Storage-and-Data-Model]] ·
[[Embeddings-and-Semantic-Search]]
