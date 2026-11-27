# Flagship Reboot Notes (handoff for a fresh session)

Status: **P0 complete** — originally written 2026-11-24 as a reboot handoff;
updated after the first implementation session so a fresh session resumes at P1
without re-deriving the strategy.

## Current-state note — issue #133 (2026-11-27)

A fresh implementation session is working on branch
`feat/obsidian-secondbrain-integration`; the handoff state below remains
historical. In progress: an off-by-default `[vault_index]` (metadata-only)
Markdown extractor feeding the existing SQLite graph, the opt-in
`vault_context` read tool (31 tools total, presets 5 / 13 / 31), explicit
`sources` fan-out across five read tools, and a user-level `secondbrain` Agent
Skill shipped under `skills/secondbrain/` and installed by
`nexus install opencode --global --yes`. No release evidence yet — see
[[Obsidian-Secondbrain]] and ADRs 0021/0022 before touching docs or tests.

## Read these first, in order

1. [[Flagship-Proposal]] — thesis, locked decisions, non-goals, risks.
2. [[Flagship-Implementation-Plan]] — exact tasks, touch points, gates, current status.
3. ADR 0018 and ADR 0019 — why Myelin is absorbed and why context economy is the
   flagship.
4. [[Product-Thesis-Validation]] — the honesty standard every benchmark must meet.

## State at handoff

- Repo: `/home/opsquad/Workspace/NexusContext`, git `d0aa499`, clean before docs.
- Only changes: five new untracked Markdown files in `docs/NexusContext-Wiki/`
  (proposal, plan, reboot notes, ADR 0018, ADR 0019) plus index/link edits.
- P0 landed (not committed): dual `schema_tax` reporting in `nexusd`, config
  threading through `mcp.rs`, regression test, doc updates, and a new V2 plugin
  scaffold at `integrations/opencode-nexuscontext/` with `npm test` smoke test.
- Full workspace fmt/clippy/tests green after P0 (nexusd 50/50).
- Myelin reference clone: `/tmp/opencode/Myelin` (shallow clone, read-only source
  of truth for the port; treat as scratch, not a worktree).

## Locked decisions (do not re-litigate without the user)

| Topic | Decision |
|---|---|
| Flagship | Context-economy control plane / "NexusContext Brain", not a Graphify clone |
| Memory in v1 | Curated facts: decisions, incidents, lessons, dead ends, owners. No transcript, no code structure, no preferences |
| Sharing | Per-repo local-first SQLite + portable Markdown/JSON for git; no team cloud |
| Trust | Myelin's loop: deterministic staging → agent decides → human/CLI verifies → living artifact with feedback; redact before storage |
| Injection | Tiny verified facts only, hard ~800-token default; agent may pull more |
| Staleness | Facts: anchored file changed → stale; skills: last-used atrophy. Distinct labels |
| Ledger | Real harness tokens/cost keyed by session; labelled bytes/4 fallback |
| Presets | `recall` in `standard`; curation/ingestion in `full` or plugin-only |
| Harness #1 | OpenCode V2 native plug |
| Harness #2 | Claude Code via `SessionEnd` + MCP |
| Myelin | Absorbed into NC, MIT set before copying, no code merge before that |
| Success metric | Correctness-gated token/cost savings + stale-memory rate |

## P0 is done — next steps are P1

1. Set MIT in the merged workspace before copying any Myelin file (ADR 0018).
2. Add `myelin-core`, `myelin-index`, `myelind`, `myelin-cli` to NC's workspace
   members; port files unchanged first so the ~80 inherited tests stay meaningful.
3. Fold Myelin's 11 tools into NC presets without raising the default advertised
   set; update the doc-drift test and all five count-bearing docs together.
4. Expose the CLI as `nexus memory …`; then the GUI review queue.

P0 details, for the record: `schema_tax` now reports `bytes_ceiling` /
`estimated_tokens_ceiling` and `bytes_advertised` / `estimated_tokens_advertised`;
`tools::call(params, config)` receives the same resolved config as `tools/list`;
the plugin scaffold uses the current V2 API (see below).

## Known traps

- `docs/Phase-History.md` is a frozen historical log; do not "fix" its old tool
  counts or embeddings references. README sections 1-3/5-6 are the current overview.
- Myelin has no `license` field; NC is MIT with a `LICENSE` file. Set MIT in the
  merged workspace before copying code (ADR 0018).
- NC's `get_session_usage` is per-MCP-process, not harness-session keyed; the plugin
  must supply the real identity/tokens, not NC's bytes/4 guess.
- Don't conflate skill atrophy with fact staleness in the GUI or recall defaults.
- Graphify's 136.8x/83.6x claims are against a naive baseline and their docs say
  so; never quote them as a quality or billing comparison.
- Graphify comparison page admits nobody has run both on the same tasks — do not
  publish a head-to-head until that is literally true.

## Reference facts gathered

- NC baseline before P1: Rust workspace v0.1.19, 12 MCP tools, presets 5/10/12,
  MIT, CI fmt/clippy `-D warnings` + tests on Linux/macOS/Windows. After P1 the
  single catalog was 23 tools with presets 5/11/23; P2 adds curated-fact
  tools, taking it to 29 with presets 5/12/29.
- Myelin: 4 crates, 11 MCP tools, 13 CLI subcommands, 80 `#[test]` attributes,
  no license field, last push 2026-07-18, transcript parser verified against one
  real session, thresholds admitted guesses.
- OpenCode V2 surfaces (re-verified 2026-11-24, **updated API**): one plugin
  package with server/TUI/GUI/RPC parts, loaded from `.opencode/plugins/` or
  `plugins` in `opencode.json(c)`; SDK `@opencode/plugin` with `Plugin.define`;
  runtime hooks `ctx.session.hook("prompt"|"context"|"compaction"|…)` and
  `ctx.tool.hook("execute.before"|"execute.after")`;
  `ctx.session.instructions.entry.put/list/remove`, `ctx.storage`,
  `ctx.mcp.transform`, TUI slots (`home.footer.status`, `session.panel`, …).
  The old `server() { return { "tool.execute.before": … } }` shape is V1-only.
- Compaction is lossy and cannot help when a request is mostly fixed instructions
  and tool schemas; warming sends real provider requests. Both are reasons the
  ledger must measure schemas and real tokens, not just conversation bytes.
- Claude Code: hooks incl. `SessionEnd`; auto memory is machine-local Markdown with
  a 25KB/200-line index, no anchors/freshness/ledger — NC is additive.
- OKF v0.3 (`https://github.com/open-knowledge-format/open-knowledge-format`,
  SPEC.md, published 2026-11-18): Markdown + YAML frontmatter bundle; `type` is the
  only required key; `sources` / `generated` / `verified` / `status` /
  `superseded_by` / `stale_after`; extension keys `x-<producer>-<name>`. Trust tiers
  are unverified / machine-confirmed / human-reviewed, and only **current**
  verification (`verified.at >= generated.at`) counts. `[[wikilinks]]` are *not*
  OKF links — emitted concepts use standard Markdown links. Pre-1.0 minors may
  break, so pin `okf_version: "0.3"`. See ADR 0020.

## User's later instructions

- "go head" — implementation authorised; P0 and P1 1.1–1.3 are now done (not
  committed). Do not stop at docs again.
- User suggested Obsidian + **OKF** as a quiet way to offload project knowledge for
  a developer working across many repos. Verified as real: OKF v0.3, published
  2026-11-18. Accepted as ADR 0020 and wired into P2/P3 of the plan. Division of
  labour: **OKF = contract, Obsidian = viewer, NC = freshness + ledger engine**;
  no knowledge service, no account, no sync.
- User's additional advice: tell them when it is ready to install locally and use
  on this or another session to go faster **without losing confidence**. Treat that
  as opt-in advice, not a request to bypass the trust gate. The explicit dogfood
  gate in the implementation plan is the bar; it is not met yet at P1.

## Honest install status (2026-11-24)

`nexus install` exists but today only wires Claude Code / Claude Desktop MCP
(`crates/nexus-cli/src/install.rs`). **`nexus install opencode` does not exist yet**
(plan row 3.3). The P0 OpenCode plugin has no-op hooks, so it can be loaded but
gives no boost. Do not tell the user it is usable until row 3.3 and the dogfood
gate are actually done.

### Addendum (later on 2026-11-24)

That status is now historical. Rows 3.1–3.5 are implemented: the OpenCode plug
has real hooks, the daemon renders and measures the exact injection, the ledger
separates harness numbers from estimates, both installers are opt-in, and OKF
bundle-root routing is available. The six-item gate passes in local synthetic
tests, but `opencode` is not on this machine's `PATH`, so the last step is still
a real OpenCode dogfood session — not another implementation task.
