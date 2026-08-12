use crate::graph::{GraphStore, NodeKind, NodeRecord};
use crate::language::Language;
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
///
/// Each note also carries the node's actual source snippet (re-read from
/// disk via `get_file_context`, the same bounded/allowed_roots-checked
/// reader the MCP tool uses - no schema change needed, since `NodeRecord`
/// already has everything required: `file_path`/`start_line`/`end_line`).
/// A first version of this only emitted Kind/Location plus the call graph,
/// which read as a bare link map with no idea what a symbol actually *is*.
/// Source is what turns a note into something worth reading on its own,
/// not just a node in a graph.
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

        // Best-effort: a note without a Source section (file moved/deleted
        // since indexing, or an unreadable range) still gets written with
        // whatever it does have, rather than failing the whole export over
        // one stale node.
        if let Ok(snippet) = crate::queries::get_file_context(
            repo_path,
            &node.file_path,
            Some(node.start_line as usize),
            Some(node.end_line as usize),
            false,
        ) {
            content.push_str(&format!(
                "\n## Source\n```{}\n{}\n```\n",
                fence_lang(node),
                snippet
            ));
        }

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

/// Markdown fence info-string for syntax highlighting - `Section` nodes
/// (markdown heading bodies, see `docs.rs`) are fenced as `markdown` mainly
/// to stop a heading inside the snippet (e.g. a `## Something` in the
/// body) from being misread as a real subheading of the note itself, not
/// because the snippet needs syntax coloring. Function/Type nodes use the
/// same extension-to-language mapping `ingest.rs` already uses to decide
/// what to parse, so this can't drift out of sync with it.
fn fence_lang(node: &NodeRecord) -> &'static str {
    if node.kind == NodeKind::Section {
        return "markdown";
    }
    match Language::from_path(Path::new(&node.file_path)) {
        Some(Language::Rust) => "rust",
        Some(Language::Python) => "python",
        Some(Language::JavaScript) => "javascript",
        Some(Language::TypeScript) => "typescript",
        Some(Language::Tsx) => "tsx",
        Some(Language::Go) => "go",
        Some(Language::Java) => "java",
        Some(Language::C) => "c",
        Some(Language::Cpp) => "cpp",
        Some(Language::CSharp) => "csharp",
        Some(Language::Ruby) => "ruby",
        Some(Language::Php) => "php",
        None => "",
    }
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

#[cfg(test)]
mod fence_lang_tests {
    use super::fence_lang;
    use crate::graph::{NodeKind, NodeRecord};

    fn node(kind: NodeKind, file_path: &str) -> NodeRecord {
        NodeRecord {
            id: 1,
            kind,
            name: "x".to_string(),
            qualified_name: "x".to_string(),
            file_path: file_path.to_string(),
            start_line: 1,
            end_line: 2,
        }
    }

    #[test]
    fn maps_known_source_extensions_to_their_fence_language() {
        assert_eq!(fence_lang(&node(NodeKind::Function, "f.rs")), "rust");
        assert_eq!(fence_lang(&node(NodeKind::Function, "f.py")), "python");
        assert_eq!(fence_lang(&node(NodeKind::Type, "f.go")), "go");
        assert_eq!(fence_lang(&node(NodeKind::Type, "f.tsx")), "tsx");
    }

    #[test]
    fn section_nodes_are_always_fenced_as_markdown_regardless_of_extension() {
        // Section nodes come from .md/.markdown files (see docs.rs), but
        // the point being tested is that the *kind* decides this, not the
        // extension - fencing exists here specifically to stop a heading
        // inside the body from being misread as a real subheading.
        assert_eq!(
            fence_lang(&node(NodeKind::Section, "README.md")),
            "markdown"
        );
    }

    #[test]
    fn unrecognized_extension_falls_back_to_an_untagged_fence() {
        assert_eq!(fence_lang(&node(NodeKind::Function, "f.unknown")), "");
    }
}
