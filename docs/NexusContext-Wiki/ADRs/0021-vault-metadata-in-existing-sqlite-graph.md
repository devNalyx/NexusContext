# 0021. Store a narrow vault metadata subset in the existing SQLite graph

Status: Accepted
Date: 2026-11-27

## Context

Agents working across repos keep notes in Obsidian-style vaults and ask small,
repeated questions: what does this note link to, what links back, which notes
carry a tag, what is reachable in two hops. NexusContext already indexes
Markdown headings and has one SQLite graph per project, so a second retrieval
stack for vaults would duplicate the index, the freshness envelope and the
privacy checks. At the same time, a vault is untrusted note content: property
values, callout bodies, Canvas labels, Base formulas and fetched pages can hold
secrets or prompt-injection text, and none of them are needed to answer those
structural questions.

## Decision

Add a narrow lexical vault metadata extractor to the existing Markdown ingest
pass, gated by an off-by-default `[vault_index] enabled = false` config section
and independent of `[config_index]` and source registration. The extracted
metadata lives in **two tables in the same `graph.db`**: `vault_links`
(source/destination note ids or raw target text, line number, resolution and
confidence labels, optional heading/display text and embed flag) and
`vault_aliases` (frontmatter alias identifier to note id).

Only wikilink targets, inline `#tags`, and frontmatter `tags`/`aliases`
identifiers are extracted, with line numbers and resolution status. Unresolved
or ambiguous targets are recorded with their raw text and explicit provenance
labels instead of being dropped or turned into confident edges. `vault_context`
queries those tables in modes `links`, `backlinks`, `tags`, `notes_with_tag` and
`map` with bounded `depth`/`limit`, and errors clearly when `vault_index.enabled
= false` or before the vault has been reindexed with the gate on.

No Obsidian CLI, `obsidian eval`, Dataview query execution, plugin hooks,
template/Base rendering or network fetch is part of the index path. No property
values, callout bodies, embed contents, Canvas text, formulas or fetched targets
are stored. Registration of a source never enables this gate by itself.

## Alternatives considered

- **Drive the Obsidian app / CLI or Dataview:** would execute untrusted note
  expressions and add a host process plus a new trust domain; answers would
  depend on a running app rather than an indexed snapshot.
- **Separate vault store or vector index:** a second index to build, secure and
  keep fresh, with no benefit for lexical wikilink/tag questions.
- **Store full note bodies and frontmatter values:** larger context payloads and
  a straightforward secret/injection surface for questions that need only
  identifiers and locations.
- **Fabricate edges for unresolved wikilinks:** convenient graph density, but
  it would present a basename guess as a verified relationship.

## Consequences

Coverage is deliberately narrow: resolution stays heuristic (exact path, alias,
unique basename, unresolved), ambiguous links stay unresolved, and the metadata
is only as current as the last reindex. The two tables are rebuilt with a full
reindex and removed on `delete_project`, matching ADR 0006. Enabling the gate
requires an explicit reindex; the freshness envelope in every tool response
still says that `warm` means recently indexed, not verified against the working
tree. Issue #133 implementation is in progress on
`feat/obsidian-secondbrain-integration`; no release evidence is claimed here.
