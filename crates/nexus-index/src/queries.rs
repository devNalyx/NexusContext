use crate::graph::{Direction, GraphStore};
use crate::project::graph_db_path;
use crate::{CodeSearchHit, NodeRecord};
use anyhow::{bail, Result};
use nexus_core::{truncate_to_byte_boundary, Paths};
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

/// Canonicalizes `repo_path` and checks it against `allowed_roots` *before*
/// any store is opened - shared by every query-side function that takes a
/// caller-supplied `repo_path` (`search_code`, `get_architecture`,
/// `detect_dead_code`, `call_graph_dot`), matching the pattern
/// `get_file_context`/`detect_changes` already established. Canonicalizing
/// *before* the allowed_roots check, not after, matters: `Path::starts_with`
/// (what `is_path_allowed` uses under the hood) is a component-wise prefix
/// check that does not resolve `..`, so a raw `"<allowed_root>/../../etc"`
/// would pass a check done on the uncanonicalized path and only reveal the
/// escape on a *later* canonicalize - see issue #29. Without this function
/// at all, these four query-side tools skipped the check entirely and went
/// straight to `open_store` on an uncanonicalized path, so any `repo_path`
/// on disk with a graph DB under it - not just a registered/allowed
/// project - was readable through them even when a user had opted into
/// `allowed_roots`. See issue #61.
fn canonicalize_and_authorize(repo_path: &Path) -> Result<std::path::PathBuf> {
    let canonical = repo_path
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("repo_path does not exist: {}", repo_path.display()))?;
    crate::project::require_path_allowed(&Paths::resolve(), &canonical)?;
    Ok(canonical)
}

pub struct ArchitectureSummary {
    pub total_nodes: i64,
    pub total_edges: i64,
    pub busiest_files: Vec<(String, i64)>,
    pub language_breakdown: Vec<(String, i64)>,
}

pub fn get_architecture(repo_path: &Path) -> Result<ArchitectureSummary> {
    let repo_path = canonicalize_and_authorize(repo_path)?;
    let store = open_store(&repo_path)?;
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
    let repo_path = canonicalize_and_authorize(repo_path)?;
    open_store(&repo_path)?.dead_functions()
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
    let repo_path = canonicalize_and_authorize(repo_path)?;
    let store = open_store(&repo_path)?;
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
    nodes.extend(neighbors.into_iter().map(|traced| traced.node));
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
    let repo_path = canonicalize_and_authorize(repo_path)?;
    open_store(&repo_path)?.search_code(query, limit)
}

pub fn detect_changes(repo_path: &Path) -> Result<Vec<NodeRecord>> {
    // Canonicalize *before* the allowed_roots check, not after - see the
    // matching comment in `get_file_context` below for why the ordering
    // itself is the bug (issue #29).
    let repo_path = repo_path
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("repo_path does not exist: {}", repo_path.display()))?;
    let repo_path = repo_path.as_path();
    crate::project::require_path_allowed(&Paths::resolve(), repo_path)?;
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
    // Canonicalize *before* the allowed_roots check, not after -
    // `Path::starts_with` (what `is_path_allowed` uses) is a component-wise
    // prefix check that does not resolve `..`, so a raw repo_path like
    // "<allowed_root>/../../etc" passed the check as written here, then
    // canonicalized to a root entirely outside allowed_roots afterward -
    // an MCP-agent-reachable bypass of a safety feature a user opted into.
    // See issue #29.
    let canonical_root = repo_path
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("repo_path does not exist: {}", repo_path.display()))?;
    crate::project::require_path_allowed(&Paths::resolve(), &canonical_root)?;
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
        // Both bounds given: an explicit two-sided ask - still passes
        // through the MAX_RETURNED_LINES ceiling below, though, since
        // start_line=1/end_line=999999 is otherwise indistinguishable from
        // full=true for how much it can return.
        (Some(s), Some(e)) => {
            let s = s.saturating_sub(1).min(total);
            let e = e.min(total);
            Ok(bounded_lines(&lines, s, e))
        }
        // full=true is the explicit escape hatch for the whole file - still
        // capped, just at MAX_RETURNED_LINES rather than the default
        // preview's DEFAULT_CONTEXT_LINES. A single indexed file dumping
        // tens of thousands of lines into the calling agent's context is an
        // expensive mistake even when explicitly asked for - see the
        // GitHub issue this fixes.
        _ if full => Ok(bounded_lines(&lines, 0, total)),
        // Only one bound given: today this silently returned the whole
        // file - a bounded window anchored at the given bound instead.
        (Some(s), None) => {
            let s = s.saturating_sub(1).min(total);
            let e = (s + DEFAULT_CONTEXT_LINES).min(total);
            Ok(bounded_lines(&lines, s, e))
        }
        (None, Some(e)) => {
            let e = e.min(total);
            let s = e.saturating_sub(DEFAULT_CONTEXT_LINES);
            Ok(bounded_lines(&lines, s, e))
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

/// Hard ceiling on any single `get_file_context` response, independent of
/// which branch above computed `[s, e)` - `full=true` and an explicit
/// two-sided range are the two paths that were previously unbounded (see
/// the GitHub issue this fixes); this applies to every branch uniformly so
/// a future one can't reintroduce the same gap. Well above
/// `DEFAULT_CONTEXT_LINES` so the existing bounded-window branches (which
/// only ever request up to `DEFAULT_CONTEXT_LINES` lines) are never
/// affected by this cap in practice.
const MAX_RETURNED_LINES: usize = 4000;

/// A second, independent ceiling from `MAX_RETURNED_LINES` - a *line count*
/// cap alone doesn't bound a response whose lines are individually huge (a
/// minified bundle, a generated one-line JSON blob: few lines, megabytes).
/// Caught in PR review on the fix that added `MAX_RETURNED_LINES` - see the
/// GitHub issue this fixes. ~300KB is generous for legitimate source but
/// still a real ceiling.
const MAX_RETURNED_BYTES: usize = 300_000;

fn bounded_lines(lines: &[&str], s: usize, e: usize) -> String {
    let line_capped_e = e.min(s + MAX_RETURNED_LINES);

    // A single line bigger than the whole byte budget can't be included
    // whole - truncate that one line's text directly (byte-safe, not a raw
    // slice that could land mid-codepoint) rather than either skip it
    // (useless response) or return it unbounded (the exact gap this cap
    // exists to close).
    if let Some(first) = lines.get(s) {
        if first.len() > MAX_RETURNED_BYTES {
            let (truncated_line, _) = truncate_to_byte_boundary(first, MAX_RETURNED_BYTES);
            return format!(
                "{truncated_line}\n\n--- truncated: line {} alone is {} bytes, over the server's {MAX_RETURNED_BYTES}-byte cap per call - showing a byte-truncated prefix of it. Narrow the range, or make another call starting from line {} for the rest. ---",
                s + 1,
                first.len(),
                s + 2
            );
        }
    }

    let mut included = s;
    let mut byte_total = 0usize;
    let mut byte_capped = false;
    for line in &lines[s..line_capped_e] {
        let add = line.len() + if included > s { 1 } else { 0 }; // +1 for the '\n' joiner
        if byte_total + add > MAX_RETURNED_BYTES {
            byte_capped = true;
            break;
        }
        byte_total += add;
        included += 1;
    }

    let shown = lines[s..included].join("\n");
    if included < e {
        let reason = if byte_capped {
            format!("server cap is {MAX_RETURNED_BYTES} bytes per call")
        } else {
            format!("server cap is {MAX_RETURNED_LINES} lines per call")
        };
        format!(
            "{shown}\n\n--- truncated: showing {} of {} requested lines ({reason}). Narrow the range, or make another call starting from line {} for the rest. ---",
            included - s,
            e - s,
            included + 1
        )
    } else {
        shown
    }
}

pub struct QueryPlanResult {
    pub strategy: &'static str,
    pub note: Option<&'static str>,
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
/// descriptive falls back to a naive per-word graph search over the graph.
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
            file_content: None,
            records: results,
        });
    }

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

    Ok(QueryPlanResult {
        strategy: "keyword_fallback_graph_search",
        note: None,
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
    fn both_bounds_set_under_the_cap_stays_unmarked() {
        let dir = temp_project("both_bounds");
        let total = DEFAULT_CONTEXT_LINES + 50;
        fs::write(dir.join("f.txt"), numbered_lines(total)).unwrap();
        let result = get_file_context(&dir, "f.txt", Some(1), Some(total), false).unwrap();
        assert_eq!(result, numbered_lines(total));
        assert!(!result.contains("truncated"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_true_under_the_cap_stays_unmarked() {
        let dir = temp_project("full_true");
        let total = DEFAULT_CONTEXT_LINES + 50;
        fs::write(dir.join("f.txt"), numbered_lines(total)).unwrap();
        let result = get_file_context(&dir, "f.txt", None, None, true).unwrap();
        assert_eq!(result, numbered_lines(total));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_true_beyond_the_cap_is_truncated_with_a_note() {
        let dir = temp_project("full_true_huge");
        let total = super::MAX_RETURNED_LINES + 500;
        fs::write(dir.join("f.txt"), numbered_lines(total)).unwrap();
        let result = get_file_context(&dir, "f.txt", None, None, true).unwrap();
        let (body, note) = result.split_once("\n\n--- truncated").unwrap();
        assert_eq!(body, numbered_lines(super::MAX_RETURNED_LINES));
        assert!(note.contains(&format!(
            "server cap is {} lines",
            super::MAX_RETURNED_LINES
        )));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn both_bounds_set_beyond_the_cap_is_truncated_with_a_note() {
        let dir = temp_project("both_bounds_huge");
        let total = super::MAX_RETURNED_LINES + 500;
        fs::write(dir.join("f.txt"), numbered_lines(total)).unwrap();
        let result = get_file_context(&dir, "f.txt", Some(1), Some(total), false).unwrap();
        let (body, note) = result.split_once("\n\n--- truncated").unwrap();
        assert_eq!(body, numbered_lines(super::MAX_RETURNED_LINES));
        assert!(note.contains(&format!(
            "server cap is {} lines",
            super::MAX_RETURNED_LINES
        )));
        let _ = fs::remove_dir_all(&dir);
    }

    // PR-review-caught gap: a *line count* cap alone doesn't bound a
    // response whose lines are individually enormous - a minified bundle
    // or a one-line generated blob passes MAX_RETURNED_LINES (2 lines is
    // nowhere near 4000) while still being megabytes. These two tests
    // exercise the byte-based backstop that closes that gap.
    #[test]
    fn many_lines_each_moderately_sized_hits_the_byte_cap_before_the_line_cap() {
        let dir = temp_project("byte_cap_many_lines");
        // Each line is 1000 bytes; MAX_RETURNED_LINES (4000) lines of that
        // would be ~4MB, far past MAX_RETURNED_BYTES (300_000) - so the
        // byte cap, not the line cap, has to be what stops this.
        let line = "x".repeat(1000);
        let total = 1000;
        let content = std::iter::repeat_n(line.as_str(), total)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.join("f.txt"), &content).unwrap();
        let result = get_file_context(&dir, "f.txt", None, None, true).unwrap();
        let (body, note) = result.split_once("\n\n--- truncated").unwrap();
        assert!(body.len() <= super::MAX_RETURNED_BYTES);
        assert!(note.contains(&format!(
            "server cap is {} bytes",
            super::MAX_RETURNED_BYTES
        )));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_single_line_bigger_than_the_whole_byte_budget_is_truncated_in_place() {
        let dir = temp_project("byte_cap_one_giant_line");
        // The pathological case from the review: one line (a minified
        // bundle, a generated one-line blob), no newlines at all, bigger
        // than MAX_RETURNED_BYTES on its own.
        let huge_line = "y".repeat(super::MAX_RETURNED_BYTES + 50_000);
        fs::write(dir.join("f.txt"), &huge_line).unwrap();
        let result = get_file_context(&dir, "f.txt", None, None, true).unwrap();
        let (body, note) = result.split_once("\n\n--- truncated").unwrap();
        assert_eq!(body.len(), super::MAX_RETURNED_BYTES);
        assert!(note.contains("line 1 alone is"));
        assert!(note.contains(&format!("{}-byte cap", super::MAX_RETURNED_BYTES)));
        let _ = fs::remove_dir_all(&dir);
    }
}

/// Regression tests for #40 follow-up 2 ("make query_planner skip itself"):
/// the study that opened #40 worried a `query_planner` call might just
/// return a routing decision, requiring a second call (e.g.
/// `get_file_context`) to actually get the answer - a real two-round-trip
/// cost for a tool meant to save one. `plan_query`'s `file` path already
/// doesn't do that (it calls `get_file_context` itself and returns the
/// content in-band), so this locks that in as tested behavior rather than
/// leaving it as an unverified claim in an issue writeup.
#[cfg(test)]
mod plan_query_tests {
    use super::plan_query;
    use std::fs;

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_plan_query_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_named_file_returns_its_content_in_band_not_just_a_routing_decision() {
        let dir = temp_project("named_file");
        fs::write(dir.join("f.txt"), "fn main() {}").unwrap();

        // No second call (e.g. get_file_context) needed to get the actual
        // answer - the plan already carried it.
        let plan = plan_query(&dir, "anything", Some("f.txt"), None, None).unwrap();
        assert_eq!(plan.strategy, "file_read");
        assert_eq!(plan.file_content.as_deref(), Some("fn main() {}"));
        assert!(
            plan.records.is_empty(),
            "file_read strategy shouldn't also carry graph records - the file_content is the whole answer"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_in_band_answer_matches_a_direct_get_file_context_call_byte_for_byte() {
        // The "strictly cheaper than two calls" bar from the #40 review:
        // this asserts the in-band answer *is* the same payload
        // get_file_context would return on its own, not an approximation of
        // it - so a caller genuinely never needs the second call.
        let dir = temp_project("byte_parity");
        let content = "line one\nline two\nline three\n";
        fs::write(dir.join("f.txt"), content).unwrap();

        let plan = plan_query(&dir, "q", Some("f.txt"), None, None).unwrap();
        let direct = super::get_file_context(&dir, "f.txt", None, None, false).unwrap();
        assert_eq!(plan.file_content.as_deref(), Some(direct.as_str()));

        let _ = fs::remove_dir_all(&dir);
    }
}
