//! LSP-resolved-symbol enrichment (issue #10) - runs *after* the normal
//! tree-sitter ingest pass, only on an explicit `deep` reindex, and only
//! ever adds `CallsResolved` edges alongside whatever the static pass
//! already found. Never touches, removes, or replaces a `Calls` edge, and
//! never fails a reindex: every failure mode here (server missing, a crash
//! mid-pass, a request timing out) degrades to "fewer resolved edges than
//! hoped," reported back in `EnrichmentReport`, not a propagated error -
//! see `project::index_project_deep`, the only caller.

use crate::graph::{EdgeKind, GraphStore, NodeKind};
use crate::lsp::LspClient;
use nexus_core::LspConfig;
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct EnrichmentReport {
    pub ran: bool,
    pub functions_queried: usize,
    pub resolved_edges_added: usize,
    pub duration_ms: u64,
    /// Present whenever `ran` is false, or when the pass ended early (a
    /// crashed server, the overall deadline) rather than completing every
    /// function - #40's own follow-up wants this data logged, not just a
    /// silent partial result.
    pub note: Option<String>,
}

/// Total wall-clock budget for one enrichment run, independent of
/// `request_timeout_secs` - bounds the *whole* pass (handshake + settle +
/// every reference query), not just each individual request, so a project
/// with many functions can't turn "explicit opt-in deep reindex" into
/// "reindex that might never return." Generous because `--deep` is already
/// an explicit, occasional choice, not something on the watcher's ordinary
/// auto-reindex path.
const OVERALL_BUDGET: Duration = Duration::from_secs(180);

/// How long to wait, after the handshake, for rust-analyzer's initial
/// workspace indexing to go quiet before the first real request - see
/// `LspClient::wait_until_idle`.
const SETTLE_BUDGET: Duration = Duration::from_secs(60);
const SETTLE_QUIET_FOR: Duration = Duration::from_millis(800);

/// Process-wide cap on concurrently-live LSP server child processes,
/// enforced here via a counting semaphore (`acquire`/`release` below) - the
/// "needs a cap" lesson from issue #8 (inotify exhaustion), applied to
/// this feature's own most memory-heavy resident cost per the PR review.
///
/// Honest scope note: this bounds concurrency *within one process*
/// (`nexusd mcp`, `nexus-cli`, or `nexusd serve`'s own control-method
/// handler - whichever one a `deep` reindex happens to run in), not a
/// server *reused across separate reindex calls* the way the review's
/// "day one" framing originally asked for. Real cross-call reuse would
/// mean routing every `deep` reindex through `nexusd serve`'s control
/// socket instead of running in-process wherever the request landed - a
/// real architectural change in tension with this project's "MCP tools
/// work without `nexusd serve` running at all" design principle, not a
/// one-line addition. Deferred rather than silently dropped: each `--deep`
/// call still pays rust-analyzer's own startup cost every time, capped so
/// at most `max_concurrent_servers` can be paying it at once.
static LSP_SLOTS: std::sync::OnceLock<(std::sync::Mutex<usize>, std::sync::Condvar)> =
    std::sync::OnceLock::new();

struct SlotGuard {
    slots: &'static (std::sync::Mutex<usize>, std::sync::Condvar),
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let (lock, cvar) = self.slots;
        let mut used = lock.lock().unwrap_or_else(|e| e.into_inner());
        *used = used.saturating_sub(1);
        cvar.notify_one();
    }
}

fn acquire_slot(max_concurrent: usize, deadline: Instant) -> Option<SlotGuard> {
    let slots = LSP_SLOTS.get_or_init(|| (std::sync::Mutex::new(0), std::sync::Condvar::new()));
    let (lock, cvar) = slots;
    let mut used = lock.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if *used < max_concurrent.max(1) {
            *used += 1;
            return Some(SlotGuard { slots });
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let (guard, timeout_result) = cvar
            .wait_timeout(used, remaining)
            .unwrap_or_else(|e| e.into_inner());
        used = guard;
        if timeout_result.timed_out() && *used >= max_concurrent.max(1) {
            return None;
        }
    }
}

pub fn enrich_with_lsp(root: &Path, store: &GraphStore, config: &LspConfig) -> EnrichmentReport {
    if !config.enabled {
        return EnrichmentReport {
            note: Some("lsp.enabled is false".to_string()),
            ..Default::default()
        };
    }

    let start = Instant::now();
    let overall_deadline = start + OVERALL_BUDGET;

    let Some(_slot) = acquire_slot(config.max_concurrent_servers, overall_deadline) else {
        return EnrichmentReport {
            note: Some(format!(
                "timed out waiting for an LSP server slot (max_concurrent_servers = {})",
                config.max_concurrent_servers
            )),
            duration_ms: start.elapsed().as_millis() as u64,
            ..Default::default()
        };
    };

    let mut client = match LspClient::spawn(&config.server_command, root) {
        Ok(c) => c,
        Err(err) => {
            return EnrichmentReport {
                note: Some(format!(
                    "failed to start LSP server '{}': {err:#} - falling back to the static index only",
                    config.server_command
                )),
                duration_ms: start.elapsed().as_millis() as u64,
                ..Default::default()
            };
        }
    };

    client.wait_until_idle(
        overall_deadline.min(start + SETTLE_BUDGET),
        SETTLE_QUIET_FOR,
    );

    let report = run_enrichment(&mut client, root, store, config, overall_deadline);
    client.shutdown();

    EnrichmentReport {
        duration_ms: start.elapsed().as_millis() as u64,
        ..report
    }
}

fn run_enrichment(
    client: &mut LspClient,
    root: &Path,
    store: &GraphStore,
    config: &LspConfig,
    overall_deadline: Instant,
) -> EnrichmentReport {
    let all_nodes = match store.all_nodes() {
        Ok(n) => n,
        Err(err) => {
            return EnrichmentReport {
                ran: true,
                note: Some(format!("failed to read indexed nodes: {err:#}")),
                ..Default::default()
            };
        }
    };

    // file_path -> [(start_line, end_line, node_id)], 1-based inclusive -
    // for mapping a reference's location back to whichever indexed
    // Function contains it. Only Rust files: this pilot is rust-analyzer
    // only, and a multi-language project's non-Rust functions have nothing
    // for it to resolve against anyway.
    let mut ranges_by_file: HashMap<&str, Vec<(u32, u32, i64)>> = HashMap::new();
    let mut functions = Vec::new();
    for node in &all_nodes {
        if !node.file_path.ends_with(".rs") {
            continue;
        }
        if node.kind == NodeKind::Function {
            functions.push(node);
        }
        ranges_by_file
            .entry(node.file_path.as_str())
            .or_default()
            .push((node.start_line, node.end_line, node.id));
    }

    // Word-boundary match so e.g. a function named `run` doesn't match
    // inside `run_all` on the same definition line - best-effort: if a
    // definition line genuinely doesn't contain its own name as a whole
    // word (shouldn't happen for a real `fn` line), that one function is
    // just skipped rather than guessing a wrong column.
    let name_pattern_cache: HashMap<&str, Regex> = HashMap::new();
    let mut name_pattern_cache = name_pattern_cache;

    let request_timeout = Duration::from_secs(config.request_timeout_secs);
    let mut opened_files: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut functions_queried = 0usize;
    let mut resolved_edges_added = 0usize;
    let mut ended_early = false;

    for func in functions {
        if Instant::now() >= overall_deadline {
            ended_early = true;
            break;
        }

        let Ok(file_text) = std::fs::read_to_string(root.join(&func.file_path)) else {
            continue; // file gone/unreadable since the static pass ran - skip, not fatal
        };
        let Some(start_line_text) = file_text.lines().nth((func.start_line - 1) as usize) else {
            continue;
        };

        let pattern = name_pattern_cache
            .entry(func.name.as_str())
            .or_insert_with(|| {
                Regex::new(&format!(r"\b{}\b", regex::escape(&func.name)))
                    .expect("escaped literal is always a valid regex")
            });
        let Some(m) = pattern.find(start_line_text) else {
            continue;
        };
        let character = start_line_text[..m.start()].chars().count() as u32;

        let file_uri = format!("file://{}", root.join(&func.file_path).display());
        if opened_files.insert(func.file_path.as_str()) {
            let _ = client.did_open(&file_uri, "rust", &file_text);
        }

        functions_queried += 1;
        let Ok(refs) = references_with_retry(
            client,
            &file_uri,
            func.start_line - 1,
            character,
            request_timeout,
        ) else {
            continue; // one bad/timed-out request doesn't stop the whole pass
        };

        for reference in refs {
            let rel = reference
                .file
                .strip_prefix(&format!("{}/", root.display()))
                .unwrap_or(&reference.file);
            let Some(ranges) = ranges_by_file.get(rel) else {
                continue; // reference in a file this project didn't index (a dependency, std, ...)
            };
            let ref_line_1based = reference.line + 1;
            let Some(&(_, _, caller_id)) = ranges
                .iter()
                .find(|(s, e, _)| *s <= ref_line_1based && ref_line_1based <= *e)
            else {
                continue; // reference isn't inside any indexed Function's range
            };
            if store
                .insert_edge(caller_id, func.id, EdgeKind::CallsResolved)
                .is_ok()
            {
                resolved_edges_added += 1;
            }
        }
    }

    EnrichmentReport {
        ran: true,
        functions_queried,
        resolved_edges_added,
        duration_ms: 0, // filled in by the caller, which has the real start time
        note: ended_early.then(|| "hit the overall time budget before every function was queried - resolved edges found so far were kept".to_string()),
    }
}

/// rust-analyzer answers `textDocument/references` with LSP error -32801
/// ("content modified") whenever its analysis snapshot changes out from
/// under an in-flight request - normal right after startup, while it's
/// still settling from the initial workspace load `wait_until_idle`
/// approximated rather than precisely tracked. A few short retries clears
/// this in practice; a request that keeps failing for a different reason
/// still only costs this many attempts before `enrich_with_lsp` moves on.
fn references_with_retry(
    client: &mut LspClient,
    file_uri: &str,
    line: u32,
    character: u32,
    timeout: Duration,
) -> anyhow::Result<Vec<crate::lsp::ReferenceLocation>> {
    const MAX_ATTEMPTS: u32 = 4;
    let mut last_err = None;
    for attempt in 0..MAX_ATTEMPTS {
        match client.references(file_uri, line, character, timeout) {
            Ok(refs) => return Ok(refs),
            Err(err) => {
                let retryable = err.to_string().contains("-32801");
                last_err = Some(err);
                if !retryable || attempt + 1 == MAX_ATTEMPTS {
                    break;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("references request failed")))
}

/// Regression tests for issue #10, run against a *real* rust-analyzer, not
/// a mock - per the PR review's "detect_dead_code is the highest-value,
/// lowest-cost proof" guidance, this is the honest proof of value: a
/// concrete cross-file case the static, name-based pass gets wrong (flags
/// as dead a function that's genuinely called), which LSP resolution
/// fixes. `#[ignore]`d since it needs a real `rust-analyzer` on PATH (or
/// `NEXUS_TEST_RUST_ANALYZER` pointing at one) and takes several seconds -
/// not something every `cargo test` run should pay for, but a real,
/// self-contained check for whoever has the toolchain to run it
/// (`cargo test -p nexus-index --lib -- --ignored enrich::`). CI doesn't
/// have rust-analyzer installed, so this doesn't currently run there - see
/// the crate's Cargo.toml / README for that honestly-stated gap. Skips
/// (doesn't fail) if no server is found, consistent with this whole
/// feature's degrade-cleanly contract.
#[cfg(test)]
mod real_rust_analyzer_tests {
    use super::*;
    use crate::ingest::index_directory;
    use std::fs;

    /// Two files each define a function named `helper` - the static
    /// tree-sitter pass's own documented limitation (`ingest.rs`) is that a
    /// cross-file call only resolves when the callee name is unique
    /// project-wide, so `b::helper()` called from `a.rs` is left
    /// unresolved and `b.rs`'s `helper` shows up as a dead-code false
    /// positive, even though it's genuinely called. A real LSP server
    /// resolves the qualified path correctly.
    fn write_fixture(root: &std::path::Path) {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"lspfixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [[bin]]\nname = \"a\"\npath = \"src/a.rs\"\n",
        )
        .unwrap();
        fs::write(
            root.join("src/a.rs"),
            "mod b;\n\npub fn helper() {\n    println!(\"a\");\n}\n\n\
             fn main() {\n    helper();\n    b::helper();\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/b.rs"),
            "pub fn helper() {\n    println!(\"b\");\n}\n",
        )
        .unwrap();
    }

    fn server_command() -> String {
        std::env::var("NEXUS_TEST_RUST_ANALYZER").unwrap_or_else(|_| "rust-analyzer".to_string())
    }

    fn server_available(command: &str) -> bool {
        // `.status.success()`, not just `.is_ok()` - a rustup shim for an
        // uninstalled `rust-analyzer` component spawns fine and exits
        // non-zero with "Unknown binary", which `Output::is_ok()` alone
        // wouldn't catch.
        std::process::Command::new(command)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[test]
    #[ignore]
    fn lsp_enrichment_finds_a_cross_file_call_the_static_pass_misses_and_undeadens_it() {
        let command = server_command();
        if !server_available(&command) {
            eprintln!(
                "skipping: no '{command}' on PATH - set NEXUS_TEST_RUST_ANALYZER to run this test"
            );
            return;
        }

        let root =
            std::env::temp_dir().join(format!("nexus_lsp_enrich_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write_fixture(&root);

        let store = GraphStore::open(&root.join("graph.db")).unwrap();
        index_directory(&root, &store).unwrap();

        let dead_before: Vec<String> = store
            .dead_functions()
            .unwrap()
            .into_iter()
            .map(|f| f.qualified_name)
            .collect();
        assert!(
            dead_before.iter().any(|n| n.contains("b.rs::helper")),
            "test assumption broken: the static pass should flag b.rs's helper as dead \
             before enrichment (got {dead_before:?}) - if it no longer does, this test isn't \
             proving what it claims to"
        );

        let config = LspConfig {
            enabled: true,
            server_command: command,
            max_concurrent_servers: 1,
            request_timeout_secs: 20,
        };
        let report = enrich_with_lsp(&root, &store, &config);
        assert!(report.ran, "enrichment should have run: {report:?}");
        assert!(
            report.resolved_edges_added > 0,
            "expected at least one CALLS_RESOLVED edge: {report:?}"
        );

        let dead_after: Vec<String> = store
            .dead_functions()
            .unwrap()
            .into_iter()
            .map(|f| f.qualified_name)
            .collect();
        assert!(
            !dead_after.iter().any(|n| n.contains("b.rs::helper")),
            "b.rs's helper should no longer be flagged dead after LSP enrichment resolved its \
             real caller, but got {dead_after:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_server_degrades_to_the_static_index_without_failing() {
        // The other half of the degrade-cleanly contract, and the one that
        // *does* run everywhere (no real toolchain needed) - a
        // nonexistent binary must never turn into a failed reindex.
        let root = std::env::temp_dir().join(format!(
            "nexus_lsp_enrich_missing_server_test_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        write_fixture(&root);

        let store = GraphStore::open(&root.join("graph.db")).unwrap();
        index_directory(&root, &store).unwrap();
        let dead_before = store.dead_functions().unwrap().len();

        let config = LspConfig {
            enabled: true,
            server_command: "nexus-nonexistent-lsp-binary-xyz".to_string(),
            max_concurrent_servers: 1,
            request_timeout_secs: 5,
        };
        let report = enrich_with_lsp(&root, &store, &config);
        assert!(!report.ran);
        assert!(report.note.is_some(), "should explain why it didn't run");
        assert_eq!(report.resolved_edges_added, 0);

        // The static index itself is completely unaffected.
        assert_eq!(store.dead_functions().unwrap().len(), dead_before);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn disabled_config_skips_without_even_trying_to_spawn_anything() {
        let root = std::env::temp_dir().join(format!(
            "nexus_lsp_enrich_disabled_test_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        write_fixture(&root);

        let store = GraphStore::open(&root.join("graph.db")).unwrap();
        index_directory(&root, &store).unwrap();

        let config = LspConfig {
            enabled: false,
            ..LspConfig::default()
        };
        let report = enrich_with_lsp(&root, &store, &config);
        assert!(!report.ran);
        assert_eq!(report.functions_queried, 0);

        let _ = fs::remove_dir_all(&root);
    }
}
