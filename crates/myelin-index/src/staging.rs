use crate::redact::redact;
use crate::transcript::TranscriptTurn;
use regex::Regex;
use std::sync::OnceLock;

/// Cheap, deterministic pattern-matching over a parsed transcript -
/// deliberately NOT an LLM call. This only decides "worth a look,"
/// never "worth capturing" - that judgment stays with whichever agent
/// reviews `list_pending_review` later, same bar as `record_observation`
/// today. Every excerpt is bounded and redacted before it's ever
/// returned, since this is the only path that ever sees raw transcript
/// text at all.
const MAX_EXCERPT_CHARS: usize = 300;
const MIN_MULTI_STEP_TOOLS: usize = 4;
/// Headroom for future heuristics that might produce more than one hit;
/// not reachable with the current four (each contributes at most one).
const MAX_CANDIDATES_PER_SESSION: usize = 5;

pub struct StagedCandidate {
    pub heuristic_reason: String,
    pub excerpt: String,
}

pub fn stage_candidates(turns: &[TranscriptTurn]) -> Vec<StagedCandidate> {
    let mut candidates = Vec::new();

    stage_multi_step_sequence(turns, &mut candidates);
    stage_error_then_fix(turns, &mut candidates);
    stage_correction_language(turns, &mut candidates);
    stage_high_stakes_phrasing(turns, &mut candidates);

    candidates.truncate(MAX_CANDIDATES_PER_SESSION);
    candidates
}

fn truncate_and_redact(text: &str) -> String {
    let bounded: String = text.chars().take(MAX_EXCERPT_CHARS).collect();
    redact(&bounded)
}

fn stage_multi_step_sequence(turns: &[TranscriptTurn], out: &mut Vec<StagedCandidate>) {
    let tool_names: Vec<&str> = turns
        .iter()
        .flat_map(|t| t.tool_names.iter().map(String::as_str))
        .collect();
    if tool_names.len() < MIN_MULTI_STEP_TOOLS {
        return;
    }
    let opening = turns
        .iter()
        .find(|t| t.role == "user")
        .and_then(|t| t.texts.first())
        .cloned()
        .unwrap_or_default();
    let excerpt = format!(
        "Session used {} tool calls ({}). Opening request: {opening}",
        tool_names.len(),
        tool_names.join(", ")
    );
    out.push(StagedCandidate {
        heuristic_reason: "multi-step-sequence".to_string(),
        excerpt: truncate_and_redact(&excerpt),
    });
}

fn stage_error_then_fix(turns: &[TranscriptTurn], out: &mut Vec<StagedCandidate>) {
    for (i, turn) in turns.iter().enumerate() {
        if turn.had_error_signal && turns[i + 1..].iter().any(|t| !t.tool_names.is_empty()) {
            out.push(StagedCandidate {
                heuristic_reason: "error-then-fix".to_string(),
                excerpt: truncate_and_redact(
                    "Hit an error, then continued with more tool activity before the session ended.",
                ),
            });
            return; // one hit is enough signal for the whole session
        }
    }
}

fn correction_language_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(no,|actually|instead|don't|stop|that'?s wrong|that'?s incorrect|not that)\b",
        )
        .unwrap()
    })
}

fn stage_correction_language(turns: &[TranscriptTurn], out: &mut Vec<StagedCandidate>) {
    let re = correction_language_re();
    for turn in turns.iter().filter(|t| t.role == "user") {
        for text in &turn.texts {
            if re.is_match(text) {
                out.push(StagedCandidate {
                    heuristic_reason: "correction-language".to_string(),
                    excerpt: truncate_and_redact(text),
                });
                return; // cap at one per session - avoid flooding on a chatty back-and-forth
            }
        }
    }
}

fn high_stakes_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `100\+` is split out from the shared trailing `\b`: a trailing word
    // boundary after `+` can never match when `+` is followed by a space
    // or punctuation (both non-word chars either side means no
    // boundary), which is the realistic phrasing ("100+ repos") - found
    // by testing this heuristic against a real session where it silently
    // failed to fire on exactly that text.
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(all repos|every service|fleet-wide|roll ?out|ticket|jira-\d+|across the (org|company))\b|\b100\+")
            .unwrap()
    })
}

fn stage_high_stakes_phrasing(turns: &[TranscriptTurn], out: &mut Vec<StagedCandidate>) {
    let re = high_stakes_re();
    for turn in turns.iter().filter(|t| t.role == "user") {
        for text in &turn.texts {
            if re.is_match(text) {
                out.push(StagedCandidate {
                    heuristic_reason: "high-stakes-phrasing".to_string(),
                    excerpt: truncate_and_redact(text),
                });
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_turn(text: &str) -> TranscriptTurn {
        TranscriptTurn {
            role: "user".to_string(),
            texts: vec![text.to_string()],
            tool_names: vec![],
            had_error_signal: false,
        }
    }

    fn assistant_tools(names: &[&str]) -> TranscriptTurn {
        TranscriptTurn {
            role: "assistant".to_string(),
            texts: vec![],
            tool_names: names.iter().map(|s| s.to_string()).collect(),
            had_error_signal: false,
        }
    }

    #[test]
    fn flags_multi_step_sequence_at_threshold() {
        let turns = vec![
            user_turn("fix the deploy script"),
            assistant_tools(&["Bash", "Edit", "Bash", "Read"]),
        ];
        let candidates = stage_candidates(&turns);
        assert!(candidates
            .iter()
            .any(|c| c.heuristic_reason == "multi-step-sequence"));
    }

    #[test]
    fn does_not_flag_below_threshold() {
        let turns = vec![user_turn("quick question"), assistant_tools(&["Read"])];
        let candidates = stage_candidates(&turns);
        assert!(!candidates
            .iter()
            .any(|c| c.heuristic_reason == "multi-step-sequence"));
    }

    #[test]
    fn flags_error_then_fix() {
        let mut error_turn = assistant_tools(&["Bash"]);
        error_turn.had_error_signal = true;
        let turns = vec![
            user_turn("run the tests"),
            error_turn,
            assistant_tools(&["Edit"]),
        ];
        let candidates = stage_candidates(&turns);
        assert!(candidates
            .iter()
            .any(|c| c.heuristic_reason == "error-then-fix"));
    }

    #[test]
    fn does_not_flag_error_with_no_followup_activity() {
        let mut error_turn = assistant_tools(&["Bash"]);
        error_turn.had_error_signal = true;
        let turns = vec![user_turn("run the tests"), error_turn];
        let candidates = stage_candidates(&turns);
        assert!(!candidates
            .iter()
            .any(|c| c.heuristic_reason == "error-then-fix"));
    }

    #[test]
    fn flags_correction_language_and_redacts_the_excerpt() {
        let turns = vec![user_turn(
            "no, actually use AKIAIOSFODNN7EXAMPLE for auth instead",
        )];
        let candidates = stage_candidates(&turns);
        let hit = candidates
            .iter()
            .find(|c| c.heuristic_reason == "correction-language")
            .expect("expected a correction-language hit");
        assert!(hit.excerpt.contains("[REDACTED"));
        assert!(!hit.excerpt.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn flags_high_stakes_phrasing() {
        let turns = vec![user_turn(
            "this needs to roll out fleet-wide across every service by Friday",
        )];
        let candidates = stage_candidates(&turns);
        assert!(candidates
            .iter()
            .any(|c| c.heuristic_reason == "high-stakes-phrasing"));
    }

    /// Regression test for a real bug found by running this heuristic
    /// against a real session transcript: "100+ repos" (a space after
    /// the `+`, the realistic phrasing) silently failed to match because
    /// a shared trailing `\b` can never fire right after `+` followed by
    /// whitespace - both sides of that position are non-word characters.
    #[test]
    fn flags_bare_100_plus_notation_followed_by_a_space() {
        let turns = vec![user_turn(
            "eventually need to work across 100+ repos for this rollout",
        )];
        let candidates = stage_candidates(&turns);
        assert!(candidates
            .iter()
            .any(|c| c.heuristic_reason == "high-stakes-phrasing"));
    }

    #[test]
    fn ordinary_session_produces_no_candidates() {
        let turns = vec![
            user_turn("what does this function do"),
            assistant_tools(&["Read"]),
        ];
        let candidates = stage_candidates(&turns);
        assert!(candidates.is_empty());
    }
}
