# NexusContext tool mapping

Released NexusContext exposes a single stdio MCP catalog. Do not assume a tool
exists because a doc mentions it: check the current `tools/list` response.

## Always available in released main

- `list_sources`: read-only catalog of registered sources. Returns `id`, `kind`,
  `root_path`, `resource`, `title`, `description` and the explicit note
  "provenance metadata only, not fact approval".
- `search_code`: literal-phrase FTS5 search over indexed content. Config hits
  may include `document`, `key_path`, line range, `parse_status` and `note`.
  Responses include `coverage`, `index_freshness`, `has_more` and `next_offset`;
  pass `next_offset` back as `offset` to continue.
- `get_file_context`: read a known location with bounded excerpt and indentation.
- `search_graph`: structural search over defined symbols and Markdown `Section`s.
- `get_architecture`, `trace_call_path`, `detect_changes`, `detect_dead_code`,
  `query_planner`, `get_session_usage`, `list_skills`, `recall`, plus any other
  tools advertised by the current catalog.

## Only when advertised (local hybrid or after upstreaming)

- `vault_context`: read-only structural queries over one registered vault.
  Modes: `links`, `backlinks`, `tags`, `notes_with_tag`, `map`. It requires a
  concrete `repo_path`; `depth` and `limit` are clamped. Resolution labels are
  evidence, not approval: exact path/alias matches outrank heuristic basename
  matches; ambiguous or external targets remain unresolved.

## Freshness wording to reuse

> This MCP session does not run the background watcher; warm/cold only reflects
> the registry's last-indexed time. The graph may be behind the working tree
> even when warm — reindex after external edits or verify directly.

Do not say "watcher is syncing" in a stdio MCP session. If `indexed` is false or
the hit predates the user's edit, reindex and retry before answering.

## Provenance wording to reuse

> `secondbrain` is registered as a provenance source. That registration does not
> approve any note as a fact or skill. Curated facts require a current, stable,
> in-root fact with human verification through the CLI.

If `recall` returns no facts, that is correct unless a human has signed off.
