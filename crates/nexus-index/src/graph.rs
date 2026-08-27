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
    /// A call edge from LSP reference resolution (issue #10), not the
    /// static tree-sitter name-matching pass - kept as a distinct kind
    /// rather than folded into `Calls` so static vs. resolved provenance
    /// stays visible and auditable, per the PR review's provenance-first
    /// requirement. Enrichment only ever *adds* these alongside whatever
    /// `Calls` edges the static pass already found; nothing ever removes or
    /// replaces a `Calls` edge based on this. `trace_calls`/`dead_functions`
    /// treat `Calls` and `CallsResolved` as one union when walking the call
    /// graph, so resolution only ever adds coverage, never changes existing
    /// static-only behavior when no LSP server ran.
    CallsResolved,
}

impl EdgeKind {
    fn as_str(&self) -> &'static str {
        match self {
            EdgeKind::Defines => "DEFINES",
            EdgeKind::Calls => "CALLS",
            EdgeKind::Contains => "CONTAINS",
            EdgeKind::CallsResolved => "CALLS_RESOLVED",
        }
    }

    fn from_edges_kind_str(s: &str) -> Self {
        match s {
            "CALLS_RESOLVED" => EdgeKind::CallsResolved,
            _ => EdgeKind::Calls,
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

/// A node reached by `trace_calls`, tagged with which edge kind first
/// reached it during the BFS - see `trace_calls`'s doc comment for why
/// "first reached" (not "all kinds that link to it") is the rule.
#[derive(Debug, Clone)]
pub struct TracedNode {
    pub node: NodeRecord,
    pub edge_kind: EdgeKind,
}

#[derive(Debug, Clone)]
pub struct CodeSearchHit {
    pub file_path: String,
    pub snippet: String,
}

/// Owner-only (0700) on the project's data directory, not just the `.db`
/// file itself - `graph.db` in WAL mode also creates `-wal`/`-shm` sidecar
/// files lazily on first write (after `open()` already returns), which a
/// single `harden_graph_db_file` call on the main file wouldn't reach.
/// Hardening the directory covers those too, plus anything else ever
/// written under it, same reasoning as `config.toml`'s 0600 fix - see
/// issue #32. Best-effort: a failure here must never fail opening the
/// store, since the on-disk directory permission just isn't as load-
/// bearing as actually having a working index.
#[cfg(unix)]
fn harden_project_data_dir(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn harden_project_data_dir(_dir: &Path) {}

/// Owner-only (0600) on `graph.db` itself, as defense-in-depth alongside
/// `harden_project_data_dir` - covers the case where the directory already
/// existed with a looser mode from before this fix (an existing install
/// being upgraded). `graph.db` is the most sensitive file this daemon
/// writes: it holds the full indexed source text (FTS5) for every project
/// ever indexed. Best-effort for the same reason as above.
#[cfg(unix)]
fn harden_graph_db_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn harden_graph_db_file(_path: &Path) {}

impl GraphStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            harden_project_data_dir(parent);
        }
        let conn = Connection::open(path)?;
        harden_graph_db_file(path);
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
            ",
        )?;
        // Migration for existing databases created before the embeddings/
        // semantic-search subsystem was removed (issue #62) - drops the old
        // table/index cleanly rather than just leaving them unused. Safe to
        // run on every open: DROP ... IF EXISTS is a no-op once the schema
        // has already been migrated.
        conn.execute_batch(
            "
            DROP INDEX IF EXISTS idx_embeddings_node;
            DROP TABLE IF EXISTS embeddings;
            ",
        )?;
        Ok(Self { conn })
    }

    /// Installs a cooperative bail-out on this connection: SQLite calls the
    /// handler every `num_ops` VM instructions during query execution, and a
    /// `true` return aborts the in-progress statement with
    /// `SQLITE_INTERRUPT` - rusqlite surfaces that as an ordinary `Err`, not
    /// a panic or a killed thread. Used to bound `run_cypher_query`
    /// (freeform, caller-supplied query shapes) so a pathological one can't
    /// hang the daemon indefinitely; 1000 is a small enough instruction
    /// interval that the elapsed-time check is effectively real-time
    /// without materially slowing normal queries. Call `clear_query_timeout`
    /// once the bounded query finishes so the handler doesn't keep clamping
    /// unrelated queries on the same long-lived connection.
    pub fn set_query_timeout(&self, timeout: std::time::Duration) {
        let start = std::time::Instant::now();
        self.conn
            .progress_handler(1000, Some(move || start.elapsed() > timeout));
    }

    /// Removes a progress handler previously installed by
    /// `set_query_timeout`.
    pub fn clear_query_timeout(&self) {
        self.conn.progress_handler(0, None::<fn() -> bool>);
    }

    /// Phase 1 reindexing is a full rebuild, not an incremental diff -
    /// incremental edge correctness is flagged as an open risk in the
    /// proposal and deferred past this vertical slice.
    pub fn clear(&self) -> Result<()> {
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

    /// A full reindex calls this once per Function/Type/File/Section node -
    /// thousands of times on a real project. Two efficiencies over a naive
    /// `execute` + separate `SELECT id`: `prepare_cached` skips re-parsing
    /// the same SQL text on every call (rusqlite keys its statement cache by
    /// the SQL string itself), and `RETURNING id` folds what used to be two
    /// round trips (the insert, then a `SELECT id FROM nodes WHERE
    /// qualified_name = ?1` to fetch what was just written) into one. See
    /// issue #34.
    pub fn insert_node(
        &self,
        kind: NodeKind,
        name: &str,
        qualified_name: &str,
        file_path: &str,
        start_line: u32,
        end_line: u32,
    ) -> Result<i64> {
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO nodes (kind, name, qualified_name, file_path, start_line, end_line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(qualified_name) DO UPDATE SET
                kind = excluded.kind,
                name = excluded.name,
                file_path = excluded.file_path,
                start_line = excluded.start_line,
                end_line = excluded.end_line
             RETURNING id",
        )?;
        let id: i64 = stmt.query_row(
            rusqlite::params![
                kind.as_str(),
                name,
                qualified_name,
                file_path,
                start_line,
                end_line
            ],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    pub fn insert_edge(&self, src_id: i64, dst_id: i64, kind: EdgeKind) -> Result<()> {
        self.conn
            .prepare_cached("INSERT INTO edges (src_id, dst_id, kind) VALUES (?1, ?2, ?3)")?
            .execute(rusqlite::params![src_id, dst_id, kind.as_str()])?;
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

    /// Deliberately substring, not prefix-only - `search_graph`'s whole
    /// point is finding `handleRequest` from a `pattern` of `"request"` or
    /// `"handle"` alike, not just names that *start with* it. The leading
    /// `%` in `like_pattern` below means this can't use `idx_nodes_name`
    /// (a B-tree index only serves prefix matches) - accepted at this
    /// project's real scale (a full table scan over a few thousand `nodes`
    /// rows is sub-millisecond); an FTS5-backed name index would restore
    /// index usage but is a real schema change, not proportionate to fix
    /// preemptively without a demonstrated slowdown at a much larger node
    /// count. See issue #37.
    pub fn search_by_name(&self, pattern: &str, limit: u32) -> Result<Vec<NodeRecord>> {
        let mut stmt = self.conn.prepare_cached(
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
        self.conn
            .prepare_cached("INSERT INTO file_contents_fts (file_path, content) VALUES (?1, ?2)")?
            .execute(rusqlite::params![file_path, content])?;
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

    /// All `CALLS`/`CALLS_RESOLVED` edges as (caller_id, callee_id) pairs -
    /// same rationale as `all_nodes`. Unions both kinds like `trace_calls`/
    /// `dead_functions` do - see `EdgeKind::CallsResolved`.
    pub fn all_call_edges(&self) -> Result<Vec<(i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT src_id, dst_id FROM edges WHERE kind IN ('CALLS', 'CALLS_RESOLVED')",
        )?;
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
    /// "worth a second look", not a guarantee. Also checks `CALLS_RESOLVED`
    /// (issue #10) - a function LSP enrichment found a real cross-file
    /// reference for is correctly excluded here even when the static,
    /// name-based pass alone would have missed it and flagged a false
    /// positive; this is the concrete case enrichment is proven against
    /// (see the `lsp` module's dead-code regression test).
    ///
    /// `path_prefix`, when given, restricts results to functions whose
    /// `file_path` is under that subdirectory (or is that exact file).
    /// Matched as `file_path = prefix OR file_path LIKE prefix || '/%'` -
    /// not a naive `LIKE prefix || '%'` - so a prefix like `pkg/events`
    /// only matches `pkg/events` itself and paths under `pkg/events/`, not
    /// a sibling directory that merely shares the string prefix (e.g.
    /// `pkg/events-old/foo.rs`). `file_path` is stored relative to the repo
    /// root with `/` separators (see `ingest.rs`), so the prefix is
    /// normalized the same way: backslashes swapped to `/` and any
    /// trailing slash trimmed, so `dir/`, `dir\`, and `dir` all behave
    /// identically regardless of caller platform.
    pub fn dead_functions(&self, path_prefix: Option<&str>) -> Result<Vec<NodeRecord>> {
        let trimmed_prefix = path_prefix
            .map(|p| p.replace('\\', "/"))
            .map(|p| p.trim_end_matches('/').to_string())
            .filter(|p| !p.is_empty());

        // `has_prefix = 0` short-circuits the OR via `?1 = 1`, so an absent
        // scope always falls through to the unfiltered branch regardless of
        // what placeholder values `exact`/`like_pattern` hold - same query
        // shape and row-mapping closure serve both the scoped and unscoped
        // cases instead of duplicating them.
        let has_prefix = trimmed_prefix.is_some();
        let (exact, like_pattern) = match &trimmed_prefix {
            Some(prefix) => (
                prefix.clone(),
                format!("{}/%", prefix.replace('%', "\\%").replace('_', "\\_")),
            ),
            None => (String::new(), String::new()),
        };

        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, qualified_name, file_path, start_line, end_line
             FROM nodes
             WHERE kind = 'Function' AND name != 'main'
             AND id NOT IN (SELECT dst_id FROM edges WHERE kind IN ('CALLS', 'CALLS_RESOLVED'))
             AND (?1 = 0 OR file_path = ?2 OR file_path LIKE ?3 ESCAPE '\\')
             ORDER BY file_path, start_line",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![has_prefix as i32, exact, like_pattern],
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
        )?;
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
    ///
    /// Each returned node is tagged with the `EdgeKind` of the edge that
    /// *first* reached it during the BFS (issue #59). A node can in
    /// principle be reachable via both a `Calls` (tree-sitter, name-based)
    /// edge and a `CallsResolved` (LSP-verified) edge from different
    /// callers/layers - rather than reporting every kind that ever links to
    /// it (which would turn one node into a set of provenance tags and
    /// complicate every consumer), this reports the kind of whichever edge
    /// the standard BFS visited it through first. Since a node is only ever
    /// enqueued once (the existing `visited` dedup), "first reached" and
    /// "the edge that produced this BFS result" are the same edge - so this
    /// adds provenance without changing which nodes come back or in what
    /// count, only how each one is labeled.
    pub fn trace_calls(
        &self,
        function_name: &str,
        direction: Direction,
        max_depth: u32,
    ) -> Result<Vec<TracedNode>> {
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
        let mut result_kinds: std::collections::HashMap<i64, EdgeKind> =
            std::collections::HashMap::new();

        // One batched IN (...) query per BFS level, not one query per
        // frontier node - matches the pattern `subgraph_edges` already uses.
        // A wide fan-in/fan-out function (a common "hub") used to turn each
        // level into dozens-to-hundreds of individual round trips. See
        // issue #33.
        //
        // Two things a PR reviewer flagged worth knowing, neither a
        // correctness bug: the result *set* per level is identical to the
        // old per-node loop (verified by 3 new tests, including a diamond
        // shape asserting no duplication), but rows now come back in
        // edges-table scan order rather than frontier-node order - if a
        // level is large enough to hit `trace_call_path`'s `limit`
        // truncation, which specific nodes get cut could differ from
        // before (harmless; `total_nodes` stays exact either way). And the
        // IN-list is unbounded per level - bundled SQLite caps bound
        // variables around 32766, so a single BFS level wider than that
        // would fail outright rather than degrade; `subgraph_edges` already
        // carries this same unbounded-IN-list shape, and real call-graph
        // fan-out is nowhere near that width today.
        for _ in 0..max_depth {
            if frontier.is_empty() {
                break;
            }
            let placeholders = frontier.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let column = match direction {
                Direction::Outbound => "src_id",
                Direction::Inbound => "dst_id",
            };
            let select = match direction {
                Direction::Outbound => "dst_id",
                Direction::Inbound => "src_id",
            };
            // Unions CALLS_RESOLVED alongside the static CALLS edges - see
            // EdgeKind::CallsResolved. Issue #10. The edge's own `kind` is
            // also selected now (issue #59) so each newly-visited neighbor
            // can be tagged with the provenance of the edge that found it.
            let sql = format!(
                "SELECT {select}, kind FROM edges WHERE kind IN ('CALLS', 'CALLS_RESOLVED') AND {column} IN ({placeholders})"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let params: Vec<&dyn rusqlite::ToSql> = frontier
                .iter()
                .map(|id| id as &dyn rusqlite::ToSql)
                .collect();
            let rows = stmt.query_map(params.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?;

            let mut next_frontier = Vec::new();
            for row in rows {
                let (neighbor, kind_str) = row?;
                if visited.insert(neighbor) {
                    next_frontier.push(neighbor);
                    result_ids.push(neighbor);
                    result_kinds.insert(neighbor, EdgeKind::from_edges_kind_str(&kind_str));
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
            let id: i64 = row.get(0)?;
            Ok((
                id,
                NodeRecord {
                    id,
                    kind: NodeKind::from_str(&row.get::<_, String>(1)?),
                    name: row.get(2)?,
                    qualified_name: row.get(3)?,
                    file_path: row.get(4)?,
                    start_line: row.get(5)?,
                    end_line: row.get(6)?,
                },
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(anyhow::Error::from)
            .map(|nodes: Vec<(i64, NodeRecord)>| {
                nodes
                    .into_iter()
                    .map(|(id, node)| TracedNode {
                        node,
                        // Defaults to Calls if somehow missing (shouldn't
                        // happen: every id in result_ids came from
                        // result_kinds in the same loop iteration) - a safe,
                        // conservative fallback rather than a panic.
                        edge_kind: result_kinds.get(&id).copied().unwrap_or(EdgeKind::Calls),
                    })
                    .collect()
            })
    }
}

/// Regression tests for issue #77: `dead_functions` needs a directory-scope
/// filter so a monorepo's vendored/generated code doesn't drown out the real
/// package's dead-code candidates, and the prefix match must be a real path
/// prefix (not a naive string prefix that would false-match a sibling
/// directory sharing the same leading characters).
#[cfg(test)]
mod dead_functions_path_scoping_tests {
    use super::*;

    fn temp_store(name: &str) -> GraphStore {
        let dir = std::env::temp_dir().join(format!(
            "nexus_dead_functions_scoping_test_{name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        GraphStore::open(&dir.join("graph.db")).unwrap()
    }

    /// Builds a fixture mirroring the shape that surfaced #77: one real
    /// package (`pkg/events`) and one vendored subdirectory
    /// (`pkg/events-vendor` - deliberately a *sibling* whose name shares the
    /// `pkg/events` string prefix), each with one dead function.
    fn scoping_fixture(name: &str) -> GraphStore {
        let store = temp_store(name);
        store
            .insert_node(
                NodeKind::Function,
                "real_dead",
                "pkg::events::real_dead",
                "pkg/events/lib.rs",
                1,
                3,
            )
            .unwrap();
        store
            .insert_node(
                NodeKind::Function,
                "vendor_dead",
                "pkg::events_vendor::vendor_dead",
                "pkg/events-vendor/lib.js",
                1,
                3,
            )
            .unwrap();
        // A caller/callee pair under the real package, so the fixture also
        // exercises that a call edge still suppresses the *callee* as a
        // candidate within the scoped subdirectory (the caller itself has
        // no inbound edge, so it's correctly still flagged dead).
        let caller = store
            .insert_node(
                NodeKind::Function,
                "caller",
                "pkg::events::caller",
                "pkg/events/lib.rs",
                5,
                8,
            )
            .unwrap();
        let callee = store
            .insert_node(
                NodeKind::Function,
                "callee",
                "pkg::events::callee",
                "pkg/events/lib.rs",
                10,
                12,
            )
            .unwrap();
        store.insert_edge(caller, callee, EdgeKind::Calls).unwrap();
        store
    }

    #[test]
    fn unscoped_returns_dead_functions_from_every_directory() {
        let store = scoping_fixture("unscoped");
        let dead = store.dead_functions(None).unwrap();
        let names: Vec<_> = dead.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"real_dead"));
        assert!(names.contains(&"vendor_dead"));
        assert!(names.contains(&"caller"), "nothing calls caller itself");
        assert!(
            !names.contains(&"callee"),
            "callee has an inbound edge from caller"
        );
    }

    #[test]
    fn scoped_to_the_real_package_excludes_the_vendored_sibling() {
        let store = scoping_fixture("scoped_real");
        let dead = store.dead_functions(Some("pkg/events")).unwrap();
        let names: std::collections::HashSet<&str> = dead.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            ["real_dead", "caller"].into_iter().collect(),
            "a naive string-prefix match would also pull in pkg/events-vendor"
        );
    }

    #[test]
    fn scoped_to_the_vendored_sibling_excludes_the_real_package() {
        let store = scoping_fixture("scoped_vendor");
        let dead = store.dead_functions(Some("pkg/events-vendor")).unwrap();
        let names: Vec<_> = dead.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["vendor_dead"]);
    }

    #[test]
    fn trailing_slash_and_backslashes_are_normalized() {
        let store = scoping_fixture("trailing_norm");
        let with_slash = store.dead_functions(Some("pkg/events/")).unwrap();
        let with_backslash = store.dead_functions(Some("pkg\\events")).unwrap();
        let expected: std::collections::HashSet<&str> =
            ["real_dead", "caller"].into_iter().collect();
        assert_eq!(
            with_slash
                .iter()
                .map(|n| n.name.as_str())
                .collect::<std::collections::HashSet<_>>(),
            expected
        );
        assert_eq!(
            with_backslash
                .iter()
                .map(|n| n.name.as_str())
                .collect::<std::collections::HashSet<_>>(),
            expected
        );
    }

    #[test]
    fn an_exact_file_path_prefix_matches_only_that_file() {
        let store = scoping_fixture("exact_file");
        let dead = store.dead_functions(Some("pkg/events/lib.rs")).unwrap();
        let names: std::collections::HashSet<&str> = dead.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["real_dead", "caller"].into_iter().collect());
    }

    #[test]
    fn a_prefix_containing_like_wildcards_is_treated_literally() {
        let store = temp_store("wildcards");
        store
            .insert_node(
                NodeKind::Function,
                "underscore_dead",
                "pkg::a_b::underscore_dead",
                "pkg/a_b/lib.rs",
                1,
                3,
            )
            .unwrap();
        store
            .insert_node(
                NodeKind::Function,
                "axb_dead",
                "pkg::axb::axb_dead",
                "pkg/axb/lib.rs",
                1,
                3,
            )
            .unwrap();
        // "pkg/a_b" as a LIKE pattern would (without escaping) also match
        // "pkg/axb" via SQL's single-character `_` wildcard.
        let dead = store.dead_functions(Some("pkg/a_b")).unwrap();
        let names: Vec<_> = dead.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["underscore_dead"]);
    }
}

/// Regression tests for issue #58: a pathological query must be bounded by
/// wall-clock time, not run to completion (or hang forever).
#[cfg(test)]
mod query_timeout_tests {
    use super::*;
    use std::time::Duration;

    fn temp_store(name: &str) -> GraphStore {
        let dir = std::env::temp_dir().join(format!(
            "nexus_query_timeout_test_{name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        GraphStore::open(&dir.join("graph.db")).unwrap()
    }

    #[test]
    fn a_slow_query_is_interrupted_once_the_timeout_elapses() {
        let store = temp_store("slow");
        store.set_query_timeout(Duration::from_millis(50));

        // An effectively-unbounded recursive CTE - stands in for a
        // pathological caller-supplied query. Without the progress handler
        // this would run for a very long time; with it, SQLite aborts the
        // statement shortly after the timeout elapses.
        let start = std::time::Instant::now();
        let result: rusqlite::Result<i64> = store.conn.query_row(
            "WITH RECURSIVE cnt(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM cnt LIMIT 2000000000) \
             SELECT count(*) FROM cnt",
            [],
            |row| row.get(0),
        );
        let elapsed = start.elapsed();

        store.clear_query_timeout();
        assert!(
            result.is_err(),
            "pathological query should be interrupted, not complete"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "interrupt took too long: {elapsed:?}"
        );
    }

    #[test]
    fn an_ordinary_query_is_unaffected_by_a_generous_timeout() {
        let store = temp_store("fast");
        store.set_query_timeout(Duration::from_secs(30));

        let result: rusqlite::Result<i64> = store.conn.query_row("SELECT 1", [], |row| row.get(0));

        store.clear_query_timeout();
        assert_eq!(result.unwrap(), 1);
    }
}

/// Regression test for issue #62: opening a database that still has the
/// pre-removal `embeddings` table must migrate it away (DROP) rather than
/// erroring or leaving it in place unused.
#[cfg(test)]
mod embeddings_migration_tests {
    use super::*;

    #[test]
    fn open_drops_a_pre_existing_embeddings_table() {
        let dir = std::env::temp_dir().join(format!(
            "nexus_embeddings_migration_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("graph.db");

        {
            let store = GraphStore::open(&db_path).unwrap();
            // Simulate a pre-removal database by recreating the old table
            // directly, bypassing `open`'s own (now embeddings-free) schema.
            store
                .conn
                .execute_batch(
                    "CREATE TABLE embeddings (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        node_id INTEGER NOT NULL,
                        model TEXT NOT NULL,
                        dim INTEGER NOT NULL,
                        chunk_text TEXT NOT NULL,
                        embedding BLOB NOT NULL
                    );
                    CREATE INDEX idx_embeddings_node ON embeddings(node_id);",
                )
                .unwrap();
        }

        // Reopening must migrate the old table away.
        let store = GraphStore::open(&db_path).unwrap();
        let exists: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='embeddings'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 0, "embeddings table must be dropped on open");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Regression tests for issue #32: `GraphStore::open` used to create
/// `graph.db` (and its containing directory) at the process umask's mode
/// instead of owner-only.
#[cfg(test)]
#[cfg(unix)]
mod permission_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn open_creates_the_db_file_and_project_dir_owner_only() {
        let dir = std::env::temp_dir().join(format!(
            "nexus_graphstore_permissions_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("graph.db");

        let _store = GraphStore::open(&db_path).unwrap();

        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "project data dir must be owner-only");

        let file_mode = std::fs::metadata(&db_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "graph.db must be owner-only");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_normalizes_a_pre_existing_looser_directory_mode() {
        // Simulates an existing install upgrading into this fix - the
        // directory already exists with a looser mode from before it, and
        // open() must still tighten it, not just skip create_dir_all's
        // no-op path and leave the old mode in place.
        let dir = std::env::temp_dir().join(format!(
            "nexus_graphstore_permissions_upgrade_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let db_path = dir.join("graph.db");
        let _store = GraphStore::open(&db_path).unwrap();

        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Regression tests for issue #33: `trace_calls`'s BFS used to issue one
/// query per frontier node instead of one batched query per level - these
/// specifically exercise levels with more than one node in the frontier at
/// once, since a single-node-per-level graph wouldn't distinguish the old
/// per-node loop from the new batched query.
#[cfg(test)]
mod trace_calls_tests {
    use super::*;

    fn temp_store(name: &str) -> GraphStore {
        // A dedicated per-test subdirectory, not a file directly under the
        // shared system temp root - GraphStore::open now hardens its
        // parent directory's mode (see issue #32), and that must never be
        // the actual `/tmp`.
        let dir = std::env::temp_dir().join(format!(
            "nexus_trace_calls_test_{name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        GraphStore::open(&dir.join("graph.db")).unwrap()
    }

    fn func(store: &GraphStore, name: &str) -> i64 {
        store
            .insert_node(
                NodeKind::Function,
                name,
                &format!("a.rs::{name}#1"),
                "a.rs",
                1,
                2,
            )
            .unwrap()
    }

    #[test]
    fn outbound_batches_a_multi_node_frontier_and_finds_every_callee() {
        let store = temp_store("outbound_fanout");
        // root -> {a, b, c} (one level, three nodes in the same frontier),
        // then a -> leaf, b -> leaf (same leaf reached two ways - must not
        // be duplicated), c has no further calls.
        let root = func(&store, "root");
        let a = func(&store, "a");
        let b = func(&store, "b");
        let c = func(&store, "c");
        let leaf = func(&store, "leaf");
        store.insert_edge(root, a, EdgeKind::Calls).unwrap();
        store.insert_edge(root, b, EdgeKind::Calls).unwrap();
        store.insert_edge(root, c, EdgeKind::Calls).unwrap();
        store.insert_edge(a, leaf, EdgeKind::Calls).unwrap();
        store.insert_edge(b, leaf, EdgeKind::Calls).unwrap();

        let result = store.trace_calls("root", Direction::Outbound, 3).unwrap();
        let names: std::collections::HashSet<&str> =
            result.iter().map(|n| n.node.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c", "leaf"].into_iter().collect());
        assert_eq!(
            result.len(),
            4,
            "leaf reached via two paths must not be duplicated"
        );
    }

    #[test]
    fn inbound_batches_a_multi_node_frontier_and_finds_every_caller() {
        let store = temp_store("inbound_fanin");
        // {a, b, c} -> target (three distinct callers of the same function,
        // one BFS level with three nodes in the frontier).
        let target = func(&store, "target");
        let a = func(&store, "a");
        let b = func(&store, "b");
        let c = func(&store, "c");
        store.insert_edge(a, target, EdgeKind::Calls).unwrap();
        store.insert_edge(b, target, EdgeKind::Calls).unwrap();
        store.insert_edge(c, target, EdgeKind::Calls).unwrap();

        let result = store.trace_calls("target", Direction::Inbound, 3).unwrap();
        let names: std::collections::HashSet<&str> =
            result.iter().map(|n| n.node.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"].into_iter().collect());
    }

    #[test]
    fn max_depth_stops_the_batched_walk_at_the_right_level() {
        let store = temp_store("depth_limit");
        let a = func(&store, "a");
        let b = func(&store, "b");
        let c = func(&store, "c");
        store.insert_edge(a, b, EdgeKind::Calls).unwrap();
        store.insert_edge(b, c, EdgeKind::Calls).unwrap();

        let one_level = store.trace_calls("a", Direction::Outbound, 1).unwrap();
        assert_eq!(one_level.len(), 1);
        assert_eq!(one_level[0].node.name, "b");

        let two_levels = store.trace_calls("a", Direction::Outbound, 2).unwrap();
        let names: std::collections::HashSet<&str> =
            two_levels.iter().map(|n| n.node.name.as_str()).collect();
        assert_eq!(names, ["b", "c"].into_iter().collect());
    }

    /// Regression tests for issue #59: a `TracedNode` must carry the kind of
    /// the edge that reached it, since a plain `CALLS` (tree-sitter,
    /// name-based) hop is far less trustworthy than a `CALLS_RESOLVED`
    /// (LSP-verified, issue #10) one.
    #[test]
    fn a_node_reached_only_via_calls_is_tagged_heuristic() {
        let store = temp_store("provenance_heuristic");
        let a = func(&store, "a");
        let b = func(&store, "b");
        store.insert_edge(a, b, EdgeKind::Calls).unwrap();

        let result = store.trace_calls("a", Direction::Outbound, 1).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].node.name, "b");
        assert_eq!(result[0].edge_kind, EdgeKind::Calls);
    }

    #[test]
    fn a_node_reached_via_calls_resolved_is_tagged_lsp_verified() {
        let store = temp_store("provenance_resolved");
        let a = func(&store, "a");
        let b = func(&store, "b");
        // No plain `Calls` edge here at all - this call was only ever found
        // by LSP enrichment (e.g. a cross-file reference the static,
        // same-file-only pass missed), matching how `enrich_with_lsp` only
        // ever *adds* `CallsResolved` edges alongside (or instead of, when
        // the static pass found nothing) `Calls` edges.
        store.insert_edge(a, b, EdgeKind::CallsResolved).unwrap();

        let result = store.trace_calls("a", Direction::Outbound, 1).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].node.name, "b");
        assert_eq!(result[0].edge_kind, EdgeKind::CallsResolved);
    }
}
