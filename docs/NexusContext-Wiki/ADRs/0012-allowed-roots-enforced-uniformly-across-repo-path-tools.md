# 0012. `allowed_roots` is enforced uniformly across every `repo_path`-accepting MCP tool, via one shared check

Status: Accepted
Date: 2026-08-26

## Context

Issue #29 fixed a real `..`-traversal bypass of `allowed_roots`
(canonicalize-before-check, not after), but only landed on the two
functions a prior audit happened to be looking at: `get_file_context` and
`detect_changes`. A follow-up audit (issue #61) found the fix hadn't
propagated to the rest of the `repo_path`-accepting query surface:
`search_code`, `get_architecture`, `detect_dead_code`, `call_graph_dot`
(backs the `trace_call_path` MCP tool), and `run_query`/`run_cypher_query`
(backs `search_graph`) all skipped `require_path_allowed` entirely and
went straight from a caller-supplied `repo_path` to `open_store`/
`graph_db_path`. `allowed_roots` empty (unrestricted) is intentional
default behavior, not a bug - the gap was that a user who *did* set
`allowed_roots` got inconsistent protection depending on which of the
roughly nine MCP tools they called, with no indication five of them
weren't covered. For an MCP server whose caller is an LLM agent (issue
#61's "agent confusion / repository prompt injection" framing), a tool
that silently ignores an opted-into safety boundary is a real
confused-deputy gap, not a theoretical one.

## Decision

Every `repo_path`-accepting function in `nexus-index` now calls
`crate::project::require_path_allowed` with a canonicalized path before
touching a store, using the exact ordering `get_file_context` already
established: canonicalize `repo_path` first (erroring clearly if it
doesn't exist), *then* check the canonical form against `allowed_roots`,
and use that same canonical path for the actual DB open - never the raw
caller-supplied one. In `queries.rs`, the four functions that needed this
(`search_code`, `get_architecture`, `detect_dead_code`, `call_graph_dot`)
share one new helper, `canonicalize_and_authorize`, instead of
duplicating the pattern four times. `run_query` in `cypher.rs` gets the
same two-step logic inline, since it's the only `repo_path`-accepting
function in that file.

A consolidated adversarial test suite
(`crates/nexus-index/tests/path_security.rs`) now covers all seven
`repo_path`-accepting query/read functions - the two already-fixed plus
the five newly-fixed - against the same three cases each: a `repo_path`
outside `allowed_roots` is rejected, a `..`-traversal path that resolves
outside `allowed_roots` is rejected even though a raw prefix check would
have accepted it, and a genuine subdirectory of an allowed root is still
accepted. One test module instead of scattering the same three cases
across seven ad hoc unit tests, so a future eighth `repo_path`-accepting
function has one obvious place to add its case rather than reinventing
the setup.

## Alternatives considered

- **Fix each function's call site independently, without a shared
  helper.** Rejected for the four `queries.rs` functions - it's exactly
  the "someone gets the ordering wrong on the fifth call site" risk this
  ADR exists to close, and a shared helper means the canonicalize-then-
  check ordering only has to be gotten right once, not four more times.
  `run_query` stays inline rather than sharing across a crate boundary
  disproportionate to one call site (`cypher.rs` has nothing else that
  would reuse it).
- **A trait/wrapper type that makes `repo_path` un-open-able without
  having passed the check** (a `CheckedRepoPath` newtype `open_store`
  requires). Would close this class of gap even more durably - a future
  function literally couldn't compile against `open_store` without going
  through the check. Not done this pass: a larger refactor across every
  existing call site (including the CLI and control API, which
  legitimately don't need `allowed_roots` enforcement the same way MCP
  tool dispatch does) for a gap this specific test suite plus the shared
  helper already closes for the demonstrated surface. Worth reconsidering
  if a sixth ungated function turns up later.

## Consequences

- `allowed_roots`, once set, now means what its own documentation already
  claimed: every MCP tool that takes a `repo_path` respects it, not a
  subset. See [[Security-Model]]'s "What's opt-in" section, updated
  alongside this.
- A new `repo_path`-accepting function in `nexus-index` that forgets this
  check no longer fails silently - `path_security.rs` is the obvious place
  a reviewer or contributor adds its case, and the existing seven make the
  expected pattern (canonicalize, then `require_path_allowed`, then use
  the canonical path) visible by example.
- Issue #61's broader scope - symlink TOCTOU races, and adversarial-
  prompt-injection scenario coverage beyond path boundaries - is
  explicitly not addressed by this ADR or its PR. The issue stays open for
  that; see the issue for what's left.

## Related

[[Security-Model]] · [issue #61](https://github.com/devNalyx/NexusContext/issues/61) ·
issue #29 (the original canonicalize-before-check fix this extends)
