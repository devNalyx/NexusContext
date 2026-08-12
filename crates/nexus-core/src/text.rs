//! Small shared text-truncation helper. Used anywhere an MCP tool response
//! needs a hard byte ceiling - `nexus-index`'s `get_file_context` and
//! `nexusd`'s `search_codebase`/`query_memory` chunk_text both had their
//! own copy of this exact logic until a PR review flagged the duplication;
//! this is the single shared home for it now.

/// Truncates `text` to at most `max_bytes`, cutting at the last whole UTF-8
/// character rather than a raw byte index - a raw slice can land
/// mid-codepoint, which panics (or produces invalid UTF-8) on non-ASCII
/// text. Returns the (possibly shortened) text and whether it was actually
/// shortened, so callers can surface that honestly instead of silently
/// dropping content.
pub fn truncate_to_byte_boundary(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut end = 0;
    for (i, ch) in text.char_indices() {
        if i + ch.len_utf8() > max_bytes {
            break;
        }
        end = i + ch.len_utf8();
    }
    (text[..end].to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_at_or_under_the_limit_passes_through_unmarked() {
        let (text, truncated) = truncate_to_byte_boundary("short", 1000);
        assert_eq!(text, "short");
        assert!(!truncated);
    }

    #[test]
    fn text_exactly_at_the_limit_is_not_marked_truncated() {
        let exact = "x".repeat(1000);
        let (text, truncated) = truncate_to_byte_boundary(&exact, 1000);
        assert_eq!(text.len(), 1000);
        assert!(!truncated);
    }

    #[test]
    fn ascii_text_over_the_limit_is_cut_exactly_at_the_byte_count() {
        let long = "x".repeat(1500);
        let (text, truncated) = truncate_to_byte_boundary(&long, 1000);
        assert_eq!(text.len(), 1000);
        assert!(truncated);
    }

    #[test]
    fn multi_byte_text_is_bounded_by_bytes_not_chars() {
        let cjk = "文".repeat(1000); // 3 bytes/char in UTF-8
        let (text, truncated) = truncate_to_byte_boundary(&cjk, 1000);
        assert!(truncated);
        assert!(text.len() <= 1000);
        // A char-based cap would have returned 1000 *characters* here -
        // 3x over budget in bytes. This is the regression the shared
        // helper exists to prevent from recurring in either caller.
        assert!(text.chars().count() < 1000);
        assert!(std::str::from_utf8(text.as_bytes()).is_ok());
    }
}
