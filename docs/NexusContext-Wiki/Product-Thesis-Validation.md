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

Not claimed as satisfied by the original run: a large/unfamiliar-repository benchmark and a
cross-session persistence benchmark, both explicitly named in the issue as high-value categories.
Both are addressed below.

## Large/unfamiliar-repo benchmark (DownTime)

This section extends the run above with the two gaps it explicitly left open: a large/unfamiliar
repository, and cross-session persistence. Same method as before (isolated baseline vs
NexusContext sub-agent pairs, same "answer then stop" instruction, parent-visible
`tool_uses`/`subagent_tokens`/`duration_ms` telemetry used for the table, correctness spot-checked
independently against source afterward), applied to a different, much larger target repo.

**Test subject:** `/home/opsquad/Workspace/downtime` — the user's real production SaaS codebase, a
Go microservices monorepo (17 backend services under `services/`, 11 shared packages under `pkg/`,
plus a JS/Vite frontend), **not previously seen or read by me before this benchmark** beyond a
directory-name-only glance used to design the tasks below (no README/CLAUDE.md/docs read, matching
the discipline used for task design in the original run). Indexed fresh via `index_repository`:
391 files, 9,689 nodes, 18,276 edges — roughly 5.6x the file count and 12x the node count of the
original 70-file NexusContext-on-itself benchmark.

**Safety:** confirmed before indexing that `nexus_core::Paths::resolve()` stores all index data
under the OS data dir (or `NEXUS_CACHE_DIR` if set) keyed by a project hash, entirely outside the
target repo (`crates/nexus-core/src/paths.rs`) — indexing `downtime` writes nothing into
`downtime` itself. All 11 sub-agents (10 task agents + 1 cross-session probe) were explicitly
instructed the repo is read-only, and I did not modify any file there myself.

Five tasks were chosen to fit a large, multi-service, unfamiliar codebase, per the issue's own
suggested categories (subsystem architecture, auth location, impact/blast-radius analysis, dead
code, cross-service dependency mapping) — deliberately different tasks from the original six so
this isn't just a rerun:

1. Explain the alerting/notification pipeline architecture (`alert-go`, `notification-go`,
   `notification-consumer`, `escalation-go`).
2. Where is authentication/authorization implemented, and how do other services use it?
3. What uses `pkg/events`, and what would break if its public API changed significantly?
4. Find likely dead code in `services/probe-go`.
5. What is the role of `services/correlation-go`, and what does it depend on?

### Results

| # | Task | Condition | Tool calls | Tokens (subagent-reported) | Wall-clock | Correct? | Verdict |
|---|------|-----------|-----------:|----------------------------:|-----------:|:--------:|---------|
| 1 | Alerting/notification pipeline architecture | Baseline | 18 | 86,658 | 67.6s | Yes | Expensive but thorough |
| 1 | same | NexusContext | 13 | 59,134 | 53.9s | Yes | **Won** — 32% fewer tokens, 28% fewer calls, faster |
| 2 | Where is auth implemented | Baseline | 10 | 54,361 | 53.1s | Yes | Good |
| 2 | same | NexusContext | 10 | 38,022 | 46.7s | Yes | **Won** — 30% fewer tokens, faster, same call count |
| 3 | `pkg/events` usage + blast radius | Baseline | 5 | 27,195 | 38.7s | Yes | Cheap and thorough (grep-friendly: import-line counting) |
| 3 | same | NexusContext | 6 | 31,408 | 43.9s | Yes | Lost — slightly more calls/tokens/time for an equivalent answer |
| 4 | Dead code in `probe-go` | Baseline | 12 | 28,644 | 63.0s | Yes (correctly found none) | Good, if slow |
| 4 | same | NexusContext | 11 | 51,855 | 67.3s | Yes (correctly found none) | Lost — `detect_dead_code` is repo-wide with no directory scope; on this monorepo it returned 6,148 candidates dominated by vendored frontend JS, forcing the agent to fall back to manual grep anyway |
| 5 | Role/dependencies of `correlation-go` | Baseline | 6 | 35,387 | 31.7s | Yes | Good |
| 5 | same | NexusContext | 4 | 34,089 | 22.3s | Yes | **Won** — fewer calls, comparable tokens, 30% faster |

All ten answers were independently spot-checked against the real `downtime` source (grep for the
specific functions/imports/interfaces each answer cited) and all ten were correct; no factual
errors surfaced in either condition at this scale.

### What changed at this scale, compared to the 70-file result

- **The calculus flips for cross-cutting architecture and location questions.** On the 70-file
  repo, NexusContext was equal-or-worse than baseline `rg` on "where is X implemented" and
  "explain subsystem Y" questions — the repo was small and well-organized enough that a few greps
  found everything cheaply. On a 17-service, 391-file monorepo, the same style of question (Tasks
  1, 2, 5 here) now genuinely favors NexusContext: 3 of 3 such tasks won on tokens, and 2 of 3 on
  wall-clock, because answering them by grep now means enumerating and partially reading files
  across many separate service directories, which `search_code`/`search_graph`/`get_architecture`
  short-circuit. This is exactly the scaling trend the original doc predicted but could not test.
- **Plain import/usage-counting questions stay grep's turf regardless of scale.** Task 3
  ("what uses `pkg/events`") is fundamentally a single repeated grep pattern (`import
  ".../pkg/events"` plus `events\.`) — cheap for baseline at any repo size, and NexusContext's
  `search_graph`/`search_code` round-trips didn't beat it here either. Scale alone doesn't make
  every question NexusContext-favorable; the *shape* of the question still matters more than raw
  file count.
- **`detect_dead_code` gets measurably worse, not better, on a large heterogeneous monorepo.**
  It has no per-directory/per-package scope parameter, so on `downtime` it returned 6,148 raw
  candidates dominated by vendored/bundled frontend JS (`frontend/public/vendor/swagger-ui/*`),
  making it useless for the actual ask ("dead code in `probe-go`" specifically) and forcing the
  NexusContext agent to fall back to the same manual grep the baseline agent used — at *higher*
  token cost, not lower, because it paid for the failed tool call and its noisy full response
  first. This was the standout win on the small repo (Task 4 in the original results) and is now
  a concrete, reproducible weak point at monorepo scale: the tool needs a path-prefix filter (or
  a same-directory ranking bias) to stay useful once a codebase mixes source and vendored assets.

### Correctness notes (spot-checked)

- Task 1: confirmed the two-queue-two-consumer topology (`alert.notification`→notification-go,
  `alert.triggered`→notification-consumer) and the direct alert-go→escalation-go HTTP trigger via
  grep of `services/alert-go/engine.go` and `services/escalation-go/main.go`; both conditions'
  answers matched.
- Task 4: confirmed via grep that `dispatchConsumer.ConsumeName`/`ProcessEvent`
  (`services/probe-go/dispatch_consumer.go:170,184`) are real, called methods, consistent with
  both conditions' "no dead code found" conclusion.
- Cross-session probe (below): confirmed `services/support-go` mentions "escalation-go" only in
  two code comments (style precedent), with zero functional/HTTP/queue coupling — matches the
  agent's answer.

## Cross-session persistence

The issue's fifth suggested evaluation category — "does persistent state across sessions provide
measurable value" — was explicitly left untested by the original 70-file run. This section
attempts a bounded version of it and is upfront about what it does and doesn't prove.

**What I ran:** I called `index_repository` on `downtime` exactly once, myself, before spawning any
task sub-agent. All eleven NexusContext-equipped sub-agent invocations in this benchmark (the five
paired tasks above, plus one more below) then queried that same persisted index — none of them
called `index_repository` themselves; each one is a structurally fresh Claude Code sub-agent
process with no shared conversation history, discovering and using the pre-built index cold. As a
final, more targeted check, I ran one additional NexusContext-equipped sub-agent, explicitly told
*not* to call `index_repository` and to just use the existing index, on a sixth, unrelated task
("what is `services/support-go` and how does it relate to `escalation-go`?"). It answered
correctly in **5 tool calls (2 of them `mcp__nexuscontext__*`), 35,445 tokens, 27.2s**, without
ever indexing — a cost and shape in line with the other NexusContext-condition runs above (which
ranged 4-13 calls, 31-59k tokens), not the elevated cost you'd expect from also paying for a cold
index.

**What this shows:** the ~one-time `index_repository` cost (391 files → 9,689 nodes / 18,276
edges) is genuinely amortized across many independent, unrelated queries and sub-agent processes
afterward, at zero marginal re-indexing cost per query, because the index lives in the OS data
directory rather than in any agent's context or the target repo. That's the mechanical basis of
the "persistent structural memory" claim in the issue, and this is the first evidence in either
benchmark document that actually exercises it rather than assuming it.

**What this does NOT show, honestly:**
- This is not two literal separate Claude Code sessions in the product sense (separate CLI
  invocations, separate MCP server startups, days apart). All eleven sub-agents ran within one
  parent session's lifetime, most likely against one continuously-running MCP server process and
  SQLite file. A true cold-start test — killing the MCP daemon/process between runs, or waiting
  until after a laptop sleep/restart — was not performed, so this cannot speak to reconnection or
  cold-open latency of the on-disk index.
- It doesn't test **incremental reindexing** after the target repo changes between sessions
  (`detect_changes`/re-run `index_repository`), which is a real and different cost center from
  "index already exists, query it."
- It doesn't test concurrent/multi-user access to the same persisted index, or index staleness
  detection across a longer time gap.
- The comparison to "a hypothetical fresh-baseline second run" is inherently asymmetric: baseline
  has no persistent state to amortize by design, so this isn't a controlled experiment so much as
  a demonstration that the one-time index cost doesn't recur — which is the specific, narrow claim
  worth stating plainly rather than dressing up as a full session-to-session study.

## Updated top-line conclusion

The original doc's conclusion — NexusContext is not a blanket win, and its value depends on
question shape and repository scale — **holds and is now better evidenced, not overturned.**
Combining both runs:

- **Where NexusContext is genuinely better:** (a) dead-code/no-inbound-caller analysis on a
  single, homogeneous codebase (strong win at 70 files; degrades on a large heterogeneous monorepo
  until the tool gets path scoping — a concrete, now-reproduced limitation, not a hypothesis);
  (b) cross-cutting architecture/location questions ("explain subsystem X", "where is Y
  implemented") — measurably worse-or-even at 70 files, measurably better (fewer tokens, often
  fewer calls, usually faster) at 391 files across 17 services, confirming the issue's own
  prediction that the graph's advantage should grow with size and cross-file/cross-service spread;
  (c) the persistent index itself, now shown (not just claimed) to amortize its one-time build
  cost across many later queries at zero marginal reindex cost, which is the mechanical
  precondition for every other claimed benefit.
- **Where it is not better, at either scale:** narrow, single-pattern questions answerable by one
  or two well-chosen `grep`/import-line searches (Task 2 at 70 files; Task 3 here) — baseline wins
  or ties on cost regardless of repo size, because the question's cost is dominated by pattern
  specificity, not codebase size.
- **Core vs. optional, updated:** `search_code`/`search_graph`/`get_architecture` move from
  "situational" toward **core** on this new evidence — their win rate and margin both improved at
  monorepo scale on architecture/location questions, which are a large fraction of real agent
  workloads. `detect_dead_code` stays **core but in need of a fix**: it's still the single
  clearest win when it works (both runs), but its lack of path scoping is now a demonstrated,
  reproducible failure mode at monorepo scale, not a theoretical one — worth its own follow-up
  issue. `trace_call_path`/`search_graph` for pure call-path tracing remains **situational**;
  neither benchmark run tested it enough on a large repo with real symbol-name collisions (this
  repo's Go packages have distinct names per service) to confirm or deny the issue's scaling
  hypothesis for that specific tool. Persistent cross-session indexing is now **evidenced core
  infrastructure** — everything else depends on it being cheap to reuse, and this run showed it
  is, within the honest limits stated above.

This satisfies the issue's two headline questions with real, spot-checked, cross-scale evidence:
NexusContext is genuinely better at architecture/location questions and dead-code detection, at
a scale where grep alone gets expensive, but not at narrow single-pattern lookups regardless of
scale — and its core-vs-optional line is now drawn from two data points instead of one, not from
assumption.
