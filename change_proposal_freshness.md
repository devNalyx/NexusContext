# Change Proposal: make the index trustworthy day-to-day, across sessions

**Status:** proposal, not yet implemented
**Context:** the project's own pitch is an "active external brain" — always-on,
always-fresh code intelligence a coding agent can lean on instead of grepping
blind. The watcher/warm-cold/catch-up machinery to make that true already
exists and is genuinely well-built (Phases 10, 18, 19, 20 in `README.md` are a
real, hard-won debugging history). But a live incident against this project's
own dogfooded `downtime` index today showed that machinery silently doesn't
apply to one of the tool's two first-class entry points, and the daemon gives
a caller no way to tell the difference between "fresh" and "stale" without
manually cross-referencing `registry.json` and `nexusd.log` by hand — which is
what it took to find this. This doc is that incident, root-caused to specific
files/lines, plus the fixes it implies.

## The incident

Ran `nexus architecture --project /home/opsquad/Workspace/downtime` and
`nexus search-code`/`search-graph` for two functions known to exist (written
and committed hours earlier that same day). All three came back clean —
`architecture` printed a plausible node/edge count, the searches returned "no
matches" with no error or warning of any kind. Nothing about the output
distinguished "these results are current" from "this index hasn't seen your
last day of work." Only cross-referencing `~/.local/share/nexuscontext/
projects.json` by hand surfaced the actual state:

```json
{
  "root_path": "/home/opsquad/Workspace/downtime",
  "last_indexed_unix": 1784327023,   // 2026-07-18 00:23 — ~37h stale
  "last_queried_unix": 0,             // never, despite real CLI use just now
  "auto_reindex_count": 0             // the watcher has never once touched this project
}
```

`nexuscontext.service` was confirmed running `nexusd serve` (not just idle) —
the watcher should have been live. It wasn't watching this project at all,
and never had been.

## Root cause: `nexus-cli` never marks a project "warm"

`ProjectEntry::is_warm` (`crates/nexus-core/src/registry.rs:44-46`) judges
warmth purely off `last_queried_unix`. Exactly two call sites update it today:

- `crates/nexusd/src/tools.rs:298-326` (`call()`, the MCP tool dispatcher) —
  calls `index::touch_queried(repo_path)` after every tool call, and *also*
  runs a synchronous catch-up reindex first if the project had gone cold
  (`is_cold`, `tools.rs:28-39`).
- `crates/nexusd/src/control.rs:85-95` (`dispatch()`, the GUI/control-API
  path) — same `touch_queried` call, with a comment explicitly noting *why*
  it's there: "without this the registry would only ever see
  `last_queried_unix` move for MCP-driven usage and never for someone just
  using the GUI directly."

That comment correctly anticipated the GUI gap and closed it. It didn't
anticipate the third caller: `crates/nexus-cli/src/main.rs`'s query
subcommands (`SearchGraph` `:186`, `Trace` `:195`, `Architecture` `:212`,
`DetectChanges` `:225`, `DeadCode` `:229`, `QueryPlanner` `:233`, `SearchCode`
`:273`, `QueryGraph` `:286`, `SearchCodebase` `:294`) all call straight into
`nexus_index`'s query functions with **no `touch_queried` and no `is_cold`
check at all**. `README.md` documents the CLI as a first-class, "manual
indexing and graph queries" interface (Phase 1) — it's not a debug-only
side door, it's the thing someone runs directly when they want to check
something without going through an agent, which is exactly what happened
here.

Net effect: **any project whose owner ever uses the CLI directly — even
just to sanity-check something, exactly this scenario — permanently reports
`last_queried_unix = 0`, is judged maximally cold forever, gets excluded
from the watcher's active-watch set by Phase 18's own gating, and never
self-heals via the Phase 18 catch-up-on-query mechanism either**, because
that mechanism only lives in the MCP dispatcher this project never went
through. All the reliability work in Phases 18-20 is real, but it only
protects one of three doors into the daemon.

**Proposal:** extract the two duplicated blocks in `tools.rs:298-326` and
`control.rs:85-95` into one shared helper in `nexus-index` (they're already
near-identical modulo the tool-name exclusion check) — something like
`nexus_index::touch_and_catchup(repo_path, skip: bool)` — and call it from
every read/query subcommand in `nexus-cli/src/main.rs` before dispatching to
the same underlying function the MCP/control paths use. `Reindex`,
`Export`/`Import`/`Delete`/`Install`/`TestEmbeddings` are unaffected (they're
either already-unconditional writes or don't take a `repo_path`). This is
the single highest-leverage fix here — it would have prevented today's
incident outright, and it's a small, mechanical change since the pattern to
replicate already exists and is proven in two places.

## Once fixed, the CLI will trigger real catch-up reindexes — check the blocking cost

Today, `auto_reindex_count: 0` for this project means the synchronous
catch-up path in `tools.rs:310-324` has never actually fired against it in
practice, so its real-world blocking cost has never been felt here. Once the
fix above lands, a `nexus search-code` run against a project that's been cold
for a while will trigger it — and `index_project` (`crates/nexus-index/src/
project.rs:26-41`) is a full clear-and-rebuild every time (no incremental
diffing exists yet — the README's own "Open Risks" section already flags
this). Phase 19's own numbers for this project's *own* embeddings-enabled
index: ~11 minutes per full reindex. A plain `nexus search-code` someone runs
expecting an instant answer could silently block for 11 minutes the first
time this fix takes effect after a normal weekend gap.

**Proposal (pick one, both are reasonable):**
- **(a) Make cold catch-up non-blocking everywhere it's invoked** (CLI
  included): return the current — possibly stale — result immediately,
  annotated with a `stale: true` / `reindex_in_progress: true` marker (see
  the observability proposal below for the shape), while kicking off
  `index_project` on a background thread. The *next* call a few seconds/
  minutes later gets the fresh index. Simpler mental model, and avoids ever
  blocking a human running the CLI interactively.
- **(b) Split catch-up into "structural-only, fast" vs. "full, with
  embeddings."** `index_directory` already treats embedding as a distinct
  third pass, skipped entirely when policy isn't `Allowed`
  (`README.md`'s Phase 12 section) — expose that as an explicit
  `index_project(repo_path, structural_only: bool)` parameter, and have
  cold-catchup call it with `structural_only = true`. Every structural tool
  (`search_graph`, `trace_call_path`, `get_architecture`, `search_code`, ...)
  becomes fresh in whatever a plain tree-sitter walk costs — seconds, not
  minutes — and the background watcher's own regular cycle still catches
  the project up on embeddings once it's warm again. `search_codebase`/
  `query_memory` callers alone would still need the full pass, so they'd be
  the one case still worth blocking on (or applying option (a) to,
  specifically for those two).

## The daemon has no way to tell a caller "this answer might be stale"

Separate from the bug above: even once catch-up reliably fires everywhere,
nothing in a tool's response shape today says whether the index it just
answered from was fresh, was just synchronously rebuilt to become fresh, or
(if async catch-up per option (a) above is adopted) is still rebuilding in
the background right now. A calling agent — or a human, as happened today —
has no signal-in-the-response to act on; finding out requires exactly the
`registry.json`/`nexusd.log` archaeology this incident needed.

**Proposal:** every MCP tool response and CLI query command that touches a
project should include a small, consistent freshness block, e.g.:

```json
"_freshness": {
  "last_indexed_unix": 1784327023,
  "index_age_secs": 134221,
  "was_cold": true,
  "catchup_triggered": true
}
```

Cheap to add (the registry entry is already loaded for the `touch_queried`
call this proposal already requires), and it turns "is this answer trustworthy
right now" from an out-of-band investigation into something the caller can
just read off the response it already got.

## No way to verify an MCP client is actually connected

A second, unrelated gap hit in the same investigation: `nexus install`
(Phase 10) configures MCP wiring for a detected agent, but there's no
companion command to verify that wiring is *currently* live. In this same
session, the daemon and CLI were both healthy and reachable, but the
MCP tool bindings themselves were absent from the active coding session
entirely — nothing in this project surfaced that mismatch; it had to be
inferred indirectly by searching for expected tool names and finding none.

**Proposal:** a `nexus doctor` subcommand (or extend `nexus status`) that,
for each agent `nexus install` knows how to configure, checks whether that
agent's *current* config actually references `nexusd mcp` (e.g. shelling out
to `claude mcp list` for Claude Code, reading `claude_desktop_config.json`
for Desktop) and reports drift — "configured for Claude Code but not
currently listed as connected" — rather than only ever confirming the
one-time `install` write succeeded.

## Smart, incremental reindexing (the real fix, not a workaround)

Everything above assumes catch-up reindexes stay full clear-and-rebuilds —
`GraphStore::clear()` (`crates/nexus-index/src/graph.rs:143-149`) unconditionally
wipes `embeddings`, `file_contents_fts`, `edges`, and `nodes` on *every* call
to `index_directory`, and its own doc comment already says the quiet part
out loud: *"Phase 1 reindexing is a full rebuild, not an incremental diff -
incremental edge correctness is flagged as an open risk in the proposal and
deferred past this vertical slice."* That's the real risk in shipping fix #1
above on its own: today, cold catch-up has fired **zero times ever** for the
`downtime` project (`auto_reindex_count: 0`), because nothing has ever
correctly marked it warm. Once CLI parity lands, catch-up will start firing
on the CLI's actual, frequent usage pattern for the first time — and if every
one of those is a full rebuild, "make the index always fresh" quietly becomes
"make the daemon do full-project reindex work far more often than it ever
has," which is a straight-up regression dressed up as a fix. This is the same
shape of problem Phase 18 already found once, from a different angle (a
project reindexing every ~11 minutes around the clock with nobody querying
it) — cost scaling with the wrong signal. Fixing *that* required real
engineering (Phases 18-20), not a config tweak; this deserves the same
seriousness rather than being left as follow-up work nobody circles back to.

**What's already there to build on:** `nodes` already has a `file_path`
column (`graph.rs:110`), so "every node belonging to file X" is one indexed
query, not a scan. `content_signature` (`ingest.rs:76-113`) already proves the
walk-and-stat-every-file mechanism works and is cheap — it's just collapsed
into one project-wide hash today instead of kept per-file.

**What's missing, concretely:**

1. **Per-file signatures, persisted.** A new `file_signatures(file_path TEXT
   PRIMARY KEY, size INTEGER, mtime_millis INTEGER)` table — the per-file
   version of what `content_signature` already computes, just not thrown away
   as a single opaque `u64`. On each reindex, diffing a fresh walk against
   this table cheaply classifies every file as unchanged / changed
   (modified-or-new) / removed, with zero parsing needed to know which is
   which.

2. **Durable call sites — the actually hard part.** `index_directory_inner`'s
   two-pass design (`ingest.rs:115-215`) builds `global_fn_registry` and
   `pending_calls` fresh every run and discards both the instant resolution
   finishes. That's fine for a full rebuild, but it's exactly what makes
   correct *incremental* resolution nontrivial: if file A (unchanged) calls a
   function in file B (just modified, renamed, or deleted), file A's own
   `PendingCall` was never re-derived — nothing re-parsed file A — so nothing
   knows its previously-resolved edge needs to be re-checked at all. This is
   precisely the case the README's own "Graph incremental-update correctness"
   risk note is worried about, and it's real: skip it and incremental mode
   silently accumulates dangling/stale call edges pointing at renamed or
   deleted functions, forever, with no path to self-correct short of a full
   rebuild — worse than today's honest "always full rebuild" in the one way
   that actually matters (correctness, not just cost).

   Fix: persist the textual call fact, not just its resolution — a
   `call_sites(id, caller_id INTEGER REFERENCES nodes(id), callee_name TEXT)`
   table, written whenever a file is (re)parsed, kept around afterward instead
   of discarded. This turns "which callers might be affected by a change
   elsewhere" from "re-parse everything to find out" into "a plain SQL query
   over already-stored facts."

**The incremental algorithm this enables**, replacing `clear()` + full walk:

1. Walk once (same `WalkBuilder` config `index_directory_inner` already uses),
   stat every file, diff against `file_signatures` → `unchanged` / `changed`
   / `removed` sets.
2. For `changed ∪ removed` files: look up their node ids (`file_path`
   column), then delete — in order — `edges` where `src_id` **or** `dst_id`
   is in that id set (both directions matters: an edge from an *unchanged*
   file pointing at a node about to disappear is exactly the dangling-FK
   case above if only one direction is checked), `embeddings` for those ids,
   `file_contents_fts` rows for those paths, `call_sites` where `caller_id`
   is in that id set, then the node rows themselves.
3. Re-parse only the `changed` set (same `index_file`/`index_markdown_file`
   as today) — new nodes, new pending calls, new pending embeddings.
4. Resolution pass: rebuild `global_fn_registry` from one `SELECT name, id
   FROM nodes` over the *whole* project (cheap — an indexed read, no
   parsing), then resolve (a) every pending call from the files just
   re-parsed, **and** (b) every surviving `call_sites` row whose edge got
   deleted in step 2 because its target moved or vanished — this second set
   is what correctly propagates a rename/removal in a changed file out to
   unchanged callers elsewhere, without re-parsing them. Insert fresh `Calls`
   edges for whatever resolves; leave the rest unresolved, same as today's
   existing ambiguous-name behavior. Insert the re-parsed files' own call
   sites into `call_sites` for the next pass.
5. Re-embed only nodes that are new, or whose source text differs from the
   existing `embeddings.chunk_text` for the same `qualified_name` (match on
   name, not `node_id` — ids aren't stable across a delete+reinsert of a
   modified file's nodes).

**Staged rollout** (matches this project's own established pattern of
shipping a narrow, honest slice first rather than a big-bang rewrite):

- **Stage 0, ships independently and fast:** skip step 5's embedding call
  for anything whose `chunk_text` is unchanged, even inside *today's*
  full-clear-and-rebuild model. Phase 12's own numbers make the embeddings
  HTTP round-trip the dominant cost of this project's own ~11-minute
  reindex, by far more than local tree-sitter parsing — this alone,
  with no schema change beyond comparing already-stored text, could cut the
  number that matters most for the blocking-cost problem above before any
  node/edge incrementality work even starts.
- **Stage 1:** the full `file_signatures` + `call_sites` design above. This
  is genuinely nontrivial — the unchanged-caller-of-a-changed-callee case is
  real extra design, not a one-line fix — and deserves the same
  iterate-against-a-real-dogfooded-project rigor Phases 19-20 already used
  (their own "four iterations before the fix actually held" is the right
  bar to hold this to, not a one-and-done PR).
- **Permanent fallback:** keep a `force_full: bool` on `index_project` (or
  `nexus reindex --full`) as an explicit escape hatch for whenever
  incremental drift is suspected — the README's own Open Risks section
  already grants itself this exact permission ("worth a full-reindex
  fallback if incremental graph diffing gets too complex").

## Priority order

1. **CLI `touch_queried`/cold-catchup parity (root-cause fix)** — small,
   mechanical, reuses an already-proven pattern from two other call sites,
   and is the one change that would have prevented today's incident outright.
2. **Smart/incremental reindexing, at least Stage 0** — should land
   *alongside* #1, not after it. Shipping #1 without this is what turns
   "always fresh" into "always doing full-project rebuilds far more often,"
   which is the regression the whole point of this proposal is to avoid, not
   introduce. Stage 0 (skip re-embedding unchanged content) is cheap enough
   to gate #1's release on; Stage 1 (full node/edge incrementality) is the
   real fix and can follow once proven.
3. **Blocking-cost mitigation for catch-up** (the async/non-blocking or
   structural-only-first options above) — becomes far less urgent once
   Stage 1 lands (catch-up gets cheap enough that blocking briefly stops
   mattering), but worth having in the meantime since Stage 1 is the bigger,
   slower-to-land piece of work.
4. **`_freshness` response metadata** — cheap, additive, turns silent
   staleness into a visible, actionable signal for every future incident
   like this one.
5. **`nexus doctor` MCP-connection check** — smaller, independent, closes a
   real but separate gap (config correctness vs. index freshness).
