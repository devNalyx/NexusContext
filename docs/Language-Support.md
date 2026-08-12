# Language Support

11 languages via the generic `tree-sitter-tags` mechanism (see
[[Indexing-Pipeline]]). Definitions, types, and architecture summaries are
solid for **all 11** — this table is specifically about call-graph edge
quality, which depends on how complete each language's own community-
maintained tags query is.

## Full call graph (functions, types, and call edges all resolve)

Rust, Python, JavaScript, TypeScript/TSX, Go, Java, Ruby.

## Structural only (functions/types correct; no call edges)

- **C, C++** — the bundled `tags.scm` for these has no call-reference
  pattern at all.
- **C#** — only captures member-access calls (`obj.Method()`), not bare
  calls.
- **PHP** — similarly only captures qualified/variable calls.

This mirrors the same tiering other projects using this technique run
into — not every language gets equally good results from tree-sitter-only
analysis, and that's stated here plainly rather than smoothed over.

## Two real parsing bugs, for context

Worth knowing if you're ever debugging a resolution gap:

1. `tree-sitter-tags`'s `Tag::span` is deliberately just the *name token's*
   position (built for "jump to definition" UIs), not the full
   definition's range. Using it directly collapsed every multi-line
   function to a single line. Fixed by deriving line numbers from
   `Tag::range` (the correct byte range) via a line-offset index instead.
2. Some languages' bundled `tags.scm` only tags the function *signature* as
   the definition's range (C/C++'s `function_declarator`, not the whole
   `function_definition` body) — a call inside the body then falls outside
   a naive "does this call fall within the definition's range" check.
   Replaced that with "which function's *start* most recently precedes this
   call" — doesn't depend on the range's end being accurate at all.

## Related

[[Indexing-Pipeline]] · [[Known-Limitations]]
