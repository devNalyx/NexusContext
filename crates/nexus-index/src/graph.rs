use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

/// One SQLite file per indexed project (matches the proposal's
/// `<project-hash>/graph.db` layout) - so there is no `project_id` column,
/// each store is already scoped to a single project.
pub struct GraphStore {
    conn: Connection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Function,
    /// Covers struct/class/interface alike - we don't do full type semantics
    /// in Phase 1, just "this is a named type definition".
    Type,
    /// A markdown heading and its body, down to (not including) the next
    /// heading of equal-or-shallower level - see `docs::extract_sections`.
    Section,
}

impl NodeKind {
    fn as_str(&self) -> &'static str {
        match self {
            NodeKind::File => "File",
            NodeKind::Function => "Function",
            NodeKind::Type => "Type",
            NodeKind::Section => "Section",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "File" => NodeKind::File,
            "Function" => NodeKind::Function,
            "Section" => NodeKind::Section,
            _ => NodeKind::Type,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Defines,
    Calls,
    /// Parent heading -> child heading (nesting). `Defines` stays as
    /// File -> top-level heading (no parent in its own file's nesting -
    /// not necessarily an H1), matching the File -> Function/Type pattern.
    Contains,
}

impl EdgeKind {
    fn as_str(&self) -> &'static str {
        match self {
            EdgeKind::Defines => "DEFINES",
            EdgeKind::Calls => "CALLS",
            EdgeKind::Contains => "CONTAINS",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone)]
pub struct NodeRecord {
    pub id: i64,
    pub kind: NodeKind,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone)]
pub struct CodeSearchHit {
    pub file_path: String,
    pub snippet: String,
}

impl GraphStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        // WAL lets readers (nexusd mcp) and a writer (nexusd serve, or vice
        // versa) work concurrently instead of the whole-file locking the
        // default rollback journal uses - relevant now that the daemon and
        // an MCP session can both hold a connection to the same graph.db.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // Two full-rebuild writers (e.g. the auto-sync watcher and a manual
        // reindex) can legitimately target the same project at once -
        // without a busy timeout, the second one to reach BEGIN IMMEDIATE
        // fails immediately instead of waiting for the first to finish.
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS nodes (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                kind            TEXT NOT NULL,
                name            TEXT NOT NULL,
                qualified_name  TEXT NOT NULL UNIQUE,
                file_path       TEXT NOT NULL,
                start_line      INTEGER NOT NULL,
                end_line        INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS edges (
                id      INTEGER PRIMARY KEY AUTOINCREMENT,
                src_id  INTEGER NOT NULL REFERENCES nodes(id),
                dst_id  INTEGER NOT NULL REFERENCES nodes(id),
                kind    TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);
            CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src_id, kind);
            CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst_id, kind);
            CREATE VIRTUAL TABLE IF NOT EXISTS file_contents_fts
                USING fts5(file_path UNINDEXED, content);
            CREATE TABLE IF NOT EXISTS embeddings (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id     INTEGER NOT NULL REFERENCES nodes(id),
                model       TEXT NOT NULL,
                dim         INTEGER NOT NULL,
                chunk_text  TEXT NOT NULL,
                embedding   BLOB NOT NULL,
                UNIQUE(node_id, model)
            );
            CREATE INDEX IF NOT EXISTS idx_embeddings_node ON embeddings(node_id);
            ",
        )?;
        Ok(Self { conn })
    }

    /// Phase 1 reindexing is a full rebuild, not an incremental diff -
    /// incremental edge correctness is flagged as an open risk in the
    /// proposal and deferred past this vertical slice.
    pub fn clear(&self) -> Result<()> {
        self.conn.execute("DELETE FROM embeddings", [])?;
        self.conn.execute("DELETE FROM file_contents_fts", [])?;
        self.conn.execute("DELETE FROM edges", [])?;
        self.conn.execute("DELETE FROM nodes", [])?;
        Ok(())
    }

    /// `BEGIN IMMEDIATE` acquires the write lock up front rather than on
    /// first write, so a second full-rebuild (e.g. the auto-sync watcher
    /// firing while a manual reindex is already running) blocks here -
    /// via the busy timeout set in `open` - until the first one commits,
    /// instead of interleaving with it. Two-pass indexing (nodes now,
    /// cross-file edges at the very end) widened the window where that
    /// interleaving could produce a dangling foreign key, which is what
    /// surfaced this in practice.
    pub fn begin_immediate(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    pub fn rollback(&self) -> Result<()> {
        self.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }

    pub fn insert_node(
        &self,
        kind: NodeKind,
        name: &str,
        qualified_name: &str,
        file_path: &str,
        start_line: u32,
        end_line: u32,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO nodes (kind, name, qualified_name, file_path, start_line, end_line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(qualified_name) DO UPDATE SET
                kind = excluded.kind,
                name = excluded.name,
                file_path = excluded.file_path,
                start_line = excluded.start_line,
                end_line = excluded.end_line",
            rusqlite::params![
                kind.as_str(),
                name,
                qualified_name,
                file_path,
                start_line,
                end_line
            ],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM nodes WHERE qualified_name = ?1",
            [qualified_name],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    pub fn insert_edge(&self, src_id: i64, dst_id: i64, kind: EdgeKind) -> Result<()> {
        self.conn.execute(
            "INSERT INTO edges (src_id, dst_id, kind) VALUES (?1, ?2, ?3)",
            rusqlite::params![src_id, dst_id, kind.as_str()],
        )?;
        Ok(())
    }

    pub fn stats(&self) -> Result<(i64, i64)> {
        let nodes: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
        let edges: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
        Ok((nodes, edges))
    }

    /// `search_graph`-equivalent: substring match over node names.
    /// Backs the Cypher-lite query engine's single supported pattern shape:
    /// `(a:kind_a)-[:edge_kind]->(b:kind_b)`, with an optional `WHERE var.name
    /// = value` filter on either side and a choice of which side to return.
    pub fn match_pattern(
        &self,
        kind_a: &str,
        edge_kind: &str,
        kind_b: &str,
        where_clause: Option<(bool, &str)>,
        return_a: bool,
        limit: u32,
    ) -> Result<Vec<NodeRecord>> {
        let select_alias = if return_a { "a" } else { "b" };
        let mut sql = format!(
            "SELECT DISTINCT {select_alias}.id, {select_alias}.kind, {select_alias}.name, \
             {select_alias}.qualified_name, {select_alias}.file_path, \
             {select_alias}.start_line, {select_alias}.end_line
             FROM nodes a JOIN edges e ON e.src_id = a.id JOIN nodes b ON e.dst_id = b.id
             WHERE a.kind = ?1 AND e.kind = ?2 AND b.kind = ?3"
        );

        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(kind_a.to_string()),
            Box::new(edge_kind.to_string()),
            Box::new(kind_b.to_string()),
        ];

        if let Some((is_on_a, value)) = where_clause {
            let target = if is_on_a { "a" } else { "b" };
            sql.push_str(&format!(" AND {target}.name = ?{}", params.len() + 1));
            params.push(Box::new(value.to_string()));
        }
        sql.push_str(&format!(" LIMIT ?{}", params.len() + 1));
        params.push(Box::new(limit));

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(NodeRecord {
                id: row.get(0)?,
                kind: NodeKind::from_str(&row.get::<_, String>(1)?),
                name: row.get(2)?,
                qualified_name: row.get(3)?,
                file_path: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn search_by_name(&self, pattern: &str, limit: u32) -> Result<Vec<NodeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, qualified_name, file_path, start_line, end_line
             FROM nodes WHERE name LIKE ?1 ORDER BY name LIMIT ?2",
        )?;
        let like_pattern = format!("%{pattern}%");
        let rows = stmt.query_map(rusqlite::params![like_pattern, limit], |row| {
            Ok(NodeRecord {
                id: row.get(0)?,
                kind: NodeKind::from_str(&row.get::<_, String>(1)?),
                name: row.get(2)?,
                qualified_name: row.get(3)?,
                file_path: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Stores a file's raw text for full-text search - separate from the
    /// symbol graph entirely, since `search_by_name` only ever matched
    /// symbol names, never file content.
    pub fn insert_file_content(&self, file_path: &str, content: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO file_contents_fts (file_path, content) VALUES (?1, ?2)",
            rusqlite::params![file_path, content],
        )?;
        Ok(())
    }

    /// Grep-like search over indexed file content (not symbol names) via
    /// SQLite FTS5. The query is always treated as a literal phrase (quoted
    /// and internal quotes escaped) rather than passed through as raw FTS5
    /// query syntax - safer for arbitrary free-text input, at the cost of
    /// not exposing FTS5's AND/OR/NOT/prefix operators in this version.
    pub fn search_code(&self, query: &str, limit: u32) -> Result<Vec<CodeSearchHit>> {
        let phrase = format!("\"{}\"", query.replace('"', "\"\""));
        let mut stmt = self.conn.prepare(
            "SELECT file_path, snippet(file_contents_fts, 1, '>>>', '<<<', ' ... ', 20)
             FROM file_contents_fts WHERE file_contents_fts MATCH ?1
             ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![phrase, limit], |row| {
            Ok(CodeSearchHit {
                file_path: row.get(0)?,
                snippet: row.get(1)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn node_by_id(&self, id: i64) -> Result<Option<NodeRecord>> {
        self.conn
            .query_row(
                "SELECT id, kind, name, qualified_name, file_path, start_line, end_line
                 FROM nodes WHERE id = ?1",
                [id],
                |row| {
                    Ok(NodeRecord {
                        id: row.get(0)?,
                        kind: NodeKind::from_str(&row.get::<_, String>(1)?),
                        name: row.get(2)?,
                        qualified_name: row.get(3)?,
                        file_path: row.get(4)?,
                        start_line: row.get(5)?,
                        end_line: row.get(6)?,
                    })
                },
            )
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other.into()),
            })
    }

    /// One row per embedded chunk (currently: one per `Function`/`Type`
    /// node - see `ingest.rs`'s embedding pass). `ON CONFLICT` lets a
    /// reindex refresh an existing node's vector in place rather than
    /// accumulating stale duplicates, matching `insert_node`'s own pattern.
    pub fn insert_embedding(
        &self,
        node_id: i64,
        model: &str,
        dim: usize,
        chunk_text: &str,
        embedding: &[u8],
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO embeddings (node_id, model, dim, chunk_text, embedding)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(node_id, model) DO UPDATE SET
                dim = excluded.dim,
                chunk_text = excluded.chunk_text,
                embedding = excluded.embedding",
            rusqlite::params![node_id, model, dim as i64, chunk_text, embedding],
        )?;
        Ok(())
    }

    /// Every embedded chunk for one model - callers must always scope by
    /// model (mixing vectors from two different embedding models in one
    /// ranking is meaningless, not just suboptimal - dimensions and vector
    /// spaces aren't comparable across models).
    pub fn embeddings_for_model(&self, model: &str) -> Result<Vec<(i64, String, Vec<u8>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT node_id, chunk_text, embedding FROM embeddings WHERE model = ?1")?;
        let rows = stmt.query_map([model], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// All nodes in the graph - used by the Obsidian export, which needs
    /// the whole graph rather than a name/range-scoped query.
    pub fn all_nodes(&self) -> Result<Vec<NodeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, qualified_name, file_path, start_line, end_line FROM nodes",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(NodeRecord {
                id: row.get(0)?,
                kind: NodeKind::from_str(&row.get::<_, String>(1)?),
                name: row.get(2)?,
                qualified_name: row.get(3)?,
                file_path: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// All `CALLS` edges as (caller_id, callee_id) pairs - same rationale as
    /// `all_nodes`.
    pub fn all_call_edges(&self) -> Result<Vec<(i64, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT src_id, dst_id FROM edges WHERE kind = 'CALLS'")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Every edge of `edge_kind` where both endpoints are already in
    /// `node_ids` - for rendering a bounded subgraph (e.g. a `trace_calls`
    /// result) without a second full-graph traversal. Generic and reusable
    /// beyond call graphs.
    pub fn subgraph_edges(&self, node_ids: &[i64], edge_kind: &str) -> Result<Vec<(i64, i64)>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = node_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT DISTINCT src_id, dst_id FROM edges
             WHERE kind = ? AND src_id IN ({placeholders}) AND dst_id IN ({placeholders})"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&edge_kind];
        for id in node_ids {
            params.push(id);
        }
        for id in node_ids {
            params.push(id);
        }
        let rows = stmt.query_map(params.as_slice(), |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Functions with no inbound `CALLS` edge, excluding `main` as the
    /// obvious entry-point heuristic. Caveat inherited from same-file-only
    /// call resolution (see `ingest.rs`): a function only ever called from
    /// a *different* file will show up here as a false positive, since that
    /// call site never produced an edge to begin with. Treat results as
    /// "worth a second look", not a guarantee.
    pub fn dead_functions(&self) -> Result<Vec<NodeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, qualified_name, file_path, start_line, end_line
             FROM nodes
             WHERE kind = 'Function' AND name != 'main'
             AND id NOT IN (SELECT dst_id FROM edges WHERE kind = 'CALLS')
             ORDER BY file_path, start_line",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(NodeRecord {
                id: row.get(0)?,
                kind: NodeKind::from_str(&row.get::<_, String>(1)?),
                name: row.get(2)?,
                qualified_name: row.get(3)?,
                file_path: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// File extension counts (a rough proxy for "language breakdown") -
    /// derived from `File` nodes rather than parsed language metadata, since
    /// we don't store the latter separately from what tree-sitter grammar
    /// matched the extension in the first place.
    pub fn file_extension_counts(&self) -> Result<Vec<(String, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT file_path FROM nodes WHERE kind = 'File'")?;
        let paths: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;

        let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for path in paths {
            let ext = Path::new(&path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("(no extension)")
                .to_string();
            *counts.entry(ext).or_insert(0) += 1;
        }

        let mut result: Vec<_> = counts.into_iter().collect();
        result.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(result)
    }

    /// `detect_changes`-equivalent: definitions whose line range overlaps a
    /// given span in a file (e.g. a git diff hunk).
    pub fn nodes_overlapping(
        &self,
        file_path: &str,
        start_line: u32,
        end_line: u32,
    ) -> Result<Vec<NodeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, qualified_name, file_path, start_line, end_line
             FROM nodes
             WHERE file_path = ?1 AND kind != 'File' AND start_line <= ?3 AND end_line >= ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![file_path, start_line, end_line], |row| {
            Ok(NodeRecord {
                id: row.get(0)?,
                kind: NodeKind::from_str(&row.get::<_, String>(1)?),
                name: row.get(2)?,
                qualified_name: row.get(3)?,
                file_path: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// `get_architecture`-equivalent building block: files ranked by how
    /// many definitions they contain.
    pub fn busiest_files(&self, limit: u32) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT file_path, COUNT(*) as cnt FROM nodes
             WHERE kind != 'File' GROUP BY file_path ORDER BY cnt DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// `trace_call_path`-equivalent: BFS over CALLS edges up to `max_depth`.
    pub fn trace_calls(
        &self,
        function_name: &str,
        direction: Direction,
        max_depth: u32,
    ) -> Result<Vec<NodeRecord>> {
        let start_ids: Vec<i64> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM nodes WHERE name = ?1 AND kind = 'Function'")?;
            let rows = stmt.query_map([function_name], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut visited: std::collections::HashSet<i64> = start_ids.iter().copied().collect();
        let mut frontier = start_ids;
        let mut result_ids = Vec::new();

        for _ in 0..max_depth {
            if frontier.is_empty() {
                break;
            }
            let mut next_frontier = Vec::new();
            for &id in &frontier {
                let query = match direction {
                    Direction::Outbound => {
                        "SELECT dst_id FROM edges WHERE src_id = ?1 AND kind = 'CALLS'"
                    }
                    Direction::Inbound => {
                        "SELECT src_id FROM edges WHERE dst_id = ?1 AND kind = 'CALLS'"
                    }
                };
                let mut stmt = self.conn.prepare(query)?;
                let rows = stmt.query_map([id], |row| row.get::<_, i64>(0))?;
                for neighbor in rows {
                    let neighbor = neighbor?;
                    if visited.insert(neighbor) {
                        next_frontier.push(neighbor);
                        result_ids.push(neighbor);
                    }
                }
            }
            frontier = next_frontier;
        }

        if result_ids.is_empty() {
            return Ok(Vec::new());
        }

        let placeholders = result_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, kind, name, qualified_name, file_path, start_line, end_line
             FROM nodes WHERE id IN ({placeholders})"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = result_ids
            .iter()
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok(NodeRecord {
                id: row.get(0)?,
                kind: NodeKind::from_str(&row.get::<_, String>(1)?),
                name: row.get(2)?,
                qualified_name: row.get(3)?,
                file_path: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}
