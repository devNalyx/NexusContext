# 0003. Parse via the generic `tree-sitter-tags` mechanism, not hand-written per-language queries

Status: Accepted
Date: Phase 11 (language expansion)

## Context

Adding a new supported language originally meant hand-writing a
tree-sitter query for that language's grammar - functions, types, call
sites, all captured manually. That worked but didn't scale: a ceiling of
roughly 2 languages before the per-language effort stopped being worth it,
directly capping how many languages the tool could realistically support.

## Decision

Migrate to consuming the `TAGS_QUERY` that nearly every actively-
maintained tree-sitter grammar crate already bundles - a community-
maintained query using conventional capture names (`@definition.function`,
`@reference.call`, ...), the same mechanism GitHub code navigation and
Neovim's `nvim-treesitter` rely on.

## Alternatives considered

- **Keep hand-writing queries per language.** Rejected: doesn't scale past
  a couple of languages, and duplicates maintenance effort the grammar
  authors already do for their own `TAGS_QUERY`.
- **A separate language-server-per-language approach (LSP-based parsing).**
  Not seriously considered at this phase - a much heavier dependency
  surface per language than tree-sitter grammars. (LSP does come back
  later as a *complementary* enrichment idea, not a parsing replacement -
  see issue #10's proposal, deliberately not built.)

## Consequences

- Adding a language now costs "add the grammar crate + map its
  extensions," not "write and debug a new query language" - this is what
  made the current 11-language list realistic to reach.
- Call-graph fidelity now varies by language based on how complete that
  grammar's own `TAGS_QUERY` capture set is, not by how much hand-tuning
  effort NexusContext put in - some languages (C/C++/C#/PHP) only get
  structural nodes, no call edges, purely because their bundled
  `TAGS_QUERY` doesn't capture call sites. See [[Language-Support]] for
  the current per-language tier breakdown.
- Resolution itself is still name-based, not import-aware, regardless of
  language - see [[0004-name-based-call-resolution]], a separate decision
  layered on top of whatever the grammar captures.

## Related

[[Indexing-Pipeline]] · [[Language-Support]] · [[0004-name-based-call-resolution]]
