// NexusContext "reads avoided" tracker for opencode (issue #11).
//
// What this is: an opencode plugin that hooks `tool.execute.after` and
// keeps a live, per-session tally of NexusContext's own "reads avoided"
// counterfactual (see `get_session_usage` in nexusd's `tools.rs`, and
// README.md's Phase 31), writing it to disk after every qualifying call.
//
// What this is NOT: a status-bar widget. As of this writing, opencode has
// no plugin hook for contributing to its TUI status line/status bar - see
// https://github.com/anomalyco/opencode/issues/23539,
// https://github.com/anomalyco/opencode/issues/30295, and
// https://github.com/anomalyco/opencode/issues/8619, all open feature
// requests for exactly that. `tool.execute.after` can observe and persist,
// it cannot render. So this plugin does the honest, buildable half of #11
// - live *data* - and leaves *display* to whatever the user already uses
// for a terminal status line (tmux status-right, ocstatusline, a shell
// prompt segment, etc), pointed at the output file below. If/when opencode
// ships a real status-line hook, wiring this data into it directly is a
// small follow-up, not a rewrite.
//
// Where the numbers come from: every NexusContext MCP tool call already
// flows through this hook as `output.output` - the exact same JSON text
// nexusd itself returned. Rather than re-invoke a tool from inside a hook
// (unconfirmed to be safe - it could show up as a nested call the model
// sees), this plugin re-derives the same "reads avoided" tally locally
// from that same observed text, using the *same* conservative allow-list
// nexusd's own `get_session_usage` uses server-side (see
// `READS_AVOIDED_TOOLS` in `crates/nexusd/src/tools.rs` - keep this list
// in sync with that one if it ever changes). Both are computed from
// literally the same tool responses, so they can't disagree in practice
// even though the arithmetic runs twice; call NexusContext's own
// `get_session_usage` tool at any point for the authoritative,
// server-computed version of the same numbers (it also has `schema_tax`,
// which a passive observer here has no way to measure).

import { homedir } from "node:os"
import { join } from "node:path"
import { mkdirSync, writeFileSync } from "node:fs"

// Keep in sync with READS_AVOIDED_TOOLS in crates/nexusd/src/tools.rs.
const READS_AVOIDED_TOOLS = new Set([
  "get_file_context",
  "trace_call_path",
  "search_graph",
  "get_architecture",
  "detect_changes",
  "query_planner",
])

const OUTPUT_DIR = join(homedir(), ".cache", "nexuscontext")
const JSON_PATH = join(OUTPUT_DIR, "opencode-session-usage.json")
const TEXT_PATH = join(OUTPUT_DIR, "opencode-session-usage.txt")

function estimateTokens(bytes) {
  // Same rough bytes/4 heuristic as nexusd's own estimate_tokens - not a
  // real tokenizer, just a ballpark, kept consistent with the number this
  // mirrors.
  return Math.floor(bytes / 4)
}

export const NexusContextStatuslinePlugin = async ({ directory }) => {
  const state = {
    sessionStartedUnix: Math.floor(Date.now() / 1000),
    readsAvoidedCount: 0,
    bytesAvoided: 0,
    totalCalls: 0,
    totalOutputBytes: 0,
  }

  try {
    mkdirSync(OUTPUT_DIR, { recursive: true })
  } catch {
    // Best-effort, same as nexusd's own directory-hardening calls - a
    // write failure here shouldn't break the agent's actual tool call.
  }

  function persist() {
    const summary = {
      session_started_unix: state.sessionStartedUnix,
      directory,
      total_calls: state.totalCalls,
      total_output_bytes: state.totalOutputBytes,
      reads_avoided: {
        count: state.readsAvoidedCount,
        bytes: state.bytesAvoided,
        estimated_tokens: estimateTokens(state.bytesAvoided),
      },
      note:
        "Locally observed from this opencode session's own NexusContext tool calls - " +
        "mirrors nexusd's get_session_usage server-side computation, doesn't replace it. " +
        "See plugin.js's header comment for why opencode has no native status-line slot " +
        "to render this in yet.",
      updated_unix: Math.floor(Date.now() / 1000),
    }

    try {
      writeFileSync(JSON_PATH, JSON.stringify(summary, null, 2))
      writeFileSync(
        TEXT_PATH,
        `NexusContext: ${state.readsAvoidedCount} reads avoided (~${(state.bytesAvoided / 1024).toFixed(1)}KB, ~${estimateTokens(state.bytesAvoided)} tokens est.)\n`,
      )
    } catch {
      // Best-effort - see mkdirSync above.
    }
  }

  return {
    "tool.execute.after": async (input, output) => {
      if (!READS_AVOIDED_TOOLS.has(input.tool)) {
        return
      }
      state.totalCalls += 1
      const bytes = output.output ? Buffer.byteLength(output.output, "utf8") : 0
      state.totalOutputBytes += bytes
      state.readsAvoidedCount += 1
      state.bytesAvoided += bytes
      persist()
    },
  }
}

export default NexusContextStatuslinePlugin
