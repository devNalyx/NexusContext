# 0009. Windows ships `nexusd mcp` + `nexus` CLI only, via `cfg(unix)` module gating

Status: Accepted
Date: 2026-08-15 (Phase 33)

## Context

`nexusd`'s control API (`crates/nexusd/src/control.rs`) is built directly
on `std::os::unix::net::{UnixListener, UnixStream}`, with no cross-platform
abstraction - a choice that made sense when Linux was the only real target
([[Architecture]]'s two-transport design), but meant the *whole* `nexusd`
binary failed to compile on Windows regardless of which subcommand would
actually run at runtime, since Rust compiles all reachable code for the
target. Issue #16 investigated this and found the blocker smaller and more
contained than "needs a platform split" - `nexusd mcp` (stdio only, no
sockets) has zero dependency on any of it.

## Decision

Gate `mod control` (and `mod watcher` alongside it - its only two callers
are `Command::Serve` and `control.rs` itself, so it's equally unreachable
without `serve`) behind `#[cfg(unix)]` in `crates/nexusd/src/main.rs`.
`Command::Serve` gets a `#[cfg(not(unix))]` branch that prints a clear
error pointing at `nexusd mcp` and this issue, instead of a compile
failure. Windows ships `nexusd mcp` + `nexus` CLI - every MCP tool works
fully - with `serve` (the control API, background watcher, GUI target)
simply absent there, not degraded or partially working.

## Alternatives considered

- **Port the control API to a cross-platform transport** (named pipes on
  Windows, or switching everything - Linux/macOS included - to a loopback
  TCP socket for uniformity). Rejected for v1: a real, larger undertaking
  (new framing/auth considerations per transport, or a behavior change on
  every platform to unify on one), and nothing in the actual product
  surface (the MCP tools) needs it - `serve` exists for the GUI/background
  watcher, both of which are separate, larger follow-ups on Windows in
  their own right (there's no Windows GUI build plan, and a background
  watcher needs its own control-plane decision regardless of socket type).
  Not ruled out permanently, just not this pass's job.
- **Attempt the build with Unix-only code left in and let it fail per
  subcommand at runtime instead of at compile time.** Not viable in Rust
  as written - the compiler doesn't know `Command::Serve` won't be
  reached at runtime; `std::os::unix::net` types simply don't exist on
  the Windows target, so this is a compile-time-or-nothing decision, not
  a runtime one.
- **Ship no Windows build at all until `serve` has real parity.**
  Rejected: this is the same reasoning issue #16 itself made - `nexusd
  mcp` is the actual product surface (every MCP tool works fully), and
  gating an entire platform's release on a feature (`serve`) most MCP
  users never directly touch would trade a real, immediately useful
  release for a theoretical completeness that doesn't change what an
  agent-driving user actually gets.

## Consequences

- Windows has no persistent background daemon, so no auto-reindex-on-
  file-change and no warm/cold project tracking there - a Windows user
  reindexes manually (`nexus reindex`) or eats the first-query catch-up
  cost via `touch_and_catchup`, same cold-project behavior every platform
  already has for a project that's gone cold, just without a watcher ever
  making one warm on its own. See [[Watcher-and-Freshness]].
- No GUI or GNOME-extension-equivalent on Windows is possible until
  `serve` exists there, since both talk to the control socket exclusively
  - this decision doesn't block that (a future named-pipe-based `serve`
  could still land later), but it doesn't get built for free either.
- Every future addition to `control.rs`/`watcher.rs` stays implicitly
  Unix-only without further action - a contributor adding a new control
  method doesn't need to think about Windows at all, since the whole
  module never compiles there. The moment someone *does* want `serve` on
  Windows, this ADR's alternatives-considered section is the starting
  point, not a re-derivation from scratch.

## Related

[[Architecture]] · [[Known-Limitations]] · [[Watcher-and-Freshness]] ·
[issue #16](https://github.com/devNalyx/NexusContext/issues/16)
