# 0010. Remove the optional embeddings/semantic-search subsystem entirely

Status: Accepted
Date: 2026-08-26

## Context

The embeddings/semantic-search layer (`[embeddings]` config, the
`search_codebase`/`query_memory` MCP tools, `nexus search-codebase`/
`nexus test-embeddings` on the CLI, the GUI's Config tab, and the
`embeddings` SQLite table storing vectors as BLOBs - see
[[0002-embeddings-as-sqlite-blobs|0002]] and
[[0007-embeddings-safe-by-default|0007]]) was built as an explicitly
optional, off-by-default layer on top of the structural knowledge graph.
Issue #62 asked the question directly: is it worth keeping?

Three things came out of evaluating that:

- **It was unused.** Off by default, and nothing in this project's own
  dogfooding, review passes, or usage data showed anyone turning it on for
  real work - the "narrowing layer for large codebases" pitch never
  actually got exercised.
- **It cuts against the project's own thesis.** NexusContext's entire
  pitch (see [[Home]], [[Architecture]]) is that a *structural* graph -
  functions, types, real call edges - beats vector similarity for a coding
  agent's actual questions ("who calls this," "what's the architecture,"
  "what changed"). Keeping a whole optional subsystem whose value
  proposition is the thing the rest of the project argues against was an
  inconsistency, not a feature.
- **It was real, non-trivial surface.** A config section with its own
  policy model (`NotConfigured`/`Disabled`/`RemoteBlocked`/`Allowed`), two
  MCP tools with their own response caps, two CLI subcommands, a GUI
  panel, a SQLite table, a reuse-across-reindexes optimization
  ([[0006-full-rebuild-reindexing|0006]]), and its own network-egress
  security story ([[0007-embeddings-safe-by-default|0007]]) - all of it
  had to be maintained, reviewed, and reasoned about on every future
  change, in proportion to how little it was used.

## Decision

Remove the embeddings/semantic-search subsystem entirely, not gate it
further or leave it opt-in-and-dormant. Concretely:

- `crates/nexus-index/src/embeddings.rs` deleted; all embeddings code
  paths removed from `graph.rs`, `queries.rs`, `ingest.rs`, `project.rs`.
- The `embeddings` SQLite table and its index are dropped via a migration
  in `GraphStore::open` (`DROP TABLE/INDEX IF EXISTS`, idempotent) so an
  existing on-disk `graph.db` from a pre-0.1.18 install cleans up
  automatically on first open, rather than just being stopped-from-being-
  written-to and left behind.
- `EmbeddingsConfig`/`EmbeddingsPolicy` and `[embeddings]` removed from
  `nexus-core`'s `Config`.
- `search_codebase`/`query_memory` MCP tools removed; `search_graph`,
  `search_code`, and every other structural tool are unchanged.
- `nexus search-codebase`/`nexus test-embeddings` CLI subcommands removed.
- The GUI's Config tab (which was entirely the embeddings panel) removed;
  no GUI-editable config field exists in its place today.
- ADRs 0002 and 0007 kept, marked Superseded, not deleted - they're the
  accurate historical record of decisions made while the subsystem
  existed.

## Alternatives considered

- **Gate it further** (e.g. a build-time feature flag, or push it behind
  an even more obscure config path). Rejected: the issue this ADR closes
  already established the removal decision - a feature flag still carries
  the maintenance cost of code nobody exercises, just with an extra layer
  of indirection on top.
- **Leave it as-is, off by default.** Rejected for the reasons in
  Context above: unused, cuts against the project's own pitch, and real
  ongoing surface for zero observed benefit.

## Consequences

- Smaller MCP tool surface (12 tools, down from 14) and a smaller config
  schema - less for a new user to read past, less for `tools/list`'s
  fixed per-session token cost to carry.
- No more optional network-egress path in this daemon at all - every tool
  is now a local filesystem read plus a local SQLite query, simplifying
  the security story in [[Security-Model]] (no endpoint/`allow_remote`
  gate to reason about).
- Reindexing lost its one partial-reuse optimization
  ([[0006-full-rebuild-reindexing|0006]]) - a reindex is now uniformly a
  full rebuild, which was already true for everything except embedded
  chunks.
- An existing installation's `graph.db` sheds its `embeddings` table
  automatically on next open - no manual migration step, no stale schema
  left behind.
- If real semantic search is wanted again later, it would need to be
  rebuilt from scratch rather than re-enabled - an accepted cost, given
  it was never actually exercised.

## Related

[[0002-embeddings-as-sqlite-blobs|0002]] ·
[[0007-embeddings-safe-by-default|0007]] ·
[[0006-full-rebuild-reindexing|0006]] · [[Security-Model]] ·
[[Storage-and-Data-Model]] · [[MCP-Tools]] ·
[issue #62](https://github.com/devNalyx/NexusContext/issues/62)
