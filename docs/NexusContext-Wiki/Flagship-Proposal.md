# Flagship Proposal: One Daemon, Two Memories, One Ledger

Status: **Accepted — development started 2026-11-24**
Owner: devNalyx
Related: [[Home]] · [[Architecture]] · [[MCP-Surface-Evaluation]] ·
[[Product-Thesis-Validation]] · [[Known-Limitations]] · [[ADRs/README]]
Implementation tracker: [[Flagship-Implementation-Plan]]

## 1. Why this, why now

Graphify has won the "codebase knowledge graph for agents" mindshare race: hosted MCP,
a browser-visible wiki, cloud Memory, an OpenCode integration, enormous GitHub/PyPI
traction, and published token-ratio marketing. Chasing that product shape would be a
losing move for NC: it is smaller, local-only, and its graph is deliberately
name-based with documented accuracy ceilings ([[Known-Limitations]], ADR 0004).

NC already owns something Graphify explicitly does not have: **honest measurement**.
- `[[Product-Thesis-Validation]]` compares a realistic Read/Grep/Glob/Bash baseline
  against NC on the same six tasks, scores correctness independently, and publishes
  the losses (narrow grep-shaped tasks, some latency regressions, a real
  `detect_dead_code` false positive).
- NC's graph has no account, no cloud, no corpus upload, works on uncommitted local
  code, and is cold-startable with one binary.
- `get_session_usage` already meters NC's own response bytes; tool presets already
  cut advertised schema (`minimal` 5 / `standard` 10 / `full` 12, ADR 0005).

Graphify's own comparison page admits nobody has run both on the same tasks. Its
headline 136.8x/83.6x numbers are against a naive "send the whole corpus" estimate,
and its docs say so. Its more honest ERPNext result (~1M LOC, 70.8% → 82.0% key-fact
coverage with one tool at ~140K tokens/query) still does not score correctness
against a good local agent.

So the wedge is not "a better graph". It is:

> **The context economy control plane for local coding agents: measure what the
> agent actually spends, prove which memories pay for themselves, and cap what
> gets injected.**

The flagship product is an **auto second-brain plug** that combines three kinds of
memory behind one install and one ledger. Working name: **NexusContext Brain**.

## 2. Product thesis

### 2.1 Three memories, one daemon

| Memory | Source of truth | Owned by | Example | Staleness signal |
|---|---|---|---|---|
| **Structural** | NC's SQLite graph + FTS | `nexus-index` today | `Config::is_path_allowed` is called from 8 sites | file hash / watcher → `index_freshness` |
| **Procedural** | Learned `SKILL.md` files, reps/atrophy | Myelin, absorbed into NC | "To add an MCP tool here, update 4 docs + a test" | last-used atrophy (~30d) |
| **Declarative** | Code-anchored facts in SQLite + Markdown export | New NC layer | "We chose tool presets to cap schema tax (ADR 0005)" | anchored file changed → `stale` |

These are deliberately different. They need different retrieval, different UI labels,
and different decay semantics. A skill going stale means *"we haven't done this
lately"*; a fact going stale means *"the code it describes changed."* Collapsing
them would repeat the mistake of treating every graph edge as equally certain
(ADR 0013 fixed that for call edges).

### 2.2 Why absorb Myelin instead of composing it over MCP

`devNalyx/Myelin` (same owner) already implements the procedural half end to end:
session `SessionEnd` ingestion with redaction, deterministic token-overlap candidate
staging, review queue, promotion into real `SKILL.md`, confirmation/correction
feedback that mutates the same file, usage counting and 30-day atrophy. Its README
says explicitly that it is *not* a codebase indexer.

Composing two MCP servers would leave two daemons, two configs, two installers and
two tool catalogs — exactly the schema tax NC is trying to eliminate. Absorbing it
into the NC workspace gives one process, one control socket, one budget, one preset
system and one review queue, with no license ambiguity once the merged workspace is
explicitly MIT (NC already is; Myelin has no license field and must not be reused
until it is set).

Decision: **merge Myelin's four crates into NC's workspace** (ADR 0018). Keep the
`myelin-*` crate names and module boundaries initially so the port is mechanical and
its ~80 tests stay meaningful; rename to `nexus-memory-*` only if a later cleanup
pays for itself. No transcript becomes NC structural memory; Myelin's transcript
handling stays procedural-only, behind the review queue.

### 2.3 What NC must become

1. **Context governor** — OpenCode V2 hooks (`prompt`, `context`,
   `execute.before`/`after`, `compaction`) plus keyed instruction entries, with a
   hard per-turn injection budget (default ~800 tokens for declarative memory,
   separately configurable for skills). It prunes *both* tool catalogs per turn;
   it does not force the agent to load 12+11 tool schemas every session.
2. **Honest ledger** — real provider token/cost numbers from the harness where
   available, NC's measured response bytes as a labelled fallback. This replaces
   "we saved 30% of bytes" with "this task cost X tokens and Y dollars with NC vs
   without, at equal correctness".
3. **Self-expiring second brain** — facts anchored to graph nodes/files; when the
   watcher sees a change, anchored facts flip to `stale` and leave recall by
   default. No background LLM runs in the daemon: distillation is deterministic,
   judgment stays with the agent/human (same rule as Myelin).
4. **Reproducible proof** — `nexus bench` runs paired baseline/NC tasks, checks
   correctness with a rubric, and publishes a report in the same honest style as
   [[Product-Thesis-Validation]].

### 2.4 Non-goals for v1

- No team cloud, account, or hosted graph. Sharing is a git-tracked folder of
  portable Markdown/JSON, per repo.
- No raw conversation transcript in NC's fact store (transcripts stay in Myelin's
  procedural pipeline, redacted and reviewed).
- No preference/AGENTS.md management: harnesses already own that.
- No out-Graphify-ing Graphify: no hosted wiki, no PR-review bot, no formal
  verification, no cloud memory API.
- No "NC is smarter" claim. NC remains memory + accounting; the agent reasons.

## 3. Locked decisions

| # | Decision | Choice |
|---|---|---|
| 1 | Memory scope in v1 | Curated facts only: decisions, incidents, lessons, dead ends, owners. No raw chat, no code structure, no preferences. |
| 2 | Sharing at launch | Per-repo, local-first SQLite; portable Markdown/JSON export for git. Multi-repo/team sync deferred. |
| 3 | Canonical store | SQLite (reuse atomic writes, anchors, freshness queries); portable export/import is an **OKF v0.3 bundle** (ADR 0020), not a private schema. |
| 4 | Trust mode | Myelin's model: deterministic staging → agent decides → human/CLI verifies → living artifact with feedback. Redaction before storage. |
| 5 | Injection | Auto-inject only tiny *verified* facts tied to current files/diff, hard per-recall budget (~800 tokens); agent can pull more deliberately via `recall`. |
| 6 | Staleness | File-level for facts (anchored file changed → stale, excluded by default); last-used atrophy for skills. Symbol-level only after import-aware edges land. |
| 7 | Ledger identity | Harness session ID + real token/cost via plugin control API; NC response bytes/4 remains the labelled fallback. `schema_tax` reports both the ceiling and the *filtered* preset tax. |
| 8 | Tool placement | `recall` joins `standard`; ingestion/curation tools live in `full` or plugin-only control-socket operations. Preset drift tests must stay green. |
| 9 | Install | `nexus install opencode` detects or installs `nexusd`, registers MCP + plugin + minimal config in one command. |
| 10 | Harness #2 | Claude Code: `SessionEnd` hook feeds the absorbed Myelin CLI; MCP exposes `recall`. NC is a freshness-aware backend, additive to Claude's own auto-memory. |
| 11 | Success metric | Correctness-gated token/cost savings per task + stale-memory rate. Never raw ratio claims. |
| 12 | Myelin relation | Absorbed into NC (ADR 0018), MIT, no code merge before licence is set. |
| 13 | Exchange format | OKF v0.3 (ADR 0020). One bundle per repo; agent selects it by the bundle-root `description` before reading concepts. Obsidian is the viewer, OKF the contract, NC the freshness/ledger engine. |

## 4. Target architecture

```
                    ┌──────────────────────────────────────────┐
  OpenCode V2  ---> │  nexus plugin (TS, .opencode/plugins/)    │
  Claude Code --->  │  prompt / context / compaction / tool     │
  hooks ----------> │  hooks, keyed instruction entries, TUI    │
                    │  status slot, real token/cost telemetry   │
                    └───────────────┬──────────────────────────┘
                                    │ control socket / MCP
                    ┌───────────────▼──────────────────────────┐
                    │             nexusd (one process)          │
                    │  context governor  ·  ledger ·  presets   │
                    ├────────────┬──────────────┬───────────────┤
                    │ structural │ declarative  │ procedural    │
                    │ graph+FTS  │ facts+review │ Myelin skills │
                    └────────────┴──────────────┴───────────────┘
                                    │ OKF v0.3 export/import
                        repo/.nexus/memory/  (one bundle per repo, git-tracked)
```

Guiding constraint from [[Architecture]]: **the agent is the intelligence, NC is
the memory and the meter.** No LLM inside `nexusd`.

## 5. Phased implementation plan

Each phase has an acceptance gate. Do not start a later phase with an open gate.

### P0 — Credibility gates (first, small, no new product surface)

- [ ] Fix `schema_tax` to report the filtered preset/whitelist tax alongside the
      full-catalog ceiling, with explicit field names and unchanged note semantics.
- [ ] Add a regression test asserting filtered tax ≤ ceiling for all three presets.
- [ ] ADR 0018 (absorb Myelin, MIT) and ADR 0019 (context economy flagship).
- [ ] Scaffold the OpenCode V2 plugin package under `integrations/` with a typed
      control-client stub and smoke test that loads under a temp `.opencode/plugins/`.

**Gate:** `cargo test --workspace`, fmt, clippy `-D warnings` green; schema tax
numbers in docs match output; plugin loads without touching a real agent.

### P1 — Port Myelin into the workspace

- [ ] Add `myelin-core`, `myelin-index`, `myelind`, `myelin-cli` to workspace members
      with `license = "MIT"` inherited.
- [ ] Fold Myelin's MCP tools into NC's preset system (`ALL_TOOL_NAMES` + tests);
      keep default advertised set at 10 + at most 1 procedural tool.
- [ ] Expose the CLI as `nexus memory …` (keep `myelin` shim for one release).
- [ ] One persisted review queue surfaced in the GUI beside the NC fact queue.

**Gate:** all ~80 inherited tests pass under NC's CI matrix; no new default tool
advertised without a preset-drift doc update; stdio MCP integration test still passes.

### P2 — Declarative facts

- [ ] SQLite tables: facts, anchors (file/symbol), reviews, provenance, freshness.
- [ ] `recall` tool in `standard`, budgeted, freshness in-band; curation tools in `full`.
- [ ] Watcher/anchor invalidation: anchored file change → `stale`, excluded by default.
- [ ] OKF v0.3 bundle export/import (ADR 0020): one `type`d concept per fact,
      bundle-root `index.md` with `description` as the many-repo routing key,
      `x-nexus-*` keys for anchors/ledger, standard Markdown links (not wikilinks).
- [ ] OKF consumer gate: auto-inject only current human-reviewed, `stable`,
      non-stale concepts; tolerate unknown keys/types/links without trusting them;
      OKF §11 conformance tests including unparseable `stale_after` never fresh.
- [ ] Regeneration never carries forward a verification older than `generated.at`.

**Gate:** a fact about `tools.rs` flips to stale after touching `tools.rs`, and
recall omits it unless explicitly requested; round-trip export/import preserves
anchors and verification but invalidates a stale human sign-off; a bundle exported
from one repo is readable by an OKF consumer; 0700/0600 + `allowed_roots` unchanged.

### P3 — Flagship plug

- [ ] OpenCode V2 plugin (`@opencode/plugin` / `Plugin.define`, server + TUI
      parts): `prompt`/`context` injection with hard budget,
      `execute.before/after` for tool telemetry, `compaction` hook to re-seed only
      verified facts, keyed instruction entries, TUI status slot.
- [ ] Ledger accepts real tokens/cost keyed by harness session; falls back to
      labelled bytes/4 estimate; `get_session_usage` exposes both.
- [ ] `nexus install opencode` one-command registration of MCP + plugin + config.
- [ ] Claude Code `SessionEnd` hook feeding the absorbed Myelin pipeline; `recall`
      available over MCP. Respect Claude's own auto-memory (additive, no takeover).

**Gate:** end-to-end recorded session shows a bounded injection, a real token number,
a stale fact omitted, and a reviewed skill promoted — with no daemon-side LLM call.

### P4 — GUI, portability, budgeting

- [ ] One review queue for proposed facts and skill candidates, with diff preview.
- [ ] `query_planner` enforces the injected-context budget, not just response caps.
- [ ] Per-repo `.nexus/memory/` OKF bundle documented and git-friendly; a
      personal Obsidian vault may link many bundles, but no cross-repo sync.

**Gate:** two machines can share a repo's facts via git with no account; a runaway
agent cannot exceed the configured injection cap.

### P5 — Proof and publication

- [ ] Extend `[[Product-Thesis-Validation]]` into `nexus bench`: paired tasks,
      correctness rubric, real provider tokens where available, stale-rate report.
- [ ] Publish the Graphify head-to-head **only when both products have been run on
      the same tasks**; until then, publish NC's own honest baseline comparison.

**Gate:** benchmark is reproducible from the repo; writes up losses as well as wins.

## 6. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Two memory systems confuse users | One install, one review queue, distinct labels ("skill: unused lately" vs "fact: code changed") |
| Injection becomes the token burn it fights | Tiny default budget, verified-only, freshness filter, explicit opt-out; ledger shows its actual cost |
| Myelin port rots during merge | Port crates unchanged first; rely on its 80 tests; no licence ambiguity (set MIT before copying) |
| Facts go stale faster than they're curated | File-level invalidation, export as source of truth, report stale rate in benchmark |
| Connector/preset drift breaks the flagship promise | Keep and extend the existing doc-drift tests; schema tax reports both numbers |
| Claiming parity with Graphify without evidence | Correctness-gated benchmark; no ratio marketing; cite the honest limits in docs |
| Harness hooks change | Adapter boundary is the plugin package, not NC core; core speaks MCP/control socket only |

## 7. What this is not

This is not a rewrite of NC's indexer, not a cloud product, not an attempt to replace
AGENTS.md or Claude's auto-memory, and not a claim that NC's graph is more accurate
than Graphify's. It is a small, measurable, local control plane that makes existing
memory assets — code graph, learned skills, curated facts — affordable and honest
inside the agent harnesses people actually use.
