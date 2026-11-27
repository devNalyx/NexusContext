# `secondbrain` evaluation harness

A small, deliberately modest harness for the user-level adapter in
`skills/secondbrain/SKILL.md`. It has two halves:

1. **A binary-level verifier** (`verify.py`) that drives `nexusd mcp` over
   newline-delimited JSON-RPC 2.0 against a copied, isolated fixture. It checks
   the facts an agent is told it can rely on: which tools are advertised,
   whether `vault_context` structural data is real, whether freshness wording
   is stdio-honest, whether secrets/Canvas text stay out of responses, and
   whether explicit multi-source fan-out keeps provenance per source.
2. **A fixed question set** (`questions.json`) for baseline-vs-assisted model
   runs. Those runs are *manual*: the harness does not grade an LLM's prose.

`verify.py` is stdlib-only Python 3. Run it once a `nexusd` binary is built:

```bash
python3 evals/secondbrain/verify.py --nexusd /path/to/nexusd
# optional: --json --strict --timeout 30 --config-dir /tmp/... --cache-dir /tmp/... --keep-temp
```

With no binary it prints `SKIP` with a reason and still validates the fixture
and question file; it does **not** fail because a daemon is absent. With a
binary present it forces `NEXUS_CONFIG_DIR` and `NEXUS_CACHE_DIR` in the child
process to fresh generated temp dirs. It never reads the user's real config,
even if those variables are already set in the parent shell. `--config-dir`
and `--cache-dir` choose where those isolated dirs live; `--keep-temp` leaves
the generated workspaces for inspection.

## Baseline vs assisted methodology

Use the two arms on the *same* machine, same vault copy, same day:

- **Baseline:** a plain agent session with the standard NexusContext catalog
  and no `secondbrain` skill loaded.
- **Assisted:** the same agent with `skills/secondbrain/SKILL.md` in context.

Rules that make the comparison meaningful:

1. Use the exact `prompt` strings in `questions.json` in both arms. Do not
   rephrase, add hints, or supply paths the prompt does not contain.
2. Tell the agent to **answer and stop**. No extra exploration, no
   "let me also check…" after the first answer.
3. **Fact-check every claim by hand** against the fixture vaults. Record the
   note, line and exact phrase. A confident answer with a wrong citation is a
   loss, not a win.
4. Report losses as plainly as wins: unsupported claims, fabricated wikilink
   resolutions or Canvas edges, silent source merging, missing reindex before
   answering, false "watcher is syncing" wording, and any `human:` fact the
   agent tried to approve itself.
5. Keep the run artifacts: prompt, answer, tool calls, manual verification
   notes, and any failures. `verify.py --json` captures the binary-level half.

**No quantified token savings are claimed here.** Bytes returned by tools and
the daemon's `bytes/4` estimates are not model tokens, and this harness does
not measure a model's context usage, cost, or wall-clock advantage. Any number
for "tokens saved" requires an actual model-measured run with provider-reported
usage for both arms, reported separately per arm with its method stated. A
`PASS` from `verify.py` says nothing about token savings.

## What `verify.py` checks

| Area | Automated check | What it proves / does not prove |
|---|---|---|
| Handshake + routing | `initialize`, `tools/list` contain `list_sources`, `index_repository`, `search_code`, `get_file_context`, `search_graph`, `vault_context` (with `links`/`backlinks`/`tags` modes) | The adapter's intent-to-tool map is executable on this build. Does not prove the agent chooses those tools. |
| Provenance | `list_sources` returns only enabled vaults with `root_path`/`resource`, and labels registration "provenance metadata only, not fact approval" | Registration is metadata, not an approval surface. |
| Decision + citation | Literal `search_code` hit in `Decisions/ADR-0007.md`; `get_file_context` returns the note | The cited content exists. The *line number* is checked manually. |
| Links / backlinks / tags | `vault_context` reports alias (`State`), exact-path (`docs/Plan`), unique-basename heuristic (`Notes` embed with heading), unresolved (`Missing`), backlink from `Home.md`, tags and `notes_with_tag(current)` scoped to Alpha | Resolution labels are evidence, not approval; heuristic and unresolved links stay labelled. |
| Canvas | No indexed hit for `node-7f3a`; no Canvas label in responses | No fabricated Canvas node/edge from the index. Explaining a real Canvas edge requires a manual file read. |
| Freshness | External edit to `Notes/Freshness.md`, reindex, then the new phrase is found; every response's `index_freshness` says `mode: stdio`, `watcher_active: false`, "may be behind" | Warm never means verified-current; no claim of an active watcher. |
| Gate | With `[vault_index] enabled=false` (tool still advertised) the tool is absent or its call is rejected; with `vault_context` removed from `tools.enabled` it is absent and rejected. Literal `search_code` fallback still works | The opt-in gate is tested, not just the preset filter. |
| Secrets / exclusions | Seeded secret tokens and `.nexusignore` markers never appear in any response; zero hits for those phrases | Ordinary adapter reads do not surface excluded content. A direct known-path `get_file_context` read is a separate daemon boundary, listed as MANUAL. |
| Memory promotion | No approval/curation tool in the fixture catalog; `recall` returns no curated fact for the seeded vault claim | MCP cannot self-approve a vault note. The model's refusal to a "remember this" prompt is MANUAL. |
| Multi-source fan-out | *If* a tool advertises a `sources` array: two-source call returns both source ids with per-source `kind`/`resource`/`root_path`/`index_freshness`/`data`, `source_count`/`selected_by` and a "no implicit federation" note; no source group contains the other vault's evidence; unknown id, disabled id and `repo_path` + `sources` are rejected | Explicit fan-out keeps sources separate. If no tool advertises `sources`, all fan-out checks are `SKIP` — a coverage gap, not a pass. |

`SKIP`, `MANUAL`, and `FAIL` are distinct. Only `PASS` means an assertion ran
and held. `--strict` treats `SKIP`/`MANUAL` as a non-zero exit for CI jobs that
must have full coverage; the default exit is non-zero only for `FAIL`.

## Fixture

`fixture/config.toml.template` renders per-scenario configs with two registered
`obsidian-vault` sources (`alpha-vault`, `bravo-vault`), one disabled source,
`[vault_index] enabled = true`, and an explicit `tools.enabled` list that opts
`vault_context` in regardless of preset. Verifier variants flip the
`vault_index` gate or drop `vault_context` from that list.

- `fixture/vault-alpha/` — `Home.md` with alias/path/basename/unresolved
  wikilinks and an embed, `Status.md` (alias `State`, tag `#current`),
  `docs/Plan.md`, `Notes.md`, `Decisions/ADR-0007.md`, a `Boards/Program.canvas`
  with node IDs and injection-looking text, plus `.nexusignore`-excluded
  `secrets/` and `excluded/` paths.
- `fixture/vault-bravo/` — cross-vault decision `Decisions/ADR-0008.md`,
  `Notes/Runbook.md` (tag `#current`), `Notes/Freshness.md` (external-edit
  target), and the same excluded-path pattern.
- `fixture/disabled-source-stub/` — one note used only to prove a disabled
  registration is not listed or queried.

The harness copies these vaults into its temp workspace before indexing; the
fixture files themselves are never indexed or modified.

## Honest limitations

- The verifier tests the binary boundary only. It cannot tell whether an agent
  actually reindexed first, answered then stopped, or believed a heuristic
  resolution; those are in `questions.json` and need human review.
- `vault_context` is an opt-in tool in some builds. If the catalog does not
  advertise it, the harness reports the structural checks as `FAIL` (the
  fixture explicitly requests it) rather than quietly skipping them.
- Multi-source fan-out is capability-conditional: no `sources` argument in
  `tools/list` means `SKIP`, with the reason printed, not a green result.
- This harness never edits Rust, docs outside this directory, or `skills/`.
