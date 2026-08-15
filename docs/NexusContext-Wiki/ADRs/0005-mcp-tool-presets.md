# 0005. MCP tools are gated behind presets (`minimal`/`standard`/`full`), not always-on

Status: Accepted
Date: Phase 21 (2026, dogfooding-driven)

## Context

Every MCP session start pays a fixed token cost loading `tools/list`'s
schemas, regardless of whether that session ever calls most of them.
`tool_definitions()` originally returned all tools unconditionally
(~2.5k tokens at the time) with no way to opt into fewer. A user who
restarts agent sessions often was paying this tax repeatedly - traced to
a real, recurring cost, not a theoretical one, written up in
`change_proposal.md`.

## Decision

A `[tools]` config section (`preset = "minimal" | "standard" | "full"`,
default `"standard"`) controls what `tools/list` actually advertises, with
an optional explicit `enabled = [...]` list that takes precedence over
the preset. `tools/list`'s handler loads `Config` and filters
`tool_definitions()`'s output against the resolved set before returning
it - the tools still exist in the binary, they're just not all
advertised by default.

## Alternatives considered

- **Trim tool descriptions instead, keep all tools always advertised.**
  Done too (Phase 22), but insufficient alone - schema *count*, not just
  description length, drives the fixed per-session cost.
- **A single global on/off flag per tool** (fully manual, no presets).
  Rejected as the default UX: too much config for the common case: three
  presets cover read-heavy-core / everyday-useful / everything, and
  `enabled` still exists for the fully-manual case.

## Consequences

- Real behavior change for the default case, not purely additive: a user
  with no `[tools]` section goes from seeing every tool to seeing only the
  `standard` set. Anyone relying on `delete_project`, `query_graph`,
  `search_codebase`, or `query_memory` via MCP needs `preset = "full"` (or
  an explicit `enabled` list).
- New tools must be deliberately placed into `MINIMAL_TOOLS`,
  `STANDARD_EXTRA_TOOLS`, or `FULL_EXTRA_TOOLS` in `tools.rs` - a drift
  guard test (`full_preset_matches_all_tool_definitions`) fails the build
  if a tool is added to `tool_definitions()` without being added to a
  preset, so this can't silently regress in code. **The equivalent guard
  does not exist for prose documentation** (README/wiki/INSTALL.md/landing
  page tool counts) - this has already drifted once in practice (see
  `get_session_usage` shipping in PR #19 without any doc update, caught
  and fixed retroactively in PR #21). Worth a lightweight doc-consistency
  check if it drifts again; not yet built.

## Related

[[MCP-Tools]] · [[Configuration]] · [[0004-name-based-call-resolution]]
