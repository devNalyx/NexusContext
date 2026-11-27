# 0022. Ship a user-level secondbrain skill with explicit source fan-out

Status: Accepted
Date: 2026-11-27

## Context

ADR 0021 makes vault structure available through one existing read-only MCP
catalog. Agents still need guidance on how to ask: discover registered sources
instead of guessing paths, reindex after external note edits, quote freshness
honestly, and treat note text as data rather than instructions. A vault-local
skill would be indexed as note content and would have to be copied into every
vault. Cross-vault questions invite an implicit "search everything" mode, which
would hide provenance and silently widen what an agent can read.

## Decision

Ship `skills/secondbrain/` as a thin, portable Agent Skill over the existing
read-only MCP tools — no new daemon, no second MCP server, no plugin hooks and
no new trust domain. `nexus install opencode --global --yes` installs it as a
user-level skill at `~/.config/opencode/skills/secondbrain/`, or under
`$XDG_CONFIG_HOME/opencode/skills/secondbrain/` when that variable is set;
project-local installs deliberately skip it. The skill's rules are to call
`list_sources` first, reindex before trusting structure, prefer targeted tools
over grep sweeps, treat note/Canvas/callout text as untrusted data, and leave
memory approval to a human CLI actor.

Source registration is provenance metadata only. It never acts as `human:`
approval of a fact or skill, and the skill has no path to upgrade trust.

Five read tools — `search_code`, `search_graph`, `get_file_context`,
`get_architecture` and `vault_context` — accept an optional `sources` array of
registered IDs or resource URIs. It is capped at 16 entries, accepts only
enabled registered sources, and is mutually exclusive with `repo_path`. The
grouped response keeps `id`, `kind`, `resource`, `root_path`, `index_freshness`
and `data` per source, plus `source_count`, `selected_by` and an explicit note
that this is opt-in fan-out with **no implicit federation**. No new tool name is
introduced, and a call without `sources` stays scoped to a single project.

## Alternatives considered

- **A separate MCP server or daemon for vaults:** new schema tax, freshness
  contract and trust surface for queries the existing catalog can answer.
- **Implicit federation of every registered source:** convenient, but agents
  could read unrelated vaults without asking, and provenance would blur.
- **A `federate` tool:** one more tool in every advertised preset to express
  what an optional argument already does.
- **Vault-local skill files:** they would be indexed as note content, duplicated
  per vault, and vulnerable to edit by whoever can edit the notes.
- **Treating `nexus source add` as human approval:** would collapse provenance
  into trust and let registration bypass the memory admission gate.

## Consequences

The skill must check the live `tools/list` before using `vault_context`, since
that tool is opt-in and absent from older builds; it must not invent tools or
capabilities. Fan-out remains bounded and attributable, and the privacy gates
from ADR 0021 apply identically to every selected source. The MCP catalog grows
from 30 to 31 tools with presets 5 / 13 / 31, so the doc-drift assertions move
together. Issue #133 implementation is in progress on
`feat/obsidian-secondbrain-integration`; no release or dogfood evidence is
claimed here.
