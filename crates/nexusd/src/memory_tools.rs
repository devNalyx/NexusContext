//! MCP surface for curated declarative memory (ADR 0019/0020).
//!
//! Kept in its own module so `tools.rs` remains the single catalog owner:
//! it extends `tool_definitions()` and delegates `call()` here. `recall` is
//! the only one of these advertised by default; every writing/review tool
//! is `full`-only because unverified memory is the risk we are removing.

use anyhow::{anyhow, bail, Result};
use nexus_core::{project_hash, Config, Paths};
use nexus_memory::fact::{
    Anchor, Fact, FactKind, FactStatus, Source, DEFAULT_INJECTION_BUDGET_TOKENS,
};
use nexus_memory::{okf, recall, recall_with_lower_trust, trust_tier, Store};
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "recall",
            "description": "Budgeted recall of curated, code-anchored facts (decisions, incidents, lessons, dead ends, owners). Only current human-reviewed facts that are still fresh and anchored in allowed_roots; stale facts are never injected. Set include_lower_trust=true to deliberately inspect machine-confirmed or unverified candidates — they are labelled, not trusted. Hard ~800-token cap; no LLM involved.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "query": { "type": "string", "default": "" },
                    "budget_tokens": { "type": "integer", "default": 800 },
                    "include_lower_trust": { "type": "boolean", "default": false }
                },
                "required": ["repo_path"]
            }
        }),
        json!({
            "name": "list_facts",
            "description": "Review queue for curated facts: every fact with trust tier, staleness, lifecycle status and anchor paths. Read-only — it never promotes anything. Use this to decide what deserves human review.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo_path": { "type": "string" } },
                "required": ["repo_path"]
            }
        }),
        json!({
            "name": "curate_fact",
            "description": "Stage a code-anchored fact from agent judgment. Always starts unverified/draft and is redacted before storage; it becomes injectable only after a human/CLI review approves it. Anchors are repo-relative paths. Do not paste raw transcripts, code structure or preferences — curated facts only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "id": { "type": "string" },
                    "kind": { "type": "string", "enum": ["Decision", "Incident", "Lesson", "Dead End", "Owner"] },
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "anchors": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" },
                                "symbol": { "type": "string" },
                                "line": { "type": "integer" },
                                "hash": { "type": "string" }
                            },
                            "required": ["path"]
                        },
                        "minItems": 1
                    },
                    "sources": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "resource": { "type": "string" },
                                "title": { "type": "string" }
                            },
                            "required": ["resource"]
                        }
                    },
                    "stale_after": { "type": "string", "description": "RFC 3339 instant at which this fact must be re-checked" },
                    "actor": { "type": "string", "default": "agent/unknown" }
                },
                "required": ["repo_path", "kind", "title", "body", "anchors"]
            }
        }),
        json!({
            "name": "review_fact",
            "description": "Human/CLI review gate for a staged fact. verdict=approve requires an actor starting with 'human:' and sets current verification; 'reject' clears any earlier sign-off. An agent must not approve its own staging — this tool is full-preset only and the CLI is the intended human path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "id": { "type": "string" },
                    "actor": { "type": "string" },
                    "verdict": { "type": "string", "enum": ["approve", "reject", "comment"] }
                },
                "required": ["repo_path", "id", "actor", "verdict"]
            }
        }),
        json!({
            "name": "export_memory",
            "description": "Export this repo's curated facts as an OKF v0.3 bundle at repo/.nexus/memory/ — one typed concept per fact, bundle-root index.md with a routing description, standard Markdown links, no wikilinks. Git-friendly and Obsidian-viewable; SQLite stays canonical.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo_path": { "type": "string" } },
                "required": ["repo_path"]
            }
        }),
        json!({
            "name": "import_memory",
            "description": "Read an OKF v0.3 bundle into the canonical SQLite store. Unknown types/keys and broken links are tolerated but never trusted. SQLite's own anchor-staleness truth and human verification are never downgraded by an import.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo_path": { "type": "string" },
                    "bundle_path": { "type": "string" }
                },
                "required": ["repo_path"]
            }
        }),
    ]
}

fn str_arg(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("missing '{key}' argument"))
}

fn opt_str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

fn u64_arg(args: &Value, key: &str, default: u64) -> Result<u64> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| anyhow!("'{key}' must be a non-negative integer")),
        _ => bail!("'{key}' must be a non-negative integer"),
    }
}

fn bool_arg(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn store_for(repo: &Path, config: &Config) -> Result<Store> {
    if !config.is_path_allowed(repo) {
        bail!("repo_path is outside allowed_roots");
    }
    Store::open(&Paths::resolve().memory_db_file(&project_hash(repo)))
}

fn repo_arg(args: &Value) -> Result<std::path::PathBuf> {
    Ok(std::path::PathBuf::from(str_arg(args, "repo_path")?))
}

fn parse_anchors(args: &Value) -> Result<Vec<Anchor>> {
    let items = args
        .get("anchors")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("'anchors' must be a non-empty array"))?;
    if items.is_empty() {
        bail!("a curated fact must be anchored to at least one file");
    }
    items
        .iter()
        .map(|item| {
            Ok(Anchor {
                path: str_arg(item, "path")?,
                symbol: opt_str_arg(item, "symbol"),
                line: item.get("line").and_then(Value::as_u64).map(|n| n as u32),
                hash: opt_str_arg(item, "hash"),
            })
        })
        .collect()
}

fn parse_sources(args: &Value) -> Result<Vec<Source>> {
    let Some(items) = args.get("sources").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    items
        .iter()
        .map(|item| {
            let id = opt_str_arg(item, "id").unwrap_or_default();
            Ok(Source {
                id: if id.is_empty() {
                    "source".to_string()
                } else {
                    id
                },
                resource: str_arg(item, "resource")?,
                title: opt_str_arg(item, "title"),
            })
        })
        .collect()
}

fn deterministic_id(repo: &Path, title: &str, anchors: &[Anchor]) -> String {
    let mut hasher = DefaultHasher::new();
    repo.hash(&mut hasher);
    title.hash(&mut hasher);
    for anchor in anchors {
        anchor.path.hash(&mut hasher);
        anchor.symbol.hash(&mut hasher);
        anchor.line.hash(&mut hasher);
    }
    format!("f{:016x}", hasher.finish())
}

fn fact_json(fact: &Fact, now: i64) -> Value {
    json!({
        "id": fact.id,
        "kind": fact.kind.as_okf_type(),
        "title": fact.title,
        "body": fact.body,
        "status": fact.status.as_okf(),
        "trust": trust_tier(fact).label(),
        "stale": fact.is_stale(now),
        "anchors": fact.anchors,
        "sources": fact.sources,
        "generated_by": fact.generated_by,
        "generated_at": fact.generated_at,
        "verified_by": fact.verified_by,
        "verified_at": fact.verified_at,
        "stale_after": fact.stale_after,
    })
}

/// Shared by the MCP tool and the control-socket injection path, so the
/// adapter cannot claim a wider budget or a looser trust gate than `recall`.
pub fn recall_value(args: &Value, config: &Config, now: i64) -> Result<Value> {
    let repo = repo_arg(args)?;
    let store = store_for(&repo, config)?;
    let budget = u64_arg(args, "budget_tokens", DEFAULT_INJECTION_BUDGET_TOKENS)?
        .min(DEFAULT_INJECTION_BUDGET_TOKENS);
    let query = opt_str_arg(args, "query").unwrap_or_default();
    let result = if bool_arg(args, "include_lower_trust", false) {
        recall_with_lower_trust(
            &store,
            config,
            repo.to_str().unwrap_or_default(),
            &query,
            budget,
            now,
        )?
    } else {
        recall(
            &store,
            config,
            repo.to_str().unwrap_or_default(),
            &query,
            budget,
            now,
        )?
    };
    Ok(result.to_json())
}

/// Read-only review feed for the GUI and the MCP tool alike. Never promotes.
pub fn list_facts_value(args: &Value, config: &Config, now: i64) -> Result<Value> {
    let repo = repo_arg(args)?;
    let store = store_for(&repo, config)?;
    let facts = store.list(repo.to_str().unwrap_or_default())?;
    let queue: Vec<Value> = facts.iter().map(|f| fact_json(f, now)).collect();
    let mut by_trust = std::collections::BTreeMap::new();
    let mut stale = 0usize;
    for f in &facts {
        *by_trust.entry(trust_tier(f).label()).or_insert(0usize) += 1;
        if f.is_stale(now) {
            stale += 1;
        }
    }
    Ok(json!({
        "facts": queue,
        "by_trust": by_trust,
        "stale_count": stale,
        "note": "review queue only: listing a fact never promotes it",
    }))
}

pub fn call(name: &str, args: Value, config: &Config) -> Result<String> {
    let now = now_unix();
    match name {
        "recall" => Ok(serde_json::to_string_pretty(&recall_value(
            &args, config, now,
        )?)?),
        "list_facts" => Ok(serde_json::to_string_pretty(&list_facts_value(
            &args, config, now,
        )?)?),
        "curate_fact" => {
            let repo = repo_arg(&args)?;
            let mut store = store_for(&repo, config)?;
            let kind_raw = str_arg(&args, "kind")?;
            let Some(kind) = FactKind::from_okf_type(&kind_raw) else {
                bail!("unknown fact kind: {kind_raw}");
            };
            let anchors = parse_anchors(&args)?;
            let title = str_arg(&args, "title")?;
            let id = opt_str_arg(&args, "id")
                .unwrap_or_else(|| deterministic_id(&repo, &title, &anchors));
            let stale_after = match opt_str_arg(&args, "stale_after") {
                Some(raw) => {
                    Some(okf::parse_instant(&raw).map_err(|e| anyhow!("bad stale_after: {e}"))?)
                }
                None => None,
            };
            let fact = Fact {
                id: id.clone(),
                repo_path: repo.to_string_lossy().to_string(),
                kind,
                title,
                body: str_arg(&args, "body")?,
                status: FactStatus::Draft,
                superseded_by: None,
                anchors,
                sources: parse_sources(&args)?,
                generated_by: opt_str_arg(&args, "actor").unwrap_or_else(|| "agent/unknown".into()),
                generated_at: now,
                // Staging never carries a verification stamp. Only a human
                // review through `review_fact` can add one.
                verified_by: None,
                verified_at: None,
                stale_after,
                anchor_stale: false,
            };
            store.put(&fact)?;
            Ok(serde_json::to_string_pretty(&json!({
                "id": id,
                "trust": "unverified",
                "status": "draft",
                "note": "staged only; call review_fact with a human actor before it can be injected",
            }))?)
        }
        "review_fact" => {
            let repo = repo_arg(&args)?;
            let mut store = store_for(&repo, config)?;
            let id = str_arg(&args, "id")?;
            let actor = str_arg(&args, "actor")?;
            let verdict = str_arg(&args, "verdict")?;
            if !matches!(verdict.as_str(), "approve" | "reject" | "comment") {
                bail!("verdict must be approve, reject or comment");
            }
            if verdict == "approve" && !actor.starts_with("human:") {
                bail!(
                    "only a 'human:' actor can approve a fact; agent judgment needs a human review"
                );
            }
            store.record_review(&id, &actor, &verdict, now)?;
            let Some(fact) = store.get(&id)? else {
                bail!("no such fact: {id}");
            };
            Ok(serde_json::to_string_pretty(&json!({
                "id": id,
                "verdict": verdict,
                "trust": trust_tier(&fact).label(),
                "stale": fact.is_stale(now),
            }))?)
        }
        "export_memory" => {
            let repo = repo_arg(&args)?;
            let store = store_for(&repo, config)?;
            let facts = store.list(repo.to_str().unwrap_or_default())?;
            let dir = okf::export_bundle(&repo, &facts)?;
            Ok(serde_json::to_string_pretty(&json!({
                "bundle": dir.to_string_lossy(),
                "concepts": facts.len(),
                "okf_version": nexus_memory::OKF_VERSION,
                "note": "OKF is the exchange format; SQLite remains canonical",
            }))?)
        }
        "import_memory" => {
            let repo = repo_arg(&args)?;
            let mut store = store_for(&repo, config)?;
            let bundle = match opt_str_arg(&args, "bundle_path") {
                Some(raw) => std::path::PathBuf::from(raw),
                None => okf::bundle_dir(&repo),
            };
            let incoming = okf::import_bundle(&bundle)?;
            let mut imported = 0usize;
            let mut preserved_stale = 0usize;
            for mut fact in incoming {
                if fact.repo_path == repo.to_string_lossy().as_ref() {
                    fact.repo_path = repo.to_string_lossy().to_string();
                }
                if let Some(existing) = store.get(&fact.id)? {
                    // Canonical truth wins: a bundle cannot un-stale a fact
                    // whose anchor changed locally, nor downgrade a human
                    // verification recorded in SQLite.
                    if existing.anchor_stale {
                        fact.anchor_stale = true;
                        preserved_stale += 1;
                    }
                    let existing_is_human =
                        trust_tier(&existing) == nexus_memory::TrustTier::HumanReviewed;
                    if existing_is_human
                        && trust_tier(&fact) != nexus_memory::TrustTier::HumanReviewed
                    {
                        fact.verified_by = existing.verified_by;
                        fact.verified_at = existing.verified_at;
                    }
                }
                store.put(&fact)?;
                imported += 1;
            }
            Ok(serde_json::to_string_pretty(&json!({
                "imported": imported,
                "staleness_flags_preserved": preserved_stale,
                "note": "unknown concepts were tolerated but not trusted",
            }))?)
        }
        _ => bail!("unknown curated-memory tool: {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_arg_is_hard_capped_at_the_default() {
        let args = json!({ "budget_tokens": 1_000_000 });
        assert_eq!(
            u64_arg(&args, "budget_tokens", DEFAULT_INJECTION_BUDGET_TOKENS)
                .unwrap()
                .min(DEFAULT_INJECTION_BUDGET_TOKENS),
            800
        );
    }

    #[test]
    fn anchor_parsing_requires_a_real_anchor() {
        assert!(parse_anchors(&json!({})).is_err());
        assert!(parse_anchors(&json!({ "anchors": [] })).is_err());
        let anchors =
            parse_anchors(&json!({ "anchors": [{ "path": "src/lib.rs", "symbol": "run" }] }))
                .unwrap();
        assert_eq!(anchors[0].path, "src/lib.rs");
        assert_eq!(anchors[0].symbol.as_deref(), Some("run"));
    }

    #[test]
    fn definitions_are_stable_and_have_unique_names() {
        let defs = definitions();
        let names: std::collections::BTreeSet<_> =
            defs.iter().map(|d| d["name"].as_str().unwrap()).collect();
        assert_eq!(names.len(), defs.len());
        assert!(names.contains("recall"));
    }
}
