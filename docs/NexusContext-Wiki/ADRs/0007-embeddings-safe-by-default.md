# 0007. Embeddings endpoints are loopback/private by default, remote is opt-in

Status: Superseded by [[0010-remove-embeddings-subsystem|0010]] - the whole
embeddings/semantic-search subsystem this ADR governed was removed. Kept as
the historical record of why this safety property existed while it did.
Date: Phase 7

## Context

Embeddings requires sending code chunks to an HTTP endpoint - unlike every
structural tool, this is the one place NexusContext, by design, sends
project content somewhere outside the local process. Nothing in `config.toml`
otherwise stops a user from pointing `endpoint` at an arbitrary remote URL,
intentionally or by a typo, and having code silently start leaving the
machine.

## Decision

`Config::embeddings_policy()` refuses to use a non-loopback/non-private
embeddings endpoint unless `allow_remote = true` is set explicitly, both in
`config.toml` directly and via the GUI's Config tab checkbox. Embeddings
are also off by default entirely (`embeddings.enabled = true` required) -
this is a second, independent switch on top of the endpoint check.

## Alternatives considered

- **Warn but allow any endpoint by default.** Rejected: a warning that's
  easy to miss doesn't prevent the actual leak; the point is that sending
  code off-box should require an affirmative choice, not just be
  logged.
- **Block remote endpoints entirely, no opt-in.** Rejected as too rigid -
  a legitimate LAN-hosted Ollama/vLLM instance (this project's own
  dogfooding setup uses a Tailscale-range endpoint) is a real, common use
  case; the goal is an explicit choice, not a hard ban.

## Consequences

- A misconfigured or malicious-default remote endpoint fails closed with a
  specific, actionable error rather than silently sending code - verified
  directly (blocking a remote endpoint, then unblocking it with the
  opt-in).
- This policy governs the *endpoint*, not the *content* sent to it - see
  [[Security-Model]] for the separate, still-open question of what's in
  the payload itself (chunk text size caps exist for response-token
  reasons, not data-minimization reasons).
- Two independent gates (`enabled` + `allow_remote`) means two places a
  user must deliberately opt in, not one - a small UX cost, traded
  explicitly for making an accidental data-leak default harder to reach.

## Related

[[0010-remove-embeddings-subsystem|0010]] · [[Security-Model]] · [[Configuration]]
