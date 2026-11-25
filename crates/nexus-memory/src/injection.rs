//! Deterministic injection rendering with a hard budget.
//!
//! The daemon renders the exact text that the harness will inject, so the
//! bytes/4 estimate recorded in the ledger is a measurement of the same bytes
//! the model actually saw — not a guess about a plugin-side envelope. The
//! adapter receives this text verbatim and cannot widen the budget or smuggle
//! in an extra fact.

use crate::estimate_tokens;
use anyhow::Result;
use serde_json::Value;

const HEADER: &str = "## NexusContext verified memory\nCurated, human-reviewed facts about this repo. Anchors point to current code.\n\n";
/// Reserve room for the metadata footer while choosing facts. The real footer
/// is always shorter than this, so the final byte count is an over-estimate
/// only during selection and exact once rendered.
const FOOTER_RESERVE: usize = 220;

#[derive(Debug, Clone)]
pub struct Injection {
    /// The exact string the adapter must inject, keyed by a stable entry name.
    pub text: String,
    pub bytes: usize,
    pub estimated_tokens: u64,
    pub budget_tokens: u64,
    pub fact_ids: Vec<String>,
}

impl Injection {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "text": self.text,
            "bytes": self.bytes,
            "estimated_tokens": self.estimated_tokens,
            "budget_tokens": self.budget_tokens,
            "fact_ids": self.fact_ids,
            "token_estimate_note": "bytes/4 estimate of this exact injected text, not a provider token count",
        })
    }
}

/// Render a recall result into one bounded block. Facts that are stale or not
/// human-reviewed are dropped defensively even though the recall gate already
/// excludes them: the injection surface is the last line of defence before
/// context, and it must not rely on every upstream caller being correct.
pub fn render_injection(recall: &Value, budget_tokens: u64) -> Result<Injection> {
    let facts = recall
        .get("facts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut lines: Vec<String> = Vec::with_capacity(facts.len());
    let mut ids: Vec<String> = Vec::with_capacity(facts.len());
    for fact in &facts {
        if fact["stale"].as_bool().unwrap_or(false) {
            continue;
        }
        if fact["trust"].as_str() != Some("human-reviewed") {
            continue;
        }
        let title = fact["title"].as_str().unwrap_or("").trim();
        let text = fact["text"].as_str().unwrap_or("").trim();
        if title.is_empty() && text.is_empty() {
            continue;
        }
        let kind = fact["kind"].as_str().unwrap_or("fact");
        let anchors = fact["anchors"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|a| a["path"].as_str())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let body = if title.is_empty() {
            text.to_string()
        } else if text.is_empty() {
            title.to_string()
        } else {
            format!("{title}: {text}")
        };
        let anchor_suffix = if anchors.is_empty() {
            String::new()
        } else {
            format!(" — {anchors}")
        };
        lines.push(format!("- [{kind}] {body}{anchor_suffix}\n"));
        ids.push(fact["id"].as_str().unwrap_or_default().to_string());
    }

    // Drop trailing (lowest-ranked) facts until the block fits. Recall already
    // ordered them by relevance, so the least useful go first.
    loop {
        let body = lines.concat();
        let projected = HEADER.len() + body.len() + FOOTER_RESERVE;
        if estimate_tokens(projected) <= budget_tokens || lines.is_empty() {
            break;
        }
        lines.pop();
        ids.pop();
    }

    let body = lines.concat();
    // The in-context hint is a conservative bound for the same projected size
    // used by the cap check; the ledger records the exact bytes of the final
    // block, so its number is always <= this hint.
    let projected = estimate_tokens(HEADER.len() + body.len() + FOOTER_RESERVE);
    let text = format!(
        "{HEADER}{body}<!-- nexuscontext: {} facts | ≤{}/{} tokens (bytes/4 estimate) -->\n",
        ids.len(),
        projected,
        budget_tokens
    );
    let bytes = text.len();
    let estimated_tokens = estimate_tokens(bytes);
    // The footer is shorter than the reserve, so this should never fire; keep
    // the check as a hard invariant rather than trusting that arithmetic.
    if estimated_tokens > budget_tokens {
        // Last resort: a minimal block with no facts still fits any sane cap.
        // Never emit text we know busts the cap, even if the arithmetic above
        // was wrong about how much room the footer needs.
        let minimal =
            format!("{HEADER}<!-- nexuscontext: 0 facts | over budget, nothing injected -->\n");
        return Ok(Injection {
            bytes: minimal.len(),
            estimated_tokens: estimate_tokens(minimal.len()),
            text: minimal,
            budget_tokens,
            fact_ids: vec![],
        });
    }

    Ok(Injection {
        text,
        bytes,
        estimated_tokens,
        budget_tokens,
        fact_ids: ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fact(id: &str, trust: &str, stale: bool, body: &str) -> Value {
        json!({
            "id": id,
            "kind": "decision",
            "title": format!("title {id}"),
            "text": body,
            "trust": trust,
            "stale": stale,
            "anchors": [{ "path": "src/lib.rs" }],
        })
    }

    #[test]
    fn renderer_drops_stale_and_lower_trust_defensively() {
        let recall = json!({ "facts": [
            fact("h", "human-reviewed", false, "safe"),
            fact("s", "human-reviewed", true, "stale"),
            fact("m", "machine-confirmed", false, "machine"),
            fact("u", "unverified", false, "unverified"),
        ]});
        let out = render_injection(&recall, 800).unwrap();
        assert_eq!(out.fact_ids, vec!["h"]);
        assert!(out.text.contains("safe"));
        assert!(!out.text.contains("stale"));
        assert!(!out.text.contains("machine"));
    }

    #[test]
    fn renderer_never_exceeds_the_hard_cap() {
        let facts: Vec<Value> = (0..200)
            .map(|i| {
                fact(
                    &format!("f{i}"),
                    "human-reviewed",
                    false,
                    &"a long fact body that costs context ".repeat(20),
                )
            })
            .collect();
        for budget in [64, 200, 800] {
            let out = render_injection(&json!({ "facts": facts }), budget).unwrap();
            assert!(
                out.estimated_tokens <= budget,
                "budget {budget} exceeded with {} facts",
                out.fact_ids.len()
            );
            assert_eq!(out.bytes, out.text.len());
        }
    }

    #[test]
    fn an_empty_recall_still_produces_a_bounded_block() {
        let out = render_injection(&json!({ "facts": [] }), 800).unwrap();
        assert!(out.fact_ids.is_empty());
        assert!(out.estimated_tokens <= 800);
        assert!(out.to_json()["token_estimate_note"].is_string());
    }
}
