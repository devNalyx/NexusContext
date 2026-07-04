use crate::graph::{EdgeKind, GraphStore, NodeKind};
use crate::language::{self, Language};
use anyhow::Result;
use ignore::WalkBuilder;
use nexus_core::{Config, EmbeddingsPolicy, Paths};
use std::collections::HashMap;
use std::path::Path;
use tree_sitter_tags::{TagsConfiguration, TagsContext};

#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub files_indexed: usize,
    pub nodes: i64,
    pub edges: i64,
    /// What happened with the embeddings pass on this run - e.g. "skipped:
    /// not configured", "skipped: disabled", "ok: 342 chunks embedded",
    /// "partial: endpoint became unreachable after 96 chunks". Always
    /// present so a caller never has to guess why semantic search may or
    /// may not work after this reindex.
    pub embeddings_status: String,
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
    store.begin_immediate()?;
    match index_directory_inner(root, store) {
        Ok(stats) => {
            store.commit()?;
            Ok(stats)
        }
        Err(err) => {
            let _ = store.rollback();
            Err(err)
        }
    }
}

fn index_directory_inner(root: &Path, store: &GraphStore) -> Result<IndexStats> {
    store.clear()?;

    let mut files_indexed = 0;
    let mut global_fn_registry: HashMap<String, Vec<i64>> = HashMap::new();
    let mut pending_calls: Vec<PendingCall> = Vec::new();
    let mut pending_embeddings: Vec<(i64, String)> = Vec::new();

    // Building a TagsConfiguration recompiles that language's query, so it's
    // cached per-language rather than rebuilt for every single file; the
    // TagsContext (parser + query cursor) is likewise reused across files.
    let mut tags_configs: HashMap<Language, TagsConfiguration> = HashMap::new();
    let mut tags_context = TagsContext::new();

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

        let config = match tags_configs.entry(language) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => match language.build_tags_config() {
                Ok(config) => e.insert(config),
                Err(err) => {
                    tracing::warn!(?language, error = %err, "failed to build tags query for language, skipping its files");
                    continue;
                }
            },
        };

        match index_file(path, config, &mut tags_context, root, store) {
            Ok(result) => {
                for (name, id) in &result.fn_nodes {
                    global_fn_registry
                        .entry(name.clone())
                        .or_default()
                        .push(*id);
                }
                pending_embeddings.extend(result.pending_embeddings);
                let same_file_names: HashMap<String, i64> = result.fn_nodes.into_iter().collect();
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

    let embeddings_status = embed_pending_nodes(store, pending_embeddings);

    let (nodes, edges) = store.stats()?;
    Ok(IndexStats {
        files_indexed,
        nodes,
        edges,
        embeddings_status,
    })
}

/// Third pass, after every file and every cross-file call edge is already
/// resolved - embeds each Function/Type node's source text, entirely
/// best-effort: skipped up front (zero cost) unless embeddings are
/// configured, enabled, and allowed, and if the endpoint fails partway
/// through, whatever succeeded before that stays persisted rather than
/// being thrown away (see `embed_in_batches`).
fn embed_pending_nodes(store: &GraphStore, pending: Vec<(i64, String)>) -> String {
    let config = match Config::load(&Paths::resolve().config_file()) {
        Ok(c) => c,
        Err(err) => return format!("skipped: failed to load config: {err}"),
    };
    match config.embeddings_policy() {
        EmbeddingsPolicy::NotConfigured => return "skipped: not configured".to_string(),
        EmbeddingsPolicy::Disabled => return "skipped: disabled".to_string(),
        EmbeddingsPolicy::RemoteBlocked => {
            return "skipped: embeddings endpoint is remote and allow_remote isn't set".to_string()
        }
        EmbeddingsPolicy::Allowed => {}
    }
    if pending.is_empty() {
        return "ok: 0 chunks embedded (no functions/types found)".to_string();
    }

    let model = config.embeddings.model.clone().unwrap_or_default();
    let ids: Vec<i64> = pending.iter().map(|(id, _)| *id).collect();
    let texts: Vec<String> = pending.iter().map(|(_, text)| text.clone()).collect();
    let mut embedded = 0usize;
    let mut insert_err: Option<anyhow::Error> = None;

    let result =
        crate::embeddings::embed_in_batches(&config.embeddings, &texts, |offset, vectors| {
            for (i, vector) in vectors.into_iter().enumerate() {
                if insert_err.is_some() {
                    break;
                }
                let idx = offset + i;
                let dim = vector.len();
                let bytes = crate::embeddings::vector_to_bytes(&vector);
                match store.insert_embedding(ids[idx], &model, dim, &texts[idx], &bytes) {
                    Ok(()) => embedded += 1,
                    Err(err) => insert_err = Some(err),
                }
            }
        });

    match (result, insert_err, embedded) {
        (Ok(()), None, _) => format!("ok: {embedded} chunks embedded"),
        (Ok(()), Some(err), _) => {
            format!("partial: {embedded} chunks embedded, then a storage error: {err}")
        }
        (Err(err), _, 0) => {
            tracing::warn!(error = %err, "embeddings endpoint unreachable, skipping embeddings for this index run");
            format!("skipped: embeddings endpoint unreachable: {err}")
        }
        (Err(err), _, embedded) => {
            tracing::warn!(error = %err, embedded, "embeddings endpoint failed partway through indexing");
            format!("partial: endpoint became unreachable after {embedded} chunks: {err}")
        }
    }
}

struct FileIndexResult {
    /// (name, node_id) for every function defined in this file.
    fn_nodes: Vec<(String, i64)>,
    /// (caller_id, callee_name) for every call site, left unresolved until
    /// the project-wide pass in `index_directory`.
    pending_calls: Vec<(i64, String)>,
    /// (node_id, chunk_text) for every Function/Type node defined in this
    /// file - left unembedded until the project-wide embeddings pass in
    /// `index_directory`, which only actually calls the endpoint if
    /// embeddings are configured/enabled/allowed at all.
    pending_embeddings: Vec<(i64, String)>,
}

fn index_file(
    path: &Path,
    config: &TagsConfiguration,
    tags_context: &mut TagsContext,
    root: &Path,
    store: &GraphStore,
) -> Result<FileIndexResult> {
    let source = std::fs::read(path)?;
    let rel_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    let file_id = store.insert_node(NodeKind::File, &rel_path, &rel_path, &rel_path, 0, 0)?;
    // Decoded once, reused both for full-text search and for slicing each
    // node's chunk text below - the file is already in memory either way.
    let text = String::from_utf8_lossy(&source).into_owned();
    // Full-text search only covers files tree-sitter already parses (i.e.
    // the languages in `Language::from_path`) - it doesn't walk every file
    // in the repo independently, so config/doc files outside that set
    // aren't searchable yet.
    store.insert_file_content(&rel_path, &text)?;
    let lines: Vec<&str> = text.lines().collect();
    let chunk_text_for = |range: &tree_sitter::Range| -> String {
        let start = range.start_point.row.min(lines.len().saturating_sub(1));
        let end = range.end_point.row.min(lines.len().saturating_sub(1));
        lines[start..=end].join("\n")
    };

    let extracted = language::extract(config, tags_context, &source)?;

    let mut pending_embeddings: Vec<(i64, String)> = Vec::new();
    let mut fn_nodes: Vec<(String, tree_sitter::Range, i64)> = Vec::new();
    for (name, range) in extracted.functions {
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
        pending_embeddings.push((id, chunk_text_for(&range)));
        fn_nodes.push((name, range, id));
    }

    for (name, range) in extracted.types {
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
        pending_embeddings.push((id, chunk_text_for(&range)));
    }

    // Find which function contains each call site, by nearest-preceding-
    // start rather than full range containment: some languages' tags.scm
    // only tags the declarator/signature as the function's range, not the
    // whole body (C/C++ do this - `@definition.function` sits on
    // `function_declarator`, which ends before the body even starts), so a
    // containment check against the definition's *end* would wrongly find
    // no enclosing function for every call inside the body. The most
    // recent function whose start precedes the call is right for ordinary,
    // non-nested function bodies regardless of how wide the source
    // grammar's tags.scm made the definition's own range - it only relies
    // on the *start* position, which tags.scm gives reliably everywhere
    // checked so far. Doesn't handle nested/closure calls precisely, same
    // as the plain containment check didn't either.
    let mut fn_nodes_by_start = fn_nodes.clone();
    fn_nodes_by_start.sort_by_key(|(_, r, _)| r.start_point.row);

    let mut pending_calls = Vec::new();
    for (callee_name, call_range) in extracted.calls {
        let call_line = call_range.start_point.row;
        let caller = fn_nodes_by_start
            .iter()
            .rfind(|(_, r, _)| r.start_point.row <= call_line);

        if let Some((_, _, caller_id)) = caller {
            pending_calls.push((*caller_id, callee_name));
        }
    }

    Ok(FileIndexResult {
        fn_nodes: fn_nodes.into_iter().map(|(n, _, id)| (n, id)).collect(),
        pending_calls,
        pending_embeddings,
    })
}
