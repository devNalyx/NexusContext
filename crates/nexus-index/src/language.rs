use anyhow::{anyhow, Result};
use std::path::Path;
use tree_sitter::{Language as TsLanguage, Parser, Query, QueryCursor, Range, Tree};

/// Phase-1 vertical slice covers two languages to prove the pipeline;
/// widening language coverage is additive, not architectural.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
}

impl Language {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "rs" => Some(Language::Rust),
            "py" => Some(Language::Python),
            _ => None,
        }
    }

    fn ts_language(&self) -> TsLanguage {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
        }
    }

    fn fn_query(&self) -> &'static str {
        match self {
            Language::Rust => "(function_item name: (identifier) @name) @def",
            Language::Python => "(function_definition name: (identifier) @name) @def",
        }
    }

    fn type_query(&self) -> &'static str {
        match self {
            Language::Rust => "(struct_item name: (type_identifier) @name) @def",
            Language::Python => "(class_definition name: (identifier) @name) @def",
        }
    }

    fn call_query(&self) -> &'static str {
        match self {
            Language::Rust => {
                "[
                    (call_expression function: (identifier) @name)
                    (call_expression function: (field_expression field: (field_identifier) @name))
                ] @def"
            }
            Language::Python => {
                "[
                    (call function: (identifier) @name)
                    (call function: (attribute attribute: (identifier) @name))
                ] @def"
            }
        }
    }

    pub fn parse(&self, source: &[u8]) -> Result<Tree> {
        let mut parser = Parser::new();
        parser.set_language(&self.ts_language())?;
        parser
            .parse(source, None)
            .ok_or_else(|| anyhow!("tree-sitter failed to parse file"))
    }

    pub fn extract_functions(&self, tree: &Tree, src: &[u8]) -> Result<Vec<(String, Range)>> {
        self.run_capture_query(self.fn_query(), tree, src)
    }

    pub fn extract_types(&self, tree: &Tree, src: &[u8]) -> Result<Vec<(String, Range)>> {
        self.run_capture_query(self.type_query(), tree, src)
    }

    pub fn extract_calls(&self, tree: &Tree, src: &[u8]) -> Result<Vec<(String, Range)>> {
        self.run_capture_query(self.call_query(), tree, src)
    }

    fn run_capture_query(
        &self,
        query_str: &str,
        tree: &Tree,
        src: &[u8],
    ) -> Result<Vec<(String, Range)>> {
        let ts_language = self.ts_language();
        let query = Query::new(&ts_language, query_str)?;
        let name_idx = query
            .capture_index_for_name("name")
            .ok_or_else(|| anyhow!("query missing @name capture"))?;
        let def_idx = query
            .capture_index_for_name("def")
            .ok_or_else(|| anyhow!("query missing @def capture"))?;

        let mut cursor = QueryCursor::new();
        let mut out = Vec::new();
        let mut matches = cursor.matches(&query, tree.root_node(), src);
        use streaming_iterator::StreamingIterator;
        while let Some(m) = matches.next() {
            let mut name = None;
            let mut range = None;
            for cap in m.captures {
                if cap.index == name_idx {
                    name = cap.node.utf8_text(src).ok().map(|s| s.to_string());
                } else if cap.index == def_idx {
                    range = Some(cap.node.range());
                }
            }
            if let (Some(name), Some(range)) = (name, range) {
                out.push((name, range));
            }
        }
        Ok(out)
    }
}
