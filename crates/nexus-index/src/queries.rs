use crate::graph::{Direction, GraphStore};
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

/// Renders a function's call neighborhood as a Graphviz DOT string - reuses
/// `trace_calls` (the same BFS `trace_call_path` already runs) for the node
/// set, so the visualization is bounded by the same `depth` limit rather
/// than ever attempting a whole-project graph (which turns into an
/// unreadable hairball past a few hundred nodes on any real project).
pub fn call_graph_dot(
    repo_path: &Path,
    function_name: &str,
    direction: Direction,
    depth: u32,
) -> Result<String> {
    let store = open_store(repo_path)?;
    // trace_calls only returns *discovered neighbors*, not the starting
    // function itself (correct for its own established use backing
    // trace_call_path, where the caller already knows the name they asked
    // about) - but a graph render needs the anchor node drawn too, or the
    // function the user actually searched for would be invisible in its
    // own neighborhood diagram.
    let start_nodes: Vec<NodeRecord> = store
        .search_by_name(function_name, 50)?
        .into_iter()
        .filter(|n| n.name == function_name && n.kind == crate::graph::NodeKind::Function)
        .collect();
    if start_nodes.is_empty() {
        // Without this check, "no such function" silently produced a valid
        // but empty DOT graph - Graphviz renders that as an 11x11 all-white
        // PNG (verified directly), which a GUI Picture widget then stretches
        // to fill its container: a confusing blank image instead of a clear
        // "not found" - exactly what surfaced when a user tried a function
        // name that didn't actually exist in their project.
        bail!(
            "no function named '{function_name}' found in this project - check the exact name \
             with search_graph first"
        );
    }
    let neighbors = store.trace_calls(function_name, direction, depth)?;

    let mut nodes = start_nodes;
    nodes.extend(neighbors);
    let ids: Vec<i64> = nodes.iter().map(|n| n.id).collect();
    let edges = store.subgraph_edges(&ids, "CALLS")?;
    let by_id: std::collections::HashMap<i64, &NodeRecord> =
        nodes.iter().map(|n| (n.id, n)).collect();

    let mut dot = String::from("digraph G {\n  rankdir=LR;\n  node [shape=box, style=\"rounded,filled\", fontname=\"sans-serif\", fillcolor=\"#eef1f8\"];\n");
    for node in &nodes {
        // Escape each piece before composing the label - escaping the
        // already-composed string (with its literal `\n` line-break
        // sequence already in place) would double-escape that backslash.
        let label = format!(
            "{}\\n{}:{}",
            dot_escape(&node.name),
            dot_escape(&node.file_path),
            node.start_line
        );
        let is_root = node.name == function_name;
        let fill = if is_root { "#ffd166" } else { "#eef1f8" };
        dot.push_str(&format!(
            "  \"{}\" [label=\"{}\", fillcolor=\"{}\"];\n",
            dot_escape(&node.qualified_name),
            label,
            fill
        ));
    }
    for (src, dst) in &edges {
        if let (Some(a), Some(b)) = (by_id.get(src), by_id.get(dst)) {
            dot.push_str(&format!(
                "  \"{}\" -> \"{}\";\n",
                dot_escape(&a.qualified_name),
                dot_escape(&b.qualified_name)
            ));
        }
    }
    dot.push_str("}\n");
    Ok(dot)
}

fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
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

/// Default window size when no explicit range is given and `full` isn't set -
/// keeps a plain "read this file" call from returning an unbounded response
/// on a large file. See change_proposal.md.
const DEFAULT_CONTEXT_LINES: usize = 300;

pub fn get_file_context(
    repo_path: &Path,
    file: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
    full: bool,
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
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    match (start_line, end_line) {
        // Both bounds given: an explicit two-sided ask, stays unbounded.
        (Some(s), Some(e)) => {
            let s = s.saturating_sub(1).min(total);
            let e = e.min(total);
            Ok(lines[s..e].join("\n"))
        }
        // full=true is the explicit escape hatch for the whole file.
        _ if full => Ok(content),
        // Only one bound given: today this silently returned the whole
        // file - a bounded window anchored at the given bound instead.
        (Some(s), None) => {
            let s = s.saturating_sub(1).min(total);
            let e = (s + DEFAULT_CONTEXT_LINES).min(total);
            Ok(lines[s..e].join("\n"))
        }
        (None, Some(e)) => {
            let e = e.min(total);
            let s = e.saturating_sub(DEFAULT_CONTEXT_LINES);
            Ok(lines[s..e].join("\n"))
        }
        // Neither bound given, not full: first DEFAULT_CONTEXT_LINES lines,
        // with a trailing note if there's more.
        (None, None) => {
            let e = DEFAULT_CONTEXT_LINES.min(total);
            let shown = lines[..e].join("\n");
            if total > e {
                Ok(format!(
                    "{shown}\n\n--- truncated: showing lines 1-{e} of {total} total. Pass end_line or full=true for the rest. ---"
                ))
            } else {
                Ok(shown)
            }
        }
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
        let text = get_file_context(repo_path, file, start_line, end_line, false)?;
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

#[cfg(test)]
mod get_file_context_tests {
    use super::{get_file_context, DEFAULT_CONTEXT_LINES};
    use std::fs;

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_get_file_context_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn numbered_lines(n: usize) -> String {
        (1..=n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn small_file_with_no_range_is_returned_whole_and_unmarked() {
        let dir = temp_project("small");
        fs::write(dir.join("f.txt"), numbered_lines(10)).unwrap();
        let result = get_file_context(&dir, "f.txt", None, None, false).unwrap();
        assert_eq!(result, numbered_lines(10));
        assert!(!result.contains("truncated"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn large_file_with_no_range_is_truncated_with_a_note() {
        let dir = temp_project("large");
        let total = DEFAULT_CONTEXT_LINES + 50;
        fs::write(dir.join("f.txt"), numbered_lines(total)).unwrap();
        let result = get_file_context(&dir, "f.txt", None, None, false).unwrap();
        assert!(result.contains("line 1\n"));
        assert!(result.contains(&format!("line {DEFAULT_CONTEXT_LINES}")));
        assert!(!result.contains(&format!("line {}", DEFAULT_CONTEXT_LINES + 1)));
        assert!(result.contains(&format!(
            "truncated: showing lines 1-{DEFAULT_CONTEXT_LINES} of {total} total"
        )));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lone_start_line_returns_a_bounded_window_not_the_whole_file() {
        let dir = temp_project("lone_start");
        let total = DEFAULT_CONTEXT_LINES + 50;
        fs::write(dir.join("f.txt"), numbered_lines(total)).unwrap();
        let result = get_file_context(&dir, "f.txt", Some(10), None, false).unwrap();
        assert!(result.starts_with("line 10\n"));
        assert!(!result.contains(&format!("line {total}")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lone_end_line_returns_a_bounded_window_not_the_whole_file() {
        let dir = temp_project("lone_end");
        let total = DEFAULT_CONTEXT_LINES + 50;
        fs::write(dir.join("f.txt"), numbered_lines(total)).unwrap();
        let result = get_file_context(&dir, "f.txt", None, Some(total), false).unwrap();
        assert!(result.ends_with(&format!("line {total}")));
        assert!(!result.contains("line 1\n"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn both_bounds_set_stays_unbounded_and_unmarked() {
        let dir = temp_project("both_bounds");
        let total = DEFAULT_CONTEXT_LINES + 50;
        fs::write(dir.join("f.txt"), numbered_lines(total)).unwrap();
        let result = get_file_context(&dir, "f.txt", Some(1), Some(total), false).unwrap();
        assert_eq!(result, numbered_lines(total));
        assert!(!result.contains("truncated"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_true_bypasses_truncation_regardless_of_size() {
        let dir = temp_project("full_true");
        let total = DEFAULT_CONTEXT_LINES + 50;
        fs::write(dir.join("f.txt"), numbered_lines(total)).unwrap();
        let result = get_file_context(&dir, "f.txt", None, None, true).unwrap();
        assert_eq!(result, numbered_lines(total));
        let _ = fs::remove_dir_all(&dir);
    }
}
