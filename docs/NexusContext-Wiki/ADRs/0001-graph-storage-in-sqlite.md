# 0001. Knowledge graph lives in SQLite, not a dedicated graph database

Status: Accepted
Date: Phase 1-2 (original design)

## Context

The core product is a graph: functions/types as nodes, calls/definitions/
containment as edges, queried via BFS-style traversal (`trace_call_path`),
pattern matching (`query_graph`), and full-text search (`search_code`). A
dedicated graph database (Neo4j, or an embedded equivalent) is the obvious
first instinct for that shape of data.

## Decision

Store the graph in plain SQLite (WAL mode), one file per indexed project at
`~/.local/share/nexuscontext/<project-hash>/graph.db` - nodes and edges as
ordinary tables, traversal done in Rust over simple queries, full-text
search via SQLite's own FTS5 extension, and a minimal Cypher-lite layer
(`query_graph`) hand-rolled on top rather than adopted from an existing
graph query engine.

## Alternatives considered

- **A dedicated embedded graph database.** Rejected: extra runtime
  dependency, extra failure mode, extra thing to keep in sync - for a
  single-user, single-machine, disk-backed daemon where query volume and
  graph size (thousands of nodes, not millions) never approach where a
  real graph engine's traversal-algorithm advantages would matter.
- **A server-mode graph database (Neo4j, etc.).** Rejected outright - this
  is a self-hosted, zero-infrastructure tool; requiring a separate running
  service to install would contradict the entire "useful with zero config"
  positioning.

## Consequences

- One file per project, trivially backed up/exported/deleted - no separate
  service to install, run, or keep alive.
- `query_graph`'s Cypher-lite is deliberately minimal (one pattern shape)
  rather than a real query language, since it's hand-rolled, not adopted -
  see [[Known-Limitations]].
- Traversal (BFS for `trace_call_path`, `detect_dead_code`) is implemented
  in application code over simple row queries, not pushed into the storage
  engine - fine at this project's actual scale, would need revisiting only
  if graph size grew by orders of magnitude.
- This choice was never revisited even after Phase 12 added embeddings and
  Phase 15 added a real vector-adjacent workload - see
  [[0002-embeddings-as-sqlite-blobs]], which extends the same SQLite file
  rather than reopening this decision.

## Related

[[Architecture]] · [[Storage-and-Data-Model]] · [[0002-embeddings-as-sqlite-blobs]]
