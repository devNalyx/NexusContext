use anyhow::{anyhow, bail, Result};
use nexus_core::{Config, Paths, Registry};
use nexus_index::{
    delete_project, export_project, get_architecture, graph_db_path, import_project, index_project,
    project_disk_usage, GraphStore,
};
use nexus_memory::{InjectionRecord, Ledger, UsageTokens};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

/// Control API for the GUI/GNOME extension, not for MCP clients - same
/// JSON-RPC framing as the stdio transport, but a distinct method
/// namespace (`status.*`, `config.*`, `projects.*`, `search.*`) and a
/// different transport (Unix domain socket instead of stdio), so a GUI
/// session never competes with whatever MCP client is attached to stdio.
pub fn serve(socket_path: PathBuf) -> Result<()> {
    let listener = bind_socket(&socket_path)?;
    tracing::info!(socket = %socket_path.display(), "control API listening");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(move || {
                    if let Err(err) = handle_connection(stream) {
                        tracing::warn!(error = %err, "control connection error");
                    }
                });
            }
            Err(err) => tracing::warn!(error = %err, "failed to accept control connection"),
        }
    }
    Ok(())
}

/// Removes a stale socket file, ensures the parent directory exists, binds,
/// then locks the socket file itself down to owner-only.
///
/// The control API has no authentication of its own - today's only real
/// protection is that `socket_path`'s
/// parent directory (`$XDG_RUNTIME_DIR/nexuscontext` under the systemd user
/// service default) is itself 0700, owner-only. `UnixListener::bind` has no
/// way to create the socket file with a restrictive mode atomically, so the
/// `set_permissions` call below is a second, explicit layer rather than
/// relying solely on an inherited directory-permission convention - defends
/// the case where `socket_path` ever lives somewhere that convention doesn't
/// hold (a custom `NEXUS_CACHE_DIR`, a non-systemd init). There's a small
/// unavoidable race between `bind` and `set_permissions` where the socket is
/// briefly at the process umask's mode instead - acceptable since the
/// directory permission already covers that window in the default
/// deployment. See issue #23.
///
/// If `set_permissions` itself fails, the socket is already bound and
/// `?` propagates the error up through `serve()` without unbinding it -
/// harmless in practice (the process is exiting on that error path anyway,
/// and the next `serve()` call's stale-file removal above cleans it up),
/// just not a fully symmetric rollback.
fn bind_socket(socket_path: &std::path::Path) -> Result<UnixListener> {
    if socket_path.exists() {
        std::fs::remove_file(socket_path)?;
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(socket_path)?;
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn handle_connection(stream: UnixStream) -> Result<()> {
    let reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(error = %err, "failed to parse control request, ignoring");
                continue;
            }
        };

        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        let response = match dispatch(&method, params) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(err) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32000, "message": err.to_string() }
            }),
        };

        writeln!(writer, "{}", serde_json::to_string(&response)?)?;
        writer.flush()?;
    }
    Ok(())
}

fn dispatch(method: &str, params: Value) -> Result<Value> {
    tracing::debug!(method, "control request received");
    // Same "actually used" signal as the MCP tool-call path in tools.rs -
    // the GUI talks to this control API, not the MCP tool list, so without
    // this the registry would only ever see last_queried_unix move for
    // MCP-driven usage and never for someone just using the GUI directly.
    //
    // Deliberately touch_queried only, NOT nexus_index::touch_and_catchup -
    // a v0.1.11 regression, found live within hours of release, briefly had
    // this call the full catch-up-if-cold reindex here too. That's wrong
    // for this transport specifically: nexus-gui's client.rs calls this
    // synchronously on GTK's main thread, on the documented assumption that
    // a control-socket round-trip is sub-millisecond (see its own comment).
    // A catch-up reindex against a cold project can take minutes - blocking
    // the GTK main thread for that long freezes the whole app, and the
    // window manager/compositor kills it as unresponsive well before a
    // reindex like that finishes. tools.rs's MCP dispatch and the CLI don't
    // have this problem (a slow tool call or a CLI command blocking is
    // normal, expected UX, not a frozen persistent GUI event loop), so only
    // this transport needed to be reverted.
    if method != "projects.delete" {
        if let Some(repo_path) = params.get("repo_path").and_then(|v| v.as_str()) {
            nexus_index::touch_queried(std::path::Path::new(repo_path));
        }
    }
    let call_start = std::time::Instant::now();
    let result = match method {
        "status.get" => status_get(),
        "projects.list" => projects_list(),
        "projects.reindex" => projects_reindex(params),
        "projects.delete" => projects_delete(params),
        "projects.export" => projects_export(params),
        "projects.import" => projects_import(params),
        "projects.architecture" => projects_architecture(params),
        "config.get" => config_get(),
        "config.set" => config_set(params),
        "search.adhoc" => search_adhoc(params),
        "viz.call_graph" => viz_call_graph(params),
        "stats.get" => stats_get(),
        // Flagship plug + GUI read paths. The daemon performs recall and
        // records it itself: a plugin cannot report an injection it never
        // made, or re-label an estimate as provider tokens.
        "review.list" => review_list(params),
        "memory.recall" => memory_recall(params),
        "ledger.inject" => ledger_inject(params),
        "ledger.usage" => ledger_usage(params),
        "ledger.tool" => ledger_tool(params),
        "ledger.get" => ledger_get(params),
        "ledger.latest" => ledger_latest(),
        _ => bail!("unknown control method: {method}"),
    };

    // Same Phase 1 observability as the MCP tool-call path in tools.rs, kept
    // in a separate `control_methods` bucket so GUI-originated usage never
    // gets mixed into "how are MCP agents using this" signal.
    {
        let latency_ms = call_start.elapsed().as_millis() as u64;
        let (is_error, output_bytes) = match &result {
            Ok(v) => (
                false,
                serde_json::to_string(v).map(|s| s.len()).unwrap_or(0) as u64,
            ),
            Err(err) => (true, err.to_string().len() as u64),
        };
        nexus_core::stats::record_control_call(
            &Paths::resolve().usage_stats_file(),
            method,
            latency_ms,
            output_bytes,
            is_error,
        );
    }

    result
}

fn loaded_config() -> Config {
    Config::load(&Paths::resolve().config_file()).unwrap_or_default()
}

fn control_str(params: &Value, key: &str) -> anyhow::Result<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("missing '{key}' argument"))
}

fn control_u64(params: &Value, key: &str, default: u64) -> anyhow::Result<u64> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| anyhow!("'{key}' must be a non-negative integer")),
        _ => bail!("'{key}' must be a non-negative integer"),
    }
}

fn control_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn open_ledger() -> anyhow::Result<Ledger> {
    Ledger::open(&Paths::resolve().ledger_db_file())
}

/// One queue, two memories. Facts carry anchor staleness; skill candidates
/// have no atrophy data yet (not promoted) and say so explicitly. Promoted
/// skills carry their own atrophy flag — deliberately a different label from
/// a fact's anchor staleness.
fn review_list(params: Value) -> anyhow::Result<Value> {
    let limit = control_u64(&params, "limit", 50)?.clamp(1, 200) as i64;
    let config = loaded_config();
    let now = control_now();

    let facts = match params.get("repo_path").and_then(Value::as_str) {
        Some(repo) => {
            crate::memory_tools::list_facts_value(&json!({ "repo_path": repo }), &config, now)?
        }
        None => json!({ "facts": [], "note": "no repo_path supplied" }),
    };

    let pending_raw = myelind::tools::call(json!({
        "name": "list_pending_review",
        "arguments": { "limit": limit }
    }))?;
    let skills_raw = myelind::tools::call(json!({
        "name": "list_skills",
        "arguments": {}
    }))?;
    let pending: Vec<Value> = pending_raw["pending"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(limit as usize)
        .map(|mut item| {
            item["freshness_label"] = json!("skill candidate — no atrophy data (not promoted)");
            item
        })
        .collect();
    let promoted: Vec<Value> = skills_raw["skills"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(limit as usize)
        .map(|mut item| {
            let atrophied = item["stale"].as_bool().unwrap_or(false);
            item["freshness_label"] = json!(if atrophied {
                "skill — atrophied (last used too long ago)"
            } else {
                "skill — active"
            });
            item
        })
        .collect();

    Ok(json!({
        "facts": facts["facts"],
        "fact_stale_count": facts["stale_count"],
        "fact_trust_counts": facts["by_trust"],
        "skill_candidates": pending,
        "promoted_skills": promoted,
        "labels": {
            "fact": "staleness = anchored file changed or stale_after elapsed",
            "skill": "atrophy = skill not used within its window (independent of fact staleness)",
        },
        "note": "read-only review feed: listing never promotes any candidate",
    }))
}

fn memory_recall(params: Value) -> anyhow::Result<Value> {
    // Never accept `include_lower_trust` over this surface: auto-injection
    // is for verified facts only. An agent can pull lower-trust candidates
    // explicitly through the MCP tool, labelled, but not into context.
    let mut safe = params;
    if let Some(obj) = safe.as_object_mut() {
        obj.remove("include_lower_trust");
        obj.remove("budget_tokens"); // capped inside recall_value
    }
    crate::memory_tools::recall_value(&safe, &loaded_config(), control_now())
}

/// The daemon runs recall, renders the exact injection text, and writes the
/// ledger entry in one place. The adapter injects `text` verbatim, so it can
/// neither widen the budget nor claim an injection it did not perform.
fn ledger_inject(params: Value) -> anyhow::Result<Value> {
    let session_id = control_str(&params, "session_id")?;
    let repo_path = control_str(&params, "repo_path")?;
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(2000)
        .collect::<String>();
    let budget = control_u64(
        &params,
        "budget_tokens",
        nexus_memory::DEFAULT_INJECTION_BUDGET_TOKENS,
    )?
    .min(nexus_memory::DEFAULT_INJECTION_BUDGET_TOKENS);
    let recall = crate::memory_tools::recall_value(
        &json!({
            "repo_path": repo_path,
            "query": query,
            "budget_tokens": budget,
            "include_lower_trust": false,
        }),
        &loaded_config(),
        control_now(),
    )?;
    let injection = nexus_memory::render_injection(&recall, budget)?;
    let entry = open_ledger()?.record_injection(InjectionRecord {
        session_id: &session_id,
        repo_path: &repo_path,
        query: &query,
        fact_ids: &injection.fact_ids,
        injected_bytes: injection.bytes,
        budget_tokens: budget,
        now: control_now(),
    })?;
    let mut result = injection.to_json();
    result["ledger"] = entry;
    result["recall_excluded"] = recall["excluded"].clone();
    Ok(result)
}

fn ledger_usage(params: Value) -> anyhow::Result<Value> {
    let session_id = control_str(&params, "session_id")?;
    let tokens = params
        .get("tokens")
        .ok_or_else(|| anyhow!("missing 'tokens' argument"))?;
    let tokens = UsageTokens::from_json(tokens)?;
    let cost = params
        .get("cost_usd")
        .and_then(Value::as_f64)
        .filter(|c| c.is_finite() && *c >= 0.0);
    let event_id = open_ledger()?.record_usage(&session_id, tokens, cost, control_now())?;
    Ok(json!({ "event_id": event_id, "source": "harness" }))
}

fn ledger_tool(params: Value) -> anyhow::Result<Value> {
    let session_id = control_str(&params, "session_id")?;
    let tool = control_str(&params, "tool")?;
    let status = params
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
        .to_string();
    if !matches!(status.as_str(), "completed" | "error") {
        bail!("status must be completed or error");
    }
    let bytes = control_u64(&params, "bytes", 0)?.min(1_000_000);
    let event_id = open_ledger()?.record_tool(&session_id, &tool, &status, bytes, control_now())?;
    Ok(json!({ "event_id": event_id, "source": "estimate" }))
}

fn ledger_get(params: Value) -> anyhow::Result<Value> {
    let session_id = control_str(&params, "session_id")?;
    open_ledger()?.summary(&session_id)
}

/// Read-only status feed for the TUI footer, which has no session id of its
/// own. Reports the most recent session the daemon saw; never writes.
fn ledger_latest() -> anyhow::Result<Value> {
    let ledger = open_ledger()?;
    match ledger.latest_session()? {
        Some(session_id) => ledger.summary(&session_id),
        None => Ok(json!({
            "session_id": null,
            "harness": { "token_source": "provider-reported", "available": false },
            "estimate": { "token_source": "bytes/4", "injected_tokens": 0 },
        })),
    }
}

/// Backs the GUI's Usage tab: lifetime aggregate call/latency/output-size
/// counters per MCP tool and per control method, plus background-watcher
/// auto-reindex frequency/cost per project. Phase 1 observability only - no
/// enforcement, no per-call log, see nexus_core::stats.
fn stats_get() -> Result<Value> {
    let paths = Paths::resolve();
    let usage = nexus_core::UsageStats::load(&paths.usage_stats_file());
    let registry = Registry::load(&paths.registry_file());

    let total_auto_reindex_count: u64 =
        registry.projects.iter().map(|p| p.auto_reindex_count).sum();
    let total_auto_reindex_fail_count: u64 = registry
        .projects
        .iter()
        .map(|p| p.auto_reindex_fail_count)
        .sum();
    let total_auto_reindex_ms: u64 = registry
        .projects
        .iter()
        .map(|p| p.auto_reindex_total_ms)
        .sum();

    Ok(json!({
        "mcp_tools": tool_stats_json(&usage.mcp_tools),
        "control_methods": tool_stats_json(&usage.control_methods),
        "reindex": {
            "total_auto_reindex_count": total_auto_reindex_count,
            "total_auto_reindex_fail_count": total_auto_reindex_fail_count,
            "avg_auto_reindex_ms": avg(total_auto_reindex_ms, total_auto_reindex_count),
            "projects": registry.projects.iter()
                .filter(|p| p.auto_reindex_count + p.auto_reindex_fail_count > 0)
                .map(|p| json!({
                    "root_path": p.root_path,
                    "auto_reindex_count": p.auto_reindex_count,
                    "auto_reindex_fail_count": p.auto_reindex_fail_count,
                    "avg_auto_reindex_ms": avg(p.auto_reindex_total_ms, p.auto_reindex_count),
                    "last_auto_reindex_ms": p.last_auto_reindex_ms,
                    "last_auto_reindex_unix": p.last_auto_reindex_unix,
                }))
                .collect::<Vec<_>>()
        },
        "collecting_since_unix": usage.collecting_since_unix
    }))
}

fn tool_stats_json(map: &std::collections::HashMap<String, nexus_core::ToolCallStats>) -> Value {
    json!(map
        .iter()
        .map(|(name, s)| json!({
            "name": name,
            "call_count": s.call_count,
            "error_count": s.error_count,
            "avg_latency_ms": avg(s.total_latency_ms, s.call_count),
            "max_latency_ms": s.max_latency_ms,
            "total_output_bytes": s.total_output_bytes,
            "last_called_unix": s.last_called_unix,
        }))
        .collect::<Vec<_>>())
}

fn avg(total: u64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        total as f64 / count as f64
    }
}

fn status_get() -> Result<Value> {
    let paths = Paths::resolve();
    let registry = Registry::load(&paths.registry_file());
    let watch = crate::watcher::watch_status();
    let indexing = nexus_index::indexing_status();
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "data_dir": paths.data_dir.display().to_string(),
        "log_file": paths.log_file().display().to_string(),
        "projects_indexed": registry.projects.len(),
        "projects_watched": watch.watched_projects,
        // Self-imposed inotify watch budget (see nexusd::watcher's own doc
        // comment) - `pressure_events` non-zero means a project has been
        // skipped or evicted at least once, the loud signal that
        // fs.inotify.max_user_watches is actually constraining real usage
        // on this machine rather than staying buried in a log file.
        "watch_budget": {
            "estimated_watches_used": watch.estimated_watches_used,
            "estimated_watches_budget": watch.estimated_watches_budget,
            "pressure_events": watch.pressure_events,
            // Depth/backpressure of the bounded debounced-event channel
            // (issue #58 follow-up to PR #64's `sync_channel(256)`) - see
            // `WATCHER_QUEUE_DEPTH`'s doc comment in watcher.rs.
            "queue_depth": watch.queue_depth,
            "queue_bound": watch.queue_bound,
            "queue_full_events": watch.queue_full_events
        },
        // Whether a full-rebuild reindex (MCP `index_repository`, watcher
        // auto-reindex, or `projects.reindex`) is running right now -
        // `REINDEX_LOCK` in nexus-index's project.rs already serializes all
        // of them process-wide, so this is a 0/1 flag, not a real queue
        // depth (issue #58's "active indexing job state" ask).
        "indexing": {
            "active": indexing.active,
            "completed_count": indexing.completed_count,
            // How many in-flight reindexes bailed out early because a newer
            // request for the same project superseded them mid-run (issue
            // #58's remaining gap, closed out by ADR 0014's amendment) -
            // zero in the common case; a follow-up reindex always fires
            // afterward and picks up the newer changes, so this is a wasted-
            // work signal, not a data-loss one.
            "superseded_count": indexing.superseded_count
        },
        // Resident set size of this process, for the memory half of issue
        // #58's observability ask - Linux-only for now (this whole control
        // module is already `#[cfg(unix)]`-gated at its `mod control`
        // declaration in main.rs; `rss_kb: None` on a non-Linux Unix, e.g.
        // macOS, where `/proc` doesn't exist).
        "rss_kb": read_rss_kb()
    }))
}

/// Reads this process's resident set size from `/proc/self/status`'
/// `VmRSS` line (already reported in KiB by the kernel). `None` if `/proc`
/// isn't available (non-Linux Unix) or the line can't be parsed - a status
/// response should degrade gracefully, not fail outright, over a
/// best-effort observability field.
fn read_rss_kb() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/self/status").ok()?;
    parse_rss_kb(&contents)
}

/// Parsing logic factored out from `read_rss_kb` so it's testable against
/// fixture text without depending on `/proc` actually existing (CI runs
/// this on Linux, but keeping the parser itself Linux-agnostic and testable
/// in isolation is cheap and matches how the rest of this file separates
/// I/O from logic).
fn parse_rss_kb(contents: &str) -> Option<u64> {
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

fn projects_list() -> Result<Value> {
    let paths = Paths::resolve();
    let registry = Registry::load(&paths.registry_file());
    // Disk usage is computed live rather than stored in the registry entry -
    // it only costs a directory listing per project, and staying live means
    // it can never drift out of sync with what's actually on disk.
    let projects: Vec<Value> = registry
        .projects
        .iter()
        .map(|p| {
            let mut entry = serde_json::to_value(p).unwrap_or(json!({}));
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("disk_bytes".to_string(), json!(project_disk_usage(&p.hash)));
            }
            entry
        })
        .collect();
    Ok(json!(projects))
}

fn projects_reindex(params: Value) -> Result<Value> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    let repo_path = PathBuf::from(repo_path);

    let stats = index_project(&repo_path)?;

    Ok(json!({
        "status": "indexed",
        "files_indexed": stats.files_indexed,
        "nodes": stats.nodes,
        "edges": stats.edges
    }))
}

fn projects_delete(params: Value) -> Result<Value> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    delete_project(std::path::Path::new(repo_path))?;
    Ok(json!({ "status": "deleted" }))
}

fn projects_export(params: Value) -> Result<Value> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    let artifact = export_project(std::path::Path::new(repo_path))?;
    Ok(json!({ "status": "exported", "artifact_path": artifact.display().to_string() }))
}

fn projects_import(params: Value) -> Result<Value> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    let stats = import_project(std::path::Path::new(repo_path))?;
    Ok(json!({ "status": "imported", "nodes": stats.nodes, "edges": stats.edges }))
}

fn projects_architecture(params: Value) -> Result<Value> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    let repo_path = std::path::Path::new(repo_path);

    let summary = get_architecture(repo_path)?;

    let hash = nexus_core::project_hash(repo_path);
    let last_indexed_unix = Registry::load(&Paths::resolve().registry_file())
        .projects
        .into_iter()
        .find(|p| p.hash == hash)
        .map(|p| p.last_indexed_unix)
        .unwrap_or(0);

    Ok(json!({
        "total_nodes": summary.total_nodes,
        "total_edges": summary.total_edges,
        "busiest_files": summary.busiest_files.into_iter()
            .map(|(file, count)| json!({ "file": file, "definitions": count }))
            .collect::<Vec<_>>(),
        "language_breakdown": summary.language_breakdown.into_iter()
            .map(|(ext, count)| json!({ "extension": ext, "files": count }))
            .collect::<Vec<_>>(),
        "last_indexed_unix": last_indexed_unix
    }))
}

fn config_get() -> Result<Value> {
    let paths = Paths::resolve();
    let config = Config::load(&paths.config_file())?;
    Ok(serde_json::to_value(config)?)
}

/// No GUI-editable config field exists anymore now that the embeddings
/// panel (the only thing that ever called this) is gone - kept as a no-op
/// round trip (load, re-save unchanged) rather than removed outright, since
/// `config.set` is still a reachable control-socket method other future
/// settings can hang fields off of.
fn config_set(_params: Value) -> Result<Value> {
    let paths = Paths::resolve();
    let config = Config::load(&paths.config_file())?;
    config.save(&paths.config_file())?;
    Ok(serde_json::to_value(config)?)
}

fn search_adhoc(params: Value) -> Result<Value> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    let pattern = params
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'pattern' argument"))?;
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;

    let db_path = graph_db_path(std::path::Path::new(repo_path));
    if !db_path.exists() {
        bail!("no index found for {repo_path} - call projects.reindex first");
    }
    let store = GraphStore::open(&db_path)?;
    let results = store.search_by_name(pattern, limit)?;

    Ok(json!(results
        .iter()
        .map(|n| json!({
            "kind": format!("{:?}", n.kind),
            "name": n.name,
            "qualified_name": n.qualified_name,
            "file": n.file_path,
            "start_line": n.start_line,
            "end_line": n.end_line,
        }))
        .collect::<Vec<_>>()))
}

/// Renders a bounded call-graph neighborhood as Graphviz DOT source - the
/// GUI shells out to `dot` to turn this into an image. Kept as a plain DOT
/// string over the wire (not a rendered image) so the actual rendering
/// dependency (whether `dot` is installed at all) stays entirely in the
/// GUI, which is the only client that needs a picture rather than data.
fn viz_call_graph(params: Value) -> Result<Value> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'repo_path' argument"))?;
    let function_name = params
        .get("function_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'function_name' argument"))?;
    let direction = match params.get("direction").and_then(|v| v.as_str()) {
        Some("inbound") => nexus_index::Direction::Inbound,
        _ => nexus_index::Direction::Outbound,
    };
    let depth =
        crate::tools::clamp_depth(params.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as u32);

    let dot = nexus_index::call_graph_dot(
        std::path::Path::new(repo_path),
        function_name,
        direction,
        depth,
    )?;
    Ok(json!({ "dot": dot }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory per test - unlike `nexus_core::config`'s own
    /// permission test (which this originally copied the long
    /// thread-id+nanos naming from), the path here ends up inside a real
    /// `AF_UNIX` socket address, not just a regular file path: `sun_path` is
    /// capped at 108 bytes on Linux and only 104 on macOS, and CI's macOS
    /// runner's `$TMPDIR` is already a long `/private/var/folders/...`
    /// prefix - the long name blew that budget and failed with `path must
    /// be shorter than SUN_LEN` there while passing on Linux (caught by a
    /// PR reviewer, not caught locally). Kept short and pid-only instead -
    /// distinct `label`s already avoid collisions between the two tests
    /// below sharing one process id.
    fn scratch_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ncsock-{label}-{}", std::process::id()))
    }

    #[test]
    fn parse_rss_kb_extracts_the_vmrss_line() {
        let fixture = "\
VmPeak:\t  123456 kB
VmSize:\t  123000 kB
VmRSS:\t    45678 kB
VmData:\t   10000 kB
";
        assert_eq!(parse_rss_kb(fixture), Some(45678));
    }

    #[test]
    fn parse_rss_kb_returns_none_without_a_vmrss_line() {
        let fixture = "VmPeak:\t  123456 kB\nVmSize:\t  123000 kB\n";
        assert_eq!(parse_rss_kb(fixture), None);
    }

    #[test]
    fn parse_rss_kb_returns_none_on_garbage_input() {
        assert_eq!(parse_rss_kb(""), None);
        assert_eq!(parse_rss_kb("VmRSS:\tnot-a-number kB\n"), None);
    }

    /// Sanity check against the real thing on the CI/dev Linux box this
    /// runs on - this process definitely has *some* resident memory.
    /// `read_rss_kb` itself is `#[cfg(unix)]` (this whole module is), but
    /// `/proc` only exists on Linux - on macOS this correctly returns
    /// `None` rather than reading anything, so this test (which asserts
    /// the Linux-only success path) is `target_os = "linux"`-gated too,
    /// not just `cfg(unix)`.
    #[test]
    #[cfg(target_os = "linux")]
    fn read_rss_kb_reads_a_plausible_value_from_the_real_proc_self() {
        let rss = read_rss_kb().expect("expected /proc/self/status to be readable on Linux CI");
        assert!(rss > 0);
    }

    #[test]
    fn bind_socket_locks_the_socket_file_to_owner_only() {
        let dir = scratch_dir("owner-only");
        let socket_path = dir.join("nested").join("test.sock");

        let _listener = bind_socket(&socket_path).unwrap();

        let mode = std::fs::metadata(&socket_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bind_socket_removes_a_stale_socket_file_left_over_from_a_previous_run() {
        let dir = scratch_dir("stale-file");
        std::fs::create_dir_all(&dir).unwrap();
        let socket_path = dir.join("test.sock");
        // A genuine leftover socket, not just a proxy file: bind once and
        // drop the listener without removing it - the same shape a
        // not-cleanly-shut-down daemon leaves behind. `bind_socket` must
        // remove it before re-binding, not error on it already existing.
        drop(UnixListener::bind(&socket_path).unwrap());
        assert!(socket_path.exists());

        let _listener = bind_socket(&socket_path).unwrap();

        let mode = std::fs::metadata(&socket_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        std::fs::remove_dir_all(&dir).ok();
    }
}
