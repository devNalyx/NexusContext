# 0015. O_NOFOLLOW is defense-in-depth against symlink-substitution TOCTOU, not full TOCTOU-proofing

Status: Accepted
Date: 2026-08-27

## Context

Issue #72 (split out of #61, see ADR 0012's 2026-08-27 amendment and the
"Symlink escape vs. TOCTOU vs. confused-deputy" section of
Security-Model.md) tracks a TOCTOU (time-of-check-to-time-of-use) gap: a
co-resident attacker with concurrent filesystem write access could swap a
real file/directory for a symlink *between* the existing
canonicalize()+`allowed_roots` check and the actual `fs::read`/DB open
that follows it, since nothing re-verifies the resolved path is still what
it was at read time.

The issue's original full scope - `openat2(RESOLVE_NO_SYMLINKS)` (Linux
5.6+) or equivalent atomic check-and-open, plus whatever's feasible on
macOS and Windows - is genuinely large, platform-specific work: it needs
resolving the entire path relative to an already-open directory fd rather
than by name at any point, which touches every filesystem-read call site,
not just the two hot ones. Not worth blocking a cheaper, real improvement
on that full scope landing.

## Decision

Ship the cheap, high-value slice instead: on Unix, open files with
`O_NOFOLLOW` at the two call sites that actually touch the filesystem
after a `canonicalize()`/`allowed_roots` check passes -
`get_file_context`'s file read and the indexer's `read_source_capped`
(`crates/nexus-index/src/ingest.rs`) - via a shared
`nexus-index::secure_fs` module (`read_verified` / `read_to_string_verified`).

`O_NOFOLLOW` makes the `open()` call fail (`ELOOP`) if the final path
component is a symlink, rather than transparently following it. Mapped to
a clean, non-leaking error message
("path resolved to something unexpected mid-check...") rather than the raw
OS errno, matching the error style already used elsewhere in these
functions (e.g. `canonicalize_and_authorize`'s "repo_path does not
exist").

This is gated `#[cfg(unix)]`. On Windows and macOS, behavior is unchanged
- no `O_NOFOLLOW` and no substitute mechanism attempted. `libc` was added
as a direct, Unix-only (`[target.'cfg(unix)'.dependencies]`) dependency of
`nexus-index` for the `O_NOFOLLOW`/`ELOOP` constants; it was already
present transitively, so this adds no new supply-chain surface.

## Alternatives considered

- **Full `openat2`-based atomic resolution now.** Correctly closes the
  entire TOCTOU window, including non-symlink substitution, but is Linux-
  only by default (needs a fallback path anyway), requires threading an
  open directory fd through every intermediate path component (not a
  drop-in change to the two call sites), and has no macOS equivalent and
  a different Windows story - a multi-crate, multi-platform effort. Left
  for a future pass if the threat model's priority ever rises enough to
  justify it (see Security-Model.md's framing of why it's lower priority
  today: it requires a co-resident attacker with local write access, who
  generally has cheaper paths to the same data already).
- **`fstat` inode/device comparison after a plain open.** Would catch
  *some* non-symlink substitutions (a swap to a different inode) but not
  all (e.g. content rewritten in place, same inode), and doesn't stop a
  symlink from being followed in the first place the way `O_NOFOLLOW`
  does - strictly weaker for the attack shape this pass targets, so not
  pursued as a substitute.
- **Do nothing until the full fix lands.** Rejected: `O_NOFOLLOW` closes
  the cheapest, most common attack shape (symlink substitution) today, for
  a small, self-contained, well-tested change. Leaving that on the table
  while waiting for a much larger effort isn't a good trade.

## Consequences

- **What's now covered:** a path swapped for a symlink between the
  canonicalize/`allowed_roots` check and the actual read, on Unix (Linux +
  macOS's `cfg(unix)` counts here too), now fails cleanly instead of
  silently following the attacker's symlink target. Proven by
  `secure_fs::tests::rejects_a_file_swapped_for_a_symlink_between_check_and_read`,
  which performs the check-step then swaps the file for an outside-`allowed_roots`
  symlink before the read-step, confirming the read fails and never returns
  the substituted content.
- **What's still NOT covered (the honest gap):** a path swapped for a
  *different regular file or directory* at the same name between check and
  use - `O_NOFOLLOW` only rejects symlinks, not other substituted content.
  Full closure needs the atomic `openat2`-based approach described above,
  which remains out of scope.
- **Windows and macOS:** unchanged. No `O_NOFOLLOW` equivalent is attempted
  on either - a documented gap, not a silent one.
- Issue #72 stays open (not closed) to track the remaining atomic-open
  work; this ADR and this PR only address the symlink-substitution slice.
