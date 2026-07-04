/// A markdown heading and its body, down to (not including) the next
/// heading of equal-or-shallower level, or EOF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    /// 1..=6, the number of leading '#' characters.
    pub level: u8,
    /// 1-indexed, matching the system-wide convention used everywhere else
    /// `start_line`/`end_line` flow (`get_file_context`, `detect_changes`'s
    /// git-diff-hunk comparison) - not tree-sitter's 0-indexed row.
    pub start_line: u32,
    /// 1-indexed, inclusive.
    pub end_line: u32,
    /// Index into the same `Vec<Section>` this came from - `None` means no
    /// parent in this file's nesting (not necessarily an H1; a file whose
    /// first heading is `###` still has `parent: None` for it).
    pub parent: Option<usize>,
}

/// Detects `^#{1,6}\s+` ATX headings only - not Setext (`Title\n===`), which
/// is a deliberate v1 exclusion despite READMEs commonly using it for the
/// top title line. Skips detection entirely inside fenced code blocks
/// (``` or ~~~) - without this, a shell comment (`# do a thing`) inside
/// almost any real README's example commands would otherwise be misread as
/// a heading, producing garbage sections and garbage embeddings.
pub fn extract_sections(text: &str) -> Vec<Section> {
    let lines: Vec<&str> = text.lines().collect();
    let mut sections: Vec<Section> = Vec::new();
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut in_fence = false;
    let mut fence_marker = ' ';

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if let Some(marker) = fence_delim(trimmed) {
            if !in_fence {
                in_fence = true;
                fence_marker = marker;
            } else if marker == fence_marker {
                // Only the same fence character that opened this block
                // closes it - a `~~~` appearing inside a ``` block (e.g.
                // demonstrating fence syntax in an example) must not be
                // mistaken for the closing fence.
                in_fence = false;
            }
            continue;
        }
        if in_fence {
            continue;
        }

        let Some((level, name)) = parse_atx_heading(line) else {
            continue;
        };

        while matches!(stack.last(), Some((lvl, _)) if *lvl >= level) {
            stack.pop();
        }
        let parent = stack.last().map(|(_, idx)| *idx);
        let idx = sections.len();
        sections.push(Section {
            name,
            level,
            start_line: i as u32 + 1,
            end_line: 0, // filled in below
            parent,
        });
        stack.push((level, idx));
    }

    for i in 0..sections.len() {
        let level = sections[i].level;
        sections[i].end_line = sections[(i + 1)..]
            .iter()
            .find(|s| s.level <= level)
            .map(|s| s.start_line - 1)
            .unwrap_or(lines.len() as u32);
    }

    sections
}

/// Returns the fence character (`` ` `` or `~`) if `line` opens or closes a
/// fenced code block - at least 3 repeated fence characters, optionally
/// followed by a language tag on opening (e.g. ` ```bash `).
fn fence_delim(line: &str) -> Option<char> {
    for marker in ['`', '~'] {
        let run_len = line.chars().take_while(|&c| c == marker).count();
        if run_len >= 3 {
            return Some(marker);
        }
    }
    None
}

fn parse_atx_heading(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &trimmed[level..];
    // A real ATX heading requires whitespace after the '#'s - "#5 items"
    // isn't a heading, it's just a line starting with a hash character.
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    let name = rest.trim().trim_end_matches('#').trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some((level as u8, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_headings_link_to_correct_parents() {
        let text = "# Title\n\n## Section A\n\n### Sub A1\n\n## Section B\n";
        let sections = extract_sections(text);
        let names: Vec<&str> = sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Title", "Section A", "Sub A1", "Section B"]);
        assert_eq!(sections[0].parent, None); // Title
        assert_eq!(sections[1].parent, Some(0)); // Section A -> Title
        assert_eq!(sections[2].parent, Some(1)); // Sub A1 -> Section A
        assert_eq!(sections[3].parent, Some(0)); // Section B -> Title (sibling of A)
    }

    #[test]
    fn level_skip_attaches_to_nearest_open_ancestor() {
        let text = "# Title\n\n### Deep Sub\n";
        let sections = extract_sections(text);
        assert_eq!(sections[1].name, "Deep Sub");
        assert_eq!(sections[1].parent, Some(0)); // attaches to Title, no phantom H2
    }

    #[test]
    fn multiple_top_level_headings_are_independent_trees() {
        let text = "# First\n\n# Second\n";
        let sections = extract_sections(text);
        assert_eq!(sections[0].parent, None);
        assert_eq!(sections[1].parent, None);
    }

    #[test]
    fn heading_before_any_parent_is_top_level_regardless_of_its_own_level() {
        let text = "### Banner\n\nsome text\n";
        let sections = extract_sections(text);
        assert_eq!(sections[0].level, 3);
        assert_eq!(sections[0].parent, None);
    }

    #[test]
    fn start_line_is_one_indexed() {
        let text = "line one\nline two\n# Heading\n";
        let sections = extract_sections(text);
        assert_eq!(sections[0].start_line, 3);
    }

    #[test]
    fn hash_inside_fenced_code_block_is_not_a_heading() {
        let text = "# Real Heading\n\n```bash\n# this is a shell comment, not a heading\necho hi\n```\n\n## Another Real Heading\n";
        let sections = extract_sections(text);
        let names: Vec<&str> = sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Real Heading", "Another Real Heading"]);
    }

    #[test]
    fn tilde_fences_are_also_respected() {
        let text = "# Heading\n\n~~~\n# not a heading\n~~~\n";
        let sections = extract_sections(text);
        assert_eq!(sections.len(), 1);
    }

    #[test]
    fn mismatched_fence_marker_does_not_close_the_block() {
        // A ``` block whose example content itself contains a ~~~ line
        // (e.g. demonstrating fence syntax) must not be closed by it -
        // only a matching ``` closes this block.
        let text =
            "```\n~~~\n# not a heading, still inside the ``` block\n```\n\n## Real Heading\n";
        let sections = extract_sections(text);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].name, "Real Heading");
    }

    #[test]
    fn end_line_stops_before_next_same_or_shallower_heading() {
        let text = "# A\nbody a\n## B\nbody b\n# C\nbody c\n";
        let sections = extract_sections(text);
        assert_eq!(sections[0].name, "A");
        // A's range includes its child B (deeper level) - only a
        // same-or-shallower heading (C, line 5) ends it.
        assert_eq!(sections[0].end_line, 4);
        assert_eq!(sections[1].name, "B");
        assert_eq!(sections[1].end_line, 4); // stops before "# C" on line 5
        assert_eq!(sections[2].name, "C");
        assert_eq!(sections[2].end_line, 6); // runs to EOF
    }

    #[test]
    fn bare_hash_line_without_space_is_not_a_heading() {
        let text = "#5 items in stock\n\n# Real Heading\n";
        let sections = extract_sections(text);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].name, "Real Heading");
    }
}
