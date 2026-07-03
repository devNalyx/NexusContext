use crate::graph::{EdgeKind, GraphStore, NodeKind};
use crate::language::Language;
use anyhow::Result;
use ignore::WalkBuilder;
use std::path::Path;

#[derive(Debug, Clone, Copy, Default)]
pub struct IndexStats {
    pub files_indexed: usize,
    pub nodes: i64,
    pub edges: i64,
}

/// Full rebuild of the project's graph - see `GraphStore::clear` for why
/// incremental diffing is deferred past this vertical slice.
pub fn index_directory(root: &Path, store: &GraphStore) -> Result<IndexStats> {
    store.clear()?;

    let mut files_indexed = 0;
    let walker = WalkBuilder::new(root)
        .add_custom_ignore_filename(".nexusignore")
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let Some(language) = Language::from_path(path) else {
            continue;
        };
        if let Err(err) = index_file(path, language, root, store) {
            tracing::warn!(file = %path.display(), error = %err, "failed to index file, skipping");
            continue;
        }
        files_indexed += 1;
    }

    let (nodes, edges) = store.stats()?;
    Ok(IndexStats {
        files_indexed,
        nodes,
        edges,
    })
}

fn index_file(path: &Path, language: Language, root: &Path, store: &GraphStore) -> Result<()> {
    let source = std::fs::read(path)?;
    let rel_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    let tree = language.parse(&source)?;

    let file_id = store.insert_node(NodeKind::File, &rel_path, &rel_path, &rel_path, 0, 0)?;
    // Full-text search only covers files tree-sitter already parsed (i.e.
    // the languages in `Language::from_path`) - it doesn't walk every file
    // in the repo independently, so config/doc files outside that set
    // aren't searchable yet.
    store.insert_file_content(&rel_path, &String::from_utf8_lossy(&source))?;

    let functions = language.extract_functions(&tree, &source)?;
    let types = language.extract_types(&tree, &source)?;
    let calls = language.extract_calls(&tree, &source)?;

    let mut fn_nodes: Vec<(String, tree_sitter::Range, i64)> = Vec::new();
    for (name, range) in functions {
        let qualified_name = format!("{rel_path}::{name}#{}", range.start_point.row);
        let id = store.insert_node(
            NodeKind::Function,
            &name,
            &qualified_name,
            &rel_path,
            range.start_point.row as u32 + 1,
            range.end_point.row as u32 + 1,
        )?;
        store.insert_edge(file_id, id, EdgeKind::Defines)?;
        fn_nodes.push((name, range, id));
    }

    for (name, range) in types {
        let qualified_name = format!("{rel_path}::{name}#{}", range.start_point.row);
        let id = store.insert_node(
            NodeKind::Type,
            &name,
            &qualified_name,
            &rel_path,
            range.start_point.row as u32 + 1,
            range.end_point.row as u32 + 1,
        )?;
        store.insert_edge(file_id, id, EdgeKind::Defines)?;
    }

    // Same-file call resolution only: find the innermost function whose
    // range contains the call site, then link to a same-file function of
    // that name if one exists. Cross-file/type-aware resolution is future
    // work (see the Hybrid-LSP-style tradeoff noted in the proposal).
    for (callee_name, call_range) in calls {
        let call_line = call_range.start_point.row;
        let caller = fn_nodes
            .iter()
            .filter(|(_, r, _)| r.start_point.row <= call_line && call_line <= r.end_point.row)
            .min_by_key(|(_, r, _)| r.end_point.row - r.start_point.row);

        let Some((_, _, caller_id)) = caller else {
            continue;
        };
        let Some((_, _, callee_id)) = fn_nodes.iter().find(|(n, _, _)| n == &callee_name) else {
            continue;
        };
        if caller_id != callee_id {
            store.insert_edge(*caller_id, *callee_id, EdgeKind::Calls)?;
        }
    }

    Ok(())
}
