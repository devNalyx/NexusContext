# NexusContext session-usage tracker for opencode

Closes the buildable half of [issue #11](https://github.com/devNalyx/NexusContext/issues/11)
("live 'tokens/reads avoided' status-line integration"). An opencode plugin
that watches your session's own NexusContext MCP tool calls and keeps a
live "reads avoided" tally on disk, mirroring the same counterfactual
`get_session_usage` (nexusd's own MCP tool, see the main README's Phase 31)
reports server-side.

## The honest gap

**This is not a rendered status line.** As of this writing opencode has no
plugin hook for contributing to its TUI status bar - `tool.execute.after`
(what this plugin uses) can observe and persist, but there's nowhere in
opencode itself to display the result. See the open feature requests:
[#23539](https://github.com/anomalyco/opencode/issues/23539),
[#30295](https://github.com/anomalyco/opencode/issues/30295),
[#8619](https://github.com/anomalyco/opencode/issues/8619).

So this plugin does the real, working part - a live data feed - and writes
it to:

- `~/.cache/nexuscontext/opencode-session-usage.json` - full structured summary
- `~/.cache/nexuscontext/opencode-session-usage.txt` - one line, ready to
  drop into any shell-command-driven status line

Pointing an actual status line at that file is on you until opencode ships
a real hook for it. Two options that exist today:

**tmux**, in `.tmux.conf`:
```
set -g status-right '#(cat ~/.cache/nexuscontext/opencode-session-usage.txt)'
```

**[ocstatusline](https://github.com/amirlehmam/ocstatusline)** (a separate
terminal-pane status line, the opencode counterpart to Claude Code's
`ccstatusline`) - add a custom segment that shells out to
`cat ~/.cache/nexuscontext/opencode-session-usage.txt` per its own config
format.

If opencode ships a native status-line hook later, wiring this plugin's
already-computed numbers into it directly is a small follow-up to this
file, not a rewrite.

## Install

Copy (or symlink) `plugin.js` into your project's `.opencode/plugin/`
directory (or opencode's global plugin directory - see opencode's own
[plugin docs](https://opencode.ai/docs/plugins/) for the current
resolution rules, since this changes between versions):

```
mkdir -p .opencode/plugin
cp plugin.js .opencode/plugin/nexuscontext-statusline.js
```

No configuration needed - it activates for any opencode session where
NexusContext's MCP tools are also configured, and does nothing (not even
create the output directory unless a qualifying tool fires) otherwise.

## What counts as "avoided"

Same conservative, explicit allow-list as `get_session_usage`'s
`reads_avoided` field: `get_file_context`, `trace_call_path`,
`search_graph`, `get_architecture`, `detect_changes`, `query_planner`.
Raw scans (`search_code`/`search_codebase`/`query_memory`),
`detect_dead_code` (documented high false-positive rate), and admin/meta
tools are excluded - see `READS_AVOIDED_TOOLS` in
`crates/nexusd/src/tools.rs` for the full reasoning. Keep `plugin.js`'s
copy of that list in sync if the server-side one ever changes; call
NexusContext's own `get_session_usage` tool directly at any point for the
authoritative, server-computed numbers (it also reports `schema_tax`,
which this plugin has no way to measure from the outside).

## Units

Bytes and call counts are measured facts about what NexusContext actually
returned. `estimated_tokens` is a bytes/4 approximation, not a real
tokenizer count, and none of this is a token or dollar claim about what
reading the same files by hand would have cost - see the note field in
either output file, and `get_session_usage`'s own note in the main repo.
