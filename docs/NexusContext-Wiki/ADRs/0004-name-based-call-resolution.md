# 0004. Call-graph resolution is name-based, not import-aware

Status: Accepted
Date: Phase 2-3 (two-pass resolution), reaffirmed through Phase 11 language expansion

## Context

Resolving a call site to the function it actually calls is, done properly,
an import/module-resolution problem per language - `use`/`import`
statements, module paths, visibility rules. Doing that correctly for 11
languages (and growing) is a large, language-specific undertaking, closer
to building a linter or a language server per language than to a
lightweight indexer.

## Decision

Resolve calls by name only, via a two-pass project-wide pass: same-file
matches win outright; a cross-file match resolves only when the callee
name is unique project-wide. Two files each defining a same-named
function, with no local match in the caller's own file, is left
unresolved rather than guessed at.

## Alternatives considered

- **Real import/module resolution per language.** Rejected as
  disproportionate to the tool's actual job - NexusContext gives an agent
  a fast structural map to reason over, not a compiler-grade symbol table.
  The agent itself can read the actual file/imports when precision
  matters for a specific decision.
- **Guessing on ambiguous same-named matches** (e.g., picking the first
  or most-recently-indexed match). Rejected explicitly: an unresolved call
  is honest about the gap; a wrongly-resolved call looks correct and can
  silently mislead an agent tracing a call path it's about to act on.

## Consequences

- Call-graph quality is honestly tiered by language and by ambiguity, not
  presented as uniformly reliable - `trace_call_path`'s and
  `detect_dead_code`'s tool descriptions both state this caveat rather
  than hiding it (moved to [[MCP-Tools]] and [[Known-Limitations]] in
  Phase 22 to keep the caveat out of the per-session schema-token cost).
- `detect_dead_code`'s false-positive rate is a direct consequence of this
  decision, not a separate bug - a function invoked via reflection,
  routing, or dependency injection (never a *direct* named call in the
  source) will never show up as called under a purely name-based scheme.
- This is the single most-repeated caveat across the tool surface -
  worth remembering as the reason before treating any specific instance
  of it as a bug to fix locally.

## Related

[[Indexing-Pipeline]] · [[Known-Limitations]] · [[MCP-Tools]] ·
[[0003-tree-sitter-tags-over-handwritten-queries]]
