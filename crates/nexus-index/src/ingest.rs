use crate::graph::{EdgeKind, GraphStore, NodeKind};
use crate::language::Language;
use anyhow::Result;
use ignore::WalkBuilder;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, Default)]
pub struct IndexStats {
    pub files_indexed: usize,
    pub nodes: i64,
    pub edges: i64,
}

/// An unresolved call site, carried past the per-file pass so it can be
/// resolved once every file's functions are known project-wide.
struct PendingCall {
    caller_id: i64,
    /// This call's own file's functions by name - checked before falling
    /// back to a global lookup, so same-file resolution still wins when
    /// it's available (preserves the original, more-certain behavior).
    same_file_names: HashMap<String, i64>,
    callee_name: String,
}

/// Full rebuild of the project's graph - see `GraphStore::clear` for why
/// incremental diffing is deferred past this vertical slice.
///
/// Runs in two passes: first every file is parsed and its own
/// File/Function/Type nodes inserted (call sites are collected but not
/// resolved yet), then a second pass resolves each call site against a
/// project-wide function-name registry built from every file. This is what
/// makes `trace_call_path` see across file boundaries - a function that's
/// only ever called from a different file used to be invisible to it
/// entirely.
///
/// This is name-based, not import-aware: there's no `use`/`import`
/// statement parsing or module-path resolution, so a cross-file call only
/// resolves when the callee's name is unique across the whole project. If
/// two files each define a function with the same name and the caller's
/// own file doesn't also define one, the call is left unresolved rather
/// than guessing which one - wrong edges would be worse than missing ones.
pub fn index_directory(root: &Path, store: &GraphStore) -> Result<IndexStats> {
    store.clear()?;

    let mut files_indexed = 0;
    let mut global_fn_registry: HashMap<String, Vec<i64>> = HashMap::new();
    let mut pending_calls: Vec<PendingCall> = Vec::new();

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
        match index_file(path, language, root, store) {
            Ok(result) => {
                for (name, id) in &result.fn_nodes {
                    global_fn_registry.entry(name.clone()).or_default().push(*id);
                }
                let same_file_names: HashMap<String, i64> =
                    result.fn_nodes.into_iter().collect();
                for (caller_id, callee_name) in result.pending_calls {
                    pending_calls.push(PendingCall {
                        caller_id,
                        same_file_names: same_file_names.clone(),
                        callee_name,
                    });
                }
            }
            Err(err) => {
                tracing::warn!(file = %path.display(), error = %err, "failed to index file, skipping");
                continue;
            }
        }
        files_indexed += 1;
    }

    for call in pending_calls {
        let resolved = call
            .same_file_names
            .get(&call.callee_name)
            .copied()
            .or_else(|| match global_fn_registry.get(&call.callee_name) {
                Some(ids) if ids.len() == 1 => Some(ids[0]),
                _ => None,
            });

        if let Some(callee_id) = resolved {
            if callee_id != call.caller_id {
                store.insert_edge(call.caller_id, callee_id, EdgeKind::Calls)?;
            }
        }
    }

    let (nodes, edges) = store.stats()?;
    Ok(IndexStats {
        files_indexed,
        nodes,
        edges,
    })
}

struct FileIndexResult {
    /// (name, node_id) for every function defined in this file.
    fn_nodes: Vec<(String, i64)>,
    /// (caller_id, callee_name) for every call site, left unresolved until
    /// the project-wide pass in `index_directory`.
    pending_calls: Vec<(i64, String)>,
}

fn index_file(
    path: &Path,
    language: Language,
    root: &Path,
    store: &GraphStore,
) -> Result<FileIndexResult> {
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

    // Find the innermost function containing each call site; resolution
    // against callee names (same-file first, then a unique project-wide
    // match) happens later in `index_directory` once every file is known.
    let mut pending_calls = Vec::new();
    for (callee_name, call_range) in calls {
        let call_line = call_range.start_point.row;
        let caller = fn_nodes
            .iter()
            .filter(|(_, r, _)| r.start_point.row <= call_line && call_line <= r.end_point.row)
            .min_by_key(|(_, r, _)| r.end_point.row - r.start_point.row);

        if let Some((_, _, caller_id)) = caller {
            pending_calls.push((*caller_id, callee_name));
        }
    }

    Ok(FileIndexResult {
        fn_nodes: fn_nodes.into_iter().map(|(n, _, id)| (n, id)).collect(),
        pending_calls,
    })
}
