use anyhow::{anyhow, bail, Result};
use nexus_core::EmbeddingsConfig;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Chunks longer than this are truncated before being sent - a plain
/// character cap rather than a real tokenizer, since it only needs to keep
/// requests under whatever context limit the local model has (commonly
/// ~2K tokens for embedding models), not be exact. What's stored as
/// `chunk_text` afterward is this truncated text, not the original, so the
/// two always match.
const MAX_CHUNK_CHARS: usize = 6000;

/// How many chunks go into one HTTP request. A naive per-chunk loop against
/// a down endpoint fails slow, not gracefully - up to `timeout_secs` per
/// chunk, which across a few thousand functions is hours, not a skip.
/// Batching turns that into dozens of round trips instead of thousands.
const BATCH_SIZE: usize = 32;

#[derive(Serialize)]
struct EmbeddingsRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingDatum>,
}

#[derive(Deserialize)]
struct EmbeddingDatum {
    index: usize,
    embedding: Vec<f32>,
}

pub struct TestResult {
    pub model: String,
    pub dim: usize,
    pub latency_ms: u128,
}

fn truncate_chunk(text: &str) -> String {
    if text.len() <= MAX_CHUNK_CHARS {
        text.to_string()
    } else {
        text.chars().take(MAX_CHUNK_CHARS).collect()
    }
}

fn agent(cfg: &EmbeddingsConfig) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(cfg.timeout_secs))
        .build()
}

fn endpoint_url(cfg: &EmbeddingsConfig) -> Result<String> {
    let endpoint = cfg
        .endpoint
        .as_deref()
        .ok_or_else(|| anyhow!("no embeddings endpoint configured"))?;
    Ok(format!("{}/embeddings", endpoint.trim_end_matches('/')))
}

/// One `POST {endpoint}/embeddings` call for up to `BATCH_SIZE` chunks at
/// once, targeting the OpenAI-compatible request/response shape (works with
/// Ollama, LM Studio, vLLM, and real OpenAI-compatible providers alike -
/// the more portable choice over Ollama's own native `/api/embeddings`
/// format). Any transport failure, non-2xx status, or malformed response is
/// treated identically as "endpoint unreachable or misconfigured" - the
/// caller doesn't need to distinguish those to decide what to do next.
fn embed_one_batch(cfg: &EmbeddingsConfig, agent: &ureq::Agent, texts: &[String]) -> Result<Vec<Vec<f32>>> {
    let model = cfg
        .model
        .as_deref()
        .ok_or_else(|| anyhow!("no embeddings model configured"))?;
    let url = endpoint_url(cfg)?;

    let mut request = agent.post(&url).set("Content-Type", "application/json");
    if let Some(api_key) = cfg.api_key.as_deref().filter(|k| !k.is_empty()) {
        request = request.set("Authorization", &format!("Bearer {api_key}"));
    }

    let response = request
        .send_json(EmbeddingsRequest { model, input: texts })
        .map_err(|err| match err {
            ureq::Error::Status(code, resp) => {
                let body = resp.into_string().unwrap_or_default();
                anyhow!("embeddings endpoint returned HTTP {code}: {body}")
            }
            ureq::Error::Transport(t) => anyhow!("embeddings endpoint unreachable: {t}"),
        })?;

    let parsed: EmbeddingsResponse = response
        .into_json()
        .map_err(|err| anyhow!("embeddings endpoint returned an unexpected response shape: {err}"))?;

    if parsed.data.len() != texts.len() {
        bail!(
            "embeddings endpoint returned {} vectors for {} inputs",
            parsed.data.len(),
            texts.len()
        );
    }

    // Sort by the response's own `index` rather than trusting array order -
    // the wire contract carries it explicitly for exactly this reason.
    let mut data = parsed.data;
    data.sort_by_key(|d| d.index);
    Ok(data.into_iter().map(|d| d.embedding).collect())
}

/// Embeds a list of texts, batching internally. Returns one vector per
/// input text, in the same order. All-or-nothing - fine for the small,
/// single-shot uses (`test_connection`, embedding one search query), but
/// see `embed_in_batches` for bulk indexing, where losing everything
/// because one batch out of hundreds failed would throw away real progress.
pub fn embed_batch(cfg: &EmbeddingsConfig, texts: &[String]) -> Result<Vec<Vec<f32>>> {
    let agent = agent(cfg);
    let truncated: Vec<String> = texts.iter().map(|t| truncate_chunk(t)).collect();

    let mut out = Vec::with_capacity(truncated.len());
    for batch in truncated.chunks(BATCH_SIZE) {
        out.extend(embed_one_batch(cfg, &agent, batch)?);
    }
    Ok(out)
}

/// Bulk-embedding entry point for indexing. Calls `on_batch(offset,
/// vectors)` after each batch succeeds, so whatever was embedded before a
/// later failure stays persisted (via the caller's side effects in
/// `on_batch`) instead of being discarded - a down endpoint should cost you
/// "whatever was in flight when it went down," not "everything indexed
/// this run." Returns `Ok(())` if every batch succeeded, or the first
/// error encountered (by which point every prior batch's `on_batch` has
/// already fired) - this is also the circuit breaker: a persistent problem
/// (bad model name, endpoint down) will recur identically on every
/// subsequent batch, so this stops at the first failure rather than
/// retrying it for every remaining chunk.
pub fn embed_in_batches(
    cfg: &EmbeddingsConfig,
    texts: &[String],
    mut on_batch: impl FnMut(usize, Vec<Vec<f32>>),
) -> Result<()> {
    let agent = agent(cfg);
    let truncated: Vec<String> = texts.iter().map(|t| truncate_chunk(t)).collect();

    for (batch_index, batch) in truncated.chunks(BATCH_SIZE).enumerate() {
        let vectors = embed_one_batch(cfg, &agent, batch)?;
        on_batch(batch_index * BATCH_SIZE, vectors);
    }
    Ok(())
}

/// Embeds a short literal probe string and times it - backs the GUI's "Test
/// Connection" button and the CLI's `test-embeddings` command. Deliberately
/// separate from `embed_batch`: this is a global config check (is the
/// endpoint/model reachable at all), not scoped to any project's index.
pub fn test_connection(cfg: &EmbeddingsConfig) -> Result<TestResult> {
    let started = Instant::now();
    let vectors = embed_batch(cfg, &["NexusContext connectivity probe".to_string()])?;
    let latency_ms = started.elapsed().as_millis();
    let dim = vectors.first().map(Vec::len).unwrap_or(0);
    Ok(TestResult {
        model: cfg.model.clone().unwrap_or_default(),
        dim,
        latency_ms,
    })
}

/// Byte-exact round trip of what's stored in the `embeddings` table's BLOB
/// column - no `bytemuck`/serialization crate needed for a plain `Vec<f32>`.
pub fn vector_to_bytes(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn bytes_to_vector(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
        .collect()
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_bytes_round_trip() {
        let original = vec![0.1_f32, -2.5, 3.75, 0.0];
        let bytes = vector_to_bytes(&original);
        let restored = bytes_to_vector(&bytes);
        assert_eq!(original, restored);
    }

    #[test]
    fn cosine_similarity_identical_vectors_is_one() {
        let v = vec![1.0_f32, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors_is_zero() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![0.0_f32, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn truncate_chunk_caps_long_text() {
        let long = "x".repeat(MAX_CHUNK_CHARS + 500);
        assert_eq!(truncate_chunk(&long).len(), MAX_CHUNK_CHARS);
    }
}
