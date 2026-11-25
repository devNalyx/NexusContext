# CLI Reference

The `nexus` binary — a first-class interface (not a debug side door),
routed through the exact same shared code as the MCP tools and the control
API (`touch_and_catchup` and friends — see [[Watcher-and-Freshness]]), so
CLI usage keeps a project warm and self-heals from cold exactly like an
agent session does.

## Everyday commands

```bash
nexus reindex /path/to/project              # build or rebuild the graph
nexus reindex /path/to/project --deep       # + LSP-resolved-symbol enrichment (Rust, opt-in)
nexus search-graph SomeFunction --project /path/to/project
nexus trace SomeFunction --project /path/to/project --direction inbound
nexus architecture --project /path/to/project
nexus dead-code --project /path/to/project
nexus dead-code --project /path/to/project --path-prefix pkg/events  # scope to a subdirectory (monorepos)
nexus search-code "some literal text" --project /path/to/project
nexus detect-changes --project /path/to/project    # uncommitted git diff -> affected symbols
nexus query-planner "some question" --project /path/to/project
nexus query-graph "MATCH (a:Function)-[:CALLS]->(b:Function) RETURN b" --project /path/to/project
nexus delete /path/to/project                       # remove a project's index, not its source
nexus status
```

Reindexing is safe to run concurrently with the background auto-sync
watcher — see [[Indexing-Pipeline]] for the transaction/locking that makes
that true.

## Sharing an index

```bash
nexus export /path/to/project     # writes .nexuscontext/index.db.zst
nexus import /path/to/checkout    # a teammate skips the first reindex
```

## Browsing the *code* in Obsidian (a different vault from this one)

```bash
nexus export /path/to/project --format obsidian   # writes .nexuscontext/vault/*.md
```

One note per function/type/section, each with its actual source snippet
(syntax-highlighted) plus `[[wikilinks]]` derived from the real call graph.
Open `.nexuscontext/vault/` as its own separate Obsidian vault — this is
generated, per-project, local-only (gitignored), and answers "what does
this codebase's call graph look like," not "how does NexusContext itself
work" (that's what *this* vault, `docs/NexusContext-Wiki/`, is for).

## Curated memory (facts, trust review, OKF bundles)

Facts are staged by an agent, but only a human can make them injectable.
The CLI is that human gate — the MCP catalog deliberately has no
"approve your own staging" tool. See [[MCP-Tools]] for the `recall` side.

```bash
nexus memory add Decision "Use stable presets" \
  --body "Fixed tool schemas are the invoice." --anchor src/lib.rs --project /path/to/repo
nexus memory list --project /path/to/repo
nexus memory approve <id> --actor human:you --project /path/to/repo
nexus memory recall "schema" --project /path/to/repo
nexus memory recall "schema" --lower-trust --project /path/to/repo  # labelled candidates only
nexus memory stale src/lib.rs --project /path/to/repo               # anchor changed by hand
nexus memory audit --project /path/to/repo
nexus memory export --project /path/to/repo                         # .nexus/memory/ (OKF v0.3)
nexus memory import --project /path/to/repo --bundle /path/to/bundle
nexus memory show <id> --project /path/to/repo
nexus memory reject <id> --actor human:you --project /path/to/repo
nexus memory skills --project /path/to/repo                         # absorbed Myelin skills
nexus memory skill-queue --project /path/to/repo       # both queues, labelled
nexus memory skill-promote <id> --project /path/to/repo # id from skill_candidates_promotable
nexus memory skill-approve <id> --actor human:you               # eligible for signpost
nexus memory skill-revoke <id> --actor human:you                # leaves the next injection
nexus memory bundles --project /path/to/repo --root /path/to/workspace
```

`skill-queue` deliberately prints two distinct id spaces, because they are not
interchangeable:

- `skill_candidates_promotable` — warming candidates created by
  `record_observation`; these are the only ids `skill-promote` accepts.
- `pending_review_excerpts` — signposted lines staged by a harness `SessionEnd`
  hook. These are *not* promotable: a human/agent must first judge one and turn
  it into an observation via the agent. Nothing auto-promotes. The promotion
  loop is `SessionEnd` → judgment → `record_observation` → `skill-promote`.

`skill-promote` creates a skill, but it does **not** make it injectable. Use
`skills` to find its id, then `skill-approve <id> --actor human:you`. Only an
active, non-atrophied, human-approved skill gets a bounded signpost in
`ledger.inject`; `skill-revoke` removes that sign-off immediately. There is no
MCP tool for this — an agent cannot approve its own procedural memory.

`--lower-trust` never bypasses freshness or `allowed_roots`: it only shows
machine-confirmed/unverified concepts, always labelled as such. A `human:`
actor with a current sign-off is the only thing that makes a stable,
non-stale fact auto-injectable.

`nexus memory bundles` is the multi-repo routing surface required by
[[ADRs/0020-export-facts-as-okf|ADR 0020]]: it reads only the `index.md`
frontmatter of each bundle under `--root`, reports the one-line `description`
and fact-file count, and marks the active repo. It never reads fact bodies or
loads another repo's memory. An agent can therefore select a bundle before it
pays for any facts; injection stays scoped to that repo under the same hard cap.

## Auto-configuring an MCP agent

```bash
nexus install                 # dry run: prints the plan, writes nothing
nexus install --yes           # apply (Claude Code / Claude Desktop)
nexus install opencode --yes  # project-local plugin + MCP + minimal config
nexus install opencode --uninstall --yes  # restore previous config exactly
```

OpenCode V2 gets `.opencode/plugins/nexuscontext/` and one
`mcp.servers.nexuscontext` entry in `opencode.json`; the install manifest
records exactly what was created so uninstall can undo the merge without
deleting a pre-existing config. Without `--yes` nothing on disk changes.
For Claude, detect Claude Code (via its own `claude mcp add` CLI) and Claude
Desktop (merges into `claude_desktop_config.json` without touching anything
else already there). It also merges a project-local `SessionEnd` hook into
`.claude/settings.local.json` plus a `.claude/hooks/nexus-session-end.sh`
wrapper. The hook only stages explicitly signposted lines (`DECISION:`,
`LESSON:`, `DEAD END:`, `INCIDENT:`, `OWNER:`, `remember:`) into Myelin's
pending-review queue — no LLM, no raw transcript storage, no auto-promotion;
an agent/human must still turn a reviewed excerpt into an observation.
Resolves Claude Desktop's config path correctly on all three platforms
(`~/.config`, `~/Library/Application Support`, `%APPDATA%` via
`directories::BaseDirs`). Prints a generic `mcpServers` snippet for anything
else, rather than guessing at a config format it can't verify.

The CLI itself runs identically everywhere - Linux, macOS, and Windows,
x86_64 and arm64 - since none of it depends on `nexusd serve`. See
[[Known-Limitations]] for what's platform-tiered (the daemon/control API,
not the CLI or MCP tools).

## Related

[[MCP-Tools]] · [[Watcher-and-Freshness]] · [[Storage-and-Data-Model]]
