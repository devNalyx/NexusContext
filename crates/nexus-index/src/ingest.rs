use crate::graph::{EdgeKind, GraphStore, NodeKind};
use crate::language::{self, Language};
use crate::vault::normalized_path_key;
use anyhow::{bail, Result};
use ignore::overrides::{Override, OverrideBuilder};
use ignore::{Match, WalkBuilder};
use nexus_core::{Config, ConfigIndexConfig, Paths};
use regex::Regex;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
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
    // O_NOFOLLOW-guarded on Unix as defense-in-depth against a path being
    // swapped for a symlink between the earlier canonicalize/allowed_roots
    // check and this read - see issue #72 and `secure_fs` module docs.
    crate::secure_fs::read_verified(path)
}

#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub files_indexed: usize,
    /// Config files actually ingested when `[config_index]` is enabled.
    pub config_files_indexed: usize,
    /// Config files read but flagged partial/unsupported (still searchable as
    /// bounded literal text unless `metadata_only` is set).
    pub config_files_partial: usize,
    pub config_files_unsupported: usize,
    /// Config files excluded by include/exclude rules before being read.
    pub config_files_excluded: usize,
    /// Config files that matched but could not be read (size cap, I/O error).
    pub config_files_skipped: usize,
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
    /// Set when this pass bailed out early because a newer reindex request
    /// for the same project superseded it mid-run (issue #58's cooperative-
    /// cancellation gap, see ADR 0014) - `false` for every ordinary
    /// completed or genuinely-failed reindex. The other fields are
    /// meaningless when this is `true`: nothing was committed, so `nodes`/
    /// `edges` are whatever `GraphStore::stats` happened to report against
    /// the not-yet-rolled-back transaction, not the project's real state.
    pub superseded: bool,
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
    index_directory_with_config(root, store, &ConfigIndexConfig::default())
}

/// Same as `index_directory`, but honors `[config_index]` (issue #112).
pub fn index_directory_with_config(
    root: &Path,
    store: &GraphStore,
    config: &ConfigIndexConfig,
) -> Result<IndexStats> {
    index_directory_checked_with_config(root, store, || false, config)
}

/// Same as `index_directory_checked`, with explicit config-index options.
pub fn index_directory_checked(
    root: &Path,
    store: &GraphStore,
    superseded: impl Fn() -> bool,
) -> Result<IndexStats> {
    index_directory_checked_with_config(root, store, superseded, &ConfigIndexConfig::default())
}

/// Same as `index_directory_checked`, plus opt-in YAML/JSON indexing.
///
/// `superseded` is polled between files in the main indexing loop (see
/// `index_directory_inner`), and if it ever returns `true` the pass stops
/// early and rolls back rather than committing a partial rebuild - the
/// caller (`nexus_index::project::index_project_locked`) treats that as "a
/// newer request already superseded this one, a follow-up pass will redo it
/// correctly" rather than either a success or a real failure. See issue #58
/// / ADR 0014's amendment for why this exists: a reindex on a large project
/// can run long enough that further edits land before it finishes, and
/// letting it run to completion anyway is pure wasted work once a follow-up
/// pass is already guaranteed to happen.
pub fn index_directory_checked_with_config(
    root: &Path,
    store: &GraphStore,
    superseded: impl Fn() -> bool,
    config: &ConfigIndexConfig,
) -> Result<IndexStats> {
    store.begin_immediate()?;
    match index_directory_inner(root, store, &superseded, config) {
        Ok(stats) if stats.superseded => {
            // Discard the partial rebuild entirely rather than committing
            // it - a rolled-back attempt leaves the previous (stale but
            // complete and consistent) graph.db in place until the
            // follow-up pass replaces it, instead of a half-written one.
            let _ = store.rollback();
            Ok(stats)
        }
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
    content_signature_with_config(root, &ConfigIndexConfig::default())
}

/// Config-aware counterpart to `content_signature`. Turning `[config_index]`
/// on/off or changing its globs changes the set of files in the signature, so
/// the watcher notices without a separate config-change signal.
pub fn content_signature_with_config(root: &Path, config: &ConfigIndexConfig) -> u64 {
    use std::hash::{Hash, Hasher};

    // Toggling vault indexing changes what a reindex would store even when
    // no file changed, so fold the gate into the watcher's signature.
    let vault_index_enabled = Config::load(&Paths::resolve().config_file())
        .map(|loaded| loaded.vault_index.enabled)
        .unwrap_or(false);

    let config_matcher = build_config_matcher(config, root);
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
        if Language::from_path(path).is_none()
            && !is_markdown(path)
            && !config_path_allowed(path, root, &config_matcher)
        {
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
    // Include the policy itself so changing include/exclude/metadata_only or
    // the vault gate is observable to the watcher even with no file edits.
    vault_index_enabled.hash(&mut hasher);
    config.enabled.hash(&mut hasher);
    config.metadata_only.hash(&mut hasher);
    config.include.hash(&mut hasher);
    config.exclude.hash(&mut hasher);
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

/// Checked for supersession between files in the main walk below (not
/// inside a single file's own parsing - a file is small enough that
/// interrupting mid-file wouldn't meaningfully speed up a bail-out, and
/// would need its own partial-write story on top of the transaction-level
/// rollback `index_directory_checked` already does). `N` files between
/// checks, not every single one, since an atomic load is cheap but
/// thousands of files still shouldn't pay thousands of redundant loads for
/// a check that's essentially never going to be true.
pub(crate) const SUPERSESSION_CHECK_INTERVAL: usize = 25;

fn index_directory_inner(
    root: &Path,
    store: &GraphStore,
    superseded: &impl Fn() -> bool,
    config: &ConfigIndexConfig,
) -> Result<IndexStats> {
    store.clear()?;

    let config_matcher = build_config_matcher(config, root);
    // Vault metadata is opt-in and independent of `[config_index]`. Loading
    // the user config here (rather than threading another parameter through
    // every `index_directory*` caller) means a reindex triggered by the
    // watcher, the CLI or an MCP call all obey the same gate. A missing or
    // unreadable config fails closed: no vault metadata is extracted.
    let vault_index_enabled = Config::load(&Paths::resolve().config_file())
        .map(|loaded| loaded.vault_index.enabled)
        .unwrap_or(false);
    let mut files_indexed = 0;
    let mut config_files_indexed = 0;
    let mut config_files_partial = 0;
    let mut config_files_unsupported = 0;
    let mut config_files_excluded = 0;
    let mut config_files_skipped = 0;
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
    // Wikilinks can only be resolved after every note's path and aliases are
    // known, so the per-file pass records raw links and this list is resolved
    // in one project-wide pass below (same shape as `pending_calls`).
    let mut pending_vault_links: Vec<PendingVaultLink> = Vec::new();

    let walker = WalkBuilder::new(root)
        .add_custom_ignore_filename(".nexusignore")
        .build();

    for entry in walker {
        if files_indexed > 0 && files_indexed % SUPERSESSION_CHECK_INTERVAL == 0 && superseded() {
            tracing::info!(
                project = %root.display(),
                files_indexed,
                "reindex superseded by a newer request for the same project - bailing out early, \
                 a follow-up pass will redo it"
            );
            return Ok(IndexStats {
                files_indexed,
                config_files_indexed,
                config_files_partial,
                config_files_unsupported,
                config_files_excluded,
                config_files_skipped,
                nodes: 0,
                edges: 0,
                lsp_enrichment: None,
                superseded: true,
            });
        }

        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let is_config_candidate = config.enabled && is_config_extension(path);
        if is_config_candidate && !config_path_allowed(path, root, &config_matcher) {
            config_files_excluded += 1;
            continue;
        }

        let result = if let Some(language) = Language::from_path(path) {
            let tags_config = match tags_configs.entry(language) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    match language.build_tags_config() {
                        Ok(built) => e.insert(built),
                        Err(err) => {
                            tracing::warn!(?language, error = %err, "failed to build tags query for language, skipping its files");
                            continue;
                        }
                    }
                }
            };
            index_file(path, tags_config, &mut tags_context, root, store)
        } else if is_config_candidate {
            match index_config_file(path, root, store, config) {
                Ok((result, status)) => {
                    match status {
                        ConfigStatus::Parsed => config_files_indexed += 1,
                        ConfigStatus::Partial => config_files_partial += 1,
                        ConfigStatus::Unsupported => config_files_unsupported += 1,
                    }
                    Ok(result)
                }
                Err(err) => Err(err),
            }
        } else if is_markdown(path) {
            index_markdown_file(path, root, store, vault_index_enabled)
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
                pending_vault_links.extend(result.vault_links);
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
                if is_config_candidate {
                    config_files_skipped += 1;
                }
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

    resolve_vault_links(store, &pending_vault_links)?;

    let (nodes, edges) = store.stats()?;
    Ok(IndexStats {
        files_indexed,
        config_files_indexed,
        config_files_partial,
        config_files_unsupported,
        config_files_excluded,
        config_files_skipped,
        nodes,
        edges,
        lsp_enrichment: None,
        superseded: false,
    })
}

/// A wikilink awaiting project-wide resolution. Source node id is known
/// immediately; the target may be a path, an alias or a bare basename, so
/// the decision is deferred until every note has been indexed.
struct PendingVaultLink {
    src_id: i64,
    link: crate::docs::VaultLink,
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
    /// Raw wikilinks from a Markdown file; empty for code files.
    vault_links: Vec<PendingVaultLink>,
}

/// Resolves and records wikilinks with an explicit, auditable precedence:
///
/// 1. explicit path (with or without a Markdown extension) -> `exact`
/// 2. frontmatter alias -> `exact`
/// 3. unique bare basename -> `heuristic` (same uncertainty contract as
///    name-based call edges)
/// 4. ambiguous/missing target -> recorded as unresolved, no edge emitted
///
/// This mirrors `CALLS` rather than guessing: two notes called `README.md`
/// leave a bare `[[README]]` unresolved instead of arbitrarily picking one.
fn resolve_vault_links(store: &GraphStore, links: &[PendingVaultLink]) -> Result<()> {
    if links.is_empty() {
        return Ok(());
    }

    let files = store.all_file_nodes()?;
    let mut by_path: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut by_basename: std::collections::HashMap<String, Vec<i64>> =
        std::collections::HashMap::new();
    for file in &files {
        // Obsidian links use `/` even on Windows, where the walker stores
        // `docs\\Plan.md`. One shared normalizer keeps both sides comparable.
        let path_lower = normalized_path_key(&file.file_path);
        by_path.insert(path_lower.clone(), file.id);
        let stem = path_lower
            .strip_suffix(".markdown")
            .or_else(|| path_lower.strip_suffix(".md"))
            .unwrap_or(&path_lower)
            .to_string();
        by_path.insert(stem.clone(), file.id);
        by_basename
            .entry(stem.rsplit('/').next().unwrap_or(&stem).to_string())
            .or_default()
            .push(file.id);
    }

    let mut by_alias: std::collections::HashMap<String, Vec<i64>> =
        std::collections::HashMap::new();
    for (alias, file_id) in store.all_vault_aliases()? {
        by_alias
            .entry(alias.to_lowercase())
            .or_default()
            .push(file_id);
    }

    for pending in links {
        let lower = normalized_path_key(&pending.link.target);
        let (dst_id, resolution, confidence): (Option<i64>, &str, &str) = if lower.contains('/') {
            // A slash means the author named a path; don't silently fall
            // back to a basename in another directory.
            match by_path.get(&lower) {
                Some(id) => (Some(*id), "exact-path", "exact"),
                None => (None, "unresolved-path", "none"),
            }
        } else if let Some(ids) = by_alias.get(&lower) {
            if ids.len() == 1 {
                (Some(ids[0]), "alias", "exact")
            } else {
                (None, "ambiguous-alias", "none")
            }
        } else if let Some(ids) = by_basename.get(&lower) {
            if ids.len() == 1 {
                (Some(ids[0]), "unique-basename", "heuristic")
            } else {
                (None, "ambiguous-basename", "none")
            }
        } else {
            (None, "unresolved", "none")
        };

        if let Some(dst_id) = dst_id {
            if dst_id != pending.src_id {
                store.insert_edge(pending.src_id, dst_id, EdgeKind::LinksTo)?;
            }
        }
        store.insert_vault_link(
            pending.src_id,
            dst_id,
            &pending.link.target,
            pending.link.heading.as_deref(),
            pending.link.display.as_deref(),
            pending.link.embed,
            pending.link.line,
            resolution,
            confidence,
        )?;
    }

    Ok(())
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
        vault_links: Vec::new(),
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

fn is_config_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yaml") | Some("yml") | Some("json")
    )
}

/// Builds the include/exclude matcher once per reindex. Excludes are added
/// after includes with gitignore-style `!` negation, so a path matching both
/// is excluded before the file is ever read. Invalid patterns fail closed:
/// no config files are indexed rather than silently ignoring policy.
fn build_config_matcher(config: &ConfigIndexConfig, root: &Path) -> Option<Override> {
    if !config.enabled {
        return None;
    }
    let mut builder = OverrideBuilder::new(root);
    for pattern in &config.include {
        if let Err(err) = builder.add(pattern) {
            tracing::warn!(pattern = %pattern, error = %err, "invalid config_index.include pattern; config indexing disabled for this pass");
            return None;
        }
    }
    for pattern in &config.exclude {
        if let Err(err) = builder.add(&format!("!{pattern}")) {
            tracing::warn!(pattern = %pattern, error = %err, "invalid config_index.exclude pattern; config indexing disabled for this pass");
            return None;
        }
    }
    builder.build().ok()
}

fn config_path_allowed(path: &Path, root: &Path, matcher: &Option<Override>) -> bool {
    let Some(matcher) = matcher else {
        return false;
    };
    if !is_config_extension(path) {
        return false;
    }
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    matches!(matcher.matched(rel, false), Match::Whitelist(_))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigStatus {
    Parsed,
    Partial,
    Unsupported,
}

impl ConfigStatus {
    fn as_str(self) -> &'static str {
        match self {
            ConfigStatus::Parsed => "parsed",
            ConfigStatus::Partial => "partial",
            ConfigStatus::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone)]
struct ConfigKey {
    doc: u32,
    key_path: String,
    parent: String,
    indent: usize,
    start_line: u32,
    end_line: u32,
    /// Last line of the YAML document (or the whole JSON file), so a top-level
    /// key's range never spills into the next `---` document.
    doc_end_line: u32,
}

fn strip_yaml_comment(line: &str) -> &str {
    let mut single = false;
    let mut double = false;
    for (idx, ch) in line.char_indices() {
        match ch {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '#' if !single && !double => return &line[..idx],
            _ => {}
        }
    }
    line
}

fn find_unquoted_colon(line: &str) -> Option<usize> {
    let mut single = false;
    let mut double = false;
    for (idx, ch) in line.char_indices() {
        match ch {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            ':' if !single && !double => return Some(idx),
            _ => {}
        }
    }
    None
}

/// Lightweight, line-based key extraction. It deliberately does not resolve
/// aliases or template expressions; it only records where a key was written.
fn extract_yaml_keys(text: &str) -> Vec<ConfigKey> {
    let lines: Vec<&str> = text.lines().collect();
    let mut keys = Vec::new();
    let line_count = lines.len().max(1) as u32;
    let mut doc_starts: Vec<(u32, u32)> = vec![(1, 1)];
    let mut doc = 1u32;
    let mut seen_start_marker = false;
    let mut keys_in_doc = false;
    // `(indent, key_path)` stack for the current document only.
    let mut stack: Vec<(usize, String)> = Vec::new();
    // 1-based sequence-item counters, keyed by the `- ` line's indentation.
    let mut array_indices: HashMap<usize, usize> = HashMap::new();
    // Indentation of the key that opened a literal/folded block scalar.
    // Lines more indented than that are scalar *content*, not mappings -
    // scanning them would fabricate keys like `message.phantom` (#121).
    let mut block_scalar_indent: Option<usize> = None;

    for (idx, raw) in lines.iter().enumerate() {
        let line_no = idx as u32 + 1;
        let trimmed = raw.trim();
        let indent = raw.len() - raw.trim_start().len();
        if let Some(scalar_indent) = block_scalar_indent {
            if trimmed.is_empty() || indent > scalar_indent {
                continue;
            }
            block_scalar_indent = None;
        }
        if trimmed == "---" {
            // A leading `---` opens document 1; each later one opens the
            // next document, matching serde_yaml's document numbering.
            if seen_start_marker || keys_in_doc {
                doc += 1;
                doc_starts.push((doc, line_no));
            }
            seen_start_marker = true;
            keys_in_doc = false;
            stack.clear();
            array_indices.clear();
            continue;
        }
        if trimmed.starts_with("...") {
            stack.clear();
            array_indices.clear();
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let content = strip_yaml_comment(trimmed).trim();
        if content.is_empty() {
            continue;
        }
        let (content, is_array_item) = match content.strip_prefix("- ") {
            Some(rest) => (rest.trim(), true),
            None => (content, false),
        };
        let Some(colon) = find_unquoted_colon(content) else {
            continue;
        };
        let key = content[..colon]
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .to_string();
        if key.is_empty() {
            continue;
        }

        // Sequence items get an explicit `[n]` component (1-based) so two
        // items with the same key don't collapse into one fabricated path.
        let effective_indent = if is_array_item { indent + 2 } else { indent };
        if !is_array_item {
            array_indices.retain(|&array_indent, _| array_indent < indent);
        }
        while stack
            .last()
            .is_some_and(|(last_indent, _)| *last_indent >= indent)
        {
            stack.pop();
        }
        let base = stack
            .last()
            .map(|(_, path)| path.clone())
            .unwrap_or_default();
        let parent = if is_array_item {
            let index = array_indices.entry(indent).or_insert(0);
            *index += 1;
            if base.is_empty() {
                format!("[{}]", *index)
            } else {
                format!("{base}[{}]", *index)
            }
        } else {
            base
        };
        let key_path = if parent.is_empty() {
            key.clone()
        } else {
            format!("{parent}.{key}")
        };
        keys.push(ConfigKey {
            doc,
            key_path: key_path.clone(),
            parent: parent.clone(),
            indent: effective_indent,
            start_line: line_no,
            end_line: line_no,
            doc_end_line: line_count,
        });
        // `|` / `>` (with optional chomping/indent indicators) introduce a
        // block scalar. Its body is text, even when it contains `key: value`
        // shaped lines - skip it instead of inventing nested keys (#121).
        let value = content[colon + 1..].trim_start();
        if let Some(indicator) = value.chars().next() {
            if matches!(indicator, '|' | '>')
                && value[1..]
                    .chars()
                    .all(|c| c == '-' || c == '+' || c.is_ascii_digit())
            {
                block_scalar_indent = Some(indent);
            }
        }
        keys_in_doc = true;
        // For a sequence item, subsequent sibling keys belong to the array
        // element, not to the first item's scalar key.
        // A sequence item is a sibling container at the dash's indentation;
        // later keys inside that item keep it as their parent.
        stack.push((indent, if is_array_item { parent } else { key_path }));
    }
    for key in &mut keys {
        key.doc_end_line = doc_starts
            .iter()
            .find(|(next_doc, _)| *next_doc > key.doc)
            .map(|(_, start)| start.saturating_sub(1))
            .unwrap_or(line_count)
            .max(key.start_line);
    }
    keys
}

/// Every mapping key path in a validated YAML value tree, including paths
/// the line scanner cannot see (flow-style `{inner: true}` mappings and maps
/// inside sequences). Sequence components are 1-based to match the scanner.
fn collect_yaml_paths(value: &serde_yaml::Value, prefix: &str, out: &mut Vec<String>) {
    match value {
        serde_yaml::Value::Mapping(map) => {
            for (key, child) in map {
                let Some(name) = key.as_str() else {
                    continue;
                };
                let path = if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{prefix}.{name}")
                };
                out.push(path.clone());
                collect_yaml_paths(child, &path, out);
            }
        }
        serde_yaml::Value::Sequence(items) => {
            for (idx, child) in items.iter().enumerate() {
                collect_yaml_paths(child, &format!("{prefix}[{}]", idx + 1), out);
            }
        }
        _ => {}
    }
}

fn yaml_path_parent(path: &str) -> String {
    let cut = path.rfind('.').or_else(|| path.rfind('['));
    cut.map(|idx| path[..idx].to_string()).unwrap_or_default()
}

/// Adds key paths from the already-validated value tree that the line scanner
/// missed, so `parsed` means every mapping key in the document is searchable.
/// Anchors come from the nearest scanned ancestor: locations stay provenance
/// leads, not byte offsets. This reads syntax only — no rendering, no
/// evaluation, no command execution (issue #124).
fn augment_yaml_keys_from_tree(text: &str, keys: &mut Vec<ConfigKey>) -> bool {
    let line_count = text.lines().count().max(1) as u32;
    let mut anchors: HashMap<(u32, String), (u32, usize)> = keys
        .iter()
        .map(|key| {
            (
                (key.doc, key.key_path.clone()),
                (key.start_line, key.indent),
            )
        })
        .collect();
    let mut added = false;
    // Stop at the first failed document: serde_yaml's deserializer does not
    // advance the reader when `Value::deserialize` errors, so `continue`
    // here would re-yield the same broken document forever. `config_status`
    // marks the file unsupported; there is no tree worth augmenting.
    let mut parsed_docs = Vec::new();
    for doc in serde_yaml::Deserializer::from_str(text) {
        match serde_yaml::Value::deserialize(doc) {
            Ok(value) => parsed_docs.push(value),
            Err(_) => break,
        }
    }
    for (doc_index, value) in parsed_docs.into_iter().enumerate() {
        let doc_no = doc_index as u32 + 1;
        let mut paths = Vec::new();
        collect_yaml_paths(&value, "", &mut paths);
        for path in paths {
            if anchors.contains_key(&(doc_no, path.clone())) {
                continue;
            }
            let parent = yaml_path_parent(&path);
            let (start, indent) = anchors
                .iter()
                .filter(|((anchor_doc, ancestor), _)| {
                    *anchor_doc == doc_no
                        && !ancestor.is_empty()
                        && (parent == *ancestor
                            || parent.starts_with(&format!("{ancestor}."))
                            || parent.starts_with(&format!("{ancestor}[")))
                })
                .max_by_key(|((_, ancestor), _)| ancestor.len())
                .map(|(_, anchor)| *anchor)
                .unwrap_or((1, 0));
            keys.push(ConfigKey {
                doc: doc_no,
                key_path: path.clone(),
                parent,
                indent: indent + 2,
                start_line: start,
                end_line: start,
                doc_end_line: line_count,
            });
            anchors.insert((doc_no, path), (start, indent + 2));
            added = true;
        }
    }
    added
}

/// One open JSON container while scanning for key locations. `path` is the
/// prefix for keys inside this container; arrays use 1-based `[n]` elements
/// so repeated leaf names under different parents stay distinct (issue #120).
#[derive(Debug)]
enum JsonFrame {
    Object {
        path: String,
        last_key: Option<String>,
    },
    Array {
        path: String,
        next: usize,
        pending: bool,
    },
}

impl JsonFrame {
    /// Path of the child value that starts here: the last object key, or the
    /// next array element.
    fn child_path(&self) -> String {
        match self {
            Self::Object { path, last_key } => match last_key {
                Some(key) if path.is_empty() => key.clone(),
                Some(key) => format!("{path}.{key}"),
                None => path.clone(),
            },
            Self::Array { path, next, .. } => format!("{path}[{next}]"),
        }
    }
}

/// Records the start of a JSON value. If the current container is an array,
/// that consumes one element index; otherwise the enclosing object's last key
/// supplies the path. Returns the path to use when the value opens a nested
/// container.
fn json_value_path(frames: &mut [JsonFrame]) -> String {
    match frames.last_mut() {
        Some(JsonFrame::Array {
            path,
            next,
            pending,
        }) => {
            let element = format!("{path}[{next}]");
            *next += 1;
            *pending = false;
            element
        }
        Some(frame) => frame.child_path(),
        None => String::new(),
    }
}

/// Structural JSON key paths with source lines. `serde_json` validates the
/// document separately in `config_status`; this bounded character scan only
/// supplies paths and locations, without building an AST or storing values.
/// It handles compact and formatted JSON, arrays and escaped keys.
fn extract_json_keys(text: &str) -> Vec<ConfigKey> {
    let bytes = text.as_bytes();
    let doc_end_line = text.lines().count().max(1) as u32;
    let mut frames: Vec<JsonFrame> = Vec::new();
    let mut keys = Vec::new();
    let mut line = 1u32;
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b' ' | b'\t' | b'\r' => i += 1,
            b'"' => {
                let start_line = line;
                let start = i;
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i = (i + 2).min(bytes.len()),
                        b'"' => {
                            i += 1;
                            break;
                        }
                        b'\n' => {
                            line += 1;
                            i += 1;
                        }
                        _ => i += 1,
                    }
                }
                let token = text.get(start..i).unwrap_or_default();
                let mut lookahead = i;
                while lookahead < bytes.len()
                    && matches!(bytes[lookahead], b' ' | b'\t' | b'\r' | b'\n')
                {
                    if bytes[lookahead] == b'\n' {
                        line += 1;
                    }
                    lookahead += 1;
                }
                if lookahead < bytes.len() && bytes[lookahead] == b':' {
                    // A string followed by ':' is an object key. Decode via
                    // serde_json so escapes never invent a path component.
                    let decoded = serde_json::from_str::<String>(token)
                        .unwrap_or_else(|_| token.trim_matches('"').to_string());
                    let indent = frames.len() * 2;
                    if let Some(JsonFrame::Object { path, last_key }) = frames.last_mut() {
                        let key_path = if path.is_empty() {
                            decoded.clone()
                        } else {
                            format!("{path}.{decoded}")
                        };
                        keys.push(ConfigKey {
                            doc: 1,
                            key_path,
                            parent: path.clone(),
                            indent,
                            start_line,
                            end_line: start_line,
                            doc_end_line,
                        });
                        *last_key = Some(decoded);
                    }
                    i = lookahead + 1;
                } else {
                    json_value_path(&mut frames);
                }
            }
            b'{' | b'[' => {
                let path = json_value_path(&mut frames);
                if bytes[i] == b'{' {
                    frames.push(JsonFrame::Object {
                        path,
                        last_key: None,
                    });
                } else {
                    frames.push(JsonFrame::Array {
                        path,
                        next: 0,
                        pending: true,
                    });
                }
                i += 1;
            }
            b'}' | b']' => {
                frames.pop();
                i += 1;
            }
            b',' => {
                if let Some(JsonFrame::Array { pending, .. }) = frames.last_mut() {
                    *pending = true;
                }
                i += 1;
            }
            b':' => i += 1,
            _ => {
                // Primitive value (number, true/false/null): consume an array
                // slot if needed, then skip the token.
                json_value_path(&mut frames);
                while i < bytes.len()
                    && !matches!(bytes[i], b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n')
                {
                    i += 1;
                }
            }
        }
    }
    keys
}

fn finish_key_ranges(keys: &mut [ConfigKey]) {
    for i in 0..keys.len() {
        // A key's subtree ends at the next key in the same document at the
        // same or a shallower indentation - same-level siblings included,
        // otherwise `alpha` would swallow `beta` (issue #121). Deeper keys
        // stay inside the range, and a document boundary is a hard cap.
        let next = keys
            .iter()
            .skip(i + 1)
            .find(|k| {
                k.doc == keys[i].doc
                    && k.indent <= keys[i].indent
                    && k.start_line > keys[i].start_line
            })
            .map(|k| k.start_line.saturating_sub(1));
        let end = next
            .unwrap_or(keys[i].doc_end_line)
            .min(keys[i].doc_end_line);
        keys[i].end_line = end.max(keys[i].start_line);
    }
}

fn config_status(text: &str, is_json: bool, keys: &[ConfigKey]) -> (ConfigStatus, String) {
    // Deliberately generic: `{{ ... }}` can be Helm, Jinja/Ansible, or any
    // other unrendered template; `${{ ... }}` is common in GitHub Actions.
    // We never render or evaluate any of them. The syntax check comes FIRST:
    // a quoted template-like string is still valid YAML/JSON, so its static
    // keys stay indexed with a `template_syntax` note; only genuinely
    // unparseable template syntax is unsupported (#124).
    // `}}` alone is ordinary JSON (nested object closes), so only the
    // opening marker is consulted.
    let has_template = text.contains("{{");

    // Parse the document before deciding whether template markers matter.
    let parse_error = if is_json {
        serde_json::from_str::<serde_json::Value>(text).is_err()
    } else {
        let docs = serde_yaml::Deserializer::from_str(text);
        docs.into_iter()
            .any(|doc| serde_yaml::Value::deserialize(doc).is_err())
    };

    let mut seen: HashSet<(u32, &str)> = HashSet::new();
    let has_duplicate = keys
        .iter()
        .any(|key| !seen.insert((key.doc, key.key_path.as_str())));

    if parse_error {
        // No parser error text: it can quote source snippets, which may
        // contain secret values we do not want in graph.db. Template markers
        // are the useful compact label when they are the reason the document
        // cannot be parsed at all (#124).
        if has_template {
            return (ConfigStatus::Unsupported, "template_syntax".to_string());
        }
        if is_json {
            return (ConfigStatus::Unsupported, "invalid_json".to_string());
        }
        // A duplicate key is a well-known config pattern, not a reason to
        // throw away the source-linked key locations: keep them as a partial
        // index with an explicit note.
        return if has_duplicate {
            (
                ConfigStatus::Partial,
                "duplicate_key_invalid_yaml".to_string(),
            )
        } else {
            (ConfigStatus::Unsupported, "invalid_yaml".to_string())
        };
    }

    let mut notes = Vec::new();
    if has_duplicate {
        notes.push("duplicate_key".to_string());
    }
    // Valid syntax does not mean the values are renderable: keep the static
    // keys and mark the file so metadata-only callers know the template-like
    // strings are literal text, never evaluated (#124).
    if has_template {
        notes.push("template_syntax".to_string());
    }
    static ALIAS_RE: OnceLock<Regex> = OnceLock::new();
    let alias_re = ALIAS_RE.get_or_init(|| {
        Regex::new(r"(?m)(?:^|\s)[&*][A-Za-z_][A-Za-z0-9_-]*").expect("static alias regex")
    });
    if alias_re.is_match(text) {
        notes.push("anchor_or_alias".to_string());
    }
    static TAG_RE: OnceLock<Regex> = OnceLock::new();
    let tag_re = TAG_RE.get_or_init(|| {
        Regex::new(r#"(?m)(?:^|\s)![A-Za-z_][A-Za-z0-9_-]*"#).expect("static tag regex")
    });
    if tag_re.is_match(text) {
        notes.push("unsupported_tag".to_string());
    }

    if notes.is_empty() {
        (ConfigStatus::Parsed, String::new())
    } else {
        (ConfigStatus::Partial, notes.join(","))
    }
}

fn sanitize_note(note: &str) -> String {
    let cleaned: String = note
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let cleaned = cleaned.trim_matches('_');
    if cleaned.len() > 80 {
        cleaned[..80].trim_matches('_').to_string()
    } else {
        cleaned.to_string()
    }
}

/// Indexes a YAML/JSON config file using the same `File`/`Section` node model
/// as Markdown docs: one File node, one Section per key location, and FTS
/// content containing key paths plus (unless `metadata_only`) raw text.
fn index_config_file(
    path: &Path,
    root: &Path,
    store: &GraphStore,
    config: &ConfigIndexConfig,
) -> Result<(FileIndexResult, ConfigStatus)> {
    let source = read_source_capped(path)?;
    let rel_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    let text = String::from_utf8_lossy(&source).into_owned();
    let line_count = text.lines().count().max(1) as u32;
    let is_json = path.extension().and_then(|e| e.to_str()) == Some("json");

    let mut keys = if is_json {
        extract_json_keys(&text)
    } else {
        let mut keys = extract_yaml_keys(&text);
        // Flow mappings are valid YAML but invisible to a line scanner; fill
        // their key paths from the validated tree before computing ranges
        // and status (#124). The boolean is intentionally unused: complete
        // tree coverage makes a separate "incomplete structure" flag
        // unnecessary; template/alias/tag notes remain the compact signals.
        let _ = augment_yaml_keys_from_tree(&text, &mut keys);
        keys
    };
    finish_key_ranges(&mut keys);
    let (status, note) = config_status(&text, is_json, &keys);
    if status == ConfigStatus::Unsupported {
        // Never invent key nodes for a file we could not honestly parse.
        keys.clear();
    } else {
        // `nodes.qualified_name` is UNIQUE: duplicate keys are reported in
        // the note but represented by their first occurrence only.
        let mut seen: HashSet<(u32, String)> = HashSet::new();
        keys.retain(|key| seen.insert((key.doc, key.key_path.clone())));
    }
    let note_part = if note.is_empty() {
        String::new()
    } else {
        format!("#note={}", sanitize_note(&note))
    };

    let file_id = store.insert_node(NodeKind::File, &rel_path, &rel_path, &rel_path, 0, 0)?;
    // Raw text comes first so its line numbers stay aligned with `Section`
    // ranges; key paths are appended after it as searchable metadata only.
    // In `metadata_only` mode no raw text is stored at all.
    let mut fts = String::new();
    if !config.metadata_only {
        fts.push_str(&text);
        if !fts.ends_with('\n') {
            fts.push('\n');
        }
    }
    for key in &keys {
        fts.push_str(&format!(
            "key_path:{} doc={} status={}\n",
            key.key_path,
            key.doc,
            status.as_str()
        ));
    }
    if keys.is_empty() {
        fts.push_str(&format!("config status={}\n", status.as_str()));
    }
    store.insert_file_content(&rel_path, &fts)?;

    let mut node_ids: HashMap<(u32, String), i64> = HashMap::new();
    if keys.is_empty() {
        let qualified_name = format!(
            "{rel_path}#doc=1#key=<document>#status={}{note_part}",
            status.as_str()
        );
        let id = store.insert_node(
            NodeKind::Section,
            "<document>",
            &qualified_name,
            &rel_path,
            1,
            line_count,
        )?;
        store.insert_edge(file_id, id, EdgeKind::Defines)?;
    }
    for key in &keys {
        let qualified_name = format!(
            "{rel_path}#doc={}#key={}#status={}{note_part}",
            key.doc,
            key.key_path,
            status.as_str()
        );
        let id = store.insert_node(
            NodeKind::Section,
            &key.key_path,
            &qualified_name,
            &rel_path,
            key.start_line,
            key.end_line,
        )?;
        if key.parent.is_empty() {
            store.insert_edge(file_id, id, EdgeKind::Defines)?;
        } else if let Some(parent_id) = node_ids.get(&(key.doc, key.parent.clone())) {
            store.insert_edge(*parent_id, id, EdgeKind::Contains)?;
        } else {
            store.insert_edge(file_id, id, EdgeKind::Defines)?;
        }
        node_ids.insert((key.doc, key.key_path.clone()), id);
    }

    Ok((
        FileIndexResult {
            fn_nodes: Vec::new(),
            pending_calls: Vec::new(),
            reexport_aliases: Vec::new(),
            vault_links: Vec::new(),
        },
        status,
    ))
}

/// Markdown's structural model is headings, not functions/calls - there's
/// no call graph to build here, so this returns the same `FileIndexResult`
/// shape `index_file` does with empty `fn_nodes`/`pending_calls`.
fn index_markdown_file(
    path: &Path,
    root: &Path,
    store: &GraphStore,
    vault_index_enabled: bool,
) -> Result<FileIndexResult> {
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
    // When `[vault_index] enabled = false`, Markdown still gets ordinary
    // Section/FTS indexing exactly as before - only the narrow structural
    // metadata (link targets, tags, aliases) is withheld. No property values,
    // callout/embed bodies, Canvas text or fetched targets are ever stored.
    let vault = if vault_index_enabled {
        crate::docs::extract_vault_note(&text)
    } else {
        crate::docs::VaultNote::default()
    };

    // Tags are global (one `Tag` node per distinct tag, with a stable
    // `tag:`-prefixed qualified name) and aliases are stored alongside the
    // file node so the project-wide link pass can resolve `[[Alias]]`.
    for tag in &vault.tags {
        let tag_id = store.insert_node(NodeKind::Tag, tag, &format!("tag:{tag}"), "", 0, 0)?;
        store.insert_edge(file_id, tag_id, EdgeKind::Tagged)?;
    }
    for alias in &vault.aliases {
        store.insert_vault_alias(alias, file_id)?;
    }

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
        vault_links: vault
            .links
            .into_iter()
            .map(|link| PendingVaultLink {
                src_id: file_id,
                link,
            })
            .collect(),
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
        let result = index_markdown_file(&path, &dir, &store, false).unwrap();

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
        let result = index_markdown_file(&path, &dir, &store, false).unwrap();

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
        let result = index_markdown_file(&path, &dir, &store, false);

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
        let result = index_markdown_file(&path, &dir, &store, false);
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

/// Regression tests for issue #58's remaining gap (see ADR 0014's
/// amendment): `index_directory_checked`'s cooperative-cancellation
/// checkpoint.
#[cfg(test)]
mod index_directory_checked_tests {
    use super::{index_directory, index_directory_checked, SUPERSESSION_CHECK_INTERVAL};
    use crate::graph::GraphStore;
    use std::fs;

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_index_directory_checked_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// More files than one checkpoint interval, so a `superseded` closure
    /// that's always `true` is guaranteed to be observed before the walk
    /// finishes on its own.
    fn write_files(dir: &std::path::Path, count: usize) {
        for i in 0..count {
            fs::write(dir.join(format!("f{i}.rs")), format!("fn f{i}() {{}}\n")).unwrap();
        }
    }

    #[test]
    fn a_pass_marked_superseded_bails_early_and_rolls_back() {
        let dir = temp_project("bails_early");
        write_files(&dir, SUPERSESSION_CHECK_INTERVAL * 2 + 5);

        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        let stats = index_directory_checked(&dir, &store, || true).unwrap();

        assert!(stats.superseded, "must report itself as superseded");
        assert!(
            stats.files_indexed < SUPERSESSION_CHECK_INTERVAL * 2 + 5,
            "must have stopped before walking every file: {}",
            stats.files_indexed
        );
        // Rolled back, not committed - nothing should have landed in the
        // store at all (this project had never been indexed before).
        let (nodes, edges) = store.stats().unwrap();
        assert_eq!((nodes, edges), (0, 0));

        let _ = fs::remove_dir_all(&dir);
    }

    /// A superseded pass must not clobber a previously-good, complete
    /// index with a half-written one - the whole point of rolling back
    /// instead of committing partial work.
    #[test]
    fn a_superseded_pass_leaves_the_previous_complete_index_untouched() {
        let dir = temp_project("preserves_previous");
        write_files(&dir, 3);

        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        index_directory(&dir, &store).unwrap();
        let (nodes_before, edges_before) = store.stats().unwrap();
        assert!(
            nodes_before > 0,
            "the first real pass must have indexed something"
        );

        // A second pass over a bigger tree gets marked superseded partway
        // through - the first pass's committed data must survive intact.
        write_files(&dir, SUPERSESSION_CHECK_INTERVAL * 2 + 5);
        let stats = index_directory_checked(&dir, &store, || true).unwrap();
        assert!(stats.superseded);

        let (nodes_after, edges_after) = store.stats().unwrap();
        assert_eq!((nodes_after, edges_after), (nodes_before, edges_before));

        let _ = fs::remove_dir_all(&dir);
    }

    /// A `superseded` closure that never returns `true` must behave
    /// identically to the plain `index_directory` entry point - the
    /// checkpoint must not change ordinary, uninterrupted behavior.
    #[test]
    fn never_superseded_behaves_like_an_ordinary_full_index() {
        let dir = temp_project("never_superseded");
        write_files(&dir, SUPERSESSION_CHECK_INTERVAL + 3);

        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        let stats = index_directory_checked(&dir, &store, || false).unwrap();

        assert!(!stats.superseded);
        assert_eq!(stats.files_indexed, SUPERSESSION_CHECK_INTERVAL + 3);
        let (nodes, _) = store.stats().unwrap();
        assert!(nodes > 0);

        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod config_index_tests {
    use super::{content_signature_with_config, index_directory_with_config};
    use crate::graph::GraphStore;
    use nexus_core::ConfigIndexConfig;
    use std::fs;

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_config_index_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("secrets")).unwrap();
        dir
    }

    fn enabled() -> ConfigIndexConfig {
        ConfigIndexConfig {
            enabled: true,
            ..ConfigIndexConfig::default()
        }
    }

    fn write_fixture(dir: &std::path::Path) {
        fs::write(
            dir.join("values.yaml"),
            "---\ntelemetry:\n  enabled: true\n  endpoint: example.invalid\n---\nreplicas: 3\n",
        )
        .unwrap();
        fs::write(
            dir.join("config.json"),
            "{\"feature_flags\": {\"beta\": true}}\n",
        )
        .unwrap();
    }

    #[test]
    fn disabled_by_default_leaves_config_files_out_of_the_index() {
        let dir = temp_project("disabled");
        write_fixture(&dir);
        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        let stats =
            index_directory_with_config(&dir, &store, &ConfigIndexConfig::default()).unwrap();
        assert_eq!(stats.config_files_indexed, 0);
        assert!(store.search_by_name("telemetry", 10).unwrap().is_empty());
        assert!(store.search_code("example.invalid", 10).unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn enabled_indexes_documents_key_paths_and_source_ranges() {
        let dir = temp_project("enabled");
        write_fixture(&dir);
        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        let stats = index_directory_with_config(&dir, &store, &enabled()).unwrap();
        assert_eq!(stats.config_files_indexed, 2);

        let keys = store.search_by_name("telemetry.enabled", 10).unwrap();
        assert_eq!(keys.len(), 1, "expected one nested key node: {keys:?}");
        assert!(keys[0].qualified_name.contains("#doc=1#"));
        assert!(keys[0].qualified_name.contains("#status=parsed"));
        // `enabled`'s subtree is line 3 only; line 4 is its same-level
        // sibling `endpoint`, not part of the range (issue #121).
        assert_eq!((keys[0].start_line, keys[0].end_line), (3, 3));

        let replicas = store.search_by_name("replicas", 10).unwrap();
        assert_eq!(replicas.len(), 1);
        assert!(replicas[0].qualified_name.contains("#doc=2#"));
        assert_eq!(replicas[0].start_line, 6);

        let hits = store.search_code("telemetry.enabled", 10).unwrap();
        assert_eq!(hits.len(), 1, "key path must be searchable: {hits:?}");
        assert_eq!(hits[0].key_path.as_deref(), Some("telemetry.enabled"));
        assert_eq!(hits[0].document, Some(1));
        assert_eq!(hits[0].parse_status.as_deref(), Some("parsed"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn exclude_wins_over_include_before_any_content_is_read() {
        let dir = temp_project("exclude");
        write_fixture(&dir);
        fs::write(
            dir.join("secrets/creds.yaml"),
            "password: top-secret-value-112\n",
        )
        .unwrap();
        let config = ConfigIndexConfig {
            exclude: vec!["**/secrets/**".to_string()],
            ..enabled()
        };
        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        let stats = index_directory_with_config(&dir, &store, &config).unwrap();
        assert_eq!(stats.config_files_excluded, 1);
        assert!(store
            .search_code("top-secret-value-112", 10)
            .unwrap()
            .is_empty());
        // Exclusions happen before ingestion, so the secret must not appear
        // in graph.db or its WAL/shm sidecars either - only in the source
        // file that the exclusion protected.
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.file_name().and_then(|n| n.to_str()) == Some("secrets") {
                continue;
            }
            for candidate in [
                path.clone(),
                path.with_extension("db-wal"),
                path.with_extension("db-shm"),
            ] {
                if let Ok(bytes) = fs::read(&candidate) {
                    assert!(
                        !bytes.windows(20).any(|w| w == b"top-secret-value-112"),
                        "excluded secret leaked into {}",
                        candidate.display()
                    );
                }
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn metadata_only_stores_no_raw_values() {
        let dir = temp_project("metadata_only");
        write_fixture(&dir);
        let config = ConfigIndexConfig {
            metadata_only: true,
            ..enabled()
        };
        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        index_directory_with_config(&dir, &store, &config).unwrap();
        assert!(store.search_code("example.invalid", 10).unwrap().is_empty());
        let hits = store.search_code("telemetry.enabled", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("key_path:telemetry.enabled"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nested_json_keys_keep_distinct_paths_and_locations() {
        let dir = temp_project("json_nesting");
        fs::write(
            dir.join("nested.json"),
            "{\"left\":{\"enabled\":true},\"right\":{\"enabled\":false}}\n",
        )
        .unwrap();
        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        let stats = index_directory_with_config(&dir, &store, &enabled()).unwrap();
        assert_eq!(stats.config_files_indexed, 1);
        assert_eq!(
            stats.config_files_partial, 0,
            "same leaf under two parents is not a duplicate"
        );

        for path in ["left.enabled", "right.enabled"] {
            let nodes = store.search_by_name(path, 10).unwrap();
            assert_eq!(nodes.len(), 1, "missing nested path {path}: {nodes:?}");
            assert!(nodes[0].qualified_name.contains("#status=parsed"));
            assert!(!nodes[0].qualified_name.contains("#note=duplicate_key"));
        }
        let hits = store.search_code("left.enabled", 10).unwrap();
        assert_eq!(hits.len(), 1, "nested key must be searchable: {hits:?}");
        assert_eq!(hits[0].key_path.as_deref(), Some("left.enabled"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn genuine_json_duplicates_are_still_flagged() {
        let dir = temp_project("json_duplicate");
        fs::write(
            dir.join("dup.json"),
            "{\"service\":{\"port\":1},\"service\":{\"port\":2}}\n",
        )
        .unwrap();
        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        let stats = index_directory_with_config(&dir, &store, &enabled()).unwrap();
        assert_eq!(stats.config_files_partial, 1);
        let nodes = store.search_by_name("service.port", 10).unwrap();
        assert!(!nodes.is_empty());
        assert!(
            nodes
                .iter()
                .all(|node| node.qualified_name.contains("#note=duplicate_key")),
            "real same-object duplicates must stay visible: {nodes:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn yaml_block_scalar_text_never_becomes_a_key() {
        let dir = temp_project("yaml_scalar");
        fs::write(
            dir.join("block.yaml"),
            "message: |\n  phantom: this is scalar text\nreal:\n  enabled: true\n",
        )
        .unwrap();
        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        index_directory_with_config(&dir, &store, &enabled()).unwrap();
        assert!(store
            .search_by_name("message.phantom", 10)
            .unwrap()
            .is_empty());
        assert!(store
            .search_code("phantom", 10)
            .unwrap()
            .iter()
            .all(|hit| hit.key_path.as_deref() != Some("message.phantom")));
        assert_eq!(store.search_by_name("real.enabled", 10).unwrap().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn yaml_ranges_stop_at_same_level_siblings_and_keep_indentation() {
        let dir = temp_project("yaml_ranges");
        fs::write(
            dir.join("ranges.yaml"),
            "alpha:\n  enabled: true\nbeta:\n  count: 2\n",
        )
        .unwrap();
        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        index_directory_with_config(&dir, &store, &enabled()).unwrap();
        let alpha: Vec<_> = store
            .search_by_name("alpha", 10)
            .unwrap()
            .into_iter()
            .filter(|node| node.name == "alpha")
            .collect();
        assert_eq!(alpha.len(), 1);
        assert_eq!((alpha[0].start_line, alpha[0].end_line), (1, 2));

        let hits = store.search_code("alpha", 10).unwrap();
        let hit = hits
            .iter()
            .find(|hit| hit.key_path.as_deref() == Some("alpha"))
            .expect("alpha hit");
        assert!(
            !hit.snippet.contains("beta"),
            "sibling leaked: {}",
            hit.snippet
        );
        assert!(
            hit.snippet.contains("  enabled: true"),
            "indentation was stripped: {:?}",
            hit.snippet
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_code_pages_hits_without_duplicates_or_gaps() {
        let dir = temp_project("search_pages");
        // One matching file expands into four config key hits: paging must
        // split hits, not just files (issue #123).
        fs::write(
            dir.join("page.yaml"),
            "page:\n  alpha: 1\n  beta: 2\n  gamma: 3\n",
        )
        .unwrap();
        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        index_directory_with_config(&dir, &store, &enabled()).unwrap();

        let mut paged = Vec::new();
        for offset in 0..4 {
            let page = store.search_code_page("page", offset, 1).unwrap();
            assert_eq!(page.hits.len(), 1, "offset {offset}");
            assert_eq!(page.has_more, offset < 3, "offset {offset}");
            paged.push(page.hits[0].key_path.clone().unwrap());
        }
        let exhausted = store.search_code_page("page", 4, 1).unwrap();
        assert!(exhausted.hits.is_empty());
        assert!(!exhausted.has_more);

        let mut unique = paged.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            paged.len(),
            "duplicate across pages: {paged:?}"
        );

        let mut whole: Vec<_> = store
            .search_code("page", 10)
            .unwrap()
            .into_iter()
            .map(|hit| hit.key_path.unwrap())
            .collect();
        whole.sort_unstable();
        assert_eq!(whole, unique, "paged results differ from one full search");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn quoted_template_string_keeps_static_keys_and_marks_the_file() {
        let dir = temp_project("quoted_template");
        fs::write(
            dir.join("templated.yaml"),
            "stable:\n  enabled: true\nmessage: \"{{ .Status }}\"\n",
        )
        .unwrap();
        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        let stats = index_directory_with_config(&dir, &store, &enabled()).unwrap();
        assert_eq!(stats.config_files_partial, 1);
        assert_eq!(stats.config_files_unsupported, 0);

        let hits = store.search_code("stable", 10).unwrap();
        assert!(
            hits.iter()
                .any(|hit| hit.key_path.as_deref() == Some("stable.enabled")),
            "static keys must survive a quoted template string: {hits:?}"
        );
        let nodes = store.search_by_name("stable.enabled", 10).unwrap();
        assert!(nodes.iter().any(|node| {
            node.qualified_name.contains("#status=partial")
                && node.qualified_name.contains("#note=template_syntax")
        }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flow_mapping_keys_are_indexed_from_the_validated_tree() {
        let dir = temp_project("flow_mapping");
        fs::write(
            dir.join("flow.yaml"),
            "outer: {inner: true, nested: {deep: 1}}\nlist: [{name: first}]\n",
        )
        .unwrap();
        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        let stats = index_directory_with_config(&dir, &store, &enabled()).unwrap();
        assert_eq!(stats.config_files_indexed, 1);
        assert_eq!(stats.config_files_partial, 0);

        for path in ["outer.inner", "outer.nested.deep", "list[1].name"] {
            let hits = store.search_code(path, 10).unwrap();
            assert!(
                hits.iter().any(|hit| hit.key_path.as_deref() == Some(path)),
                "missing flow key {path}: {hits:?}"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn genuinely_unparseable_template_remains_unsupported() {
        let (status, note) = super::config_status("bad: {{ .Values.host }", false, &[]);
        assert_eq!(status.as_str(), "unsupported");
        assert_eq!(note, "template_syntax");
    }

    #[test]
    fn partial_duplicates_and_unparseable_templates_are_labelled() {
        let dir = temp_project("states");
        fs::write(
            dir.join("duplicate.yaml"),
            "service:\n  port: 80\nservice:\n  port: 8080\n",
        )
        .unwrap();
        // Valid YAML: the template-like text is a flow mapping, so its static
        // keys survive with a partial/template_syntax label (#124).
        fs::write(dir.join("template.yaml"), "image: {{ .Values.image }}\n").unwrap();
        // Genuinely unparseable: no key nodes, literal fallback only.
        fs::write(dir.join("broken.yaml"), "bad: {{ .Values.host }").unwrap();
        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        let stats = index_directory_with_config(&dir, &store, &enabled()).unwrap();
        assert_eq!(stats.config_files_partial, 2);
        assert_eq!(stats.config_files_unsupported, 1);

        let duplicate: Vec<_> = store
            .search_by_name("service", 10)
            .unwrap()
            .into_iter()
            .filter(|node| node.name == "service")
            .collect();
        assert_eq!(duplicate.len(), 1, "duplicate keys collapse to one node");
        assert!(duplicate[0].qualified_name.contains("#status=partial"));
        assert!(duplicate[0].qualified_name.contains("duplicate_key"));

        let template: Vec<_> = store
            .search_by_name("image", 10)
            .unwrap()
            .into_iter()
            .filter(|node| node.name == "image")
            .collect();
        assert_eq!(template.len(), 1);
        assert!(template[0].qualified_name.contains("#status=partial"));
        assert!(template[0].qualified_name.contains("template_syntax"));

        let broken = store.search_by_name("<document>", 10).unwrap();
        assert_eq!(broken.len(), 1);
        assert!(broken[0].qualified_name.contains("#status=unsupported"));
        assert!(broken[0].qualified_name.contains("template_syntax"));
        // Both files remain searchable as bounded literal text.
        let hits = store.search_code(".Values.image", 10).unwrap();
        assert!(!hits.is_empty());
        assert!(hits
            .iter()
            .all(|h| h.parse_status.as_deref() == Some("partial")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn signature_tracks_config_policy_and_config_files() {
        let dir = temp_project("signature");
        write_fixture(&dir);
        fs::write(dir.join("secrets/creds.yaml"), "password: first-value\n").unwrap();
        let disabled = ConfigIndexConfig::default();
        let enabled_cfg = enabled();
        let excluded = ConfigIndexConfig {
            exclude: vec!["**/secrets/**".to_string()],
            ..enabled()
        };

        let before = content_signature_with_config(&dir, &disabled);
        fs::write(dir.join("values.yaml"), "replicas: 9\n").unwrap();
        assert_eq!(
            before,
            content_signature_with_config(&dir, &disabled),
            "disabled config indexing must not wake the watcher for YAML"
        );
        assert_ne!(
            content_signature_with_config(&dir, &enabled_cfg),
            content_signature_with_config(&dir, &excluded),
            "changing config_index policy must be observable"
        );

        let before_excluded = content_signature_with_config(&dir, &excluded);
        fs::write(dir.join("secrets/creds.yaml"), "password: second-value\n").unwrap();
        assert_eq!(
            before_excluded,
            content_signature_with_config(&dir, &excluded)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn out_of_root_symlink_is_not_indexed() {
        let dir = temp_project("symlink");
        let outside =
            std::env::temp_dir().join(format!("nexus_config_index_outside_{}", std::process::id()));
        fs::write(&outside, "outside_key: outside-secret-value\n").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("linked.yaml")).unwrap();

        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        let stats = index_directory_with_config(&dir, &store, &enabled()).unwrap();
        assert_eq!(stats.config_files_indexed, 0);
        assert!(store
            .search_code("outside-secret-value", 10)
            .unwrap()
            .is_empty());
        let _ = fs::remove_file(&outside);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn arrays_and_quoted_keys_get_distinct_source_linked_paths() {
        let dir = temp_project("arrays");
        fs::write(
            dir.join("arrays.yaml"),
            "servers:\n  - \"host name\": a.example\n    port: 80\n  - \"host name\": b.example\n    port: 443\n",
        )
        .unwrap();
        let store = GraphStore::open(&dir.join("graph.db")).unwrap();
        index_directory_with_config(&dir, &store, &enabled()).unwrap();

        for expected in [
            "servers[1].host name",
            "servers[1].port",
            "servers[2].host name",
            "servers[2].port",
        ] {
            let nodes = store.search_by_name(expected, 10).unwrap();
            assert!(
                nodes.iter().any(|node| node.name == expected),
                "missing array/quoted key path {expected}: {nodes:?}"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
