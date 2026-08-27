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

## Update (2026-08-27): closing out #61's remaining checklist items

Issue #61 kept two checkboxes open after this ADR's original PR (#65):
"address symlink escape scenarios" and "add prompt-injection/confused-
deputy tests". Both are closed out now, on top of the fix above, without
changing any production code - only tests and this doc.

**Symlink escape is already defended, by the same mechanism as the
`..`-traversal fix above.** `std::path::Path::canonicalize()` resolves
every symlink component in a path, following the OS's own realpath(3)
semantics - it doesn't just normalize `.`/`..`, it substitutes each
symlink's actual target on disk. Since every `repo_path`/`file` argument
is canonicalized *before* the `allowed_roots` check runs (the exact
ordering this ADR already established), a symlink sitting inside an
allowed root but pointing outside it canonicalizes to that outside
location and gets rejected the same way a raw `../../etc` path does -
there is no separate code path for symlinks to slip through. This is
proven directly now: `crates/nexus-index/tests/path_security.rs` gained
adversarial cases planting a symlink inside an allowed root pointing
outside it (rejected, both as a `file` argument and as `repo_path`
itself), plus the reverse case - a symlink that stays inside the same
allowed root - confirmed to *not* be falsely rejected.

**True TOCTOU (time-of-check-to-time-of-use) races are a different,
explicitly out-of-scope threat model, split into
[issue #72](https://github.com/devNalyx/NexusContext/issues/72).** The
symlink case above is a single synchronous request: the symlink exists
before the request starts, and canonicalize+check resolves it once,
before any file is touched. A true TOCTOU race requires an attacker with
**concurrent filesystem write access** to swap a real file/directory for
a symlink *during* the request - after the check, before the
`fs::read`/DB open that follows it. NexusContext does not defend against
that today (no `O_NOFOLLOW` + inode comparison, no atomic
open-and-verify). That's intentionally not fixed in this pass: it
requires a co-resident attacker who already has local write access to the
filesystem racing the daemon's own check-then-use window - a
categorically higher bar than "repository content manipulated an agent
into asking for the wrong path" (#61's actual threat model, which needs
no race at all and is fully closed by server-side enforcement regardless
of caller intent). It's also platform-specific, non-trivial work
(`openat2`/`O_NOFOLLOW` on Linux, different and mostly-unavailable
mechanisms on macOS, yet another one on Windows). See issue #72 for the
proposed scope.

**Prompt-injection/confused-deputy is defended by design, and now tested
explicitly under that framing.** The security boundary
(`require_path_allowed` against `allowed_roots`) is enforced entirely
server-side, independent of what convinced the calling agent to ask for a
given path - it doesn't matter whether the agent's request originated
from the user's own intent or from something a malicious file in the
repository said. `path_security.rs` now has tests framed explicitly
around this scenario rather than leaving it implicit in the generic
"outside allowed_roots" cases: one using a fake
`$HOME/.ssh/id_rsa`-shaped path (the issue's own example of what a
manipulated agent might be steered toward), and one confirming
`search_code` enforces the same boundary, not just file reads.

## Update (2026-08-27): investigated, but did not extend, Windows CI coverage (#83)

Flagged by #78's independent review: `path_security.rs` is entirely
`#[cfg(unix)]`-gated, so none of it runs on the `test-windows` CI job. Two
questions worth separating, since it's easy to conflate them:

**Is symlink creation itself the blocker on Windows CI?** No, or at least
not anymore. Probed directly: a throwaway `#[cfg(windows)]` test calling
`std::os::windows::fs::symlink_file` was pushed and run on this repo's own
`test-windows` job (`windows-latest` GitHub Actions runner) - it succeeded
without elevation or a Developer Mode step. Removed again once the answer
was confirmed; the finding is what's kept, not the throwaway test.

**Is that actually why the file is Unix-only?** No - and this is the real
finding. Every test in `path_security.rs`, symlink-specific or not, depends
on `setup_fake_home`/`FakeHome` redirecting `$HOME`/`$XDG_CONFIG_HOME` so
`nexus_core::Paths::resolve()` picks up a scratch `config.toml` with a
controlled `allowed_roots`. That only works because `directories::
ProjectDirs`'s Unix backends resolve through those env vars. Its Windows
backend goes through the Win32 known-folder API
(`SHGetKnownFolderPath`), which no environment variable a test process sets
can redirect - and there is no other test-only override hook for
`config_dir` anywhere in the codebase (`NEXUS_CACHE_DIR` only overrides
`data_dir`). So the outside-root/`..`-traversal/confused-deputy cases that
don't touch a symlink are just as blocked on Windows today as the symlink
ones are, for a completely different reason: there is currently no way at
all to inject `allowed_roots` into these functions in a Windows test
process.

**Decision: did not split the file or force a partial Windows subset this
pass.** Splitting on "does this case use a symlink" would still leave every
resulting group unable to run on Windows, since the blocker is the
config-injection mechanism, not the symlink calls. Building a proper
cross-platform config-injection test harness (e.g. a `Paths::resolve_from`
variant, or a `NEXUS_CONFIG_DIR` override threaded through every call site)
is a real, separately-scoped piece of work, not a test tweak - filed as
[issue #85](https://github.com/devNalyx/NexusContext/issues/85) rather than
attempted inline here. Once that exists, the symlink-creation finding above
means the symlink-specific cases can extend to Windows immediately, with no
further platform investigation needed.

## Update (2026-08-27): `NEXUS_CONFIG_DIR` harness built, Windows CI now runs the suite (#85, #83)

`nexus_core::Paths::resolve()` gained a `NEXUS_CONFIG_DIR` env override for
`config_dir`, mirroring the existing `NEXUS_CACHE_DIR` override for
`data_dir` - a plain env var read, no `directories`-crate/OS-API
involvement, so it behaves identically on every platform. `path_security.
rs`'s `setup_fake_home` now sets this directly instead of redirecting
`$HOME`/`$XDG_CONFIG_HOME`, and the file's blanket `#![cfg(unix)]` gate is
removed - only the three symlink-creating tests keep an individual
`#[cfg(unix)]`. Verified via real `test-windows` CI run, not just local
compilation (see PR closing this issue and #83 for the job log evidence).

## Related

[[Security-Model]] · [issue #61](https://github.com/devNalyx/NexusContext/issues/61) ·
issue #29 (the original canonicalize-before-check fix this extends) ·
[issue #72](https://github.com/devNalyx/NexusContext/issues/72) (TOCTOU
hardening, split out as a separate threat model)
