# 0020. Export curated facts as OKF, not a bespoke NC format

Status: Accepted
Date: 2026-11-24

## Context

ADR 0019 made context economy the flagship and committed NC to shipping declarative
code-anchored facts beside the absorbed procedural memory. The proposal already said
"SQLite canonical, portable Markdown export" and even used the phrase "OKF-style",
but never pinned what the exchange format actually was. That left P2's export/import
task free to invent a private schema — which would need its own docs, validator, and
trust vocabulary, and would be unreadable by anything else.

The scale case that matters: one developer works across many repositories. A single
repo's SQLite DB is the wrong unit for that; what is needed is a portable, diffable,
human-readable artifact per repo that an agent can *select* before it reads anything.
Obsidian vaults already give humans that shape. **OKF v0.3** (published 2026-11-18,
`open-knowledge-format/open-knowledge-format`) formalises it: a bundle is a directory
of Markdown with YAML frontmatter, `type` is the only required field, and provenance
(`sources`), trust (`generated` / `verified`), lifecycle (`status`, `superseded_by`,
`stale_after`) and producer extension keys (`x-…`) are first-class. It has no service,
no schema registry, and no runtime — which matches the local-first premise exactly.

## Decision

SQLite stays the canonical store. The git-shareable export at `repo/.nexus/memory/`
is an **OKF v0.3 bundle**, not an NC-specific format.

- One bundle per repo. The bundle-root `index.md` carries `okf_version: "0.3"`,
  `title`, `description`, `license` and `x-nexus-repo`. The `description` is the
  routing key: an agent holding many bundles reads those one-line descriptions to
  pick one, instead of loading everything (OKF §8.1, progressive disclosure).
- One concept per curated fact, with `type` in { `Decision`, `Incident`, `Lesson`,
  `Dead End`, `Owner` }. `sources[].resource` points at the anchored file/symbol;
  `generated` records the producing agent/CLI; `verified` records `human:<id>` only
  after a real review; `status`, `superseded_by` and `stale_after` mirror the watcher's
  file-hash truth. Anchor and ledger references use reserved `x-nexus-*` keys.
- The import gate is NC's, not the format's: auto-injection requires a **current**
  human verification (`verified.at >= generated.at`, per OKF §5.3), `status: stable`,
  a parseable non-stale `stale_after`, and anchors under `allowed_roots`. Unknown
  types, unknown keys and broken links are tolerated as OKF requires, but tolerated
  is not trusted. Machine-confirmed concepts stay visible and may be pulled
  deliberately via `recall`; they are not auto-injected.
- Procedural `SKILL.md` files stay in the same directory tree. They carry no OKF
  `type`, so §3.2 classifies them as non-concepts and consumers skip them. Skills
  travel across repos; code-anchored facts do not.
- Emitted links are standard `[text](target.md)` links. `[[wikilinks]]` are not OKF
  links (§6.1); Obsidian renders standard links fine, so portability costs nothing.
- Pin `okf_version: "0.3"`. v0.3 is pre-1.0 and minor versions may break (§12), so
  the reader keeps a best-effort fallback and never refuses a bundle it can read.

## Alternatives considered

- **Bespoke Markdown/JSON schema:** maximum control, but reinvents provenance, trust
  and lifecycle fields and interoperates with nothing. The proposal's "OKF-style"
  wording would have drifted into a format nobody had specified.
- **Flat JSON only:** diffable, but not naturally readable in Obsidian or on GitHub,
  and loses the index/description progressive-disclosure path that keeps a
  many-repo agent under budget.
- **Wikilinks as the contract:** familiar to Obsidian users, but explicitly not links
  under OKF and tied to one viewer.
- **Hosted memory / team sync:** rejected already by locked decision 2 in the
  proposal; OKF's own rationale is "a format, not another service".

## Consequences

P2's export/import becomes an OKF producer *and* consumer, so it needs a YAML 1.2
core-schema parser/writer and conformance tests against OKF §11 (absent/empty values,
single-item lists, unparseable `stale_after` never fresh). That is a new Rust
dependency decision, not a new runtime.

The trust rules are stricter than OKF itself: OKF trust tiers are advisory and never
access control, while NC's injection gate treats them as an admission decision. A
regenerated concept must not carry forward an old human `verified` stamp — rewriting
content makes verification stale by definition. That is easy to get wrong in a
round-trip exporter and needs a test.

`repo/.nexus/memory/` becomes the unit two machines share through git with no
account, and the bundle-root description is how a multi-project session chooses which
repo's facts to inject. `nexus memory bundles` implements that read-only selection
step: it reads only `index.md` metadata and marks the active repo, never fact bodies.
Attested computations (OKF §10) are deliberately deferred:
no NC claim may be shown as attested until a `runtime` and a deterministic `attester`
exist, so benchmark caveats remain prose for now.
