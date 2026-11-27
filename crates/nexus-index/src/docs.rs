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

/// One Obsidian-style `[[link]]` found in a note body. `target` is the raw
/// link target (before `#heading` / `|display` are split off); resolution to
/// an actual file happens later, in the project-wide ingest pass, so the
/// parser here stays purely syntactic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultLink {
    pub target: String,
    pub heading: Option<String>,
    pub display: Option<String>,
    /// True for `![[embed]]` syntax. Embeds are links for graph purposes;
    /// whether to render their contents is a client concern, not ours.
    pub embed: bool,
    /// 1-indexed, matching the rest of the index.
    pub line: u32,
}

/// Vault-specific metadata extracted from a Markdown note: YAML frontmatter
/// `tags`/`aliases` plus body wikilinks and inline `#tags`. Deliberately a
/// narrow subset of Obsidian's property/link syntax - no Dataview fields,
/// block references or link rendering are attempted here.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VaultNote {
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    pub links: Vec<VaultLink>,
}

#[derive(Default)]
struct Frontmatter {
    tags: Vec<String>,
    aliases: Vec<String>,
}

/// Returns `(first_body_line_index, frontmatter)`. When there is no closed
/// leading `---` block, returns `(0, default)` so the body scan starts at the
/// first line and a stray `---` further down a file is never mistaken for a
/// frontmatter delimiter.
fn parse_frontmatter(lines: &[&str]) -> (usize, Frontmatter) {
    if lines.first().map(|line| line.trim() == "---") != Some(true) {
        return (0, Frontmatter::default());
    }
    let Some(close) = lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, line)| line.trim() == "---")
        .map(|(idx, _)| idx)
    else {
        return (0, Frontmatter::default());
    };

    let mut result = Frontmatter::default();
    // `Some(true)` = tags, `Some(false)` = aliases: block-list items (`- x`)
    // belong to whichever key was seen most recently, matching YAML's
    // indentation-insensitive shape for these two simple properties.
    let mut active: Option<bool> = None;
    for line in &lines[1..close] {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("tags:") {
            active = Some(true);
            append_yaml_values(value, &mut result.tags);
        } else if let Some(value) = trimmed.strip_prefix("aliases:") {
            active = Some(false);
            append_yaml_values(value, &mut result.aliases);
        } else if let Some(value) = trimmed.strip_prefix("- ") {
            match active {
                Some(true) => push_property(&mut result.tags, value),
                Some(false) => push_property(&mut result.aliases, value),
                None => {}
            }
        }
    }
    (close + 1, result)
}

fn append_yaml_values(raw: &str, out: &mut Vec<String>) {
    let value = raw.trim();
    if value.is_empty() || value == "[]" {
        return;
    }
    if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        for item in inner.split(',') {
            push_property(out, item);
        }
    } else {
        push_property(out, value);
    }
}

fn push_property(out: &mut Vec<String>, raw: &str) {
    let value = raw.trim().trim_matches('"').trim_matches('\'').trim();
    let value = value.strip_prefix('#').unwrap_or(value).trim();
    if !value.is_empty() {
        out.push(value.to_string());
    }
}

/// Extracts vault-specific metadata from a note's raw text. Fenced code is
/// skipped with the same marker-matching rules as `extract_sections`, so a
/// `[[link]]` inside an example shell block never creates a graph edge.
pub fn extract_vault_note(text: &str) -> VaultNote {
    let lines: Vec<&str> = text.lines().collect();
    let (body_start, frontmatter) = parse_frontmatter(&lines);
    let mut note = VaultNote {
        tags: frontmatter.tags,
        aliases: frontmatter.aliases,
        links: Vec::new(),
    };

    let mut in_fence = false;
    let mut fence_marker = ' ';
    for (idx, line) in lines.iter().enumerate().skip(body_start) {
        let trimmed = line.trim_start();
        if let Some(marker) = fence_delim(trimmed) {
            if !in_fence {
                in_fence = true;
                fence_marker = marker;
            } else if marker == fence_marker {
                in_fence = false;
            }
            continue;
        }
        if in_fence {
            continue;
        }
        collect_wikilinks(line, idx as u32 + 1, &mut note.links);
        collect_inline_tags(line, &mut note.tags);
    }

    note.tags.sort();
    note.tags.dedup();
    note.aliases.sort();
    note.aliases.dedup();
    note
}

fn collect_wikilinks(line: &str, line_no: u32, out: &mut Vec<VaultLink>) {
    let bytes = line.as_bytes();
    let mut cursor = 0;
    while let Some(rel) = line[cursor..].find("[[") {
        let start = cursor + rel;
        let Some(end_rel) = line[start + 2..].find("]]") else {
            break;
        };
        let inner_end = start + 2 + end_rel;
        let inner = &line[start + 2..inner_end];

        let (target_part, display) = match inner.split_once('|') {
            Some((target, display)) => (target, Some(display.trim().to_string())),
            None => (inner, None),
        };
        let (target, heading) = match target_part.split_once('#') {
            Some((target, heading)) => (target, Some(heading.trim().to_string())),
            None => (target_part, None),
        };
        let target = target.trim();
        if !target.is_empty() {
            out.push(VaultLink {
                target: target.to_string(),
                heading,
                display,
                embed: start > 0 && bytes[start - 1] == b'!',
                line: line_no,
            });
        }
        cursor = inner_end + 2;
    }
}

fn collect_inline_tags(line: &str, out: &mut Vec<String>) {
    let mut prev_whitespace = true;
    for (idx, ch) in line.char_indices() {
        if ch == '#' && prev_whitespace {
            let rest = &line[idx + 1..];
            let mut end = 0;
            for (offset, c) in rest.char_indices() {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/') {
                    end = offset + c.len_utf8();
                } else {
                    break;
                }
            }
            if end > 0 && rest.as_bytes()[0].is_ascii_alphabetic() {
                push_property(out, &rest[..end]);
            }
        }
        prev_whitespace = ch.is_whitespace();
    }
}

/// Detects `^#{1,6}\s+` ATX headings only - not Setext (`Title\n===`), which
/// is a deliberate v1 exclusion despite READMEs commonly using it for the
/// top title line. Skips detection entirely inside fenced code blocks
/// (``` or ~~~) - without this, a shell comment (`# do a thing`) inside
/// almost any real README's example commands would otherwise be misread as
/// a heading, producing garbage sections.
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
        let current_line = i as u32 + 1;

        // A section's `end_line` becomes knowable at exactly the moment
        // it's popped here - it's popped because *this* heading is the
        // next one at an equal-or-shallower level, which is the same
        // "next equal-or-shallower heading" definition `end_line` uses.
        // This used to be a second O(n^2) pass (for each section, scan
        // every later one to find that boundary) - computing it in the
        // same single pass that already builds `parent` via this stack
        // makes it O(n) instead, with identical results (verified against
        // the old two-pass version - both a same-level "close two nested
        // sections at once" case and a shallower-level case produce the
        // same end_line either way). See issue #38.
        while matches!(stack.last(), Some((lvl, _)) if *lvl >= level) {
            let (_, popped_idx) = stack.pop().unwrap();
            sections[popped_idx].end_line = current_line - 1;
        }
        let parent = stack.last().map(|(_, idx)| *idx);
        let idx = sections.len();
        sections.push(Section {
            name,
            level,
            start_line: current_line,
            end_line: 0, // filled in above when popped, or below at EOF
            parent,
        });
        stack.push((level, idx));
    }

    // Anything still open at EOF (no later heading ever closed it) runs to
    // the end of the file.
    for (_, idx) in stack {
        sections[idx].end_line = lines.len() as u32;
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

    /// Regression test for issue #38: `end_line` computation moved from a
    /// second O(n^2) scan to being derived in the same single pass the
    /// `parent`-tracking stack already makes, via the same pop. This
    /// specifically exercises a heading closing *three* nested sections at
    /// once (H1 -> H2 -> H3 -> H1), the case most likely to expose an
    /// off-by-one or wrong-section-gets-the-boundary bug in a stack-based
    /// rewrite of a loop that used to just linearly scan forward.
    #[test]
    fn one_heading_can_close_several_nested_sections_at_once() {
        let text = "# A\n## B\n### C\n# D\n";
        let sections = extract_sections(text);
        assert_eq!(sections.len(), 4);
        // A (line 1), B (line 2), C (line 3) are all still "open" when D
        // (line 4) arrives - all three must close at line 3, not just the
        // innermost one.
        assert_eq!(sections[0].name, "A");
        assert_eq!(sections[0].end_line, 3);
        assert_eq!(sections[1].name, "B");
        assert_eq!(sections[1].end_line, 3);
        assert_eq!(sections[2].name, "C");
        assert_eq!(sections[2].end_line, 3);
        assert_eq!(sections[3].name, "D");
        assert_eq!(sections[3].end_line, 4); // runs to EOF
    }

    #[test]
    fn bare_hash_line_without_space_is_not_a_heading() {
        let text = "#5 items in stock\n\n# Real Heading\n";
        let sections = extract_sections(text);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].name, "Real Heading");
    }

    #[test]
    fn frontmatter_flow_and_block_tags_and_aliases_are_read() {
        let text =
            "---\ntags: [moc, current]\naliases:\n  - Home\n  - \"Start here\"\n---\n# Body\n";
        let note = extract_vault_note(text);
        assert_eq!(note.tags, vec!["current", "moc"]);
        assert_eq!(note.aliases, vec!["Home", "Start here"]);
        assert!(note.links.is_empty());
    }

    #[test]
    fn wikilinks_keep_target_heading_display_embed_and_line() {
        let text = "# A\n\n[[Status]] and [[Roadmap#Next|the plan]] and ![[chart.png]]\n";
        let note = extract_vault_note(text);
        assert_eq!(note.links.len(), 3);

        assert_eq!(note.links[0].target, "Status");
        assert_eq!(note.links[0].heading, None);
        assert!(!note.links[0].embed);
        assert_eq!(note.links[0].line, 3);

        assert_eq!(note.links[1].target, "Roadmap");
        assert_eq!(note.links[1].heading.as_deref(), Some("Next"));
        assert_eq!(note.links[1].display.as_deref(), Some("the plan"));

        assert_eq!(note.links[2].target, "chart.png");
        assert!(note.links[2].embed);
    }

    #[test]
    fn wikilinks_inside_fenced_code_are_not_extracted() {
        let text = "# A\n\n```markdown\n[[NotALink]]\n```\n\n[[RealLink]]\n";
        let note = extract_vault_note(text);
        let targets: Vec<&str> = note.links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, vec!["RealLink"]);
    }

    #[test]
    fn inline_tags_do_not_swallow_headings_or_url_fragments() {
        let text = "# Heading\n\nSee https://example.com#fragment and #moc #nested/tag.\n";
        let note = extract_vault_note(text);
        assert_eq!(note.tags, vec!["moc", "nested/tag"]);
    }
}
