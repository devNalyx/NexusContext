# opencode-nexuscontext

OpenCode **V2** plug for NexusContext. One package with three parts:

- **Server** (`./`) — registers the injection/ledger hooks and the typed RPC
  surface over `nexusd`'s local control socket. It owns the daemon connection.
- **TUI** (`./tui`) — a `home.footer.status` ledger readout and a session panel
  with tabs over the RPC contract.
- **RPC** (`./rpc`) — the shared `Rpc.define` contract. Views import this, not
  the socket client, so they cannot invent new daemon calls.

There is no LLM and no approval logic in the adapter. The daemon performs
recall, renders the bounded injection, and records the ledger; the harness
reports provider tokens/cost. Estimates stay separate from provider numbers.
Source/fact approval is deliberately not part of this surface — registration
is CLI-only (`nexus source add`).

## Panels and commands

Open the panel with `/nexus` (or `/nc`). Arguments switch tabs or run an
explicit project action:

```text
/nexus                                  open the panel (status tab)
/nexus sources | projects | search | review | architecture | graph | stats | logs
/nexus path /abs/repo                   set the repo all tabs operate on
/nexus search <pattern>                 search indexed symbols
/nexus graph <function_name>            show a bounded DOT call neighborhood
/nexus reindex <path>                   reindex a project
/nexus import <path>                    import an exported project
/nexus export <path>                    export a project
/nexus remove <path> --confirm delete   delete a project (explicit confirmation required)
```

The panel is read-only except for those four project actions; `removeProject`
rejects a call without the literal `confirm: "delete"`, so an agent cannot
silently delete a project. The Desktop GUI panel (issue #107) will consume the
same RPC contract.

## Install

`nexus install opencode --yes` writes the plugin and MCP server config into the
**current project** (`.opencode/plugins/`, `opencode.json`). There is no
`--global` flag yet (tracked in #107); a cross-project install is currently a
manual copy into `~/.config/opencode/plugins/`. Start `nexusd serve` before
opening OpenCode — the plugin never starts the daemon itself.

## Tests

```bash
npm test
```

The offline smoke test stubs `@opencode/plugin` and a Unix socket, then asserts
hook ordering, one keyed instruction entry across prompt/context/compaction,
tool/usage telemetry, RPC registration and its confirmation guard, socket
resolution, and the pure panel formatters. No OpenCode install, no network.

## Notes

- `home.footer.status` polls the RPC `ledger` method every 5 s; a missing daemon
  shows `NC · offline` and never breaks a session.
- Project actions use a longer RPC deadline (5 min) because a cold reindex can
  legitimately take minutes; hook-time calls keep the short 2 s deadline.
- The separate `opencode-nexuscontext-statusline` plugin still writes a
  per-session "reads avoided" tally for tmux/ocstatusline; OpenCode has no
  native status-bar hook.
