use crate::graph::GraphStore;
use crate::project::graph_db_path;
use crate::NodeRecord;
use anyhow::{anyhow, bail, Result};
use nexus_core::Paths;
use regex::Regex;
use std::path::Path;
use std::time::Duration;

/// Wall-clock ceiling on a single `run_query` execution. This tool's
/// pattern language is deliberately minimal (see the module doc below), but
/// even a supported shape can scan a very large table with an unselective
/// `WHERE` on a big project - without a bound, a slow query blocks whatever
/// else needs this connection (or the daemon thread serving it)
/// indefinitely. A few seconds is generous for any query this pattern
/// language can express while still catching a genuinely pathological one.
/// Enforced via `GraphStore::set_query_timeout`'s cooperative SQLite
/// progress handler rather than killing a thread - see that method's doc
/// comment. See issue #58.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Deliberately minimal - not a Cypher implementation, just named after the
/// closest well-known thing it resembles. Supports exactly one pattern
/// shape:
///
///   MATCH (a:Kind)-[:EDGE_KIND]->(b:Kind) [WHERE var.name = 'value'] RETURN a|b
///
/// Anything outside that fails with a clear "unsupported" error rather than
/// silently returning nothing or guessing at intent - same philosophy as
/// `query_planner`'s honesty about what it can't do yet. A real query
/// language (MATCH chains, aggregates, multiple WHERE clauses) is a much
/// larger undertaking than this project's other gaps justified building
/// right now.
pub fn run_query(repo_path: &Path, query: &str, limit: u32) -> Result<Vec<NodeRecord>> {
    // Canonicalize *before* the allowed_roots check, not after - see the
    // matching comment on `get_file_context` in `queries.rs` for why the
    // ordering itself is the bug (issue #29). `run_query` previously
    // skipped this check entirely and went straight to `graph_db_path` on
    // an uncanonicalized path, so any `repo_path` on disk with a graph DB
    // under it was queryable through it regardless of `allowed_roots` -
    // issue #61.
    let repo_path = repo_path
        .canonicalize()
        .map_err(|_| anyhow!("repo_path does not exist: {}", repo_path.display()))?;
    let repo_path = repo_path.as_path();
    crate::project::require_path_allowed(&Paths::resolve(), repo_path)?;

    let db_path = graph_db_path(repo_path);
    if !db_path.exists() {
        bail!(
            "no index found for {} - run index_project first",
            repo_path.display()
        );
    }
    let store = GraphStore::open(&db_path)?;

    let pattern = Regex::new(
        r"(?i)^\s*MATCH\s*\(\s*(\w+)\s*:\s*(\w+)\s*\)\s*-\[\s*:\s*(\w+)\s*\]->\s*\(\s*(\w+)\s*:\s*(\w+)\s*\)\s*(?:WHERE\s+(\w+)\.name\s*=\s*'([^']*)'\s*)?RETURN\s+(\w+)\s*$",
    )
    .expect("static pattern is valid");

    let caps = pattern.captures(query.trim()).ok_or_else(|| {
        anyhow!(
            "unsupported query shape - only `MATCH (a:Kind)-[:EDGE_KIND]->(b:Kind) \
             [WHERE var.name = 'value'] RETURN a|b` is supported in this version"
        )
    })?;

    let var_a = &caps[1];
    let kind_a = normalize_kind(&caps[2])?;
    let edge_kind = caps[3].to_uppercase();
    let var_b = &caps[4];
    let kind_b = normalize_kind(&caps[5])?;
    let where_var = caps.get(6).map(|m| m.as_str());
    let where_value = caps.get(7).map(|m| m.as_str());
    let return_var = &caps[8];

    if let Some(wv) = where_var {
        if wv != var_a && wv != var_b {
            bail!("WHERE references undefined variable '{wv}'");
        }
    }

    let return_a = if return_var == var_a {
        true
    } else if return_var == var_b {
        false
    } else {
        bail!("RETURN references undefined variable '{return_var}'");
    };

    let where_clause = where_value.map(|value| (where_var == Some(var_a), value));

    store.set_query_timeout(QUERY_TIMEOUT);
    let result = store.match_pattern(&kind_a, &edge_kind, &kind_b, where_clause, return_a, limit);
    store.clear_query_timeout();
    result.map_err(|err| {
        if err.to_string().to_lowercase().contains("interrupt") {
            anyhow!("query timed out after {QUERY_TIMEOUT:?} - narrow the pattern or WHERE clause")
        } else {
            err
        }
    })
}

fn normalize_kind(raw: &str) -> Result<String> {
    match raw.to_lowercase().as_str() {
        "function" => Ok("Function".to_string()),
        "type" => Ok("Type".to_string()),
        "file" => Ok("File".to_string()),
        "section" => Ok("Section".to_string()),
        other => bail!("unknown node kind '{other}' - expected Function, Type, File, or Section"),
    }
}
