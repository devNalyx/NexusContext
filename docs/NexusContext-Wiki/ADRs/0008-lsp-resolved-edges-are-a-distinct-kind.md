# 0008. LSP-resolved call edges are a distinct kind, never merged into the static ones

Status: Accepted
Date: 2026-08-15 (Phase 32)

## Context

Issue #10 proposed enriching the graph with real symbol resolution from a
language server, to fix cases [[0004-name-based-call-resolution|the
existing name-based static pass]] provably gets wrong - a cross-file call
resolves only when the callee name is unique project-wide, so a genuine
call can be left unresolved (or, in principle, could be misattributed)
purely because two files define a same-named function. A real LSP server
(`rust-analyzer` for the pilot) can resolve that call correctly. The
question this decision answers: once resolved, does that edge become
indistinguishable from a normal `Calls` edge, or does it stay marked as
what it is?

## Decision

LSP-resolved references are stored as a new, distinct edge kind
(`EdgeKind::CallsResolved` / `CALLS_RESOLVED`), never merged into or
replacing a `Calls` edge. Enrichment only ever *adds* these alongside
whatever the static pass already found - a project that has never run a
`deep` reindex is byte-for-byte unaffected by this feature existing at
all. Consumers (`trace_call_path`'s BFS, `detect_dead_code`'s inbound-edge
check) union both kinds when walking the call graph, so resolution only
ever adds coverage on top of the static result, never changes it when no
LSP server ran.

## Alternatives considered

- **Fold resolved references directly into `Calls`.** Rejected: this
  would make static (name-matched, sometimes wrong) and resolved
  (server-verified) edges indistinguishable after the fact - the worst of
  both, since you'd get wrong-confidence resolution with no way to audit
  which edges came from which pass. A reviewer flagged this explicitly as
  the property that makes the whole feature safe to ship.
- **A confidence/provenance column on the existing `edges` table** (e.g.
  `source: 'static' | 'lsp'`) instead of a second `kind` value. Rejected
  as unnecessary complexity for the same outcome - `kind` already is the
  provenance field for every other edge type in this schema (`Defines`,
  `Contains`), so reusing that mechanism keeps the schema uniform rather
  than adding a second, edge-kind-specific column that only `Calls` rows
  would ever populate.
- **Replace a static `Calls` edge with the resolved one when both exist
  for the same pair.** Rejected: replacing loses the fact that the static
  pass found it too (useful signal on its own - it means the name-based
  heuristic and real resolution agree), and "replace" requires a
  match/dedupe step this pilot doesn't need, since a harmless duplicate
  pair across two kinds costs nothing functionally (BFS traversal already
  dedupes via a visited-set).

## Consequences

- Every future enrichment source (a second language's LSP, or the
  "agent-loopback" idea flagged but not pursued on issue #10) must follow
  the same shape if it wants to plug into `trace_call_path`/
  `detect_dead_code`: add a new distinct edge kind (or reuse
  `CallsResolved` if the semantics genuinely match), union it into the
  same two consumers, never overwrite a `Calls` row.
- `detect_dead_code`'s false-positive rate (see ADR 0004) is now provably
  reducible, not just theoretically - the regression test
  (`enrich::real_rust_analyzer_tests`) demonstrates a concrete case the
  static pass gets wrong that `CALLS_RESOLVED` fixes, run against a real
  `rust-analyzer` binary, not asserted from the protocol spec.
- Any tool that reads `edges` directly without going through
  `trace_calls`/`dead_functions` (e.g. a future Cypher-lite `query_graph`
  pattern targeting `CALLS` specifically) will *not* see resolved edges
  unless it explicitly asks for `CALLS_RESOLVED` too - the union lives in
  the two Rust functions that need it, not in the schema or a view.

## Related

[[Storage-and-Data-Model]] · [[MCP-Tools]] ·
[[0004-name-based-call-resolution]]
