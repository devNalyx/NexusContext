use anyhow::{anyhow, Result};
use std::path::Path;
use tree_sitter::{Language as TsLanguage, Point, Range};
use tree_sitter_tags::{TagsConfiguration, TagsContext};

/// Definitions extracted from a file via the generic tags mechanism (see
/// module docs below) - functions/methods, type-like definitions
/// (class/interface/struct/enum/module, lumped together the same way our
/// original hand-written Rust/Python queries did), and call sites.
pub struct Extracted {
    pub functions: Vec<(String, Range)>,
    pub types: Vec<(String, Range)>,
    pub calls: Vec<(String, Range)>,
}

/// Hand-writing a `fn_query`/`type_query`/`call_query` per language made 2
/// languages feel like the practical ceiling, per the project history.
/// Instead, this consumes the `TAGS_QUERY` that nearly every
/// actively-maintained tree-sitter grammar crate already bundles: a
/// community-maintained query using conventional capture names
/// (`@definition.function`, `@definition.class`, `@reference.call`, ...)
/// that's the same mechanism GitHub's code navigation, Neovim's
/// nvim-treesitter, and the `tree-sitter tags` CLI all rely on. Adding a
/// language now costs "add the grammar crate + map its file extensions",
/// not "write and debug a new query language you don't otherwise know."
///
/// The tradeoff: quality follows whatever that community query happens to
/// cover for a given language - some grammars' tags.scm are more complete
/// than others. Not every language's grammar crate bundles one at all
/// (Kotlin's `tree-sitter-kotlin-ng`, checked while planning this, doesn't) -
/// those still need a hand-written query or don't get supported yet.
///
/// Verified, per-language tiers (definitions always work everywhere below -
/// `search_graph`/`get_architecture`/`detect_dead_code` are solid for all 11;
/// this is specifically about `trace_call_path`/cross-file `CALLS` edges):
/// - **Full**: Rust, Python, JavaScript, TypeScript/TSX, Go, Java, Ruby -
///   call sites resolve correctly, verified against real multi-file fixtures
///   and a 115-file/2600-edge real-world project.
/// - **Structural only**: C, C++ (their bundled tags.scm has no
///   `@reference.call` pattern at all - no call edges are possible via this
///   mechanism, full stop), C# (only captures member-access calls like
///   `obj.Method()`, not bare/implicit-this calls), PHP (similarly only
///   captures qualified/variable calls, not plain global function calls).
///   Functions/types/DEFINES edges are still fully correct for these four;
///   only the call graph is incomplete. This mirrors exactly the tiering the
///   project this technique was learned from has to contend with too
///   ("Excellent/Good/Functional" - not every language gets equally good
///   results from tree-sitter-only analysis).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
    Java,
    C,
    Cpp,
    CSharp,
    Ruby,
    Php,
}

impl Language {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "rs" => Some(Language::Rust),
            "py" => Some(Language::Python),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "ts" | "mts" | "cts" => Some(Language::TypeScript),
            "tsx" => Some(Language::Tsx),
            "go" => Some(Language::Go),
            "java" => Some(Language::Java),
            "c" | "h" => Some(Language::C),
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some(Language::Cpp),
            "cs" => Some(Language::CSharp),
            "rb" => Some(Language::Ruby),
            "php" => Some(Language::Php),
            _ => None,
        }
    }

    fn ts_language(&self) -> TsLanguage {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::Java => tree_sitter_java::LANGUAGE.into(),
            Language::C => tree_sitter_c::LANGUAGE.into(),
            Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Language::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Language::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Language::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        }
    }

    /// TypeScript/TSX grammars are built as an extension of JavaScript's -
    /// the same node types JS uses for functions/calls/etc. exist
    /// identically in the TS/TSX parse tree. The TypeScript crate's own
    /// tags.scm only covers what's genuinely TS-specific (interfaces, type
    /// annotations, ambient signatures) and expects to be combined with
    /// JS's tags.scm for the "normal" definitions - the same convention
    /// tools like GitHub's code navigation follow.
    fn tags_query(&self) -> String {
        match self {
            Language::Rust => tree_sitter_rust::TAGS_QUERY.to_string(),
            Language::Python => tree_sitter_python::TAGS_QUERY.to_string(),
            Language::JavaScript => tree_sitter_javascript::TAGS_QUERY.to_string(),
            Language::TypeScript | Language::Tsx => format!(
                "{}\n{}",
                tree_sitter_javascript::TAGS_QUERY,
                tree_sitter_typescript::TAGS_QUERY
            ),
            Language::Go => tree_sitter_go::TAGS_QUERY.to_string(),
            Language::Java => tree_sitter_java::TAGS_QUERY.to_string(),
            Language::C => tree_sitter_c::TAGS_QUERY.to_string(),
            Language::Cpp => tree_sitter_cpp::TAGS_QUERY.to_string(),
            Language::CSharp => strip_bad_csharp_module_capture(tree_sitter_c_sharp::TAGS_QUERY),
            Language::Ruby => tree_sitter_ruby::TAGS_QUERY.to_string(),
            Language::Php => tree_sitter_php::TAGS_QUERY.to_string(),
        }
    }

    pub fn build_tags_config(&self) -> Result<TagsConfiguration> {
        TagsConfiguration::new(self.ts_language(), &self.tags_query(), "")
            .map_err(|err| anyhow!("failed to build tags config for {self:?}: {err}"))
    }
}

/// `tree-sitter-c-sharp` 0.23.5's bundled tags.scm (the latest published
/// version - there's no newer one to pick up a fix) has one malformed
/// pattern: a bare `@module` capture alongside the correct
/// `@definition.module` for the same node, which `tree-sitter-tags`
/// rejects outright ("Invalid capture @module") since it doesn't match any
/// of the `@definition.*`/`@reference.*`/`@name`/etc. conventions. Every
/// other pattern in that file is valid, so rather than drop C# entirely,
/// this strips just that one line before compiling the query.
fn strip_bad_csharp_module_capture(query: &str) -> String {
    query
        .lines()
        .filter(|line| !line.trim().ends_with(") @module"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a definition/reference tag maps to in our graph model. Anything not
/// matched here (constants, fields, macros, cross-references to types) is
/// silently skipped rather than erroring - a forward-compatible default for
/// tag kinds this project doesn't model yet, including ones from languages
/// nobody's checked the tags.scm vocabulary for.
enum TagCategory {
    Function,
    Type,
    Call,
}

fn classify(kind: &str, is_definition: bool) -> Option<TagCategory> {
    if is_definition {
        match kind {
            "function" | "method" | "macro" => Some(TagCategory::Function),
            "class" | "interface" | "type" | "struct" | "enum" | "module" => {
                Some(TagCategory::Type)
            }
            _ => None,
        }
    } else {
        match kind {
            // C#'s tags.scm uses `@reference.send` for method invocations
            // instead of the `@reference.call` every other language here uses.
            "call" | "send" => Some(TagCategory::Call),
            _ => None,
        }
    }
}

/// Maps byte offsets to 0-indexed row numbers. `tree-sitter-tags`'s own
/// `Tag::span` is deliberately just the *name token's* position (it's built
/// for "jump to definition" navigation UIs, not "select the whole
/// definition"), so a 3-line function's span is a single line - not what
/// `ingest.rs`'s innermost-enclosing-function and qualified-name logic
/// need. `Tag::range` (byte offsets) is correct for the whole definition;
/// this converts those byte offsets into the row numbers that logic
/// actually needs, without a fresh linear scan per tag.
struct LineIndex {
    newline_offsets: Vec<usize>,
}

impl LineIndex {
    fn build(source: &[u8]) -> Self {
        let newline_offsets = source
            .iter()
            .enumerate()
            .filter(|(_, &b)| b == b'\n')
            .map(|(i, _)| i)
            .collect();
        Self { newline_offsets }
    }

    fn row_at(&self, byte_offset: usize) -> usize {
        self.newline_offsets.partition_point(|&nl| nl < byte_offset)
    }
}

/// Runs a language's tags query against one file's source, sorting the
/// results into functions/types/calls the same shape the (now-removed)
/// hand-written per-language queries used to produce - so the rest of the
/// ingestion pipeline (same-file/cross-file call resolution in `ingest.rs`)
/// didn't need to change at all.
pub fn extract(
    config: &TagsConfiguration,
    context: &mut TagsContext,
    source: &[u8],
) -> Result<Extracted> {
    let (tags, _has_error) = context
        .generate_tags(config, source, None)
        .map_err(|err| anyhow!("tag generation failed: {err}"))?;

    let line_index = LineIndex::build(source);
    let mut functions = Vec::new();
    let mut types = Vec::new();
    let mut calls = Vec::new();

    for tag in tags {
        let tag = tag.map_err(|err| anyhow!("tag iteration failed: {err}"))?;
        let kind = config.syntax_type_name(tag.syntax_type_id);
        let Some(category) = classify(kind, tag.is_definition) else {
            continue;
        };
        let Ok(name) = std::str::from_utf8(&source[tag.name_range.clone()]) else {
            continue;
        };

        let range = Range {
            start_byte: tag.range.start,
            end_byte: tag.range.end,
            start_point: Point {
                row: line_index.row_at(tag.range.start),
                column: 0,
            },
            end_point: Point {
                row: line_index.row_at(tag.range.end),
                column: 0,
            },
        };

        match category {
            TagCategory::Function => functions.push((name.to_string(), range)),
            TagCategory::Type => types.push((name.to_string(), range)),
            TagCategory::Call => calls.push((name.to_string(), range)),
        }
    }

    Ok(Extracted {
        functions,
        types,
        calls,
    })
}
