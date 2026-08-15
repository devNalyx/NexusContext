# Watcher and Freshness

How the index stays up to date without the caller having to remember to
reindex — and the real incidents that shaped the current design. This
subsystem has been rewritten more times than any other part of the
project; the current shape is the result of several rounds of "looked
correct on review, didn't hold up live."

## The background watcher (`nexusd serve` only)

A `notify`-backed file watcher (`notify-debouncer-mini`, 2s debounce)
watches every **warm** registered project and triggers a full reindex on
real changes. `mcp`-mode sessions don't own this — it's `serve`-mode only.
`serve` (and so this whole subsystem) is Linux/macOS only as of Phase 33 —
Windows doesn't have it yet, `mod watcher` is compiled out there entirely
alongside the control API it's paired with. See [[Known-Limitations]].
`touch_and_catchup`'s cold-project catch-up reindex below still applies
everywhere, `mcp` mode included — it's the one piece of freshness handling
that doesn't depend on the watcher being present at all.

## Warm vs. cold: not every registered project stays watched

A project not queried (via any MCP tool, the CLI, or the GUI) within
`warm_window_secs` (6h default, see [[Configuration]]) is **cold** and
drops out of the active watch set entirely — it stops costing an inotify
watch, not just stops triggering reindexes. Judged **only** on
`last_queried_unix`, never `last_indexed_unix` (the latter is bumped by
auto-reindex itself, which would otherwise let a cold project's own
watcher-triggered reindex re-arm its own warm window).

Going cold doesn't mean going stale forever: `touch_and_catchup` — one
shared entry point used by the MCP dispatcher, the control API, and every
CLI read subcommand — checks staleness before answering and runs one
synchronous catch-up reindex if the project had gone cold. The first query
after a gap costs a real reindex; every one after that is normal. (This
consolidation itself was a real fix: the CLI didn't have it for a while,
which meant a project checked only via `nexus search-code` etc. could go
cold and never self-heal, silently returning stale results with no
warning.)

## Why the watcher used to reindex itself, forever

The single hardest bug this project has shipped a fix for. `notify`'s
Linux backend subscribes to file **opens**, not just writes — and indexing
a project means opening every source file to parse it. So a reindex's own
reads were indistinguishable from real edits once debounced, and it
re-triggered its own next run the instant it finished. Four fix attempts,
each real but insufficient, before the one that held:

1. Drain the event channel non-blocking before deciding what to reindex
   (a real bug on its own — a backlog of distinct edits was being replayed
   serially instead of coalesced — but not *the* bug).
2. Unwatch before reindexing, re-watch after — insufficient, because
   re-establishing a *recursive* watch itself walks the whole tree opening
   every subdirectory, generating its own burst of "changed" events right
   at the moment watching resumes.
3. Move the reindex-attempt timestamp to *after* re-watching completes,
   not before indexing starts — closer, but a large project's re-watch
   walk could still take longer than the gap.
4. **The fix that held**: raise the minimum gap between reindex attempts
   for the same project to 180 seconds — real margin over any plausible
   re-watch-walk duration. Verified live: zero further triggers over 6.65+
   minutes of observation, past every straggler timing seen in the earlier
   attempts.

A related, broader gap closed afterward: *any* read-only tool touching a
watched project's files (`git status`, a build, an editor) could still
wake the loop, even with the 180s gap respected. Fixed with
`content_signature` — a cheap hash over exactly the files a real reindex
would touch (path + size + mtime, same ignore-respecting walk indexing
uses), compared against the signature from the last real reindex. A
wake-up with an unchanged signature is now a genuine no-op: no unwatch, no
reindex, no re-watch dance at all.

## The `root_path: "."` incident and the inotify watch budget

A real incident on the dogfooding box: `nexus reindex .` stored
`root_path: "."` verbatim in the registry (no canonicalization). The
watcher later resolved that `"."` against the **daemon's own cwd**
(`$HOME`, the systemd user service default) — not the directory the CLI
had actually been run from — and recursively watched the entire home
directory. Confirmed live: ~65K inotify watches held (against the box's
65536 `fs.inotify.max_user_watches` limit), and every other app on the
desktop (Obsidian included) starved of watches until it was fixed.

Two fixes landed together:

1. **`root_path` is canonicalized at write time**, at the one place it's
   ever persisted (`index_project`/`import_project`) — every entry from
   here on is absolute and resolved. The watcher also self-heals any
   pre-existing non-canonical entry on its next periodic sync.
2. **A self-imposed watch budget**, independent of the root_path bug —
   even a legitimately huge project (an unignored `node_modules` or
   `target` tree) could exhaust the same limit on its own. The watcher now
   caps itself at **half** of whatever `/proc/sys/fs/inotify/max_user_watches`
   reports, estimates each project's watch cost before adding it (counting
   *every* directory, not just non-gitignored ones — `notify`'s recursive
   watch has no `.gitignore` concept, so counting only the ignored-filtered
   set would undercount the exact case most likely to blow the budget), and
   evicts the least-recently-*queried* currently-watched project (LRU) to
   make room for a more recently used one. A project that doesn't fit even
   after evicting everything is skipped outright with a loud warning — no
   partial watch, since `notify` has no such mode.

A 5-minute cooldown after any eviction stops a project from being evicted
and immediately re-admitted on the very next sync under sustained real
pressure (caught in code review, not a live incident).

Watch health (budget/used/pressure-event count) is surfaced via
`status.get` and the GUI Dashboard — a non-zero `pressure_events` count is
the signal that `fs.inotify.max_user_watches` is actually constraining
real usage on a given machine.

## Related

[[Storage-and-Data-Model]] (the registry) · [[Security-Model]] ·
[[Configuration]] · [[Known-Limitations]]
