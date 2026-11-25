//! SQLite is the canonical store for curated facts. The OKF bundle in
//! `repo/.nexus/memory/` is a portable projection of it, never the source of
//! truth (proposal locked decision 3, ADR 0020).

use crate::fact::{Anchor, Fact, FactKind, FactStatus, Source};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;
use std::time::Duration;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            nexus_core::paths::harden_dir_owner_only(parent);
        }
        create_owner_only_file(path)?;

        let conn = Connection::open(path)
            .with_context(|| format!("opening curated memory store {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(30))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS facts (
                 id             TEXT PRIMARY KEY,
                 repo_path      TEXT NOT NULL,
                 kind           TEXT NOT NULL,
                 title          TEXT NOT NULL,
                 body           TEXT NOT NULL,
                 status         TEXT NOT NULL,
                 superseded_by  TEXT,
                 generated_by   TEXT NOT NULL,
                 generated_at   INTEGER NOT NULL,
                 verified_by    TEXT,
                 verified_at    INTEGER,
                 stale_after    INTEGER,
                 anchor_stale   INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS fact_anchors (
                 fact_id  TEXT NOT NULL,
                 path     TEXT NOT NULL,
                 symbol   TEXT,
                 line     INTEGER,
                 hash     TEXT,
                 FOREIGN KEY(fact_id) REFERENCES facts(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_anchors_fact ON fact_anchors(fact_id);
             CREATE INDEX IF NOT EXISTS idx_anchors_path ON fact_anchors(path);
             CREATE TABLE IF NOT EXISTS fact_sources (
                 fact_id   TEXT NOT NULL,
                 id        TEXT NOT NULL,
                 resource  TEXT NOT NULL,
                 title     TEXT,
                 FOREIGN KEY(fact_id) REFERENCES facts(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS fact_reviews (
                 id       INTEGER PRIMARY KEY AUTOINCREMENT,
                 fact_id  TEXT NOT NULL,
                 actor    TEXT NOT NULL,
                 verdict  TEXT NOT NULL,
                 at       INTEGER NOT NULL,
                 FOREIGN KEY(fact_id) REFERENCES facts(id) ON DELETE CASCADE
             );",
        )?;
        Ok(Self { conn })
    }

    /// Upsert a fact and replace its anchor/source rows atomically.
    ///
    /// Redaction happens here, at the canonical-store boundary, so every
    /// ingestion path (tool, CLI or OKF import) is covered — not just the one
    /// a caller happened to remember to guard. It is deliberately lossy: a
    /// fact with a redacted secret is still useful as a fact, while a leaked
    /// one is not recoverable once injected into a harness.
    pub fn put(&mut self, fact: &Fact) -> Result<()> {
        let mut fact = fact.clone();
        fact.title = myelin_index::redact::redact(&fact.title);
        fact.body = myelin_index::redact::redact(&fact.body);
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO facts (id, repo_path, kind, title, body, status,
                                superseded_by, generated_by, generated_at,
                                verified_by, verified_at, stale_after, anchor_stale)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(id) DO UPDATE SET
                 repo_path=excluded.repo_path, kind=excluded.kind,
                 title=excluded.title, body=excluded.body,
                 status=excluded.status, superseded_by=excluded.superseded_by,
                 generated_by=excluded.generated_by, generated_at=excluded.generated_at,
                 verified_by=excluded.verified_by, verified_at=excluded.verified_at,
                 stale_after=excluded.stale_after, anchor_stale=excluded.anchor_stale",
            params![
                fact.id,
                fact.repo_path,
                kind_code(fact.kind),
                fact.title,
                fact.body,
                fact.status.as_okf(),
                fact.superseded_by,
                fact.generated_by,
                fact.generated_at,
                fact.verified_by,
                fact.verified_at,
                fact.stale_after,
                i64::from(fact.anchor_stale),
            ],
        )?;
        tx.execute(
            "DELETE FROM fact_anchors WHERE fact_id=?1",
            params![fact.id],
        )?;
        for anchor in &fact.anchors {
            tx.execute(
                "INSERT INTO fact_anchors (fact_id, path, symbol, line, hash)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    fact.id,
                    anchor.path,
                    anchor.symbol,
                    anchor.line.map(i64::from),
                    anchor.hash,
                ],
            )?;
        }
        tx.execute(
            "DELETE FROM fact_sources WHERE fact_id=?1",
            params![fact.id],
        )?;
        for source in &fact.sources {
            tx.execute(
                "INSERT INTO fact_sources (fact_id, id, resource, title)
                 VALUES (?1, ?2, ?3, ?4)",
                params![fact.id, source.id, source.resource, source.title],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list(&self, repo_path: &str) -> Result<Vec<Fact>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_path, kind, title, body, status, superseded_by,
                    generated_by, generated_at, verified_by, verified_at,
                    stale_after, anchor_stale
             FROM facts WHERE repo_path=?1 ORDER BY generated_at DESC, id",
        )?;
        let rows = stmt.query_map(params![repo_path], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<i64>>(11)?,
                row.get::<_, i64>(12)?,
            ))
        })?;

        let mut facts = Vec::new();
        for row in rows {
            let (
                id,
                repo,
                kind,
                title,
                body,
                status,
                superseded_by,
                generated_by,
                generated_at,
                verified_by,
                verified_at,
                stale_after,
                anchor_stale,
            ) = row?;
            let anchors = self.anchors(&id)?;
            let sources = self.sources(&id)?;
            facts.push(Fact {
                id,
                repo_path: repo,
                kind: kind_code_to_kind(&kind),
                title,
                body,
                status: FactStatus::from_okf(&status),
                superseded_by,
                anchors,
                sources,
                generated_by,
                generated_at,
                verified_by,
                verified_at,
                stale_after,
                anchor_stale: anchor_stale != 0,
            });
        }
        Ok(facts)
    }

    pub fn get(&self, id: &str) -> Result<Option<Fact>> {
        let repo: Option<String> = self
            .conn
            .query_row(
                "SELECT repo_path FROM facts WHERE id=?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(repo) = repo else { return Ok(None) };
        Ok(self.list(&repo)?.into_iter().find(|f| f.id == id))
    }

    fn anchors(&self, fact_id: &str) -> Result<Vec<Anchor>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, symbol, line, hash FROM fact_anchors
             WHERE fact_id=?1 ORDER BY path, symbol",
        )?;
        let rows = stmt.query_map(params![fact_id], |row| {
            Ok(Anchor {
                path: row.get(0)?,
                symbol: row.get(1)?,
                line: row.get::<_, Option<i64>>(2)?.map(|n| n as u32),
                hash: row.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn sources(&self, fact_id: &str) -> Result<Vec<Source>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, resource, title FROM fact_sources
             WHERE fact_id=?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![fact_id], |row| {
            Ok(Source {
                id: row.get(0)?,
                resource: row.get(1)?,
                title: row.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Watcher invalidation: every fact anchored to `changed_path` goes stale.
    /// Matching is by canonical path components, not a string prefix, so
    /// `pkg/events-vendor` can't be hit by an anchor on `pkg/events`.
    pub fn mark_anchors_stale(&self, repo_path: &str, changed_path: &str) -> Result<usize> {
        let repo = std::path::Path::new(repo_path);
        let changed = std::path::Path::new(changed_path);
        let changed_abs = if changed.is_absolute() {
            changed.to_path_buf()
        } else {
            repo.join(changed)
        };
        let changed_canon = changed_abs
            .canonicalize()
            .unwrap_or_else(|_| changed_abs.clone());

        let mut stmt = self
            .conn
            .prepare("SELECT fact_id, path FROM fact_anchors")?;
        let candidates: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);

        let mut updated = 0;
        for (fact_id, anchor_path) in candidates {
            let anchor_abs = if std::path::Path::new(&anchor_path).is_absolute() {
                std::path::PathBuf::from(&anchor_path)
            } else {
                repo.join(&anchor_path)
            };
            let anchor_canon = anchor_abs.canonicalize().unwrap_or(anchor_abs);
            let related = anchor_canon == changed_canon
                || changed_canon.starts_with(&anchor_canon)
                || anchor_canon.starts_with(&changed_canon);
            if related {
                updated += self.conn.execute(
                    "UPDATE facts SET anchor_stale=1 WHERE id=?1",
                    params![fact_id],
                )?;
            }
        }
        Ok(updated)
    }

    /// Record a review. Only an `approve` verdict writes a verification stamp;
    /// a correction appends history without claiming the new text was checked.
    pub fn record_review(
        &mut self,
        fact_id: &str,
        actor: &str,
        verdict: &str,
        at: i64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO fact_reviews (fact_id, actor, verdict, at) VALUES (?1, ?2, ?3, ?4)",
            params![fact_id, actor, verdict, at],
        )?;
        match verdict {
            "approve" => {
                // Promotion is atomic: a current human sign-off and stable
                // status must land together, or the admission gate would
                // keep rejecting an approved fact forever.
                tx.execute(
                    "UPDATE facts SET verified_by=?2, verified_at=?3, status='stable' WHERE id=?1",
                    params![fact_id, actor, at],
                )?;
            }
            // A rejection invalidates any earlier sign-off; leaving it in
            // place would let a rejected fact keep its injection privilege.
            "reject" => {
                tx.execute(
                    "UPDATE facts SET verified_by=NULL, verified_at=NULL, status='draft' WHERE id=?1",
                    params![fact_id],
                )?;
            }
            _ => {}
        }
        tx.commit()?;
        Ok(())
    }
}

fn kind_code(kind: FactKind) -> &'static str {
    match kind {
        FactKind::Decision => "decision",
        FactKind::Incident => "incident",
        FactKind::Lesson => "lesson",
        FactKind::DeadEnd => "dead_end",
        FactKind::Owner => "owner",
    }
}

fn kind_code_to_kind(code: &str) -> FactKind {
    match code {
        "incident" => FactKind::Incident,
        "lesson" => FactKind::Lesson,
        "dead_end" => FactKind::DeadEnd,
        "owner" => FactKind::Owner,
        _ => FactKind::Decision,
    }
}

#[cfg(unix)]
fn create_owner_only_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false) // never wipe an existing database on open
        .mode(0o600)
        .open(path)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_owner_only_file(path: &Path) -> Result<()> {
    if !path.exists() {
        std::fs::File::create(path)?;
    }
    Ok(())
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fact::TrustTier;

    fn sample(repo: &str, id: &str, anchor: &str) -> Fact {
        Fact {
            id: id.into(),
            repo_path: repo.into(),
            kind: FactKind::Decision,
            title: "Use presets".into(),
            body: "We cap schema tax.".into(),
            status: FactStatus::Stable,
            superseded_by: None,
            anchors: vec![Anchor {
                path: anchor.into(),
                symbol: None,
                line: None,
                hash: None,
            }],
            sources: vec![],
            generated_by: "agent/test".into(),
            generated_at: 100,
            verified_by: None,
            verified_at: None,
            stale_after: None,
            anchor_stale: false,
        }
    }

    #[test]
    fn facts_round_trip_through_sqlite() {
        let dir = std::env::temp_dir().join(format!("nc-mem-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir.join("memory.db")).unwrap();
        let f = sample("/repo", "f1", "src/tools.rs");
        store.put(&f).unwrap();
        let loaded = store.list("/repo").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], f);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn watcher_invalidation_marks_only_related_facts_stale() {
        let dir = std::env::temp_dir().join(format!("nc-mem-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join("pkg/events")).unwrap();
        std::fs::create_dir_all(repo.join("pkg/events-vendor")).unwrap();
        std::fs::write(repo.join("pkg/events/mod.rs"), "x").unwrap();
        std::fs::write(repo.join("pkg/events-vendor/mod.rs"), "x").unwrap();

        let mut store = Store::open(&dir.join("memory.db")).unwrap();
        store
            .put(&sample(repo.to_str().unwrap(), "a", "pkg/events/mod.rs"))
            .unwrap();
        store
            .put(&sample(
                repo.to_str().unwrap(),
                "b",
                "pkg/events-vendor/mod.rs",
            ))
            .unwrap();

        let changed = store
            .mark_anchors_stale(repo.to_str().unwrap(), "pkg/events/mod.rs")
            .unwrap();
        assert_eq!(changed, 1);
        assert!(store.get("a").unwrap().unwrap().anchor_stale);
        assert!(!store.get("b").unwrap().unwrap().anchor_stale);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn redaction_is_applied_even_when_a_caller_forgets() {
        let dir = std::env::temp_dir().join(format!("nc-mem-redact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir.join("memory.db")).unwrap();
        let mut f = sample("/repo", "f1", "x.rs");
        f.body = "token = abcdefghijklmnop1234".into();
        store.put(&f).unwrap();
        let saved = store.get("f1").unwrap().unwrap();
        assert!(!saved.body.contains("abcdefghijklmnop1234"));
        assert!(saved.body.contains("REDACTED"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn review_approval_is_the_only_thing_that_sets_verification() {
        let dir = std::env::temp_dir().join(format!("nc-mem-review-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir.join("memory.db")).unwrap();
        store.put(&sample("/repo", "f1", "x.rs")).unwrap();

        store
            .record_review("f1", "human:alice", "comment", 110)
            .unwrap();
        let f = store.get("f1").unwrap().unwrap();
        assert_eq!(crate::fact::trust_tier(&f), TrustTier::Unverified);

        store
            .record_review("f1", "human:alice", "approve", 120)
            .unwrap();
        let f = store.get("f1").unwrap().unwrap();
        assert_eq!(crate::fact::trust_tier(&f), TrustTier::HumanReviewed);
        assert_eq!(f.status, crate::fact::FactStatus::Stable);

        store
            .record_review("f1", "human:alice", "reject", 130)
            .unwrap();
        let f = store.get("f1").unwrap().unwrap();
        assert_eq!(crate::fact::trust_tier(&f), TrustTier::Unverified);
        assert_eq!(f.status, crate::fact::FactStatus::Draft);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
