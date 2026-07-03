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
}

impl NodeKind {
    fn as_str(&self) -> &'static str {
        match self {
            NodeKind::File => "File",
            NodeKind::Function => "Function",
            NodeKind::Type => "Type",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "File" => NodeKind::File,
            "Function" => NodeKind::Function,
            _ => NodeKind::Type,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Defines,
    Calls,
}

impl EdgeKind {
    fn as_str(&self) -> &'static str {
        match self {
            EdgeKind::Defines => "DEFINES",
            EdgeKind::Calls => "CALLS",
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
            ",
        )?;
        Ok(Self { conn })
    }

    /// Phase 1 reindexing is a full rebuild, not an incremental diff -
    /// incremental edge correctness is flagged as an open risk in the
    /// proposal and deferred past this vertical slice.
    pub fn clear(&self) -> Result<()> {
        self.conn.execute("DELETE FROM edges", [])?;
        self.conn.execute("DELETE FROM nodes", [])?;
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
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// All `CALLS` edges as (caller_id, callee_id) pairs - same rationale as
    /// `all_nodes`.
    pub fn all_call_edges(&self) -> Result<Vec<(i64, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT src_id, dst_id FROM edges WHERE kind = 'CALLS'")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
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
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
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
        let rows = stmt.query_map(
            rusqlite::params![file_path, start_line, end_line],
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
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
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
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
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

        let placeholders = result_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, kind, name, qualified_name, file_path, start_line, end_line
             FROM nodes WHERE id IN ({placeholders})"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> =
            result_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
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
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }
}
