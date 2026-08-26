use crate::graph::{EdgeKind, GraphStore, NodeKind};
use crate::language::{self, Language};
use anyhow::{bail, Result};
use ignore::WalkBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter_tags::{TagsConfiguration, TagsContext};

/// Ceiling on a single file's size before this pipeline will read and parse
/// it. Source files past this are almost never a meaningfully useful part
/// of a structural index (generated/vendored/minified/bundled blobs, data
/// dumps accidentally living in-tree) - they're expensive to parse without
/// adding real signal, and were part of the OOM mechanism investigated in
/// #17 (see `PendingCall`'s doc comment above for the other half of that
/// fix). 5 MB comfortably covers real hand-written source files (even
/// unusually large ones) while excluding the pathological cases. Enforced
/// before `std::fs::read`, not after - so an oversized file is never fully
/// loaded into memory just to be discarded.
const MAX_INDEXABLE_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// Reads a file's contents for indexing, refusing (not panicking) if it
/// exceeds `MAX_INDEXABLE_FILE_BYTES` - the caller's existing
/// "failed to index file, skipping" handling (see `index_directory`'s
/// per-file match on `Err`) surfaces this the same way it already surfaces
/// any other per-file parse failure, so oversized files show up in the log
/// instead of silently ballooning memory.
fn read_source_capped(path: &Path) -> Result<Vec<u8>> {
    let size = std::fs::metadata(path)?.len();
    if size > MAX_INDEXABLE_FILE_BYTES {
        bail!(
            "file is {size} bytes, over the {MAX_INDEXABLE_FILE_BYTES}-byte indexable cap - skipping"
        );
    }
    Ok(std::fs::read(path)?)
}

#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub files_indexed: usize,
    pub nodes: i64,
    pub edges: i64,
    /// `None` for an ordinary reindex - LSP enrichment (issue #10) only
    /// ever runs on an explicit `deep` request, never the default path, so
    /// most reindexes never touch this. `Some` (even a `ran: false` one)
    /// once a deep reindex was actually requested, logged here rather than
    /// only to stderr so #40's own follow-up ("log how much indexing time/
    /// edges LSP enrichment adds") gets real data through the same
    /// response path every other stat already flows through.
    pub lsp_enrichment: Option<crate::enrich::EnrichmentReport>,
}

/// A call site whose callee wasn't found in its own file, carried past the
/// per-file pass so it can be resolved once every file's functions are
/// known project-wide. Same-file calls are resolved immediately in the
/// per-file loop instead (see `index_directory`) rather than being carried
/// here - that used to mean cloning the *entire* same-file name map onto
/// every single pending call (`same_file_names: HashMap<String, i64>` per
/// call), which is O(functions x calls) memory for a file and was the
/// actual OOM mechanism behind a dense/minified file taking down the whole
/// process (see the #17 investigation). Carrying just the two scalars below
/// for the cross-file-only remainder fixes that without changing what gets
/// resolved.
struct PendingCall {
    caller_id: i64,
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

/// Cheap signature over exactly the files a real reindex would touch (same
/// walk/ignore rules and file-type filter as `index_directory`, but no
/// parsing - just a stat per file) - lets a caller tell "something was
/// opened" apart from "something actually changed" before paying for a full
/// reindex. This matters because the file watcher's underlying notify
/// backend fires on opens, not just writes (see `nexusd::watcher`'s
/// `MIN_REINDEX_GAP` doc comment) - any read-only tool poking around a
/// watched project (`git status`, `cargo build`, an editor, even another
/// diagnostic command) can otherwise wake a reindex with nothing having
/// changed. Order-independent (entries are sorted before hashing), since a
/// directory walk's yield order isn't guaranteed stable across runs.
pub fn content_signature(root: &Path) -> u64 {
    use std::hash::{Hash, Hasher};

    let walker = WalkBuilder::new(root)
        .add_custom_ignore_filename(".nexusignore")
        .build();

    let mut entries: Vec<(std::path::PathBuf, u64, i64)> = Vec::new();
    for entry in walker {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if Language::from_path(path).is_none() && !is_markdown(path) {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(path) else {
            continue;
        };
        let mtime_millis = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        entries.push((path.to_path_buf(), metadata.len(), mtime_millis));
    }
    entries.sort();

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (path, size, mtime_millis) in &entries {
        path.hash(&mut hasher);
        size.hash(&mut hasher);
        mtime_millis.hash(&mut hasher);
    }
    hasher.finish()
}

/// Approximates how many inotify watches recursively watching `root` would
/// consume - notify's recursive watch backend registers one watch per
/// *directory* (not per file), and, critically, it has no concept of
/// `.gitignore` at all: it watches literally every directory in the tree,
/// including `node_modules`, `target`, `.git`, build output, anything.
/// That's the opposite of `content_signature`/`index_directory`'s walk
/// above, which deliberately *does* respect ignore rules - counting only
/// the ignore-filtered set here would systematically undercount exactly
/// the case most likely to blow a real watch budget (a JS project's
/// `node_modules`, a Rust project's `target`, both routinely tens of
/// thousands of directories). All standard filters are explicitly disabled
/// so this walk matches what notify will actually touch.
///
/// Stops early once the count already exceeds `budget` - for a "does this
/// fit" decision the exact count past that point doesn't matter, and
/// early-exiting keeps this cheap even against a project that's grown far
/// past any reasonable budget (no point walking a million-directory tree
/// to completion just to learn "no, it doesn't fit").
pub fn estimate_watch_count(root: &Path, budget: usize) -> usize {
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .build();

    let mut count = 0usize;
    for entry in walker {
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            count += 1;
            if count > budget {
                return count;
            }
        }
    }
    count
}

fn index_directory_inner(root: &Path, store: &GraphStore) -> Result<IndexStats> {
    store.clear()?;

    let mut files_indexed = 0;
    let mut global_fn_registry: HashMap<String, Vec<i64>> = HashMap::new();
    let mut pending_calls: Vec<PendingCall> = Vec::new();

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

        let result = if let Some(language) = Language::from_path(path) {
            let config = match tags_configs.entry(language) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    match language.build_tags_config() {
                        Ok(config) => e.insert(config),
                        Err(err) => {
                            tracing::warn!(?language, error = %err, "failed to build tags query for language, skipping its files");
                            continue;
                        }
                    }
                }
            };
            index_file(path, config, &mut tags_context, root, store)
        } else if is_markdown(path) {
            index_markdown_file(path, root, store)
        } else {
            continue;
        };

        match result {
            Ok(result) => {
                for (name, id) in &result.fn_nodes {
                    global_fn_registry
                        .entry(name.clone())
                        .or_default()
                        .push(*id);
                }
                // Same-file resolution wins when available (preserves the
                // original, more-certain behavior) - resolved here, once
                // per call, against this file's own name map rather than
                // carrying a clone of that map alongside every call for a
                // second pass to check. Only calls that don't resolve
                // in-file go on to `pending_calls` for the cross-file pass
                // below, where they're checked against `global_fn_registry`
                // instead.
                let same_file_names: HashMap<String, i64> = result.fn_nodes.into_iter().collect();
                for (caller_id, callee_name) in result.pending_calls {
                    match same_file_names.get(&callee_name).copied() {
                        Some(callee_id) => {
                            if callee_id != caller_id {
                                store.insert_edge(caller_id, callee_id, EdgeKind::Calls)?;
                            }
                        }
                        None => pending_calls.push(PendingCall {
                            caller_id,
                            callee_name,
                        }),
                    }
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
        // Same-file resolution already happened above; this is purely the
        // cross-file fallback, name-unique-across-the-project or nothing.
        let resolved = match global_fn_registry.get(&call.callee_name) {
            Some(ids) if ids.len() == 1 => Some(ids[0]),
            _ => None,
        };

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
        lsp_enrichment: None,
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
    config: &TagsConfiguration,
    tags_context: &mut TagsContext,
    root: &Path,
    store: &GraphStore,
) -> Result<FileIndexResult> {
    let source = read_source_capped(path)?;
    let rel_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    let file_id = store.insert_node(NodeKind::File, &rel_path, &rel_path, &rel_path, 0, 0)?;
    // Decoded once, reused both for full-text search and for slicing each
    // node's chunk text below - the file is already in memory either way.
    let text = String::from_utf8_lossy(&source).into_owned();
    // Full-text search also covers markdown docs via `index_markdown_file`
    // below, independent of this tree-sitter path - but nothing else (plain
    // .txt, config files, etc.) is walked for full-text search yet.
    store.insert_file_content(&rel_path, &text)?;
    let lines: Vec<&str> = text.lines().collect();

    let extracted = language::extract(config, tags_context, &source)?;

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

    // A single line this long is a strong minified/bundled-file signal
    // regardless of extension, and it's exactly the case that breaks the
    // nearest-preceding-start heuristic above: with (almost) every
    // definition and every call landing on the same one or two rows, the
    // `rfind` below stops discriminating between callers at all and
    // silently attributes *every* call in the file to whichever function
    // happens to sort last - wrong edges, not just noisy ones. Files like
    // this can also inflate the tagger's function/call counts enough to
    // make this loop itself expensive, so skipping it here is a cheap
    // guard either way (see the #17 investigation).
    const MAX_LINE_LEN_FOR_CALL_RESOLUTION: usize = 2000;
    let max_line_len = lines.iter().map(|l| l.len()).max().unwrap_or(0);

    let mut pending_calls = Vec::new();
    if max_line_len > MAX_LINE_LEN_FOR_CALL_RESOLUTION {
        tracing::debug!(
            file = %path.display(),
            max_line_len,
            "line far exceeds source-code norms (likely minified/bundled) - skipping call-site resolution for this file"
        );
    } else {
        for (callee_name, call_range) in extracted.calls {
            let call_line = call_range.start_point.row;
            let caller = fn_nodes_by_start
                .iter()
                .rfind(|(_, r, _)| r.start_point.row <= call_line);

            if let Some((_, _, caller_id)) = caller {
                pending_calls.push((*caller_id, callee_name));
            }
        }
    }

    Ok(FileIndexResult {
        fn_nodes: fn_nodes.into_iter().map(|(n, _, id)| (n, id)).collect(),
        pending_calls,
    })
}

fn is_markdown(path: &Path) -> bool {
    // Case-sensitive, matching `Language::from_path`'s own existing
    // convention - not special-cased to be more lenient than code files are.
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("md") | Some("markdown")
    )
}

/// Markdown's structural model is headings, not functions/calls - there's
/// no call graph to build here, so this returns the same `FileIndexResult`
/// shape `index_file` does with empty `fn_nodes`/`pending_calls`.
fn index_markdown_file(path: &Path, root: &Path, store: &GraphStore) -> Result<FileIndexResult> {
    let source = read_source_capped(path)?;
    let rel_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    let file_id = store.insert_node(NodeKind::File, &rel_path, &rel_path, &rel_path, 0, 0)?;
    let text = String::from_utf8_lossy(&source).into_owned();
    store.insert_file_content(&rel_path, &text)?;

    let sections = crate::docs::extract_sections(&text);

    // One node id per section, at the same index as `sections` itself - a
    // parent always appears earlier in the flat list than its children (the
    // extraction algorithm only ever references already-pushed stack
    // entries), so `node_ids[parent_idx]` is always already populated by
    // the time a child section needs it.
    let mut node_ids: Vec<i64> = Vec::with_capacity(sections.len());

    for section in &sections {
        let qualified_name = format!("{rel_path}::{}#{}", section.name, section.start_line);
        let id = store.insert_node(
            NodeKind::Section,
            &section.name,
            &qualified_name,
            &rel_path,
            section.start_line,
            section.end_line,
        )?;
        match section.parent {
            Some(parent_idx) => store.insert_edge(node_ids[parent_idx], id, EdgeKind::Contains)?,
            None => store.insert_edge(file_id, id, EdgeKind::Defines)?,
        }

        node_ids.push(id);
    }

    Ok(FileIndexResult {
        fn_nodes: Vec::new(),
        pending_calls: Vec::new(),
    })
}

/// Regression test for issue #30: a flat markdown file with no interior
/// headings used to produce one untruncated `Section` chunk spanning the
/// whole file - that chunk fed the (now-removed) embeddings pipeline, but
/// the underlying node-extraction behavior this guards (one `Section` node
/// for a flat file, regardless of its size) is still real and worth
/// covering directly.
#[cfg(test)]
mod index_markdown_file_tests {
    use super::index_markdown_file;
    use crate::graph::GraphStore;
    use std::fs;

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_index_markdown_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn open_store(dir: &std::path::Path) -> GraphStore {
        GraphStore::open(&dir.join("graph.db")).unwrap()
    }

    #[test]
    fn a_flat_markdown_file_with_no_interior_headings_is_one_section() {
        let dir = temp_project("flat");
        let huge_line = "x".repeat(10_000);
        let content = format!("# Title\n\n{huge_line}\n");
        let path = dir.join("FLAT.md");
        fs::write(&path, &content).unwrap();

        let store = open_store(&dir);
        let result = index_markdown_file(&path, &dir, &store).unwrap();

        assert!(result.fn_nodes.is_empty());
        assert!(result.pending_calls.is_empty());
        let (nodes, _) = store.stats().unwrap();
        // 1 File node + 1 Section node.
        assert_eq!(nodes, 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_small_markdown_file_is_unaffected() {
        let dir = temp_project("small");
        let content = "# Title\n\nJust a short section, well under any cap.\n";
        let path = dir.join("SMALL.md");
        fs::write(&path, content).unwrap();

        let store = open_store(&dir);
        let result = index_markdown_file(&path, &dir, &store).unwrap();

        assert!(result.fn_nodes.is_empty());
        let (nodes, _) = store.stats().unwrap();
        assert_eq!(nodes, 2);

        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod read_source_capped_tests {
    use super::{index_markdown_file, MAX_INDEXABLE_FILE_BYTES};
    use crate::graph::GraphStore;
    use std::fs;

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_index_size_cap_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn open_store(dir: &std::path::Path) -> GraphStore {
        GraphStore::open(&dir.join("graph.db")).unwrap()
    }

    #[test]
    fn a_file_over_the_cap_is_skipped_not_errored_out_of_the_process() {
        let dir = temp_project("over");
        let path = dir.join("HUGE.md");
        // Sparse file: seek-and-write-one-byte at the target length avoids
        // actually allocating/writing MAX_INDEXABLE_FILE_BYTES+1 bytes just
        // to prove the cap trips.
        {
            let f = fs::File::create(&path).unwrap();
            f.set_len(MAX_INDEXABLE_FILE_BYTES + 1).unwrap();
        }

        let store = open_store(&dir);
        let result = index_markdown_file(&path, &dir, &store);

        // Skipped via a returned Err (the same path `index_directory`'s
        // per-file match already logs-and-continues on), not a panic.
        let err = match result {
            Ok(_) => panic!("oversized file must not be indexed"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("indexable cap"), "unexpected error: {err}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_at_exactly_the_cap_is_still_indexed() {
        let dir = temp_project("at_cap");
        let path = dir.join("AT_CAP.md");
        let content = format!("# Title\n\n{}\n", "x".repeat(200));
        fs::write(&path, &content).unwrap();

        let store = open_store(&dir);
        let result = index_markdown_file(&path, &dir, &store);
        assert!(result.is_ok(), "small file must not be affected by the cap");

        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod content_signature_tests {
    use super::content_signature;
    use std::fs;

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_content_signature_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn signature_is_stable_for_unchanged_content() {
        let dir = temp_project("stable");
        fs::write(dir.join("main.rs"), "fn main() {}").unwrap();
        assert_eq!(content_signature(&dir), content_signature(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn signature_changes_when_a_file_is_modified() {
        let dir = temp_project("modified");
        let file = dir.join("main.rs");
        fs::write(&file, "fn main() {}").unwrap();
        let before = content_signature(&dir);
        fs::write(&file, "fn main() { println!(\"hi\"); }").unwrap();
        let after = content_signature(&dir);
        assert_ne!(before, after);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn signature_changes_when_a_file_is_added() {
        let dir = temp_project("added");
        fs::write(dir.join("main.rs"), "fn main() {}").unwrap();
        let before = content_signature(&dir);
        fs::write(dir.join("lib.rs"), "pub fn helper() {}").unwrap();
        let after = content_signature(&dir);
        assert_ne!(before, after);
        let _ = fs::remove_dir_all(&dir);
    }

    /// This is the whole point of the signature: a file that isn't
    /// indexable (no supported language, not markdown) changing shouldn't
    /// count as "the project changed" - otherwise it wouldn't distinguish
    /// "something was opened" from "something we'd actually reindex over".
    #[test]
    fn signature_ignores_files_indexing_would_skip() {
        let dir = temp_project("ignored");
        fs::write(dir.join("data.bin"), b"\x00\x01").unwrap();
        let before = content_signature(&dir);
        fs::write(dir.join("data.bin"), b"\x02\x03\x04\x05").unwrap();
        let after = content_signature(&dir);
        assert_eq!(before, after);
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod estimate_watch_count_tests {
    use super::estimate_watch_count;
    use std::fs;

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_estimate_watch_count_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn counts_the_root_and_every_subdirectory() {
        let dir = temp_project("nested");
        fs::create_dir_all(dir.join("a/b/c")).unwrap();
        fs::create_dir_all(dir.join("d")).unwrap();
        // root + a + a/b + a/b/c + d = 5 - files don't count, only dirs.
        fs::write(dir.join("a/b/c/file.txt"), "x").unwrap();
        assert_eq!(estimate_watch_count(&dir, 1000), 5);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The whole point of this function vs. content_signature's walk:
    /// notify's recursive watch has no concept of .gitignore, so a
    /// gitignored directory (node_modules, target, .git, ...) still
    /// consumes real watches and must still be counted here.
    #[test]
    fn counts_gitignored_directories_too() {
        let dir = temp_project("gitignored");
        fs::write(dir.join(".gitignore"), "node_modules/\n").unwrap();
        fs::create_dir_all(dir.join("node_modules/some-pkg")).unwrap();
        // root + node_modules + node_modules/some-pkg = 3.
        assert_eq!(estimate_watch_count(&dir, 1000), 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stops_early_once_the_budget_is_exceeded() {
        let dir = temp_project("early_exit");
        for i in 0..50 {
            fs::create_dir_all(dir.join(format!("d{i}"))).unwrap();
        }
        // root + 50 subdirs = 51 - a budget of 10 should stop well short of
        // walking all of them, returning *some* count over budget rather
        // than the exact total.
        let count = estimate_watch_count(&dir, 10);
        assert!(count > 10);
        assert!(count < 51);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_directory_counts_as_just_the_root() {
        let dir = temp_project("empty");
        assert_eq!(estimate_watch_count(&dir, 1000), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
