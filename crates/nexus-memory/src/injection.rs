//! Deterministic injection rendering with a hard budget.
//!
//! The daemon renders the exact text that the harness will inject, so the
//! bytes/4 estimate recorded in the ledger is a measurement of the same bytes
//! the model actually saw — not a guess about a plugin-side envelope. The
//! adapter receives this text verbatim and cannot widen the budget or smuggle
//! in an extra fact or skill.

use crate::estimate_tokens;
use anyhow::Result;
use serde_json::Value;

const HEADER: &str = "## NexusContext verified memory\nCurated, human-reviewed facts about this repo. Anchors point to current code.\n\n";
/// Procedural memory is global and not code-anchored, so it gets a separate,
/// explicitly labelled section. Never mix it into the verified-fact list.
const SKILL_HEADER: &str = "### Approved skills (procedural memory, not repo-anchored)\n";
/// Cap one signpost line so a verbose SKILL.md description cannot crowd out
/// code-anchored facts. The full skill is loaded only on demand via list_skills.
const SKILL_DESCRIPTION_CHARS: usize = 140;
const SKILL_NAME_CHARS: usize = 60;
/// Reserve room for the metadata footer while choosing facts/signposts. The
/// real footer is always shorter than this, so the final byte count is an
/// over-estimate only during selection and exact once rendered.
const FOOTER_RESERVE: usize = 260;

/// The minimum a caller must supply to signpost a promoted skill. The daemon
/// obtains these only from `myelind::injectable_skills`, which filters for
/// human approval, active status and non-atrophied freshness.
#[derive(Debug, Clone)]
pub struct SkillSignpost {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct Injection {
    /// The exact string the adapter must inject, keyed by a stable entry name.
    pub text: String,
    pub bytes: usize,
    pub estimated_tokens: u64,
    pub budget_tokens: u64,
    pub fact_ids: Vec<String>,
    pub skill_ids: Vec<String>,
}

impl Injection {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "text": self.text,
            "bytes": self.bytes,
            "estimated_tokens": self.estimated_tokens,
            "budget_tokens": self.budget_tokens,
            "fact_ids": self.fact_ids,
            "skill_ids": self.skill_ids,
            "token_estimate_note": "bytes/4 estimate of this exact injected text, not a provider token count",
        })
    }
}

/// Flatten untrusted-but-human-approved signpost text into one bounded line.
/// Newlines and control characters cannot break the injection block; the full
/// procedural document is loaded only when a model asks for `list_skills`.
fn signpost_line(input: &str, max_chars: usize) -> String {
    let flat: String = input
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    flat.chars().take(max_chars).collect()
}

fn projected_tokens(header_len: usize, body_len: usize, budget_tokens: u64) -> u64 {
    estimate_tokens(header_len + body_len + FOOTER_RESERVE).min(budget_tokens)
}

/// Render a recall result plus human-approved skill signposts into one bounded
/// block. Facts that are stale or not human-reviewed are dropped defensively
/// even though the recall gate already excludes them: the injection surface is
/// the last line of defence before context, and it must not rely on every
/// upstream caller being correct. Skills arrive pre-filtered by
/// `myelind::injectable_skills`; only the daemon can request them.
pub fn render_injection(
    recall: &Value,
    budget_tokens: u64,
    skills: &[SkillSignpost],
) -> Result<Injection> {
    let facts = recall
        .get("facts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut fact_lines: Vec<String> = Vec::with_capacity(facts.len());
    let mut fact_ids: Vec<String> = Vec::with_capacity(facts.len());
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
        fact_lines.push(format!("- [{kind}] {body}{anchor_suffix}\n"));
        fact_ids.push(fact["id"].as_str().unwrap_or_default().to_string());
    }

    let mut skill_lines: Vec<String> = Vec::with_capacity(skills.len());
    let mut skill_ids: Vec<String> = Vec::with_capacity(skills.len());
    for skill in skills {
        let name = signpost_line(&skill.name, SKILL_NAME_CHARS);
        let description = signpost_line(&skill.description, SKILL_DESCRIPTION_CHARS);
        if name.is_empty() {
            continue;
        }
        skill_lines.push(format!(
            "- [skill] {name} — {description} (load with list_skills)\n"
        ));
        skill_ids.push(skill.id.clone());
    }

    // Fit the single block. Skill signposts are lower priority than verified,
    // code-anchored facts, so they are trimmed first; recall is already ranked
    // by relevance, so the least useful facts go after that.
    loop {
        let body_len = fact_lines.concat().len() + skill_lines.concat().len();
        let header_len = HEADER.len()
            + if skill_lines.is_empty() {
                0
            } else {
                SKILL_HEADER.len()
            };
        if estimate_tokens(header_len + body_len + FOOTER_RESERVE) <= budget_tokens
            || (fact_lines.is_empty() && skill_lines.is_empty())
        {
            break;
        }
        if !skill_lines.is_empty() {
            skill_lines.pop();
            skill_ids.pop();
        } else {
            fact_lines.pop();
            fact_ids.pop();
        }
    }

    let fact_body = fact_lines.concat();
    let skill_body = skill_lines.concat();
    let skill_header = if skill_lines.is_empty() {
        ""
    } else {
        SKILL_HEADER
    };
    let header_len = HEADER.len() + skill_header.len();
    let body_len = fact_body.len() + skill_body.len();
    // The in-context hint is a conservative bound for the same projected size
    // used by the cap check; the ledger records the exact bytes of the final
    // block, so its number is always <= this hint.
    let projected = projected_tokens(header_len, body_len, budget_tokens);
    let text = format!(
        "{HEADER}{fact_body}{skill_header}{skill_body}<!-- nexuscontext: {} facts | {} approved skills | ≤{}/{budget_tokens} tokens (bytes/4 estimate) -->\n",
        fact_ids.len(),
        skill_ids.len(),
        projected,
    );
    let bytes = text.len();
    let estimated_tokens = estimate_tokens(bytes);
    // The footer is shorter than the reserve, so this should never fire; keep
    // the check as a hard invariant rather than trusting that arithmetic.
    if estimated_tokens > budget_tokens {
        // Last resort: a minimal block with no entries still fits any sane cap.
        // Never emit text we know busts the cap, even if the arithmetic above
        // was wrong about how much room the footer needs.
        let minimal = format!(
            "{HEADER}<!-- nexuscontext: 0 facts | 0 approved skills | over budget, nothing injected -->\n"
        );
        return Ok(Injection {
            bytes: minimal.len(),
            estimated_tokens: estimate_tokens(minimal.len()),
            text: minimal,
            budget_tokens,
            fact_ids: vec![],
            skill_ids: vec![],
        });
    }

    Ok(Injection {
        text,
        bytes,
        estimated_tokens,
        budget_tokens,
        fact_ids,
        skill_ids,
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

    fn skill(id: &str, description: &str) -> SkillSignpost {
        SkillSignpost {
            id: id.to_string(),
            name: format!("Skill {id}"),
            description: description.to_string(),
        }
    }

    #[test]
    fn renderer_drops_stale_and_lower_trust_defensively() {
        let recall = json!({ "facts": [
            fact("h", "human-reviewed", false, "safe"),
            fact("s", "human-reviewed", true, "stale"),
            fact("m", "machine-confirmed", false, "machine"),
            fact("u", "unverified", false, "unverified"),
        ]});
        let out = render_injection(&recall, 800, &[]).unwrap();
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
        let skills: Vec<SkillSignpost> = (0..50)
            .map(|i| skill(&format!("s{i}"), &"verbose procedural memory ".repeat(20)))
            .collect();
        for budget in [64, 200, 800] {
            let out = render_injection(&json!({ "facts": facts }), budget, &skills).unwrap();
            assert!(
                out.estimated_tokens <= budget,
                "budget {budget} exceeded with {} facts and {} skills",
                out.fact_ids.len(),
                out.skill_ids.len()
            );
            assert_eq!(out.bytes, out.text.len());
        }
    }

    #[test]
    fn an_empty_recall_still_produces_a_bounded_block() {
        let out = render_injection(&json!({ "facts": [] }), 800, &[]).unwrap();
        assert!(out.fact_ids.is_empty());
        assert!(out.skill_ids.is_empty());
        assert!(out.estimated_tokens <= 800);
        assert!(out.to_json()["token_estimate_note"].is_string());
    }

    #[test]
    fn approved_skills_are_labelled_bounded_and_ledgered_separately() {
        let skills = vec![skill(
            "s1",
            "first line\nsecond line with a genuinely verbose description that must be cut down",
        )];
        let out = render_injection(&json!({ "facts": [] }), 800, &skills).unwrap();
        assert_eq!(out.skill_ids, vec!["s1"]);
        assert!(out.text.contains(SKILL_HEADER));
        assert!(out.text.contains("- [skill] Skill s1 — "));
        assert!(out.text.contains("load with list_skills"));
        assert!(!out.text.contains("\nsecond"));
        assert_eq!(out.to_json()["skill_ids"][0], "s1");
    }

    #[test]
    fn skill_signposts_are_trimmed_before_verified_facts() {
        let facts = json!({ "facts": [fact("f1", "human-reviewed", false, "keep me")] });
        let skills: Vec<SkillSignpost> = (0..30)
            .map(|i| skill(&format!("s{i}"), &"x".repeat(400)))
            .collect();
        let out = render_injection(&facts, 200, &skills).unwrap();
        assert_eq!(out.fact_ids, vec!["f1"]);
        assert!(out.text.contains("keep me"));
        assert!(
            out.skill_ids.len() < skills.len(),
            "expected tight budget to drop skill signposts before the verified fact"
        );
        assert!(out.estimated_tokens <= 200);
    }
}
