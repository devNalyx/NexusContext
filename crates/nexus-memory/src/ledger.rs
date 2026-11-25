//! The honesty ledger for context economy.
//!
//! Two sources are deliberately kept apart: provider-reported harness
//! tokens/cost (`harness`, real numbers from the agent runtime) and NC's own
//! `bytes/4` approximation (`estimate`, for injected text and tool output).
//! They are never added together into one misleading "tokens used" number.
//!
//! No LLM is involved. The daemon records what the harness reported and what
//! it itself wrote into context; an agent cannot declare its own savings.

use anyhow::{bail, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::Path;

/// Metadata for one recorded injection. Bundled so the ledger method stays a
/// single honest write rather than a long positional argument list.
pub struct InjectionRecord<'a> {
    pub session_id: &'a str,
    pub repo_path: &'a str,
    pub query: &'a str,
    pub fact_ids: &'a [String],
    /// Human-approved procedural signposts included in this exact text.
    pub skill_ids: &'a [String],
    pub injected_bytes: usize,
    pub budget_tokens: u64,
    pub now: i64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UsageTokens {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl UsageTokens {
    pub fn from_json(value: &Value) -> Result<Self> {
        let field = |name: &str| -> Result<u64> {
            let Some(v) = value.get(name) else {
                return Ok(0);
            };
            v.as_u64()
                .ok_or_else(|| anyhow::anyhow!("tokens.{name} must be a non-negative integer"))
        };
        Ok(Self {
            input: field("input")?,
            output: field("output")?,
            reasoning: field("reasoning")?,
            cache_read: value
                .get("cache")
                .and_then(|c| c.get("read"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_write: value
                .get("cache")
                .and_then(|c| c.get("write"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        })
    }

    fn to_json(self) -> Value {
        json!({
            "input": self.input,
            "output": self.output,
            "reasoning": self.reasoning,
            "cache": { "read": self.cache_read, "write": self.cache_write },
        })
    }
}

/// Bounded, local, owner-only SQLite ledger. One file for the whole machine,
/// keyed by harness session id; no account and no sync.
pub struct Ledger {
    conn: Connection,
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            nexus_core::harden_dir_owner_only(parent);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)?;
        }
        #[cfg(not(unix))]
        {
            if !path.exists() {
                std::fs::write(path, b"")?;
            }
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS ledger_events (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 source TEXT NOT NULL,
                 repo_path TEXT,
                 query TEXT,
                 input_tokens INTEGER NOT NULL DEFAULT 0,
                 output_tokens INTEGER NOT NULL DEFAULT 0,
                 reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                 cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                 cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                 cost_usd REAL,
                 injected_tokens INTEGER NOT NULL DEFAULT 0,
                 injected_bytes INTEGER NOT NULL DEFAULT 0,
                 fact_ids TEXT,
                 skill_ids TEXT,
                 tool TEXT,
                 status TEXT,
                 bytes INTEGER NOT NULL DEFAULT 0,
                 created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS ledger_session ON ledger_events(session_id, id);",
        )?;
        // Existing v0.1.23 ledgers have neither field. Add them without
        // rewriting history: old injection rows keep empty skill ids and a
        // zero byte count, which is honest because those bytes were never
        // stored there.
        Self::ensure_column(&conn, "injected_bytes", "INTEGER NOT NULL DEFAULT 0")?;
        Self::ensure_column(&conn, "skill_ids", "TEXT")?;
        Ok(Self { conn })
    }

    fn ensure_column(conn: &Connection, column: &str, declaration: &str) -> Result<()> {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('ledger_events') WHERE name = ?1",
            params![column],
            |row| row.get(0),
        )?;
        if count == 0 {
            conn.execute(
                &format!("ALTER TABLE ledger_events ADD COLUMN {column} {declaration}"),
                [],
            )?;
        }
        Ok(())
    }

    /// Record the exact bytes of the text the daemon rendered for injection.
    /// There is no plugin-side envelope to guess: the adapter injects this
    /// block verbatim, so the bytes/4 estimate is of what the model saw.
    pub fn record_injection(&self, record: InjectionRecord<'_>) -> Result<Value> {
        let injected_tokens = crate::estimate_tokens(record.injected_bytes);
        let InjectionRecord {
            session_id,
            repo_path,
            query,
            fact_ids,
            skill_ids,
            injected_bytes,
            budget_tokens,
            now,
            ..
        } = record;
        if injected_tokens > budget_tokens {
            bail!("injection would exceed the hard {budget_tokens}-token budget");
        }
        self.conn.execute(
            "INSERT INTO ledger_events
             (session_id, kind, source, repo_path, query, injected_tokens, injected_bytes,
              fact_ids, skill_ids, created_at)
             VALUES (?1, 'injection', 'estimate', ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                session_id,
                repo_path,
                query,
                injected_tokens as i64,
                injected_bytes as i64,
                serde_json::to_string(fact_ids)?,
                serde_json::to_string(skill_ids)?,
                now
            ],
        )?;
        Ok(json!({
            "event_id": self.conn.last_insert_rowid(),
            "source": "estimate",
            "token_estimate_note": "bytes/4 estimate of injected text, not a provider token count",
            "injected_tokens": injected_tokens,
            "budget_tokens": budget_tokens,
        }))
    }

    /// Provider-reported usage from the harness event stream. The daemon
    /// records it verbatim; it never invents or upgrades an estimate.
    pub fn record_usage(
        &self,
        session_id: &str,
        tokens: UsageTokens,
        cost_usd: Option<f64>,
        now: i64,
    ) -> Result<u64> {
        if let Some(cost) = cost_usd {
            if !cost.is_finite() || cost < 0.0 {
                bail!("cost_usd must be a finite non-negative number");
            }
        }
        self.conn.execute(
            "INSERT INTO ledger_events
             (session_id, kind, source, input_tokens, output_tokens, reasoning_tokens,
              cache_read_tokens, cache_write_tokens, cost_usd, created_at)
             VALUES (?1, 'usage', 'harness', ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                session_id,
                tokens.input as i64,
                tokens.output as i64,
                tokens.reasoning as i64,
                tokens.cache_read as i64,
                tokens.cache_write as i64,
                cost_usd,
                now
            ],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    /// Observed tool traffic: bytes only, always labelled as an estimate.
    /// Never written into curated memory — no raw transcript or tool output.
    pub fn record_tool(
        &self,
        session_id: &str,
        tool: &str,
        status: &str,
        bytes: u64,
        now: i64,
    ) -> Result<u64> {
        self.conn.execute(
            "INSERT INTO ledger_events
             (session_id, kind, source, tool, status, bytes, created_at)
             VALUES (?1, 'tool', 'estimate', ?2, ?3, ?4, ?5)",
            params![session_id, tool, status, bytes as i64, now],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    /// Most recently seen harness session, for read-only status surfaces that
    /// do not have a session id of their own (e.g. a TUI footer).
    pub fn latest_session(&self) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT session_id FROM ledger_events ORDER BY id DESC LIMIT 1")?;
        let mut rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn summary(&self, session_id: &str) -> Result<Value> {
        let harness = self.conn.query_row(
            "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                    COALESCE(SUM(reasoning_tokens),0), COALESCE(SUM(cache_read_tokens),0),
                    COALESCE(SUM(cache_write_tokens),0), SUM(cost_usd), COUNT(*)
             FROM ledger_events WHERE session_id=?1 AND source='harness'",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<f64>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )?;
        let estimate = self.conn.query_row(
            "SELECT COALESCE(SUM(injected_tokens),0), COALESCE(SUM(bytes),0), COUNT(*)
             FROM ledger_events WHERE session_id=?1 AND source='estimate'",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;

        let mut stmt = self.conn.prepare(
            "SELECT tool, status, bytes FROM ledger_events
             WHERE session_id=?1 AND kind='tool' ORDER BY id DESC LIMIT 200",
        )?;
        let mut tools = std::collections::BTreeMap::new();
        for row in stmt.query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })? {
            let (tool, status, bytes) = row?;
            let entry = tools
                .entry(tool)
                .or_insert_with(|| json!({ "calls": 0, "errors": 0, "bytes": 0 }));
            entry["calls"] = json!(entry["calls"].as_u64().unwrap_or(0) + 1);
            if status == "error" {
                entry["errors"] = json!(entry["errors"].as_u64().unwrap_or(0) + 1);
            }
            entry["bytes"] = json!(entry["bytes"].as_u64().unwrap_or(0) + bytes as u64);
        }
        let mut tools: Vec<Value> = tools
            .into_iter()
            .map(|(name, mut entry)| {
                let bytes = entry["bytes"].as_u64().unwrap_or(0);
                entry["name"] = json!(name);
                entry["estimated_tokens"] = json!(crate::estimate_tokens(bytes as usize));
                entry
            })
            .collect();
        tools.sort_by_key(|t| std::cmp::Reverse(t["bytes"].as_u64().unwrap_or(0)));

        Ok(json!({
            "session_id": session_id,
            "harness": {
                "token_source": "provider-reported",
                "tokens": UsageTokens {
                    input: harness.0 as u64,
                    output: harness.1 as u64,
                    reasoning: harness.2 as u64,
                    cache_read: harness.3 as u64,
                    cache_write: harness.4 as u64,
                }.to_json(),
                "cost_usd": harness.5,
                "event_count": harness.6,
                "note": "Real token/cost numbers reported by the harness. Not comparable to bytes/4 estimates; kept in a separate source.",
            },
            "estimate": {
                "token_source": "bytes/4",
                "injected_tokens": estimate.0,
                "tool_output_bytes": estimate.1,
                "tool_output_estimated_tokens": crate::estimate_tokens(estimate.1 as usize),
                "event_count": estimate.2,
                "note": "NC's own bytes/4 approximation for text it injected and tool output it observed. NEVER provider tokens and never summed with the harness source.",
            },
            "tools": tools,
            "never_mixed": "harness and estimate are separate sources; there is deliberately no combined total",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger(name: &str) -> (Ledger, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("nc-ledger-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (Ledger::open(&dir.join("ledger.db")).unwrap(), dir)
    }

    #[test]
    fn harness_and_estimate_sources_are_never_mixed() {
        let (l, dir) = ledger("sources");
        l.record_usage(
            "s1",
            UsageTokens {
                input: 100,
                output: 20,
                ..Default::default()
            },
            Some(0.0125),
            100,
        )
        .unwrap();
        l.record_injection(InjectionRecord {
            session_id: "s1",
            repo_path: "/repo",
            query: "q",
            fact_ids: &["f1".into()],
            skill_ids: &["s1".into()],
            injected_bytes: 400,
            budget_tokens: 800,
            now: 101,
        })
        .unwrap();
        l.record_tool("s1", "read", "completed", 800, 102).unwrap();

        let summary = l.summary("s1").unwrap();
        assert_eq!(summary["harness"]["token_source"], "provider-reported");
        assert_eq!(summary["harness"]["tokens"]["input"], 100);
        assert_eq!(summary["harness"]["cost_usd"], 0.0125);
        assert_eq!(summary["estimate"]["token_source"], "bytes/4");
        assert_eq!(summary["estimate"]["injected_tokens"], 100);
        assert_eq!(summary["estimate"]["tool_output_bytes"], 800);
        assert!(summary["never_mixed"].is_string());
        // SQLite keeps the database handle open until `l` drops; Windows will
        // not delete an open file, so close it before cleaning up.
        drop(l);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn injection_ledger_keeps_exact_bytes_and_skill_ids_separately() {
        let (l, dir) = ledger("skills");
        l.record_injection(InjectionRecord {
            session_id: "s-skills",
            repo_path: "/repo",
            query: "q",
            fact_ids: &["f1".into()],
            skill_ids: &["s7".into()],
            injected_bytes: 401,
            budget_tokens: 800,
            now: 101,
        })
        .unwrap();

        let row = l
            .conn
            .query_row(
                "SELECT injected_tokens, injected_bytes, fact_ids, skill_ids, bytes
                 FROM ledger_events WHERE session_id = 's-skills' AND kind = 'injection'",
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, 100);
        assert_eq!(row.1, 401);
        assert_eq!(row.2, "[\"f1\"]");
        assert_eq!(row.3, "[\"s7\"]");
        // Injection bytes live in their own column; `bytes` remains tool-only.
        assert_eq!(row.4, 0);
        // SQLite keeps the database handle open until `l` drops; Windows will
        // not delete an open file, so close it before cleaning up.
        drop(l);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn injection_above_the_hard_cap_is_rejected_not_truncated_silently() {
        let (l, dir) = ledger("cap");
        let over = InjectionRecord {
            session_id: "s2",
            repo_path: "/repo",
            query: "",
            fact_ids: &[],
            skill_ids: &[],
            injected_bytes: 4000,
            budget_tokens: 800,
            now: 100,
        };
        assert!(l.record_injection(over).is_err());
        let within = InjectionRecord {
            session_id: "s2",
            repo_path: "/repo",
            query: "",
            fact_ids: &[],
            skill_ids: &[],
            injected_bytes: 3200,
            budget_tokens: 800,
            now: 100,
        };
        assert!(l.record_injection(within).is_ok());
        // SQLite keeps the database handle open until `l` drops; Windows will
        // not delete an open file, so close it before cleaning up.
        drop(l);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_cost_is_rejected() {
        let (l, dir) = ledger("cost");
        assert!(l
            .record_usage("s3", UsageTokens::default(), Some(f64::NAN), 100)
            .is_err());
        assert!(l
            .record_usage("s3", UsageTokens::default(), Some(-1.0), 100)
            .is_err());
        // SQLite keeps the database handle open until `l` drops; Windows will
        // not delete an open file, so close it before cleaning up.
        drop(l);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cache_tokens_are_read_from_the_nested_shape() {
        let tokens = UsageTokens::from_json(&json!({
            "input": 10, "output": 2, "reasoning": 1,
            "cache": { "read": 7, "write": 3 }
        }))
        .unwrap();
        assert_eq!(tokens.input, 10);
        assert_eq!(tokens.output, 2);
        assert_eq!(tokens.reasoning, 1);
        assert_eq!(tokens.cache_read, 7);
        assert_eq!(tokens.cache_write, 3);
    }
}
