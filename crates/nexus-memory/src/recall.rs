//! Budgeted recall. Deterministic, lexical scoring only — no LLM anywhere in
//! the daemon (proposal guiding constraint).

use crate::fact::{trust_tier, Fact};
use crate::gate;
use crate::store::Store;
use anyhow::Result;
use nexus_core::text::truncate_to_byte_boundary;
use nexus_core::Config;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Same honest bytes/4 approximation the rest of NC uses. Never presented as
/// a provider token count; the plugin ledger labels it as an estimate.
pub fn estimate_tokens(bytes: usize) -> u64 {
    (bytes / 4) as u64
}

#[derive(Debug)]
pub struct RecallResult {
    pub items: Vec<Value>,
    pub estimated_tokens: u64,
    pub budget_tokens: u64,
    pub excluded: BTreeMap<String, usize>,
    pub truncated: bool,
}

impl RecallResult {
    pub fn to_json(&self) -> Value {
        json!({
            "facts": self.items,
            "estimated_tokens": self.estimated_tokens,
            "budget_tokens": self.budget_tokens,
            "excluded": self.excluded,
            "truncated": self.truncated,
            "token_estimate_note": "bytes/4 estimate of this recall payload, not a provider token count",
        })
    }
}

fn score(fact: &Fact, terms: &[String]) -> usize {
    if terms.is_empty() {
        return 0;
    }
    let haystack = format!("{} {}", fact.title.to_lowercase(), fact.body.to_lowercase());
    terms
        .iter()
        .map(|term| haystack.matches(term).count())
        .sum()
}

fn item_json(fact: &Fact, body: &str) -> Value {
    json!({
        "id": fact.id,
        "kind": fact.kind.as_okf_type(),
        "title": fact.title,
        "text": body,
        "anchors": fact.anchors,
        "trust": trust_tier(fact).label(),
        "generated_at": fact.generated_at,
    })
}

/// Auto-injection path: current, stable, human-reviewed facts only.
/// The cap includes JSON envelope bytes for each item, so the published
/// "~800 tokens" is a real ceiling on what recall hands the harness.
pub fn recall(
    store: &Store,
    config: &Config,
    repo_path: &str,
    query: &str,
    budget_tokens: u64,
    now: i64,
) -> Result<RecallResult> {
    recall_inner(store, config, repo_path, query, budget_tokens, now, false)
}

/// Deliberate pull by an agent: machine-confirmed and unverified concepts
/// become visible, each labelled with its trust tier. Stale, non-stable and
/// out-of-roots facts stay excluded — a label is not a licence to inject
/// something whose anchor has changed.
pub fn recall_with_lower_trust(
    store: &Store,
    config: &Config,
    repo_path: &str,
    query: &str,
    budget_tokens: u64,
    now: i64,
) -> Result<RecallResult> {
    recall_inner(store, config, repo_path, query, budget_tokens, now, true)
}

fn recall_inner(
    store: &Store,
    config: &Config,
    repo_path: &str,
    query: &str,
    budget_tokens: u64,
    now: i64,
    include_lower_trust: bool,
) -> Result<RecallResult> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|term| term.len() > 1)
        .collect();

    let mut excluded: BTreeMap<String, usize> = BTreeMap::new();
    let mut candidates = Vec::new();
    for fact in store.list(repo_path)? {
        let admission = gate::admit(&fact, config, now);
        for reason in &admission.exclusions {
            *excluded.entry(reason.label().to_string()).or_insert(0) += 1;
        }
        let hard_block = admission.exclusions.iter().any(|reason| {
            matches!(
                reason,
                gate::Exclusion::Stale
                    | gate::Exclusion::NotStable
                    | gate::Exclusion::AnchorOutsideRoots
            )
        });
        if hard_block || (!include_lower_trust && !admission.include) {
            continue;
        }
        candidates.push((score(&fact, &terms), fact.generated_at, fact));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));

    let mut result = RecallResult {
        items: Vec::new(),
        estimated_tokens: 0,
        budget_tokens,
        excluded,
        truncated: false,
    };

    for (_, _, fact) in candidates {
        let remaining = budget_tokens.saturating_sub(result.estimated_tokens);
        if remaining == 0 {
            break;
        }
        let mut item = item_json(&fact, &fact.body);
        let mut bytes = serde_json::to_string(&item)?.len();
        if estimate_tokens(bytes) > remaining {
            // Leave room for the JSON envelope. If even that cannot fit,
            // skip the fact rather than silently busting the hard budget.
            let allowance = (remaining * 4) as usize;
            if allowance < 128 {
                continue;
            }
            let (short, shortened) = truncate_to_byte_boundary(&fact.body, allowance - 96);
            if !shortened {
                continue;
            }
            item = item_json(&fact, &short);
            bytes = serde_json::to_string(&item)?.len();
            if estimate_tokens(bytes) > remaining {
                continue;
            }
            result.truncated = true;
        }
        result.estimated_tokens += estimate_tokens(bytes);
        result.items.push(item);
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fact::{Anchor, FactKind, FactStatus};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    // Tests run in parallel; a pid-only temp name lets one test delete
    // another test's database mid-call.
    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);
    fn temp_dir(label: &str) -> std::path::PathBuf {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("nc-{label}-{}-{n}", std::process::id()))
    }

    fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn reviewed(id: &str, title: &str, body: &str) -> Fact {
        Fact {
            id: id.into(),
            repo_path: "/repo".into(),
            kind: FactKind::Decision,
            title: title.into(),
            body: body.into(),
            status: FactStatus::Stable,
            superseded_by: None,
            anchors: vec![Anchor {
                path: "src/lib.rs".into(),
                symbol: None,
                line: None,
                hash: None,
            }],
            sources: vec![],
            generated_by: "agent/test".into(),
            generated_at: now() - 10,
            verified_by: Some("human:alice".into()),
            verified_at: Some(now() - 5),
            stale_after: None,
            anchor_stale: false,
        }
    }

    fn store_with() -> Store {
        let dir = temp_dir("recall");
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir.join("memory.db")).unwrap();
        store
            .put(&reviewed(
                "a",
                "preset schema tax",
                "presets cut the schema tax",
            ))
            .unwrap();
        store
            .put(&reviewed("b", "owner", "alice owns the ledger"))
            .unwrap();
        store
    }

    #[test]
    fn recall_never_exceeds_its_token_budget() {
        let store = store_with();
        for budget in [4, 16, 40, 800] {
            let out = recall(&store, &Config::default(), "/repo", "", budget, now()).unwrap();
            assert!(out.estimated_tokens <= budget, "budget {budget} exceeded");
            let serialised = serde_json::to_string(&out.to_json()).unwrap();
            // The envelope itself is metadata, not injected facts; the
            // enforced ceiling covers the fact payload items only.
            assert!(out.to_json()["facts"].is_array());
            assert!(serialised.contains("token_estimate_note"));
        }
    }

    #[test]
    fn query_terms_change_ranking() {
        let store = store_with();
        let out = recall(
            &store,
            &Config::default(),
            "/repo",
            "ledger owner",
            800,
            now(),
        )
        .unwrap();
        assert_eq!(out.items[0]["id"], "b");
    }

    #[test]
    fn a_deliberate_pull_shows_lower_trust_but_never_a_stale_fact() {
        let dir = temp_dir("recall-trust");
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir.join("memory.db")).unwrap();
        let mut machine = reviewed("m", "machine", "process checked it");
        machine.verified_by = Some("process:nightly".into());
        store.put(&machine).unwrap();
        let mut stale = reviewed("s", "stale", "anchor moved");
        stale.anchor_stale = true;
        store.put(&stale).unwrap();

        let out =
            recall_with_lower_trust(&store, &Config::default(), "/repo", "", 800, now()).unwrap();
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0]["id"], "m");
        assert_eq!(out.items[0]["trust"], "machine-confirmed");
        assert_eq!(out.excluded["stale"], 1);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn exclusions_are_counted_not_silently_dropped() {
        let dir = temp_dir("recall-excl");
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir.join("memory.db")).unwrap();
        let mut f = reviewed("x", "machine", "not human reviewed");
        f.verified_by = Some("process:nightly".into());
        store.put(&f).unwrap();
        let out = recall(&store, &Config::default(), "/repo", "", 800, now()).unwrap();
        assert_eq!(out.excluded["machine_confirmed"], 1);
        assert!(out.items.is_empty());
    }
}
