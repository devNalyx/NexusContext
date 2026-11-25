# AGENTS.md

Guidance for coding agents working in NexusContext. Keep this file short and
treat `README.md` and `docs/NexusContext-Wiki/` as the detailed sources.

## What this repo is

NexusContext is a local-first Rust MCP daemon that gives coding agents
structural code intelligence (tree-sitter graph + FTS5 + optional Rust LSP
enrichment) and a curated/procedural memory system. No LLM runs inside the
daemon.

## Repo map

- `crates/nexus-core/` - shared config, paths, registry, text/stats helpers.
- `crates/nexus-index/` - parsing, graph, ingest, queries, export.
- `crates/nexusd/` - daemon, MCP server, control socket, watcher, tool dispatch.
- `crates/nexus-cli/` - `nexus` CLI.
- `crates/nexus-gui/` - GTK4/libadwaita desktop app (Linux-first).
- `crates/nexus-memory/` - curated facts, admission gate, injection, ledger.
- `crates/myelin-*` - absorbed procedural memory (skills/observations/staging).
- `integrations/opencode-nexuscontext/` - OpenCode V2 plugin; has its own `npm test`.
- `docs/NexusContext-Wiki/` - current-state docs. Prefer updating these for
  behavior changes.
- `README.md` - product overview; **Section 4 is a frozen historical phase log**.
  Do not rewrite it as current behavior unless intentionally maintaining history.

## Common commands

Run from repo root:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
```

Plugin tests:

```bash
cd integrations/opencode-nexuscontext && npm test
```

Targeted checks while iterating:

```bash
cargo test -p nexus-memory --locked
cargo test -p nexusd --locked
cargo test -p nexus-cli --locked
```

Local install after Rust changes:

```bash
cargo install --path crates/nexus-cli --locked
cargo install --path crates/nexusd --locked
```

## Working conventions

- **Follow existing patterns** before adding abstractions. This codebase values
  explicit, auditable behavior over cleverness.
- **Prefer dedicated tools** for reads/edits/search; use shell for git, cargo,
  gh, and system commands.
- **Keep changes narrow**. Do not refactor unrelated code while fixing a bug.
- **Add or update tests** with behavior changes. Regression tests go near the
  code they protect.
- **Docs matter**. If you change CLI, MCP behavior, config, memory, or install
  flow, update the relevant wiki page and/or README section.
- **Use Conventional Commits** for commit messages, matching repo history.

## Safety / boundaries

- **No secrets in code, tests, logs, or docs**. Redaction is a product
  invariant; do not add raw transcript/secret storage.
- **Memory trust model is strict**: agent staging is not approval. Curated facts
  need human review; skill signposts need human `skill-approve`. Do not add
  paths that let agents approve their own memory.
- **No network-dependent product behavior** in daemon/index/MCP paths unless a
  change explicitly calls for it and documents the risk.
- **Do not delete user changes** you didn't make. Inspect unexpected dirty files
  before overwriting.
- **Avoid destructive git** (`reset --hard`, checkout discard) unless explicitly
  requested.

## Release touch points

When bumping versions, update all of these together:

- `Cargo.toml` workspace `version`
- `Cargo.lock` workspace crate versions
- `docs/index.html` version badges/text
- `docs/NexusContext-Wiki/Flagship-Implementation-Plan.md` release evidence/status

Release flow: PR/CI green -> merge to `main` -> tag `vX.Y.Z` -> watch release
workflow -> verify `gh release view vX.Y.Z` assets.

## Agentic workflow

For the recommended NexusContext-specific agent workflow (install, index, graph
tools, memory, verification), see
`docs/NexusContext-Wiki/Agentic-Development.md`.
