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
- [x] Do **not** edit `docs/Phase-History.md` (frozen historical log): its old
      tool counts and embeddings references are intentionally historical. See README note.

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
| ✅ 3.6 | Human-gated skill signpost in the same injection block | promotion alone never injects; only `skill-approve --actor human:<name>` makes an active, non-atrophied skill eligible. At most five signposts, separately labelled, trimmed before verified facts under the same hard cap, and recorded as exact bytes plus `skill_ids` in the ledger |

**Gate:** one recorded session shows bounded injection + real token number + stale
fact omitted + reviewed skill promoted and human-approved for signposting,
with zero daemon-side LLM calls.

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
   the hard ~800-token cap and contains only verified, non-stale, current-repo
   facts. Any procedural signpost is separately labelled and human-approved.
3. The ledger reports a real harness token/cost number when available and labels
   the bytes/4 fallback explicitly — never silently mixes the two.
4. No auto-promotion into memory without agent judgment + human/CLI review; a
   promoted skill still needs human sign-off before it can enter context;
   redaction happens before storage; no raw transcripts or preferences enter NC
   memory.
5. A recorded session shows a stale fact omitted and a machine-confirmed concept not
   auto-injected (ADR 0020), with zero daemon-side LLM calls.
6. Still local-first: no account, no cloud, no cross-repo sync.

Gate evidence (2026-11-25, installed v0.1.22 binaries, isolated disposable repos):

- (1) `nexus install opencode` install → uninstall round trip is covered by unit
  tests that assert byte-exact config restoration and manifest-only file deletion.
- (2) Control-socket test: `ledger.inject` rendered 244 bytes / 61 tokens against an
  800-token cap, bytes equal to the returned text, with a fresh fact admitted and a
  stale anchored fact excluded from the text.
- (2b) Skill signposts come from a separate renderer section. Store and renderer
  tests prove that promotion alone, `agent:` approval, archive, and atrophy all
  keep a skill out of context; only a `human:` sign-off passes. Signposts are
  trimmed before verified facts when the shared budget is tight, and the ledger
  records their exact bytes and ids separately from fact ids.
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

## Current status (2026-11-25)

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
  injection, `myelind` Windows gate, macOS runner fix) → v0.1.23 (second Windows
  gate, in `myelin-cli`'s Unix-only status client; v0.1.22's release build was
  rejected before publish) → v0.1.24 (human-gated skill signposts: promotion is
  not consent; only `human:<name>` sign-off makes active non-atrophied skills
  injectable, separately labelled and budget-trimmed before facts, exact bytes
  + `skill_ids` ledgered).
- v0.1.23 release run `45549280753` published all 9 assets green.
- v0.1.24: PR #98 merged as `b650f7d` (feature) and PR #99 merged as `39130ed`
  (version bump). Release run `45558470748` completed success with 9 assets.
  Installed locally via `cargo install --path crates/nexus-cli --locked` and
  `--path crates/nexusd --locked`; global OpenCode plugin files refreshed.
  Verification: workspace fmt/clippy/test green, plugin smoke green, isolated
  signpost dogfood PASS, installed-binary dogfood PASS. No leftover `/tmp/opencode/nc-*`
  test processes found.
- v0.1.25: PR #104 merged as `b67631c` (registered external sources, issue #103)
  and PR #105 merged as `cff061c` (version bump). Release run `45896827398`
  published 9 assets. `nexus source add secondbrain /home/iceman/Documents/secondBrain
  --kind obsidian-vault` canonicalized the vault, wrote `[[sources]]`, indexed
  39 Markdown files (182 nodes / 206 edges), and persisted `source_id`/
  `source_kind`/`source_resource` in `projects.json`; the MCP catalog now
  advertises `list_sources` alongside the existing `vault_context`. Local
  install caveat: this machine's `vault_context` feature lives in the
  uncommitted `agent-vault-link-graph` worktree, so the installed 0.1.25
  binaries were built from a temporary tree combining released `main` with
  that worktree's Rust changes (all workspace tests green); the published
  v0.1.25 artifacts contain the released `main` code only. Verification:
  `tools/list` returned 14 configured tools, `list_sources` and
  `vault_context` (tags/links modes) returned live vault data.
- v0.1.26: PR #108 merged as `2ccf6dd` (typed RPC + TUI panel, issue #107),
  PR #109 merged as `a2d53f1` (experimental Desktop panel, global
  `--global` install, GUI deprecation notices), and PR #110 merged as
  `972fda3` (version bump). Release run `45960578849` completed success
  with 9 published assets. Local install: `nexus`/`nexusd` 0.1.26 were
  copied into `~/.cargo/bin` from a temporary tree combining released
  `origin/main` with the uncommitted `agent-vault-link-graph` Rust changes,
  so the published artifacts contain released `main` only while this
  machine keeps `vault_context`. The old manual global plugin was backed up
  under `/tmp/opencode/plugin-backup-pre-0.1.26` and replaced by
  `nexus install opencode --global --yes`; the installer wrote
  `~/.config/opencode/plugins/nexuscontext/{index.js,control.js,tui.tsx,rpc.js,panel-data.js,gui-experimental.cjs,package.json}`
  and `~/.config/opencode/nexuscontext-install-manifest.json`. Verification:
  all PR CI jobs green; workspace fmt/clippy/test and plugin smoke/typecheck/build
  green; `nexusd serve` (PID 229966) exposed
  `/run/user/1000/nexuscontext/nexuscontext.sock`; control `status.get`
  returned version 0.1.26, `sources.list` returned `secondbrain`, and
  `logs.tail` returned live lines. MCP `list_sources` and CLI `nexus source list`
  both showed the registered vault source; installed `tools/list` exposed
  15 tools including `list_sources` and `vault_context`. OpenCode sessions
  still need a restart/reload to load the new global plugin and Desktop
  bundle; the daemon is already running.
