# Contributing to NexusContext

Thanks for contributing. NexusContext is a local-first, security-sensitive,
graph/MCP codebase; small, well-tested changes are preferred over broad
refactors.

## Before you start

- Read [`AGENTS.md`](AGENTS.md) for the fast agent/contributor contract.
- Read [`README.md`](README.md) for product overview. Note: README section 4 is
  a frozen historical phase log.
- Read [`docs/NexusContext-Wiki/Home.md`](docs/NexusContext-Wiki/Home.md) for
  current-state docs by topic.

## Development setup

Prereqs:

- Rust stable toolchain (`cargo`, `rustfmt`, `clippy`)
- Node.js for `integrations/opencode-nexuscontext` tests
- Linux GUI builds also need GTK4/libadwaita dev headers (see
  [`INSTALL.md`](INSTALL.md))

Build:

```bash
cargo build --workspace
```

## Checks

Run before opening/merging a PR:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
cd integrations/opencode-nexuscontext && npm test
```

CI also runs Linux/macOS/Windows test jobs; Windows excludes `nexus-gui`.

## Code and docs expectations

- Add regression tests for bug fixes.
- Update docs when behavior changes: CLI, MCP tools, config, install flow,
  memory/trust model, or security boundaries.
- Keep README section 4 historical; put current behavior in sections 1-3/5-6 or
  the wiki.
- Use Conventional Commits (`feat:`, `fix:`, `docs:`, `chore(release):`, etc.).

## Pull requests

- Keep PRs focused and reviewable.
- Include what changed, why, how it was verified, and any risk/rollout notes.
- Don’t include secrets, tokens, or private transcripts in issues/PRs.
- If you post external comments/reviews on behalf of a user, disclose that the
  review was performed by opencode on behalf of that user.

## Release checklist

Version bumps touch:

- `Cargo.toml`
- `Cargo.lock`
- `docs/index.html`
- `docs/NexusContext-Wiki/Flagship-Implementation-Plan.md`

Tagging `vX.Y.Z` triggers the release workflow. Verify assets with:

```bash
gh release view vX.Y.Z
```
