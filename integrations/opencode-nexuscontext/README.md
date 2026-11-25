# opencode-nexuscontext (P0 scaffold)

OpenCode **V2** plug for NexusContext Brain. This is the P0 skeleton: it pins the
plugin shape, the hook registrations and a typed control-socket client, but its
hook handlers are intentionally empty — P3 wires injection, ledger and TUI.

## V2 API shape (verified against opencode.ai/docs on 2026-11-24)

- Plugins are loaded from `.opencode/plugins/` automatically, or via `plugins`
  in `opencode.json(c)`.
- One package can export server (`./`), TUI (`./tui`), GUI and RPC parts. This
  package currently exports only the server part.
- SDK is `@opencode/plugin` and the definition is `Plugin.define({ id, setup })`.
- Hooks are runtime registrations:
  - `ctx.session.hook("prompt" | "context" | "compaction" | …)`
  - `ctx.tool.hook("execute.before" | "execute.after")`
- Keyed instruction entries (`ctx.session.instructions.entry.put/list/remove`),
  `ctx.storage`, `ctx.mcp.transform` and TUI slots (`home.footer.status`,
  `session.panel`, …) are the surfaces P3 will use.

## Status

- [x] `Plugin.define` server part with id `nexuscontext.brain`.
- [x] Registers the five flagship hooks (empty handlers until P3).
- [x] NDJSON JSON-RPC control client matching `nexusd/src/control.rs`, with
      `NEXUS_CONTROL_SOCKET` → `XDG_RUNTIME_DIR` → data-dir fallback.
- [x] Offline smoke test: `npm test` (no OpenCode install, no network).
- [ ] TUI status slot, real injection, ledger reporting, installer — P3.

## Local loading

Copy/symlink this directory into a project's `.opencode/plugins/` (or add it to
`plugins` in `opencode.jsonc`). The plugin logs its resolved control socket into
plugin storage; it never starts `nexusd` itself. P3's `nexus install opencode`
will do MCP + plugin + config registration in one command.
