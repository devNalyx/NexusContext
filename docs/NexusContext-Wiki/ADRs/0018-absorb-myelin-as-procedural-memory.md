# 0018. Absorb Myelin as NC's procedural memory

Status: Accepted
Date: 2026-11-24

## Context

`devNalyx/Myelin` already implements the procedural-memory loop NC needs for an
auto second brain: redacted `SessionEnd` ingestion, deterministic candidate staging,
a review queue, promotion into living `SKILL.md` files, correction/confirmation
feedback, usage counting and 30-day atrophy. It verified the loop over real MCP
stdio. NC's graph has no such loop.

Running both as separate MCP servers would double daemons, configs, installers and
tool schemas — directly against NC's context-economy thesis (ADR 0005). But Myelin
has no `license` field, so its code cannot be treated as reusable until that is set.

## Decision

Port Myelin's four crates (`myelin-core`, `myelin-index`, `myelind`, `myelin-cli`)
into the NC workspace under NC's MIT license, set before any code is copied. Keep
their names and module boundaries for the port so existing tests stay meaningful;
fold their MCP tools into NC's preset system and expose the CLI as `nexus memory …`.

Transcript data remains procedural-only and behind Myelin's review/redaction path.
NC's graph never ingests raw conversation turns. Procedural staleness remains
last-used atrophy; declarative staleness will be code-change based.

## Alternatives considered

- **Compose over MCP, keep both repos:** no code merge, but two processes and two
  tool catalogs; weakens the flagship "one ledger, one budget" story.
- **Build a new procedural memory in NC:** discards a working, tested loop and
  re-litigates decisions Myelin already made (no daemon-side LLM, deterministic
  matching, OKF-style portability).

## Consequences

One install, one control socket, one preset system, one review queue. NC inherits
Myelin's caveats: transcript parser verified against essentially one real session,
matching thresholds are admitted guesses, and no embeddings (deliberately) — all
must be documented, not silently fixed during the port. The merged workspace is
MIT; Myelin's dormant repo can remain as historical context.
