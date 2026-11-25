# 0019. Make context economy, not graph coverage, the flagship

Status: Accepted
Date: 2026-11-24

## Context

Graphify has distributed graph/wiki/cloud-memory mindshare and publishes large token
ratios against a naive "send the whole corpus" baseline. NC is smaller and its graph
has documented accuracy ceilings (ADR 0004, [[Known-Limitations]]). Competing on
graph coverage or hosted features would spend NC's limited effort where it is weakest.

NC already has what Graphify publicly says nobody has: a realistic baseline-vs-NC
benchmark with independently checked correctness ([[Product-Thesis-Validation]]),
tool presets that cut advertised schema (ADR 0005), in-band freshness, and
`get_session_usage`. The honest gap is that `schema_tax` currently measures only the
unfiltered ceiling and NC's token estimate is bytes/4, not provider tokens.

## Decision

Position NC as the **context economy control plane**: measure and cap what an agent
spends, prove which memories pay for themselves. Build the OpenCode V2 plug around
a ledger that prefers real harness token/cost data and labels the bytes/4 fallback.
Ship declarative code-anchored facts alongside the absorbed procedural memory, with
a hard injection budget. Publish only correctness-gated comparisons, never raw
ratio claims.

## Alternatives considered

- **Out-Graphify Graphify** (hosted MCP, wiki, cloud memory, PR reviews): too large,
  too late, and against the local-first premise.
- **Stay a pure graph server:** leaves the strongest existing differentiator
  (honest measurement + realistic benchmark) unused.

## Consequences

`get_session_usage` and tool presets become product surfaces, not diagnostics:
schema tax must report both filtered and ceiling numbers; preset drift tests must
stay green as tools are added. Marketing claims must cite the benchmark's losses as
well as wins. Work on import-aware call resolution remains valuable but is no
longer the flagship blocker.
