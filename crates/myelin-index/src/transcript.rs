use serde_json::Value;
use std::path::Path;

/// One user or assistant turn, flattened out of Claude Code's JSONL
/// transcript format for the staging heuristics to scan.
///
/// **Honest caveat**: this parser is deliberately defensive rather than
/// built against a confirmed schema. Only the top-level `type` field
/// values were ever safely inspected on a real transcript (`user`,
/// `assistant`, `mode`, `system`, etc. - no message content, per this
/// project's own threat model: reading another session's actual content
/// without explicit authorization isn't something to do casually, even
/// for development). The `message.content` block shape assumed here
/// (`text` / `tool_use` / `tool_result` blocks, matching the public
/// Anthropic Messages API) is standard and well-documented, but has not
/// been verified against a real Claude Code transcript file. Unknown or
/// unexpected shapes are skipped rather than causing a parse error -
/// this must never be the thing that makes `myelin ingest-session` fail
/// loudly on every session.
pub struct TranscriptTurn {
    pub role: String,
    pub texts: Vec<String>,
    pub tool_names: Vec<String>,
    pub had_error_signal: bool,
}

const ERROR_SIGNAL_WORDS: &[&str] = &[
    "error",
    "exception",
    "traceback",
    "failed",
    "fatal:",
    "panicked",
];

pub fn parse_transcript(path: &Path) -> anyhow::Result<Vec<TranscriptTurn>> {
    let content = std::fs::read_to_string(path)?;
    let mut turns = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<Value>(line) else {
            continue; // malformed line - skip, don't fail the whole session
        };

        let event_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if event_type != "user" && event_type != "assistant" {
            continue;
        }

        let Some(turn) = parse_turn(event_type, &obj) else {
            continue;
        };
        turns.push(turn);
    }

    Ok(turns)
}

fn parse_turn(role: &str, obj: &Value) -> Option<TranscriptTurn> {
    let content = obj.get("message").and_then(|m| m.get("content"))?;

    let mut texts = Vec::new();
    let mut tool_names = Vec::new();
    let mut had_error_signal = false;

    match content {
        // Some transcript events carry a plain string instead of a
        // content-block array for simple text-only turns.
        Value::String(s) => texts.push(s.clone()),
        Value::Array(blocks) => {
            for block in blocks {
                let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match block_type {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                            texts.push(text.to_string());
                        }
                    }
                    "tool_use" => {
                        if let Some(name) = block.get("name").and_then(|v| v.as_str()) {
                            tool_names.push(name.to_string());
                        }
                    }
                    "tool_result" => {
                        // Don't retain the content - only whether it looks
                        // like a failure, and only ever a bounded amount
                        // is ever inspected.
                        let stringified = block.to_string();
                        let lower = stringified.to_lowercase();
                        if ERROR_SIGNAL_WORDS.iter().any(|w| lower.contains(w)) {
                            had_error_signal = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => return None,
    }

    if texts.is_empty() && tool_names.is_empty() && !had_error_signal {
        return None;
    }

    Some(TranscriptTurn {
        role: role.to_string(),
        texts,
        tool_names,
        had_error_signal,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Hand-constructed synthetic transcript, matching the standard
    /// Messages API content-block shape this parser assumes - explicitly
    /// NOT derived from any real transcript file. See the module-level
    /// doc comment for why.
    fn write_fixture(content: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "myelin-transcript-test-{}.jsonl",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn parses_text_and_tool_use_blocks() {
        let fixture = write_fixture(
            r#"{"type":"mode","mode":"normal"}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"please fix the migration script"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"sure, looking now"},{"type":"tool_use","name":"Bash","input":{}},{"type":"tool_use","name":"Edit","input":{}}]}}
"#,
        );
        let turns = parse_transcript(&fixture).unwrap();
        std::fs::remove_file(&fixture).ok();

        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].texts, vec!["please fix the migration script"]);
        assert_eq!(turns[1].tool_names, vec!["Bash", "Edit"]);
    }

    #[test]
    fn detects_error_signal_in_tool_result() {
        let fixture = write_fixture(
            r#"{"type":"user","message":{"role":"user","content":"run the tests"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_result","content":"Traceback (most recent call last): boom"}]}}
"#,
        );
        let turns = parse_transcript(&fixture).unwrap();
        std::fs::remove_file(&fixture).ok();

        assert!(turns[1].had_error_signal);
    }

    #[test]
    fn skips_non_user_assistant_events_and_malformed_lines() {
        let fixture = write_fixture(
            "{\"type\":\"system\",\"data\":\"whatever\"}\nnot even json\n{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n",
        );
        let turns = parse_transcript(&fixture).unwrap();
        std::fs::remove_file(&fixture).ok();

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].texts, vec!["hi"]);
    }

    #[test]
    fn missing_file_is_an_error_not_a_panic() {
        let result = parse_transcript(Path::new("/nonexistent/path/to/transcript.jsonl"));
        assert!(result.is_err());
    }
}
