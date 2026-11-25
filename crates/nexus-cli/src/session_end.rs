//! Claude Code `SessionEnd` hook → absorbed Myelin pipeline.
//!
//! Deliberately dumb and deterministic: only messages that explicitly signpost
//! a durable fact (`DECISION:`, `LESSON:`, `DEAD END:`, `INCIDENT:`, `OWNER:`,
//! `remember:`) become staged candidates. No LLM, no raw transcript storage,
//! no auto-promotion. A human/agent still reviews and promotes the candidate,
//! and NC's curated facts remain a separate, review-gated store.

use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Read;

const MAX_HOOK_BYTES: usize = 1_048_576;
const MAX_TRANSCRIPT_BYTES: usize = 1_048_576;
const MAX_SNIPPET_CHARS: usize = 500;
const MAX_CANDIDATES: usize = 20;

/// (signpost, context signal, high stakes)
const SIGNPOSTS: [(&str, &str, bool); 6] = [
    ("DECISION:", "session_end:decision", false),
    ("LESSON:", "session_end:lesson", false),
    ("DEAD END:", "session_end:dead_end", false),
    ("INCIDENT:", "session_end:incident", true),
    ("OWNER:", "session_end:owner", false),
    ("remember:", "session_end:explicit_request", true),
];

pub fn run() -> Result<()> {
    let mut raw = String::new();
    std::io::stdin()
        .take(MAX_HOOK_BYTES as u64)
        .read_to_string(&mut raw)?;
    let hook: Value = serde_json::from_str(&raw)?;
    if hook["hook_event_name"].as_str() != Some("SessionEnd") {
        bail!("expected a Claude Code SessionEnd hook payload");
    }
    let session_id = hook["session_id"].as_str().unwrap_or("unknown");
    let project = hook["cwd"].as_str().unwrap_or(".");
    let transcript_path = hook["transcript_path"].as_str();

    let mut staged = 0usize;
    if let Some(path) = transcript_path {
        let bytes = std::fs::read(path)?;
        if bytes.len() > MAX_TRANSCRIPT_BYTES {
            bail!("transcript is larger than {MAX_TRANSCRIPT_BYTES} bytes; refusing to ingest");
        }
        // Write directly to the absorbed Myelin store's *pending review* queue.
        // This is deliberately NOT `record_observation`: that path can
        // auto-promote on repetition or a high-stakes flag, which would put an
        // un-judged transcript excerpt straight into procedural memory.
        let store = open_store()?;
        let transcript = String::from_utf8_lossy(&bytes);
        for candidate in extract_candidates(&transcript) {
            let reason = if candidate.high_stakes {
                format!(
                    "{} (prioritise review; never auto-promote)",
                    candidate.signal
                )
            } else {
                candidate.signal.to_string()
            };
            store.stage_pending_review(session_id, Some(project), &reason, &candidate.snippet)?;
            staged += 1;
            if staged >= MAX_CANDIDATES {
                break;
            }
        }
    }

    println!(
        "{}",
        serde_json::to_string(&json!({
            "session_id": session_id,
            "staged_candidates": staged,
            "note": "signpost-only deterministic staging; no LLM, no raw transcript stored, nothing auto-promoted",
        }))?
    );
    Ok(())
}

fn open_store() -> Result<myelin_index::Store> {
    let paths = myelin_core::Paths::resolve();
    let config = myelin_core::Config::load(&paths.config_file())?;
    myelin_index::Store::open(
        &paths.db_file(),
        myelin_index::StoreConfig {
            promotion_reps: config.promotion.reps,
            similarity_threshold: config.promotion.similarity_threshold,
            stale_after_secs: config.atrophy.stale_after_secs,
            max_active_skills: config.pruning.max_active_skills,
        },
    )
}

struct Candidate {
    signal: &'static str,
    high_stakes: bool,
    snippet: String,
}

/// Pull signposted messages out of a Claude Code JSONL transcript. The parser
/// is intentionally lenient about line shapes and strict about what counts as
/// a durable fact; anything unrecognised is ignored.
fn extract_candidates(transcript: &str) -> Vec<Candidate> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in transcript.lines() {
        if out.len() >= MAX_CANDIDATES {
            break;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(text) = message_text(&entry) else {
            continue;
        };
        for message_line in text.lines() {
            let trimmed = message_line.trim();
            let lower = trimmed.to_lowercase();
            for (signpost, signal, high_stakes) in SIGNPOSTS {
                let Some(pos) = lower.find(&signpost.to_lowercase()) else {
                    continue;
                };
                // Only accept the signpost at the start of the message (or
                // after a bullet), not an incidental mention mid-sentence.
                if pos > 2 || (pos > 0 && !trimmed[..pos].trim().starts_with('-')) {
                    continue;
                }
                let body = trimmed[pos + signpost.len()..].trim();
                if body.is_empty() {
                    continue;
                }
                let snippet = myelin_index::redact::redact(
                    &body.chars().take(MAX_SNIPPET_CHARS).collect::<String>(),
                );
                if snippet.trim().is_empty() || !seen.insert(snippet.clone()) {
                    continue;
                }
                out.push(Candidate {
                    signal,
                    high_stakes,
                    snippet,
                });
                break;
            }
            if out.len() >= MAX_CANDIDATES {
                break;
            }
        }
    }
    out
}

fn message_text(entry: &Value) -> Option<String> {
    let content = entry.get("message").and_then(|m| m.get("content"))?;
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let texts: Vec<&str> = parts
                .iter()
                .filter_map(|part| {
                    if part["type"].as_str() == Some("text") {
                        part["text"].as_str()
                    } else {
                        None
                    }
                })
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_signposts_become_candidates() {
        let transcript = [
            r#"{"type":"user","message":{"role":"user","content":"hey how are you"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"DECISION: presets cut the schema tax"}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":"we mentioned DECISION: mid sentence"}}"#,
            r#"{"type":"user","message":{"role":"user","content":"remember: API token sk-1234567890abcdefghijklmnop"}}"#,
        ]
        .join("\n");
        let found = extract_candidates(&transcript);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].signal, "session_end:decision");
        assert_eq!(found[0].snippet, "presets cut the schema tax");
        assert_eq!(found[1].signal, "session_end:explicit_request");
        assert!(found[1].high_stakes);
        assert!(!found[1].snippet.contains("sk-1234567890abcdefghijklmnop"));
    }

    #[test]
    fn duplicate_signposts_are_collapsed() {
        let transcript = [
            r#"{"type":"assistant","message":{"content":"LESSON: same thing"}}"#,
            r#"{"type":"assistant","message":{"content":"LESSON: same thing"}}"#,
        ]
        .join("\n");
        assert_eq!(extract_candidates(&transcript).len(), 1);
    }
}
