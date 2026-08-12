# Embeddings and Semantic Search

Optional layer. The daemon is fully useful without it — every structural
tool ([[MCP-Tools]]) works with zero embeddings configured. This exists
specifically for the case where a codebase is too large for an agent to
just read everything, and a narrowing step before reasoning pays for
itself.

## How it's wired

- Speaks the **OpenAI-compatible `/v1/embeddings` API** — the de facto
  standard Ollama, LM Studio, vLLM, and llama.cpp server all implement —
  over a plain configurable HTTP endpoint. "Ollama on this machine" and
  "Ollama on another box on the network" are the same code path.
- One embedding row per `Function`/`Type` node (reusing tree-sitter's
  already-computed line boundaries, not independently rechunking file
  content) and per markdown `Section`, stored as plain BLOBs in the same
  `graph.db` — no dedicated vector database. Ranked by brute-force cosine
  similarity at query time, appropriate at this project's actual scale
  (thousands of chunks per project, not millions).
- Requests are batched (32 chunks/call) with a streaming on-batch callback,
  so a failure partway through a large project keeps whatever succeeded
  before that rather than losing everything.
- See [[Indexing-Pipeline]] for how unchanged chunks are reused across
  reindexes instead of re-embedded.

## The policy gate: four explicit states, not a boolean

`EmbeddingsConfig` separates "is an endpoint/model filled in" from "is the
feature actually turned on" from "is this endpoint even reachable from
here" — every caller gets a specific, actionable reason, not a generic
error:

- **`NotConfigured`** — endpoint or model isn't filled in. Nothing to turn
  on.
- **`Disabled`** — endpoint and model are filled in, but
  `embeddings.enabled` is `false`. Filling in an endpoint to try it out
  never silently starts sending code to it.
- **`RemoteBlocked`** — configured and enabled, but the endpoint isn't
  loopback/private, and `allow_remote` isn't set. Refuses to send code off
  the local network unless explicitly told to. See [[Security-Model]].
- **`Allowed`** — actually usable.

Every non-`Allowed` state's error message points the calling agent back at
the structural tools that work regardless — `search_graph`,
`trace_call_path`, `get_architecture`, `search_code`, `query_planner`.

## Configuration

```toml
[embeddings]
enabled = false   # explicit feature switch - independent of endpoint/model
endpoint = "http://localhost:11434/v1"   # OpenAI-compatible
model = "nomic-embed-text"
allow_remote = false   # must be true to use a non-loopback/private endpoint
```

All four fields are also editable from the GUI's Config tab, including a
"Test Connection" button that embeds a short probe string and reports back
model/dimension/latency. From the CLI: `nexus test-embeddings` (global
check, no `--project`) and `nexus search-codebase <query> --project <path>`.

## What it backs

`search_codebase` (semantic search) and `query_memory` (currently the same
ranked search — richer RAG-style retrieval, e.g. pulling full surrounding
context per hit, is a future enhancement, not built). Both are gated behind
the `full` tools preset by default — see [[MCP-Tools]].

## Related

[[Security-Model]] · [[MCP-Tools]] · [[Indexing-Pipeline]] ·
[[Configuration]]
