use crate::graph::GraphStore;
use crate::project::graph_db_path;
use crate::{CodeSearchHit, NodeRecord};
use anyhow::{bail, Result};
use nexus_core::{Config, EmbeddingsPolicy, Paths};
use std::collections::HashSet;
use std::path::Path;

/// Shared by every caller (MCP tools, CLI, control API) so the "no index
/// found" message and the open-vs-missing check stay in one place.
pub fn open_store(repo_path: &Path) -> Result<GraphStore> {
    let db_path = graph_db_path(repo_path);
    if !db_path.exists() {
        bail!(
            "no index found for {} - run index_project first",
            repo_path.display()
        );
    }
    GraphStore::open(&db_path)
}

pub struct ArchitectureSummary {
    pub total_nodes: i64,
    pub total_edges: i64,
    pub busiest_files: Vec<(String, i64)>,
    pub language_breakdown: Vec<(String, i64)>,
}

pub fn get_architecture(repo_path: &Path) -> Result<ArchitectureSummary> {
    let store = open_store(repo_path)?;
    let (total_nodes, total_edges) = store.stats()?;
    let busiest_files = store.busiest_files(10)?;
    let language_breakdown = store.file_extension_counts()?;
    Ok(ArchitectureSummary {
        total_nodes,
        total_edges,
        busiest_files,
        language_breakdown,
    })
}

pub fn detect_dead_code(repo_path: &Path) -> Result<Vec<NodeRecord>> {
    open_store(repo_path)?.dead_functions()
}

pub fn search_code(repo_path: &Path, query: &str, limit: u32) -> Result<Vec<CodeSearchHit>> {
    open_store(repo_path)?.search_code(query, limit)
}

pub struct SemanticHit {
    pub node: NodeRecord,
    pub score: f32,
    pub chunk_text: String,
}

/// Ranks every embedded chunk for the configured model against the query's
/// own embedding via cosine similarity - brute-force, no ANN index, which
/// is the right call at the scale this project actually operates at
/// (thousands of chunks per project, not millions). Assumes the caller has
/// already checked `embeddings_policy()` is `Allowed` - this only handles
/// what happens once that's true: a live HTTP failure, or a project that's
/// never been indexed with embeddings on for this particular model.
pub fn semantic_search(
    repo_path: &Path,
    embeddings_cfg: &nexus_core::EmbeddingsConfig,
    query: &str,
    limit: u32,
) -> Result<Vec<SemanticHit>> {
    let store = open_store(repo_path)?;
    let model = embeddings_cfg
        .model
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no embeddings model configured"))?;

    let candidates = store.embeddings_for_model(model)?;
    if candidates.is_empty() {
        bail!(
            "no embeddings found for model '{model}' in this project's index - reindex this \
             project after enabling embeddings to build them"
        );
    }

    let query_vector = crate::embeddings::embed_batch(embeddings_cfg, &[query.to_string()])?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("embeddings endpoint returned no vector for the query"))?;

    let mut scored: Vec<(i64, String, f32)> = candidates
        .into_iter()
        .map(|(node_id, chunk_text, embedding_bytes)| {
            let vector = crate::embeddings::bytes_to_vector(&embedding_bytes);
            let score = crate::embeddings::cosine_similarity(&query_vector, &vector);
            (node_id, chunk_text, score)
        })
        .collect();
    scored.sort_by(|a, b| b.2.total_cmp(&a.2));
    scored.truncate(limit as usize);

    let mut hits = Vec::with_capacity(scored.len());
    for (node_id, chunk_text, score) in scored {
        if let Some(node) = store.node_by_id(node_id)? {
            hits.push(SemanticHit {
                node,
                score,
                chunk_text,
            });
        }
    }
    Ok(hits)
}

pub fn detect_changes(repo_path: &Path) -> Result<Vec<NodeRecord>> {
    let store = open_store(repo_path)?;

    let output = std::process::Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "diff", "--unified=0"])
        .output()?;
    if !output.status.success() {
        bail!(
            "git diff failed - is {} a git repository?",
            repo_path.display()
        );
    }

    let diff_text = String::from_utf8_lossy(&output.stdout);
    let mut affected = Vec::new();
    for (file, ranges) in parse_diff_hunks(&diff_text) {
        for (start, end) in ranges {
            affected.extend(store.nodes_overlapping(&file, start, end)?);
        }
    }
    Ok(affected)
}

/// Minimal unified-diff hunk parser: pulls (file, [(start_line, end_line)])
/// out of `git diff --unified=0` output. Doesn't handle renames/binary
/// files specially - good enough for mapping changes to symbol ranges.
fn parse_diff_hunks(diff: &str) -> Vec<(String, Vec<(u32, u32)>)> {
    let mut result: Vec<(String, Vec<(u32, u32)>)> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut current_ranges: Vec<(u32, u32)> = Vec::new();

    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            if let Some(f) = current_file.take() {
                result.push((f, std::mem::take(&mut current_ranges)));
            }
            current_file = Some(path.to_string());
        } else if let Some(rest) = line.strip_prefix("@@ ") {
            // rest looks like: "-old_start,old_count +new_start,new_count @@ ..."
            if let Some(plus_part) = rest.split('+').nth(1) {
                let range_str = plus_part.split(' ').next().unwrap_or("");
                let mut parts = range_str.splitn(2, ',');
                if let Some(Ok(start)) = parts.next().map(|s| s.parse::<u32>()) {
                    let count: u32 = parts.next().and_then(|c| c.parse().ok()).unwrap_or(1);
                    let end = if count == 0 { start } else { start + count - 1 };
                    current_ranges.push((start, end));
                }
            }
        }
    }
    if let Some(f) = current_file.take() {
        result.push((f, current_ranges));
    }
    result
}

pub fn get_file_context(
    repo_path: &Path,
    file: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String> {
    let canonical_root = repo_path
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("repo_path does not exist: {}", repo_path.display()))?;
    let canonical_file = canonical_root
        .join(file)
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("file not found: {file}"))?;
    if !canonical_file.starts_with(&canonical_root) {
        bail!("file path escapes project root: {file}");
    }

    let content = std::fs::read_to_string(&canonical_file)?;
    match (start_line, end_line) {
        (Some(s), Some(e)) => {
            let lines: Vec<&str> = content.lines().collect();
            let s = s.saturating_sub(1).min(lines.len());
            let e = e.min(lines.len());
            Ok(lines[s..e].join("\n"))
        }
        _ => Ok(content),
    }
}

pub struct QueryPlanResult {
    pub strategy: &'static str,
    pub note: Option<&'static str>,
    pub embeddings_policy: Option<EmbeddingsPolicy>,
    pub file_content: Option<String>,
    pub records: Vec<NodeRecord>,
}

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "of", "to", "in", "for", "and", "or", "find", "get", "where",
    "how", "what", "does", "do",
];

/// Rule-based dispatcher, not an LLM-backed one - there's no embedded
/// reasoning model here (the calling agent is the intelligence layer). This
/// just picks the cheapest of the strategies that already exist instead of
/// making the caller guess: a named file wins outright, a single
/// identifier-like token goes straight to the graph, and anything more
/// descriptive falls back to a naive per-word graph search. Real semantic
/// search now exists (`semantic_search`, backing the `search_codebase`/
/// `query_memory` tools directly) but this planner doesn't route to it yet -
/// routing a query here vs. calling those tools directly is a distinct,
/// not-yet-made decision, not an oversight to paper over with a stale claim.
pub fn plan_query(
    repo_path: &Path,
    query: &str,
    file: Option<&str>,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<QueryPlanResult> {
    if let Some(file) = file {
        let text = get_file_context(repo_path, file, start_line, end_line)?;
        return Ok(QueryPlanResult {
            strategy: "file_read",
            note: None,
            embeddings_policy: None,
            file_content: Some(text),
            records: vec![],
        });
    }

    let is_identifier = !query.trim().is_empty()
        && query
            .chars()
            .next()
            .map(|c| c.is_alphabetic() || c == '_')
            .unwrap_or(false)
        && query.chars().all(|c| c.is_alphanumeric() || c == '_');

    if is_identifier {
        let store = open_store(repo_path)?;
        let results = store.search_by_name(query, 20)?;
        return Ok(QueryPlanResult {
            strategy: "graph_search",
            note: None,
            embeddings_policy: None,
            file_content: None,
            records: results,
        });
    }

    let config = Config::load(&Paths::resolve().config_file())?;
    let store = open_store(repo_path)?;

    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for word in query.split_whitespace() {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if word.len() < 3 || STOPWORDS.contains(&word.to_lowercase().as_str()) {
            continue;
        }
        for record in store.search_by_name(word, 10)? {
            if seen.insert(record.qualified_name.clone()) {
                merged.push(record);
            }
        }
    }

    let policy = config.embeddings_policy();
    let note = match policy {
        EmbeddingsPolicy::NotConfigured => {
            "no embeddings endpoint configured - falling back to keyword search over the graph"
        }
        EmbeddingsPolicy::Disabled => {
            "an embeddings endpoint and model are configured but embeddings.enabled is false - \
             falling back to keyword search over the graph"
        }
        EmbeddingsPolicy::RemoteBlocked => {
            "an embeddings endpoint is configured but blocked (remote host, allow_remote not \
             set) - falling back to keyword search over the graph"
        }
        EmbeddingsPolicy::Allowed => {
            "an embeddings endpoint is configured and allowed - query_planner doesn't route \
             descriptive queries to it yet (call search_codebase/query_memory directly for \
             semantic search) - falling back to keyword search over the graph"
        }
    };

    Ok(QueryPlanResult {
        strategy: "keyword_fallback_graph_search",
        note: Some(note),
        embeddings_policy: Some(policy),
        file_content: None,
        records: merged,
    })
}
