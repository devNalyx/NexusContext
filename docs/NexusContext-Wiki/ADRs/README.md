# Architectural Decision Records

A lightweight log of decisions that shaped NexusContext's architecture -
not a full design doc per decision, just enough that a future contributor
(including a future session of whoever's reading this) can see *why*
something is built the way it is without re-deriving it from a diff or
re-litigating a call that was already made deliberately.

This log started 2026-08-15, backfilled with the decisions already load-bearing
in the codebase at that point (0001-0007). `README.md`'s phase-by-phase log
remains the detailed build history - an ADR here is the compressed,
durable "what we decided and why" for the decisions worth defending later,
not a replacement for that log.

## When to add one

Add an ADR when a decision is genuinely architectural - it constrains what
future work can assume, it was a real fork in the road (more than one
reasonable option existed), or someone is likely to ask "why didn't we just
use X" later. Routine bug fixes, parameter tuning, and additive features
that don't change a load-bearing assumption don't need one - `README.md`'s
phase log already covers those.

## Format

```
# NNNN. Title (imperative-ish, names the decision)

Status: Accepted | Superseded by NNNN | Deprecated
Date: YYYY-MM-DD (or "Phase N" if the exact date predates this log)

## Context
What forced a decision here - the constraint, the pain, the question.

## Decision
What was actually chosen.

## Alternatives considered
What else was on the table and why it lost, briefly.

## Consequences
What this commits future work to, including the honest downsides.
```

Numbered sequentially, never renumbered or deleted - a superseded decision
gets a new ADR that says so and a `Status` update on the old one, so the
history stays legible.

## Index

| # | Title | Status |
|---|---|---|
| [0001](0001-graph-storage-in-sqlite.md) | Knowledge graph lives in SQLite, not a dedicated graph database | Accepted |
| [0002](0002-embeddings-as-sqlite-blobs.md) | Embeddings stored as SQLite BLOBs, not a dedicated vector store | Superseded by 0010 |
| [0003](0003-tree-sitter-tags-over-handwritten-queries.md) | Parse via the generic `tree-sitter-tags` mechanism, not hand-written per-language queries | Accepted |
| [0004](0004-name-based-call-resolution.md) | Call-graph resolution is name-based, not import-aware | Accepted |
| [0005](0005-mcp-tool-presets.md) | MCP tools are gated behind presets (`minimal`/`standard`/`full`), not always-on | Accepted |
| [0006](0006-full-rebuild-reindexing.md) | Reindexing is a full rebuild, not incremental diffing | Accepted |
| [0007](0007-embeddings-safe-by-default.md) | Embeddings endpoints are loopback/private by default, remote is opt-in | Superseded by 0010 |
| [0008](0008-lsp-resolved-edges-are-a-distinct-kind.md) | LSP-resolved call edges are a distinct kind, never merged into the static ones | Accepted |
| [0009](0009-windows-gets-mcp-and-cli-only.md) | Windows ships `nexusd mcp` + `nexus` CLI only, via `cfg(unix)` module gating | Accepted |
| [0010](0010-remove-embeddings-subsystem.md) | Remove the optional embeddings/semantic-search subsystem entirely | Accepted |
| [0011](0011-explicit-bounds-on-repo-size-dependent-operations.md) | Every repository-size/agent-request-dependent operation gets an explicit bound | Accepted |
| [0012](0012-allowed-roots-enforced-uniformly-across-repo-path-tools.md) | `allowed_roots` is enforced uniformly across every `repo_path`-accepting MCP tool, via one shared check | Accepted |
| [0013](0013-provenance-confidence-surfaced-through-trace-call-path.md) | `CALLS` vs `CALLS_RESOLVED` provenance/confidence is surfaced through `trace_call_path`'s JSON response | Accepted |

## Related

[[Home]] · [[Architecture]] · [[Known-Limitations]]
