use crate::graph::{GraphStore, NodeKind, NodeRecord};
use crate::project::graph_db_path;
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Writes the knowledge graph as a folder of plain markdown files with
/// `[[wikilinks]]` between related symbols - a valid Obsidian vault with no
/// integration code, since a vault is just markdown files. Static/point-in-
/// time by design, same as the zstd export artifact; this covers the graph
/// only (functions/types + CALLS edges) - there's no ADR management built,
/// so nothing to export there.
pub fn export_obsidian(repo_path: &Path) -> Result<PathBuf> {
    let db_path = graph_db_path(repo_path);
    if !db_path.exists() {
        bail!(
            "no index found for {} - run index_project first",
            repo_path.display()
        );
    }

    let store = GraphStore::open(&db_path)?;
    let nodes = store.all_nodes()?;
    let call_edges = store.all_call_edges()?;

    let vault_dir = repo_path.join(".nexuscontext").join("vault");
    std::fs::create_dir_all(&vault_dir)?;

    let by_id: HashMap<i64, &NodeRecord> = nodes.iter().map(|n| (n.id, n)).collect();
    // A caller can call the same callee at multiple sites (e.g. `helper()`
    // twice in the same function) - dedupe so the link list reflects
    // distinct relationships, not call-site counts.
    let mut calls_out: HashMap<i64, std::collections::HashSet<i64>> = HashMap::new();
    let mut calls_in: HashMap<i64, std::collections::HashSet<i64>> = HashMap::new();
    for (src, dst) in &call_edges {
        calls_out.entry(*src).or_default().insert(*dst);
        calls_in.entry(*dst).or_default().insert(*src);
    }

    for node in &nodes {
        if node.kind == NodeKind::File {
            continue;
        }

        let mut content = format!(
            "# {}\n\n**Kind:** {:?}\n**Location:** `{}:{}-{}`\n",
            node.name, node.kind, node.file_path, node.start_line, node.end_line
        );

        if let Some(callees) = calls_out.get(&node.id) {
            content.push_str("\n## Calls\n");
            let mut names: Vec<_> = callees
                .iter()
                .filter_map(|id| by_id.get(id))
                .map(|n| wikilink_name(n))
                .collect();
            names.sort();
            for name in names {
                content.push_str(&format!("- [[{name}]]\n"));
            }
        }

        if let Some(callers) = calls_in.get(&node.id) {
            content.push_str("\n## Called by\n");
            let mut names: Vec<_> = callers
                .iter()
                .filter_map(|id| by_id.get(id))
                .map(|n| wikilink_name(n))
                .collect();
            names.sort();
            for name in names {
                content.push_str(&format!("- [[{name}]]\n"));
            }
        }

        let file_path = vault_dir.join(format!("{}.md", wikilink_name(node)));
        std::fs::write(file_path, content)?;
    }

    Ok(vault_dir)
}

/// The filename (minus `.md`) and the `[[wikilink]]` text must match
/// exactly for Obsidian to resolve the link - qualified names already
/// disambiguate same-named symbols across files (see `ingest.rs`), so this
/// just strips characters that aren't safe in a filename.
fn wikilink_name(node: &NodeRecord) -> String {
    node.qualified_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
