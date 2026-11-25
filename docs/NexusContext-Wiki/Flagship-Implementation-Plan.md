# Flagship Implementation Plan (tracker)

Status: **P0–P3 implemented; local synthetic dogfood gate passes; real OpenCode session still needed — 2026-11-24**
Decision source: [[Flagship-Proposal]]
ADRs: [[ADRs/0018-absorb-myelin-as-procedural-memory|0018]],
      [[ADRs/0019-context-economy-is-the-flagship|0019]]
Reboot context: [[Flagship-Reboot-Notes]]

This file is the work tracker. The proposal explains *why*; this file says exactly
*what to do, in what order, and what proves it worked*. Update checkboxes as work
lands. Do not mark a phase done unless its gate passes on a clean tree.

## Preflight (do this in the first implementation session)

- [x] Confirm clean tree: `git status --short` (only the new docs were untracked).
- [x] Confirm baseline green before touching code:
      `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --locked` — all passed before any source change.
- [x] Re-read [[Known-Limitations]] and ADR 0004/0005/0013/0018/0019.
- [x] Re-fetch OpenCode plugin/hook docs. **V2 has changed since the proposal
      was written**: the SDK is `@opencode/plugin` with `Plugin.define`,
      hooks are `ctx.session.hook(...)` / `ctx.tool.hook(...)`, and the plugin
      is a package with server/TUI/GUI/RPC parts. See
      `integrations/opencode-nexuscontext/README.md`.
- [x] Do **not** edit `README.md`'s historical phase log: its old tool counts and
      embeddings references are intentionally frozen (see README note).

## P0 — Credibility gates

| # | Task | Touch points | Done when |
|---|---|---|---|
| 0.1 | Report both schema-tax numbers | `crates/nexusd/src/tools.rs` (`get_session_usage` ~line 608, `schema_tax` ~line 644); `resolved_tool_names`/`enabled_tool_definitions` ~lines 326-364 | `schema_tax` has e.g. `bytes_ceiling`/`estimated_tokens_ceiling` and `bytes_advertised`/`estimated_tokens_advertised` for the resolved preset/whitelist; ceiling note still says what it is |
| 0.2 | Thread resolved config into usage reporting | `crates/nexusd/src/mcp.rs::dispatch` (`tools/call` at line 110) and `tools::call` signature (~line 366); keep `Config::load(...).unwrap_or_default()` behaviour identical to `tools/list` | no second config source, no behaviour change for other tools |
| 0.3 | Add regression test | `tools.rs` test module (~lines 929+) | filtered ≤ ceiling for minimal/standard/full and for an explicit `enabled` list |
| 0.4 | ADRs | `docs/NexusContext-Wiki/ADRs/` | 0018 + 0019 written and indexed (done during preparation) |
| 0.5 | Plugin scaffold | new `integrations/opencode-nexuscontext/` | package loads from a temp `.opencode/plugins/` in a smoke test; no real agent session required |

**P0 result:** 0.1–0.5 done. `schema_tax` now reports `bytes_ceiling` /
`estimated_tokens_ceiling` and `bytes_advertised` / `estimated_tokens_advertised`;
config is threaded from `mcp.rs::dispatch`; a regression test covers all presets
plus an explicit `enabled` list; MCP-Tools.md and the landing page were updated;
the V2 plugin package loads from a temp `.opencode/plugins/` in an offline smoke
test (`npm test`).

**Gate:** ✅ fmt + clippy + tests green (nexusd 50/50); doc-drift test at
`tools.rs:1250` still passes; no new tool names added yet.

## P1 — Port Myelin

Inventory verified from `https://github.com/devNalyx/Myelin` clone:

- Workspace crates: `myelin-core` (config/error/paths/lib),
  `myelin-index` (graph/redact/skillfile/staging/store/transcript),
  `myelind` (control/mcp/tools/main + `tests/mcp_protocol.rs`),
  `myelin-cli` (install/main, 13 subcommands).
- 80 `#[test]` attributes in-tree; README claims 40 tests including a real stdio
  MCP integration test. Treat the code count as authoritative until run.
- 11 MCP tools: `record_observation`, `list_warmup_queue`, `list_skills`,
  `promote_skill`, `record_skill_feedback`, `mark_skill_used`,
  `list_pending_review`, `dismiss_pending_review`, `archive_skill`,
  `restore_skill`, `render_skill_graph`.
- Dependencies overlap NC almost exactly (`rusqlite 0.32` — NC adds `hooks`;
  `serde`, `serde_json`, `toml 0.8`, `clap 4`, `directories 5`, `regex 1`,
  `thiserror 2`). No GUI crate, no C grammar linking.
- **Hard prerequisite:** no `license` field anywhere in Myelin. Set MIT in the
  merged workspace *before* copying any file. NC's root `Cargo.toml:14` already
  has `license = "MIT"` and `LICENSE` exists.

| # | Task | Done when |
|---|---|---|
| ✅ 1.1 | Add the four crates to NC's workspace members + workspace dependency aliases | `cargo metadata` resolves; inherited MIT via `license.workspace = true` on all four crates |
| ✅ 1.2 | Port files unchanged first (no rename) | 80 inherited tests compile and pass; `myelind` gets a `lib.rs` so NC can call its tools while the binary stays a shim |
| ✅ 1.3 | Fold 11 tools into NC presets/tests | one catalog of 23; default `standard` = 10 NC + `list_skills`; other 10 procedural tools in `full`; all five count-bearing docs updated and doc-drift test green |
| ✅ 1.4 | `nexus memory …` CLI front end (keep `myelin` shim one release) | all 13 subcommands reachable (`add`, `list`, `show`, `approve`, `reject`, `recall`, `stale`, `audit`, `export`, `import`, `skills`, `skill-queue`, `skill-promote`); `Install` no longer overwrites Claude settings without `--yes`; OpenCode install/uninstall has a manifest and round-trip tests |
| ✅ 1.5 | One review queue in GUI | proposed skills and proposed facts visible in one UI with distinct labels; `review.list` is read-only, and the GUI never approves/promotes on the user's behalf |

**Gate:** ✅ on Linux (303 tests pass, 1 ignored, fmt + clippy -D warnings clean,
plugin `npm test` green); macOS/Windows CI matrix still to be exercised by CI;
stdio MCP integration test passes; no new default tool without doc update.

## P2 — Declarative facts

| # | Task | Done when |
|---|---|---|
| ✅ 2.1 | Schema: facts, anchors, reviews, provenance, freshness | SQLite canonical; 0700/0600 and `allowed_roots` unchanged |
| ✅ 2.2 | `recall` in `standard`; curation in `full`/plugin-only | preset-drift tests updated; hard ~800-token default cap |
| ✅ 2.3 | Watcher invalidation | touching an anchored file flips fact to `stale`; recall excludes stale by default; `.nexus/` writes treated as noise so export can't cause a reindex loop |
| ✅ 2.4 | OKF v0.3 bundle export/import (ADR 0020) | one bundle per repo at `.nexus/memory/`; bundle-root `index.md` declares `okf_version: "0.3"` + routing `description`; one `type`d concept per fact; `x-nexus-*` anchors; standard Markdown links |
| ✅ 2.5 | YAML 1.2 core-schema reader/writer + OKF §11 conformance tests | absent/empty/single-item families handled; unparseable `stale_after` never fresh; unknown keys/types/broken links tolerated but not trusted |
| ✅ 2.6 | Consumer admission gate | auto-inject requires current `human:` verification (`verified.at >= generated.at`), `status: stable`, non-stale anchor; regeneration drops stale verification stamps |

**Gate:** ✅ freshness behaviour demonstrated on a throwaway repo (approve →
inject → mark anchor stale → recall excludes with `"stale": 1`); exported bundle
round-tripped through the OKF v0.3 reader; no silent trust of stale or
machine-confirmed facts; no LLM in `nexusd`.

## P3 — Flagship plug

| # | Task | Done when |
|---|---|---|
| ✅ 3.1 | OpenCode V2 hooks: `prompt`, `context`, `execute.before/after`, `compaction`; keyed instruction entries; TUI status slot | daemon renders the exact bounded text; the adapter re-puts one keyed entry, so compaction cannot accumulate NC context. TUI slot follows the documented `home.footer.status` pattern but has not been exercised in a real OpenCode TUI on this machine |
| ✅ 3.2 | Ledger with real harness tokens/cost keyed by session; labelled bytes/4 fallback | daemon records exact injected bytes; `session.usage.recorded` feeds provider tokens/cost; `get_session_usage` and `ledger.get` keep harness and estimate sources separate and never sum them |
| ✅ 3.3 | `nexus install opencode` | one command registers MCP + plugin + minimal config; `--uninstall --yes` restores the previous `opencode.json` entry and deletes only files from the install manifest |
| ✅ 3.4 | Claude Code `SessionEnd` hook → absorbed Myelin pipeline; `recall` via MCP | `nexus install claude --yes` merges a project-local `SessionEnd` command hook; signposted lines are staged into Myelin's pending-review queue (never `record_observation`), so no excerpt auto-promotes. No CLAUDE.md/AGENTS.md takeover |
| ✅ 3.5 | Multi-repo routing via OKF bundle-root `description` (ADR 0020) | `nexus memory bundles` reads only `index.md` metadata for many repos and marks the active one; injection stays scoped to the active repo and the same hard cap. No cross-repo sync or account |

**Gate:** one recorded session shows bounded injection + real token number + stale
fact omitted + reviewed skill promoted, with zero daemon-side LLM calls.

## P4 — GUI, portability, budgeting

- One review queue with diff preview for both facts and skill candidates.
- `query_planner` enforces injected-context budget, not just response caps.
- `.nexus/memory/` documented as the git-shareable unit.

**Gate:** two machines share facts through git with no account; injection cap holds
under a deliberate runaway-agent test.

## P5 — Proof

- Extend [[Product-Thesis-Validation]] into `nexus bench`: paired tasks, correctness
  rubric, provider tokens where available, stale-rate report.
- Publish a Graphify head-to-head only after actually running both on the same
  tasks; until then, keep publishing NC's honest baseline comparison.

**Gate:** reproducible from repo; losses documented alongside wins.

## Dogfood gate — before we install this on ourselves

Local install is deliberately gated later than "it compiles". We only put this in
our own harness when all of the following hold, so speed never buys us confidence:

1. `nexus install opencode --yes` registers MCP + plugin + minimal config in one
   command, and `nexus install opencode --uninstall --yes` restores the previous
   harness with no data loss (deleting the plug directory by hand cannot undo a
   config merge; use the uninstall command).
2. The plugin's hook handlers are real, not the P0 no-ops. Injection is bounded by
   the hard ~800-token cap and contains only verified, non-stale, current-repo facts.
3. The ledger reports a real harness token/cost number when available and labels
   the bytes/4 fallback explicitly — never silently mixes the two.
4. No auto-promotion into memory without agent judgment + human/CLI review; redaction
   happens before storage; no raw transcripts or preferences enter NC memory.
5. A recorded session shows a stale fact omitted and a machine-confirmed concept not
   auto-injected (ADR 0020), with zero daemon-side LLM calls.
6. Still local-first: no account, no cloud, no cross-repo sync.

Gate evidence (2026-11-25, installed v0.1.22 binaries, isolated disposable repos):

- (1) `nexus install opencode` install → uninstall round trip is covered by unit
  tests that assert byte-exact config restoration and manifest-only file deletion.
- (2) Control-socket test: `ledger.inject` rendered 244 bytes / 61 tokens against an
  800-token cap, bytes equal to the returned text, with a fresh fact admitted and a
  stale anchored fact excluded from the text.
- (3) Ledger keeps `harness` (provider tokens/cost from `session.usage.recorded`)
  separate from labelled bytes/4 `estimate`; they are never summed.
- (4) A Claude `SessionEnd` signpost staged one redacted excerpt into
  `pending_reviews`; `skill_candidates_promotable` stayed empty and no excerpt
  auto-promoted. The 19-character `sk-` key that dogfooding found in that excerpt
  is now redacted at the store boundary (known API-key prefixes, including cases
  below the entropy fallback's 20-character floor).
- (5) Admission tests confirm machine-confirmed/unverified facts are never
  auto-injected and stale anchors are excluded; `nexusd` contains no LLM call.
- (6) All writes are project-local; no account, cloud, or cross-repo sync exists.

**Real harness (2026-11-25):** OpenCode v2.4.1 is bundled with the local Desktop
app at `/opt/OpenCode/resources/opencode-cli` (not on `PATH`). A standalone,
auto-approved session in a disposable repo with the installed plug and live
daemon, prompted to use only injected memory, answered "Alice owns the ledger in
this repo". The stored instruction entry contained that fresh fact and omitted
the anchored stale fact; the daemon recorded exactly one `estimate` injection of
61 tokens. This exercise caught a real bug: the first attempt ledgered the
injection from the `context` hook, but that hook runs after OpenCode assembles
the system instruction snapshot, so the agent said it had no memory despite a
stored entry. The awaited `prompt` hook now performs the injection, with the
smoke test locking in that ordering. Two release-blocking issues found in the
same pass — an unguarded `std::os::unix` import in absorbed `myelind` (Windows
builds) and GitHub's retirement of the `macos-14` label — are fixed in v0.1.22.

## Current status (2026-11-24)

- Docs prepared: proposal, plan, ADRs 0018/0019/0020, reboot notes.
- P0 complete: honest dual schema tax + V2 plugin scaffold.
- P1 1.1–1.5 complete: four Myelin crates absorbed under MIT, 11 tools folded into
  NC's 29-tool catalog; `standard` stays at 12 with only `recall` from curated
  memory. GUI review queue is read-only and labels fact staleness separately from
  skill atrophy.
- P2 2.1–2.6 implemented and verified: SQLite facts + watcher invalidation +
  budgeted recall + OKF v0.3 + admission gate. Smoke-tested end to end on a
  throwaway repo.
- P3 3.1–3.5 implemented: real OpenCode V2 handlers, exact-byte injection renderer,
  separated ledger, installers for both harnesses, and OKF bundle-root routing.
  Full Linux verification green: 303 tests passed, 1 ignored, fmt + clippy -D
  warnings clean, plugin `npm test` green.
- Released v0.1.20 (flagship) → v0.1.21 (redaction fix) → v0.1.22 (real-harness
  injection, Windows build, and macOS runner fixes). Installed locally from the
  v0.1.22 source with `cargo install --path ... --locked` and dogfooded as above.
  Release/publish status is tracked in GitHub Actions for tag `v0.1.22`.
