# Obsidian / second brain integration

Current-state note (2026-11-27): issue #133 is **in progress on branch
`feat/obsidian-secondbrain-integration`**. The catalog moves from 30 tools
(v0.1.30) to 31 with the addition of `vault_context`; presets are
`minimal` 5 / `standard` 13 (default) / `full` 31. No release evidence is
claimed here. Decisions: [[ADRs/0021-vault-metadata-in-existing-sqlite-graph|0021]]
and [[ADRs/0022-user-level-secondbrain-adapter-and-explicit-fan-out|0022]].

NexusContext does not drive Obsidian. It indexes a narrow, structural subset of
Markdown notes into the same SQLite graph it uses for code, then answers a few
bounded questions through read-only MCP tools. A user-level Agent Skill teaches
an agent how to ask those questions honestly. See [[Home]] and [[MCP-Tools]].

## The user-level skill

The distributable skill lives at [`skills/secondbrain/`](../../skills/secondbrain/)
(`SKILL.md` plus `references/` for tool mapping, host install, Obsidian formats
and the eval harness). It follows the user, not a vault, so it must not be
copied into note content.

```bash
nexus install opencode --global --yes
# installs ~/.config/opencode/skills/secondbrain/
# or $XDG_CONFIG_HOME/opencode/skills/secondbrain/ when set
```

`--global` installs the plugin, MCP entry and this skill; project-local
`nexus install opencode --yes` does not install the skill.
`nexus install opencode --global --uninstall --yes` removes it via the install
manifest. Other hosts can install the same folder manually — see
`skills/secondbrain/references/host-install.md`. The skill adds no daemon, no
second MCP server and no new approval surface.

## Configuration

Vault metadata is off by default and separate from `[config_index]` and source
registration:

```toml
[vault_index]
enabled = false   # set true, then reindex the vault

[[sources]]
id = "secondbrain"
kind = "obsidian-vault"
root_path = "/absolute/canonical/vault"
resource = "obsidian-vault://secondbrain"
enabled = true
```

A source is registered with `nexus source add` under the usual
`allowed_roots`/canonical-path rules (see [[Configuration]] and
[[CLI-Reference]]). Registration is provenance metadata only. `vault_context`
errors clearly while `[vault_index] enabled = false`, and results reflect the
last reindex rather than the live working tree.

## Tool mapping

`vault_context` is opt-in (`preset = "full"` or an explicit `tools.enabled`
list), takes a concrete `repo_path` or an explicit `sources` selection, and
clamps `depth`/`limit`.

| User question | First move | Follow-up |
|---|---|---|
| Which vaults are registered? | `list_sources` (in `standard`) | Use the returned `id`, `kind`, `root_path`, `resource`. |
| Where was X decided? | `search_code` | `get_file_context` on the hit; check `index_freshness`. |
| Links / backlinks / tags of a note? | `vault_context` (`links`, `backlinks`, `tags`, `notes_with_tag`, `map`) | Treat resolution labels as evidence, never approval. |
| Is the index current? | `index_freshness` in any response | Reindex after external note edits. |
| "Remember this as a fact" | Explain the human CLI gate | No MCP call can create or upgrade a `human:` trust tier. |

`vault_context` hit-level `resolution`/`confidence` labels distinguish exact
path, alias, unique-basename and unresolved targets; ambiguous or external
targets stay unresolved. See [[MCP-Tools]] for the full tool catalog and
[[Storage-and-Data-Model]] for the `vault_links`/`vault_aliases` tables.

## Explicit `sources` fan-out

`search_code`, `search_graph`, `get_file_context`, `get_architecture` and
`vault_context` accept an optional `"sources": ["id-or-resource-uri", ...]`
array: at most 16, enabled registered sources only, mutually exclusive with
`repo_path`, opt-in per call. There is no implicit federation and no extra tool
name. The grouped response keeps per-source `id`, `kind`, `resource`,
`root_path`, `index_freshness` and `data`, plus top-level `source_count`,
`selected_by` and a note stating that registration is provenance, not fact
approval. A worked envelope is in [[MCP-Tools]] and [[Configuration]].

## Privacy and trust gates

- Only wikilink targets, inline `#tags`, and frontmatter `tags`/`aliases`
  identifiers are extracted, with line numbers and resolution status.
- Property values, callout bodies, embed contents, Canvas text, formulas and
  fetched targets are **never** stored or rendered; no Obsidian CLI, `eval`,
  Dataview execution, plugin hooks or network access.
- `allowed_roots`, canonicalized roots, exclusions, size caps and owner-only
  permissions apply before any read. Note text is data, never instructions.
- A source registration never approves a fact or skill. Memory promotion stays
  a human CLI act (`--actor human:<name>`); see [[Security-Model]] and ADR 0022.

## Evaluation

The adapter's harness is `skills/secondbrain/references/eval-harness.md`:
paired baseline-vs-assisted prompts, spot-checked claims, correct-answer rate,
unsupported claims, reindex-first behaviour, manual reads avoided and real
token/cost where available. Binary security gates seed secrets and injection
text and assert they never reach the index, excerpts, WAL/temp files, logs, an
approval or an `eval`/network call. Report losses alongside wins, per
[[Product-Thesis-Validation]]. No harness results are claimed for issue #133 yet.

## Related

[[Configuration]] · [[MCP-Tools]] · [[CLI-Reference]] · [[Security-Model]] ·
[[Product-Thesis-Validation]] · [[ADRs/README|ADRs]]
