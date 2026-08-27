use crate::graph::{EdgeKind, GraphStore, NodeKind};
use crate::language::{self, Language};
use anyhow::{bail, Result};
use ignore::WalkBuilder;
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
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
    // `(alias, original_name)` pairs collected from every file's `pub use
    // ... as alias;` re-exports (see `extract_reexport_aliases` / issue
    // #67) - merged into `global_fn_registry` below only once every file's
    // definitions are known, since the re-exported symbol is very often
    // defined in a different file than the `pub use` that renames it.

    // Building a TagsConfiguration recompiles that language's query, so it's
    // cached per-language rather than rebuilt for every single file; the
    // TagsContext (parser + query cursor) is likewise reused across files.
    let mut tags_configs: HashMap<Language, TagsConfiguration> = HashMap::new();
    let mut tags_context = TagsContext::new();
    let mut reexport_aliases: Vec<(String, String)> = Vec::new();

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
                reexport_aliases.extend(result.reexport_aliases);
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

    // Fold `pub use ... as alias;` re-exports into the registry (issue #67):
    // once every file's definitions are known, an alias whose original name
    // resolves to a real definition gets that same set of ids registered
    // under the alias too, so a call site written against the alias (like
    // `run_cypher_query`, re-exported from `run_query`) resolves exactly
    // like a call to the original name would. If the original name itself
    // isn't found (typo, external crate, macro-generated, etc.) the alias
    // is silently skipped - same "don't guess" posture as the rest of this
    // name-based pass.
    //
    // Iterated to a fixed point rather than a single pass over
    // `reexport_aliases` (issue #78's independent review): a chain like
    // `pub use a::b as c;` followed by `pub use c as d;` needs `c` resolved
    // before `d` can be, and `reexport_aliases`' order is whatever
    // `extract_reexport_aliases`' final `.sort()` produced (alphabetical by
    // `(alias, original_name)`, not dependency order) - a single pass only
    // resolved a chain when alphabetical luck happened to visit it in the
    // right order. Every pass still folds every `(alias, original_name)`
    // pair unconditionally, same as the original single-pass version
    // (rather than skipping an alias once it has any entry at all), so two
    // different original names re-exported under the same alias name both
    // still contribute their ids - `entry.push`'s own `contains` check
    // already makes re-processing an already-resolved pair a cheap no-op.
    // Bounded at `reexport_aliases.len()` passes: a chain can be at most
    // that long, and `any_grew` exits as soon as a full pass adds nothing
    // new, so this can't loop needlessly even on a pathological/cyclic
    // input.
    for _ in 0..reexport_aliases.len().max(1) {
        let mut any_grew = false;
        for (alias, original_name) in &reexport_aliases {
            if let Some(target_ids) = global_fn_registry.get(original_name).cloned() {
                let entry = global_fn_registry.entry(alias.clone()).or_default();
                for id in target_ids {
                    if !entry.contains(&id) {
                        entry.push(id);
                        any_grew = true;
                    }
                }
            }
        }
        if !any_grew {
            break;
        }
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
    /// `(alias, original_name)` pairs from this file's `pub use ... as
    /// alias;` re-exports (see `extract_reexport_aliases`, issue #67).
    reexport_aliases: Vec<(String, String)>,
}

/// Extracts `pub use ... as alias;` re-export aliases from a file's raw
/// text via a lightweight regex scan. This is deliberately *not* a general
/// `use`-declaration parser: the indexer runs on each language's bundled
/// tree-sitter *tags* query (see `language.rs`'s module docs), which
/// doesn't capture `use`/import statements at all, and building real
/// module-path resolution on top of that would be a much bigger project
/// (see issue #59). What this covers is the concrete, common shape from
/// issue #67 - a crate-root re-export that renames a symbol:
///
/// ```text
/// pub use path::to::original_name as alias_name;
/// pub use path::to::{original_name as alias_name, other_item};
/// ```
///
/// Each match yields `(alias, original_name)` so the alias can be linked
/// back to the real definition for call-graph/dead-code purposes. A plain
/// `pub use path::name;` (no rename) isn't covered here because it doesn't
/// need to be: the call-site name already matches the definition's name,
/// so the existing name-based resolution already handles it correctly.
///
/// Comments are stripped before matching via `strip_comments` - both `//`
/// line comments and `/* ... */` block comments (including multi-line
/// ones) - since this is a raw-text regex scan, not a real parse, and
/// without that a doc-comment example of the exact re-export syntax (like
/// this very function's own doc comment above, or a `/* pub use x as y; */`
/// commented-out import) would otherwise be picked up as if it were real
/// code. Issue #78's independent review caught the block-comment gap: only
/// `//`-prefixed lines were being stripped, so code inside a `/* */` block
/// was still scanned and could produce a spurious alias for a re-export
/// that isn't actually live. Deduped on return: the same `(alias, original_name)`
/// pair can otherwise appear more than once (e.g. matched by both the
/// simple and brace patterns, or if source happens to repeat the same
/// re-export text), and a duplicate would make the alias's merged id list
/// in `index_directory_inner` look spuriously ambiguous even when every
/// duplicate points at the same, genuinely-unique definition.
///
/// Strips both `//` line comments and `/* ... */` block comments from
/// `text`, replacing stripped content with nothing (not even whitespace
/// preserved) except that a block comment spanning a `;`-terminated
/// statement boundary still leaves the surrounding code's line structure
/// intact for the caller's regexes, which tolerate arbitrary whitespace
/// between tokens via `\s`. Deliberately non-nesting (Rust technically
/// allows nested `/* /* */ */` block comments, but that's rare enough in
/// practice that handling it would meaningfully complicate this without a
/// corresponding real-world payoff for a "not a real parser" scan) - an
/// unterminated or nested block comment degrades to stripping through the
/// first `*/` it finds, which is always a safe direction to err in for
/// this function's purpose (a spurious strip can only make it miss a real
/// alias, never fabricate one; the reverse - `/* */` not being stripped
/// at all - was the actual bug, since it can fabricate one).
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    let bytes = text.as_bytes();
    while let Some((i, c)) = chars.next() {
        if c == '/' && bytes.get(i + 1) == Some(&b'/') {
            // Line comment - skip to (not including) the next newline, so
            // the newline itself still lands in `out` and later line-based
            // reasoning elsewhere isn't affected.
            for (_, c2) in chars.by_ref() {
                if c2 == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else if c == '/' && bytes.get(i + 1) == Some(&b'*') {
            chars.next(); // consume the '*'
            let mut prev_star = false;
            for (_, c2) in chars.by_ref() {
                if prev_star && c2 == '/' {
                    break;
                }
                // Newlines inside the block comment are preserved so a
                // multi-line block comment doesn't fuse two unrelated
                // lines of real code together on either side of it.
                if c2 == '\n' {
                    out.push('\n');
                }
                prev_star = c2 == '*';
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn extract_reexport_aliases(text: &str) -> Vec<(String, String)> {
    static SIMPLE: OnceLock<Regex> = OnceLock::new();
    static BRACE: OnceLock<Regex> = OnceLock::new();
    // `\s*::\s*` rather than `::` between path segments, and around the
    // leading `::` before the final segment - real code sometimes wraps a
    // long `use` path across lines (`\s` matches newlines), and without
    // this the whole match silently failed rather than just losing the
    // line break. See issue #78's independent review.
    let simple = SIMPLE.get_or_init(|| {
        Regex::new(r"pub\s+use\s+(?:[\w]+\s*::\s*)*(\w+)\s+as\s+(\w+)\s*;").expect("valid regex")
    });
    let brace = BRACE.get_or_init(|| {
        Regex::new(r"pub\s+use\s+(?:[\w]+\s*::\s*)*\{([^}]*)\}\s*;").expect("valid regex")
    });

    let code_only = strip_comments(text);

    let mut aliases = Vec::new();
    for caps in simple.captures_iter(&code_only) {
        aliases.push((caps[2].to_string(), caps[1].to_string()));
    }
    for caps in brace.captures_iter(&code_only) {
        for item in caps[1].split(',') {
            let item = item.trim();
            if let Some((name, alias)) = item.split_once(" as ") {
                let name = name.trim();
                let alias = alias.trim();
                if !name.is_empty() && !alias.is_empty() {
                    aliases.push((alias.to_string(), name.to_string()));
                }
            }
        }
    }
    aliases.sort();
    aliases.dedup();
    aliases
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

    let reexport_aliases = extract_reexport_aliases(&text);

    Ok(FileIndexResult {
        fn_nodes: fn_nodes.into_iter().map(|(n, _, id)| (n, id)).collect(),
        pending_calls,
        reexport_aliases,
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
        reexport_aliases: Vec::new(),
    })
}

/// Regression test for issue #30: a flat markdown file with no interior
/// headings used to produce one untruncated `Section` chunk spanning the
/// whole file - that chunk fed the (now-removed) embeddings pipeline, but
/// the underlying node-extraction behavior this guards (one `Section` node
/// for a flat file, regardless of its size) is still real and worth
/// covering directly.
/// Regression test for issue #67: a function only ever called through a
/// `pub use original as alias;` re-export must not be flagged dead, since
/// it genuinely has a live caller - just one that spells its name
/// differently than the definition does. Modeled directly on the real
/// `nexus_index::cypher::run_query` / `run_cypher_query` case that exposed
/// this (`crates/nexus-index/src/lib.rs`'s own re-export), but as a
/// self-contained synthetic fixture rather than indexing this crate's own
/// source, so the test doesn't depend on this crate's real layout staying
/// exactly as-is.
#[cfg(test)]
mod reexport_alias_tests {
    use super::{extract_reexport_aliases, index_directory};
    use crate::graph::GraphStore;
    use std::fs;

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_index_reexport_alias_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extracts_a_simple_pub_use_as_alias() {
        let aliases = extract_reexport_aliases("pub use cypher::run_query as run_cypher_query;");
        assert_eq!(
            aliases,
            vec![("run_cypher_query".to_string(), "run_query".to_string())]
        );
    }

    #[test]
    fn extracts_aliases_from_a_brace_list_and_ignores_unaliased_items() {
        let aliases =
            extract_reexport_aliases("pub use foo::{run_query as run_cypher_query, other_fn};");
        assert_eq!(
            aliases,
            vec![("run_cypher_query".to_string(), "run_query".to_string())]
        );
    }

    #[test]
    fn plain_pub_use_with_no_rename_yields_no_alias() {
        let aliases = extract_reexport_aliases("pub use cypher::run_query;");
        assert!(aliases.is_empty());
    }

    /// Regression for issue #78's independent review: a `pub use ... as
    /// alias;` shape sitting inside a `/* ... */` block comment (commented
    /// out, or a doc-comment example using this exact block-comment style
    /// instead of `//`) must not be picked up as a real re-export - only
    /// `//` line comments were being stripped before, so this used to
    /// fabricate a spurious alias for dead/non-existent code.
    #[test]
    fn a_pub_use_inside_a_block_comment_is_not_extracted() {
        let aliases =
            extract_reexport_aliases("/* pub use cypher::run_query as run_cypher_query; */");
        assert!(
            aliases.is_empty(),
            "commented-out code must not produce an alias: {aliases:?}"
        );
    }

    #[test]
    fn a_pub_use_inside_a_multiline_block_comment_is_not_extracted() {
        let aliases = extract_reexport_aliases(
            "/*\n * Example:\n * pub use cypher::run_query as run_cypher_query;\n */\n",
        );
        assert!(
            aliases.is_empty(),
            "commented-out code must not produce an alias: {aliases:?}"
        );
    }

    /// A block comment sitting *between* two real, separate `pub use`
    /// statements must not fuse them together or swallow the second one.
    #[test]
    fn a_block_comment_between_two_real_reexports_does_not_swallow_either() {
        let aliases = extract_reexport_aliases(
            "pub use a::x as y;\n/* an explanatory note */\npub use b::p as q;\n",
        );
        assert_eq!(
            aliases,
            vec![
                ("q".to_string(), "p".to_string()),
                ("y".to_string(), "x".to_string()),
            ]
        );
    }

    /// Regression for issue #78's independent review: a `use` path wrapped
    /// across multiple lines (a real rustfmt output shape for a long path)
    /// used to silently fail to match at all, since the regex previously
    /// required `::` segments with no whitespace/newlines around them.
    #[test]
    fn a_pub_use_with_the_path_wrapped_across_lines_is_still_extracted() {
        let aliases = extract_reexport_aliases(
            "pub use some::deeply::nested::\n    cypher::run_query as run_cypher_query;\n",
        );
        assert_eq!(
            aliases,
            vec![("run_cypher_query".to_string(), "run_query".to_string())]
        );
    }

    /// The end-to-end case: `lib.rs` re-exports `helper` (defined in
    /// `inner.rs`) as `renamed_helper`, and the only call site anywhere in
    /// the project uses the alias. Without alias resolution, `helper` shows
    /// up as dead (no `CALLS` edge matches its name); with it, the alias's
    /// call site resolves back to `helper`'s real definition.
    #[test]
    fn a_function_only_called_via_its_reexported_alias_is_not_flagged_dead() {
        let dir = temp_project("end_to_end");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/inner.rs"),
            "pub fn helper() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/lib.rs"),
            "mod inner;\npub use inner::helper as renamed_helper;\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/caller.rs"),
            "fn main() {\n    renamed_helper();\n}\n",
        )
        .unwrap();

        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        index_directory(&dir, &store).unwrap();

        let dead: Vec<String> = store
            .dead_functions(None)
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert!(
            !dead.contains(&"helper".to_string()),
            "helper is live via its re-exported alias and must not be flagged dead: {dead:?}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// The exact real-world shape from issue #67: the call site is
    /// path-qualified (`crate_name::run_cypher_query(...)`, mirroring
    /// `nexus_index::run_cypher_query(...)` in `nexusd`/`nexus-cli`), not a
    /// bare identifier. This also exercises the companion fix in
    /// `language.rs` (`RUST_SCOPED_CALL_QUERY`): without it, a
    /// path-qualified call is never even recorded as a call site, so alias
    /// resolution alone wouldn't be enough to un-deaden `run_query` here.
    #[test]
    fn a_function_only_called_via_a_qualified_reexported_alias_is_not_flagged_dead() {
        let dir = temp_project("qualified_end_to_end");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/cypher.rs"),
            "pub fn run_query() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/lib.rs"),
            "mod cypher;\npub use cypher::run_query as run_cypher_query;\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/caller.rs"),
            "fn main() {\n    nexus_index::run_cypher_query();\n}\n",
        )
        .unwrap();

        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        index_directory(&dir, &store).unwrap();

        let dead: Vec<String> = store
            .dead_functions(None)
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert!(
            !dead.contains(&"run_query".to_string()),
            "run_query is live via its qualified, re-exported alias and must not be flagged \
             dead: {dead:?}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Regression for issue #78's independent review: a two-hop alias
    /// chain (`inner::helper` re-exported as `z_mid` in one file, then
    /// `z_mid` re-exported again as `a_final` in another) with only the
    /// final alias actually called anywhere.
    ///
    /// The alias names are deliberately chosen so plain alphabetical
    /// sorting of `(alias, original_name)` pairs visits `("a_final",
    /// "z_mid")` *before* `("z_mid", "helper")` (verified: `"a_final" <
    /// "z_mid"` lexicographically) - a single pass over that sorted list
    /// tries to resolve `a_final` against `z_mid` before `z_mid` itself has
    /// been resolved against `helper`, so `a_final` was left unresolved and
    /// `helper` incorrectly flagged dead under the old single-pass code.
    /// The fixed-point loop's second pass catches it.
    #[test]
    fn a_two_hop_alias_chain_resolves_to_the_real_definition() {
        let dir = temp_project("alias_chain");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/inner.rs"),
            "pub fn helper() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/lib.rs"),
            "mod inner;\npub use inner::helper as z_mid;\n\
             pub use z_mid as a_final;\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/caller.rs"),
            "fn main() {\n    a_final();\n}\n",
        )
        .unwrap();

        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        index_directory(&dir, &store).unwrap();

        let dead: Vec<String> = store
            .dead_functions(None)
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert!(
            !dead.contains(&"helper".to_string()),
            "helper is live via a two-hop re-exported alias chain and must not be flagged \
             dead: {dead:?}"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}

/// Issue #59's "add tests for ambiguous symbol resolution": pins down the
/// *actual* current behavior when a call site's callee name matches more
/// than one function defined project-wide, with no same-file match to break
/// the tie. Read straight from `index_directory_inner` above (this is not
/// guessed): same-file resolution is tried first; only calls that don't
/// resolve in-file fall through to the cross-file pass, which resolves a
/// name against `global_fn_registry` **only when exactly one id is
/// registered under that name** (`ids.len() == 1`). Two or more candidates
/// means `resolved` is `None` and no `CALLS` edge is inserted at all - not
/// to the first candidate found, not to every candidate, not even
/// deterministically to "whichever the walker happened to visit first".
/// The call site is silently dropped from the graph.
///
/// This is a deliberate, documented choice (see
/// `docs/NexusContext-Wiki/Known-Limitations.md`, "stays unresolved rather
/// than guessed wrong") and arguably the *right* one for an honest
/// name-based heuristic - but it is also a real, user-visible gap: nothing
/// in `trace_call_path`, `search_graph`, or `detect_dead_code` today
/// signals "this call site exists but its target was ambiguous" versus "no
/// call site was found here at all". Both same-named candidate functions
/// end up indistinguishable from genuinely-dead code if nothing else calls
/// them, even though one of them almost certainly *is* being called by
/// `main` - the graph just can't say which. Surfacing that distinction
/// (e.g. a dedicated `ambiguous`/`ambiguity` confidence marker instead of
/// silent omission) is exactly the kind of richer provenance/confidence
/// modeling issue #59 flags as future work, not something this test
/// attempts to fix.
#[cfg(test)]
mod ambiguous_resolution_tests {
    use super::index_directory;
    use crate::graph::GraphStore;
    use std::fs;

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_index_ambiguous_resolution_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `foo` is defined in two different files (`module_a.rs`,
    /// `module_b.rs`), and a third file calls `foo()` with no local
    /// definition of its own to resolve against in-file. Current, actual
    /// behavior: neither definition gets a `CALLS` edge, so both show up as
    /// dead even though the call site is real. This is the "same-named
    /// function in two modules" scenario from issue #59's own example
    /// (`module_a.foo` / `module_b.foo`).
    #[test]
    fn a_call_to_a_name_defined_in_two_modules_resolves_to_neither() {
        let dir = temp_project("two_modules");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/module_a.rs"),
            "pub fn foo() {\n    println!(\"from module_a\");\n}\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/module_b.rs"),
            "pub fn foo() {\n    println!(\"from module_b\");\n}\n",
        )
        .unwrap();
        fs::write(dir.join("src/caller.rs"), "fn main() {\n    foo();\n}\n").unwrap();

        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        index_directory(&dir, &store).unwrap();

        // No CALLS edge was created for this call site at all - not to
        // module_a::foo, not to module_b::foo, not to both.
        let edges = store.all_call_edges().unwrap();
        assert!(
            edges.is_empty(),
            "an ambiguous cross-file call must not resolve to any candidate: {edges:?}"
        );

        // Both same-named candidates therefore read as dead, even though
        // one of them is genuinely called by `main` - this is the real,
        // user-visible cost of "unresolved rather than guessed wrong".
        let dead: Vec<String> = store
            .dead_functions(None)
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(
            dead.iter().filter(|n| *n == "foo").count(),
            2,
            "both ambiguous same-named candidates should read as dead under current \
             name-based resolution: {dead:?}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Same fixture, but the caller's own file also defines a `foo` - the
    /// same-file match wins deterministically over the cross-file ambiguity,
    /// exactly per the "same-file matches win" rule. This isn't an
    /// ambiguous case at all once a same-file candidate exists; included to
    /// make that precedence explicit and contrast with the fully-ambiguous
    /// case above.
    #[test]
    fn a_same_file_definition_wins_over_other_same_named_candidates_elsewhere() {
        let dir = temp_project("same_file_wins");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/module_a.rs"),
            "pub fn foo() {\n    println!(\"from module_a\");\n}\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/caller.rs"),
            "fn foo() {\n    println!(\"local\");\n}\n\nfn main() {\n    foo();\n}\n",
        )
        .unwrap();

        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        index_directory(&dir, &store).unwrap();

        let edges = store.all_call_edges().unwrap();
        assert_eq!(
            edges.len(),
            1,
            "the same-file foo should resolve deterministically: {edges:?}"
        );

        // module_a::foo is never called and is genuinely dead here; the
        // caller's own local foo is not (it has an inbound CALLS edge).
        let dead: Vec<String> = store
            .dead_functions(None)
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(
            dead.iter().filter(|n| *n == "foo").count(),
            1,
            "only the never-called module_a::foo should be dead, not the called local one: \
             {dead:?}"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}

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
