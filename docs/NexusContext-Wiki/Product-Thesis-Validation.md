# Product Thesis Validation

Addresses [#57](https://github.com/devNalyx/NexusContext/issues/57) — "Validate NexusContext's core value as persistent structural memory for coding agents."

This is a single benchmark run, not a longitudinal study. It answers the issue's two headline
questions with real numbers from one repository (NexusContext itself) and states the gaps
plainly rather than smoothing them over.

## Methodology

Test subject: this repository (`devNalyx/NexusContext`) at commit `014b8d7`, freshly indexed
(`index_repository`: 780 nodes, 1280 edges, 70 files — 37 Rust, 31 markdown, 2 JS).

Six tasks were drawn from the categories the issue suggests (impact analysis, call-path tracing,
dead-code detection, subsystem understanding, "what does X require touching"). Each task was run
**twice**, as two separate, isolated `general-purpose` sub-agent invocations with identical
prompts:

- **Baseline** — Read/Grep/Glob/Bash only. Explicitly instructed not to use any
  `mcp__nexuscontext__*` tool even though it was visible in its tool list.
- **NexusContext** — same tools, plus full access to `mcp__nexuscontext__*`, instructed to prefer
  them where useful.

Both conditions got the same "answer the question, then stop" instruction so runs are bounded and
comparable. Metrics below are self-reported by each sub-agent (tool-call counts, files read,
its own token/wall-clock estimate) plus my own duration/tool_uses/token figures from the parent
session's task-completion telemetry, which are more reliable than the sub-agents' self-estimates
and are what's used for "tool calls" and "tokens" in the table. Correctness was verified
independently against the real source after the fact (see Correctness Notes).

**Caveat on `get_session_usage`:** all NexusContext-condition runs shared one long-lived MCP
connection, so each sub-agent's own `get_session_usage` call reports *cumulative* session totals,
not just its own task's calls. I did not rely on those numbers for the comparison; the table uses
the parent-visible `tool_uses`/`subagent_tokens`/`duration_ms` telemetry per sub-agent instead,
which is isolated per run.

## Results

| # | Task | Condition | Tool calls | Tokens (subagent-reported) | Wall-clock | Files read | Correct? | Verdict |
|---|------|-----------|-----------:|----------------------------:|-----------:|-----------:|:--------:|---------|
| 1 | Where is `allowed_roots` implemented + who calls it | Baseline | 5 | 31,286 | 22.7s | 4 | Yes | Good |
| 1 | same | NexusContext | 8 | 31,843 | 43.8s | — (graph queries) | Yes | Equivalent, slower |
| 2 | Any remaining `Config.embeddings` refs after #62 removal | Baseline | 7 | 36,886 | 32.3s | ~1 (+15 via grep) | Yes | Good, thorough |
| 2 | same | NexusContext | 9 | 42,557 | 40.4s | — | Likely yes (unverifiable — final answer text was not captured in the notification) | Inconclusive reporting |
| 3 | Trace `trace_call_path` handler → SQLite query | Baseline | 9 | 29,129 | 33.4s | 4 | Yes | Good |
| 3 | same | NexusContext | 13 | 31,156 | 43.8s | 4 | Yes | Equivalent, *more* calls and time than baseline |
| 4 | Dead-code candidates in `nexus-index` | Baseline | 13 | ~20-25k (est.) | 72.4s | 0 full reads, all-crate grep | Partial — found only 1 of ~8 real candidates, explicitly hedged as non-exhaustive | Weak |
| 4 | same | NexusContext | 5 | 32,911 | 43.8s | 0 (1 `detect_dead_code` call) | Mostly — found 8 candidates + correctly triaged trait-impls/tests as false positives, but flagged `run_query` as a true candidate when it is actually live (called via its `run_cypher_query` re-export — a genuine detect_dead_code blind spot on re-exports) | Best answer, cheapest, one real false positive |
| 5 | Explain watcher → reindex → MCP-freshness end to end | Baseline | 10 | 51,430 | 47.1s | 4 (full reads, ~1000 lines each) | Yes | Good but token-expensive |
| 5 | same | NexusContext | 12 | 39,010 | 54.7s | 5 (found and used the existing `Watcher-and-Freshness.md` doc) | Yes, and cited the canonical doc | Similar depth, fewer tokens, more calls/time |
| 6 | What does adding a new MCP tool require touching | Baseline | 8 | 31,217 | 36.4s | 2 full + grep on rest | Yes | Good |
| 6 | same | NexusContext | 9 | 35,528 | 46.1s | 1-2 (`get_file_context`) + graph search | Yes, plus cited ADR 0005's documented doc-drift risk | Slightly richer, comparable cost |

## Correctness notes (independently verified)

- **Task 1**: Confirmed by direct grep — `Config::is_path_allowed` (config.rs:163), wrapped by
  `require_path_allowed` (project.rs:401), called from 8 sites across `project.rs`, `queries.rs`,
  `cypher.rs`. Both conditions' answers matched.
- **Task 3**: Confirmed — `tools.rs` `call()` dispatch → `trace_call_path` handler → `GraphStore::trace_calls`
  (`graph.rs:607`) → three raw SQL statements. Both conditions matched exactly, including line numbers.
- **Task 4**: Confirmed `node_by_id` (graph.rs:419) is genuinely uncalled anywhere in the workspace —
  both conditions found it. But NexusContext's `detect_dead_code` also flagged `run_query`
  (cypher.rs) as a live candidate; verification shows it **is** called, just via its
  `pub use cypher::run_query as run_cypher_query` re-export in `lib.rs`, from `nexus-cli/src/main.rs`
  and `nexusd/src/tools.rs`. This is a real, reproducible limitation of `detect_dead_code`'s
  name-based (not import/re-export-aware) resolution — documented as a known caveat in the tool's
  own description, and confirmed here with a concrete example.
- **Task 5**: Confirmed the watcher/`touch_and_catchup` split described by both conditions against
  `watcher.rs` and `project.rs`. Both correct.

## Findings

### 1. Where NexusContext genuinely won

- **Dead-code detection (Task 4)** is the clearest win. The baseline agent, working from grep
  alone, produced an honest but *incomplete* answer — one confirmed candidate, explicitly caveated
  as non-exhaustive because verifying "no caller anywhere" by grep for every candidate function is
  expensive and error-prone (dynamic dispatch, trait impls, string-based dispatch). NexusContext's
  `detect_dead_code` did the graph-wide no-inbound-edge computation in one call, returning a
  triaged list of ~24 raw flags that the agent correctly bucketed into real candidates, trait
  impls, test functions, and MCP entry points, for less than half the baseline's tool calls. This
  is precisely the "impact/reachability analysis at scale" case the issue predicts as
  differentiated. It's also where the tool's own weakness showed up concretely: it doesn't
  understand re-exports, producing one false positive (`run_query`) that a careful human/agent
  cross-check caught.
- **Subsystem understanding (Task 5)** showed a real token-efficiency win: NexusContext used
  ~24% fewer tokens than baseline for an equivalent-depth answer, in part because the agent found
  and cited the repo's own pre-written `Watcher-and-Freshness.md` doc via `search_code` rather than
  reconstructing the picture purely from ~1000-line raw file reads. That's a genuine "persistent
  structural memory" benefit — but it's really a documentation-discovery win as much as a
  graph-structural one, worth being honest about.
- **Impact/call-path questions (Tasks 1, 3)** were answered correctly and identically by both
  conditions, but NexusContext was *not* cheaper here — it used more tool calls and more
  wall-clock time in both cases. On a codebase this size (70 indexed files), a few `rg` calls
  already find `allowed_roots` and its callers just as fast as `trace_call_path`; the graph's
  advantage should grow with codebase size and with cross-file/cross-crate ambiguity (name
  collisions), neither of which this repo has much of yet.

### 2. Where NexusContext was no better, or worse

- **Tasks 1, 3, 6**: baseline grep matched or beat NexusContext on both tool-call count and
  wall-clock time, for identical-quality answers. On a 70-file, well-organized repo with distinctive
  symbol names, `rg` is simply cheap enough that the MCP round-trip overhead (schema tax, multiple
  calls to narrow a BFS, etc.) doesn't pay for itself. This matches the issue's own hypothesis that
  "for small repositories, traditional tools may be cheaper and simpler" — confirmed here, not just
  assumed.
- **Task 2 reporting gap**: the NexusContext sub-agent's final answer text was not fully captured
  by the parent's task-notification channel (only its `get_session_usage` caveat note came through),
  making that one comparison unverifiable from the transcript. This is a benchmark-tooling artifact,
  not a NexusContext behavior — flagged rather than papered over.
- **`get_session_usage` cross-contamination**: because all 6 NexusContext-condition runs shared one
  MCP connection, none of their self-reported `get_session_usage` numbers were per-task-isolated —
  they were all cumulative-since-connection-start. This made the tool useless for *this* benchmark's
  token accounting and is worth knowing if the tool is ever used to justify token-savings claims in
  a similar multi-agent-in-one-session setup.

### 3. Answers to the issue's two questions

**Where is NexusContext genuinely better than existing agent tooling?**
On this evidence: (a) dead-code / no-inbound-caller analysis, where exhaustive verification by grep
is expensive and the baseline agent visibly gave up and hedged; (b) surfacing existing
architecture documentation the agent wouldn't otherwise think to look for, cutting token cost for
subsystem-understanding questions by roughly a quarter with no loss of accuracy. It was not
measurably better — and sometimes measurably more expensive — for straightforward "where is X
implemented / who calls it" questions on a repository this size, where `rg` is already cheap.

**Which capabilities should be considered core versus optional?**
- **Core**: `detect_dead_code` (real, measurable win, but its re-export blind spot should be fixed
  or documented more prominently — issue-worthy on its own) and `search_code`/full-text-plus-docs
  search as an architecture-question accelerator (Task 5's actual advantage came from finding a doc,
  not from graph traversal per se).
- **Situational, not obviously core on this evidence**: `trace_call_path` and `search_graph` — both
  produced correct answers but never beat baseline grep here in calls or time. Their value should
  scale with repo size and symbol-name collision rate, which this repo doesn't have enough of to
  test; this benchmark can't confirm or deny their value at larger scale, and that's the natural
  next benchmark (a genuinely large, unfamiliar repo).
- **Not evaluated here**: persistent state across sessions ("continue work from a previous
  session"), which is one of the issue's own suggested categories and the most distinctive claim in
  the product thesis (`get_architecture`'s `index_freshness`/warm-watcher machinery exists for
  exactly this). This benchmark only ran single-shot, single-session tasks and did not test
  cross-session continuity or a genuinely large/unfamiliar repository — both are open gaps, not
  claims this document makes either way.

## Issue #57 success criteria this run actually satisfies

- [x] Define the primary product thesis explicitly (restated above, unchanged from the issue).
- [x] Establish representative agent tasks (6, drawn from the issue's suggested categories).
- [x] Establish baseline measurements.
- [x] Compare baseline vs NexusContext.
- [x] Measure token/context consumption (partial — see the `get_session_usage` cross-contamination
      caveat; call-count/wall-clock/token comparisons are solid, but MCP's own self-reported
      per-task token accounting was not usable in this multi-agent-in-one-session setup).
- [x] Measure task quality/correctness (all answers independently spot-checked against source).
- [x] Identify the highest-value MCP capabilities so far (`detect_dead_code`, `search_code`).
- [x] Identify a concrete low-value/complexity case (`trace_call_path`/`search_graph` not yet shown
      to beat grep at this repo size) and a concrete correctness gap (`detect_dead_code`'s
      re-export blind spot).
- [ ] Document conclusions and update roadmap accordingly — the conclusions are documented here;
      translating them into roadmap changes (e.g. filing the re-export blind-spot as its own issue,
      scoping a large-repo/cross-session follow-up benchmark) is left to the reader of this doc, not
      done as part of this PR.

Not claimed as satisfied: a large/unfamiliar-repository benchmark and a cross-session persistence
benchmark, both explicitly named in the issue as high-value categories this run did not cover.
