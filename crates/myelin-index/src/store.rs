use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;

/// Fallback tuning when a caller doesn't have a `myelin_core::Config` to
/// load one from (e.g. tests). Kept here rather than depending on
/// myelin-core, to keep this crate's dependency graph shallow. Must match
/// `myelin_core::config`'s defaults.
pub const DEFAULT_PROMOTION_REPS: i64 = 3;
pub const DEFAULT_SIMILARITY_THRESHOLD: f64 = 0.4;
/// 30 days.
pub const DEFAULT_STALE_AFTER_SECS: i64 = 30 * 24 * 3600;
pub const DEFAULT_MAX_ACTIVE_SKILLS: i64 = 25;

pub struct StoreConfig {
    pub promotion_reps: i64,
    pub similarity_threshold: f64,
    /// A skill with no activity (never invoked, or not invoked) for this
    /// long is flagged `stale` in `list_skills` — informational for
    /// humans/agents (never blocks anything you do); the auto-eviction cap
    /// below uses the same underlying signal internally.
    pub stale_after_secs: i64,
    /// Soft cap on live (`status = 'active'`) skills, enforced at promotion
    /// time by auto-archiving the least-recently-used active skill(s).
    /// `<= 0` disables auto-eviction entirely.
    pub max_active_skills: i64,
}

// Deliberately not `#[derive(Default)]`: i64/f64's own defaults are 0 and
// 0.0, which would silently make similarity_threshold 0.0 (everything
// "matches") and promotion_reps 0 (everything promotes instantly) - a
// correctness bug, not a style choice. Real sensible defaults, spelled out.
impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            promotion_reps: DEFAULT_PROMOTION_REPS,
            similarity_threshold: DEFAULT_SIMILARITY_THRESHOLD,
            stale_after_secs: DEFAULT_STALE_AFTER_SECS,
            max_active_skills: DEFAULT_MAX_ACTIVE_SKILLS,
        }
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS skill_candidates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    key TEXT NOT NULL,
    title TEXT NOT NULL,
    rep_count INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'warming',
    first_seen TEXT NOT NULL,
    last_seen TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS observations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at TEXT NOT NULL,
    title TEXT NOT NULL,
    summary TEXT NOT NULL,
    project TEXT,
    context_signal TEXT,
    high_stakes INTEGER NOT NULL DEFAULT 0,
    candidate_id INTEGER NOT NULL REFERENCES skill_candidates(id)
);

CREATE TABLE IF NOT EXISTS skills (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    candidate_id INTEGER NOT NULL REFERENCES skill_candidates(id),
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    promoted_reason TEXT NOT NULL,
    observation_count INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    last_invoked_at TEXT,
    status TEXT NOT NULL DEFAULT 'active'
);

CREATE TABLE IF NOT EXISTS corrections (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    skill_id INTEGER NOT NULL REFERENCES skills(id),
    created_at TEXT NOT NULL,
    kind TEXT NOT NULL,
    note TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS pending_reviews (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at TEXT NOT NULL,
    session_id TEXT NOT NULL,
    project TEXT,
    heuristic_reason TEXT NOT NULL,
    excerpt TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
);
";

pub struct Store {
    conn: Connection,
    promotion_reps: i64,
    similarity_threshold: f64,
    stale_after_secs: i64,
    max_active_skills: i64,
}

pub struct NewObservation {
    pub title: String,
    pub summary: String,
    pub project: Option<String>,
    pub context_signal: Option<String>,
    pub high_stakes: bool,
}

#[derive(Debug, Serialize)]
pub struct RecordResult {
    pub candidate_id: i64,
    pub rep_count: i64,
    pub promoted: bool,
    pub skill_path: Option<String>,
    /// Skills auto-archived to stay under `max_active_skills`, if this
    /// promotion pushed the active count over the cap. Empty otherwise.
    pub evicted_skills: Vec<EvictedSkill>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvictedSkill {
    pub skill_id: i64,
    pub slug: String,
}

#[derive(Debug, Serialize)]
pub struct PromoteOutcome {
    pub path: String,
    pub evicted: Vec<EvictedSkill>,
}

#[derive(Debug, Serialize)]
pub struct CandidateView {
    pub id: i64,
    pub title: String,
    pub rep_count: i64,
    pub status: String,
    pub first_seen: String,
    pub last_seen: String,
}

#[derive(Debug, Serialize)]
pub struct SkillView {
    pub id: i64,
    pub candidate_id: i64,
    pub slug: String,
    pub name: String,
    pub path: String,
    pub promoted_reason: String,
    pub observation_count: i64,
    pub created_at: String,
    pub correction_count: i64,
    pub confirmation_count: i64,
    pub last_invoked_at: Option<String>,
    /// No activity (last_invoked_at, or created_at if never invoked) for
    /// `stale_after_secs`. Purely informational - `stale` never triggers
    /// `archive_skill` automatically; an agent decides.
    pub stale: bool,
    /// `active` (live in `~/.claude/skills/`) or `archived` (moved out,
    /// via `archive_skill` - always an explicit action, never automatic).
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct FeedbackResult {
    pub skill_id: i64,
    pub correction_count: i64,
    pub confirmation_count: i64,
}

#[derive(Debug, Serialize)]
pub struct PendingReviewView {
    pub id: i64,
    pub created_at: String,
    pub session_id: String,
    pub project: Option<String>,
    pub heuristic_reason: String,
    pub excerpt: String,
}

/// A bounded slice of the graph around one skill - never the whole
/// store's data at once. Feeds `crate::graph::to_dot`.
#[derive(Debug, Serialize)]
pub struct SkillNeighborhood {
    pub skill_id: i64,
    pub skill_name: String,
    pub promoted_reason: String,
    pub candidate_id: i64,
    pub candidate_title: String,
    pub rep_count: i64,
    pub observations: Vec<ObservationRef>,
    pub corrections: Vec<CorrectionRef>,
}

#[derive(Debug, Serialize)]
pub struct ObservationRef {
    pub summary: String,
    pub project: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CorrectionRef {
    pub kind: String,
    pub note: String,
}

fn tokenize(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| s.len() > 2)
        .map(str::to_string)
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    intersection / union
}

/// Seconds elapsed from an RFC3339 timestamp to `now`. Unparseable
/// timestamps count as "just happened" (0) rather than erroring — this
/// only feeds an informational `stale` flag, not anything destructive.
fn seconds_since(now: DateTime<Utc>, ts: &str) -> i64 {
    DateTime::parse_from_rfc3339(ts)
        .map(|dt| (now - dt.with_timezone(&Utc)).num_seconds())
        .unwrap_or(0)
}

/// No real migrations system yet (see README) - this is the lightweight
/// stand-in for "add a column to a table that might already exist without
/// it." Any other error (malformed DDL, wrong type, etc.) still surfaces.
fn ensure_column(conn: &Connection, table: &str, column: &str, ddl: &str) -> anyhow::Result<()> {
    match conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {ddl}"),
        [],
    ) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(_, Some(msg)))
            if msg.contains("duplicate column name") =>
        {
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

impl Store {
    /// Opens (creating if needed) the store at `db_path`, with the given
    /// tuning. Use `StoreConfig::default()` if the caller has no
    /// `myelin_core::Config` of its own (e.g. tests).
    pub fn open(db_path: &Path, config: StoreConfig) -> anyhow::Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA)?;
        ensure_column(&conn, "skills", "last_invoked_at", "TEXT")?;
        ensure_column(&conn, "skills", "status", "TEXT NOT NULL DEFAULT 'active'")?;
        Ok(Self {
            conn,
            promotion_reps: config.promotion_reps,
            similarity_threshold: config.similarity_threshold,
            stale_after_secs: config.stale_after_secs,
            max_active_skills: config.max_active_skills,
        })
    }

    /// Records one observation, matches/creates its candidate, and promotes
    /// the candidate to a real skill if this observation crosses a
    /// promotion trigger (reps threshold, or an explicit high-stakes flag).
    pub fn record_observation(
        &self,
        input: NewObservation,
        skills_dir: &Path,
    ) -> anyhow::Result<RecordResult> {
        let tokens = tokenize(&format!("{} {}", input.title, input.summary));
        let now = Utc::now().to_rfc3339();

        let mut best: Option<(i64, f64, i64, String)> = None;
        {
            let mut stmt = self
                .conn
                .prepare("SELECT id, key, rep_count, status FROM skill_candidates")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (id, key, rep_count, status) = row?;
                let cand_tokens: HashSet<String> = key
                    .split(' ')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect();
                let score = jaccard(&tokens, &cand_tokens);
                if score >= self.similarity_threshold
                    && best.as_ref().map(|b| score > b.1).unwrap_or(true)
                {
                    best = Some((id, score, rep_count, status));
                }
            }
        }

        let (candidate_id, rep_count, status) = if let Some((id, _, rep_count, status)) = best {
            self.conn.execute(
                "UPDATE skill_candidates SET rep_count = rep_count + 1, last_seen = ?1 WHERE id = ?2",
                params![now, id],
            )?;
            (id, rep_count + 1, status)
        } else {
            let mut sorted_tokens: Vec<_> = tokens.iter().cloned().collect();
            sorted_tokens.sort();
            let key = sorted_tokens.join(" ");
            self.conn.execute(
                "INSERT INTO skill_candidates (key, title, rep_count, status, first_seen, last_seen)
                 VALUES (?1, ?2, 1, 'warming', ?3, ?3)",
                params![key, input.title, now],
            )?;
            (self.conn.last_insert_rowid(), 1, "warming".to_string())
        };

        self.conn.execute(
            "INSERT INTO observations
             (created_at, title, summary, project, context_signal, high_stakes, candidate_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                now,
                input.title,
                input.summary,
                input.project,
                input.context_signal,
                input.high_stakes,
                candidate_id
            ],
        )?;

        let mut promoted = false;
        let mut skill_path = None;
        let mut evicted_skills = Vec::new();

        if status == "warming" && (input.high_stakes || rep_count >= self.promotion_reps) {
            let reason = if input.high_stakes {
                "context_signal"
            } else {
                "reps"
            };
            let outcome = self.promote_internal(candidate_id, reason, skills_dir)?;
            skill_path = Some(outcome.path);
            evicted_skills = outcome.evicted;
            promoted = true;
        }

        Ok(RecordResult {
            candidate_id,
            rep_count,
            promoted,
            skill_path,
            evicted_skills,
        })
    }

    /// Force-promotes a candidate regardless of reps/high-stakes state.
    /// Errors if it's already promoted.
    pub fn promote_candidate(
        &self,
        candidate_id: i64,
        skills_dir: &Path,
    ) -> anyhow::Result<PromoteOutcome> {
        let status: String = self
            .conn
            .query_row(
                "SELECT status FROM skill_candidates WHERE id = ?1",
                params![candidate_id],
                |r| r.get(0),
            )
            .map_err(|_| anyhow::anyhow!("no such candidate: {candidate_id}"))?;
        if status == "promoted" {
            anyhow::bail!("candidate {candidate_id} is already promoted");
        }
        self.promote_internal(candidate_id, "manual", skills_dir)
    }

    fn promote_internal(
        &self,
        candidate_id: i64,
        reason: &str,
        skills_dir: &Path,
    ) -> anyhow::Result<PromoteOutcome> {
        let title: String = self.conn.query_row(
            "SELECT title FROM skill_candidates WHERE id = ?1",
            params![candidate_id],
            |r| r.get(0),
        )?;

        let mut stmt = self.conn.prepare(
            "SELECT summary, project FROM observations WHERE candidate_id = ?1 ORDER BY id",
        )?;
        let summaries: Vec<(String, Option<String>)> = stmt
            .query_map(params![candidate_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<Result<_, _>>()?;

        let (slug, path) =
            crate::skillfile::draft_and_write(&title, &summaries, reason, skills_dir)?;
        let now = Utc::now().to_rfc3339();
        let path_str = path.to_string_lossy().to_string();

        self.conn.execute(
            "INSERT INTO skills
             (candidate_id, slug, name, path, promoted_reason, observation_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                candidate_id,
                slug,
                title,
                path_str,
                reason,
                summaries.len() as i64,
                now
            ],
        )?;
        let new_skill_id = self.conn.last_insert_rowid();
        self.conn.execute(
            "UPDATE skill_candidates SET status = 'promoted' WHERE id = ?1",
            params![candidate_id],
        )?;

        // Promotion is fully committed at this point - a pruning failure
        // below must never look like a failed promotion. Best-effort:
        // skip (not error) any skill that fails to archive.
        let mut evicted = Vec::new();
        if self.max_active_skills > 0 {
            let active_count = self.active_skill_count()?;
            let over = active_count - self.max_active_skills;
            if over > 0 {
                for (skill_id, slug) in self.oldest_active_skills(new_skill_id, over)? {
                    if self.archive_skill(skill_id, skills_dir).is_ok() {
                        evicted.push(EvictedSkill { skill_id, slug });
                    }
                }
            }
        }

        Ok(PromoteOutcome {
            path: path_str,
            evicted,
        })
    }

    fn active_skill_count(&self) -> anyhow::Result<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM skills WHERE status = 'active'",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
    }

    /// The `n` oldest-by-reference-timestamp active skills, excluding
    /// `exclude_id` (the skill just promoted this call - never evict what
    /// was just promoted). Same reference timestamp `list_skills`'s `stale`
    /// flag uses: `last_invoked_at`, falling back to `created_at`.
    fn oldest_active_skills(&self, exclude_id: i64, n: i64) -> anyhow::Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, slug FROM skills
             WHERE status = 'active' AND id != ?1
             ORDER BY COALESCE(last_invoked_at, created_at) ASC, id ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![exclude_id, n], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Candidates still `warming` (not yet promoted), most-recently-seen
    /// first, capped at `limit` - both current callers only ever want the
    /// warming subset, so the filter lives in SQL rather than being
    /// re-applied by every caller.
    pub fn list_candidates(&self, limit: i64) -> anyhow::Result<Vec<CandidateView>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, rep_count, status, first_seen, last_seen
             FROM skill_candidates WHERE status = 'warming'
             ORDER BY last_seen DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(CandidateView {
                id: row.get(0)?,
                title: row.get(1)?,
                rep_count: row.get(2)?,
                status: row.get(3)?,
                first_seen: row.get(4)?,
                last_seen: row.get(5)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    pub fn list_skills(&self) -> anyhow::Result<Vec<SkillView>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.candidate_id, s.slug, s.name, s.path, s.promoted_reason,
                    s.observation_count, s.created_at, s.last_invoked_at, s.status,
                    (SELECT COUNT(*) FROM corrections c WHERE c.skill_id = s.id AND c.kind = 'correction'),
                    (SELECT COUNT(*) FROM corrections c WHERE c.skill_id = s.id AND c.kind = 'confirmation')
             FROM skills s ORDER BY s.created_at DESC",
        )?;
        let now = Utc::now();
        let rows = stmt.query_map([], |row| {
            let created_at: String = row.get(7)?;
            let last_invoked_at: Option<String> = row.get(8)?;
            let reference_ts = last_invoked_at
                .as_deref()
                .unwrap_or(&created_at)
                .to_string();
            Ok(SkillView {
                id: row.get(0)?,
                candidate_id: row.get(1)?,
                slug: row.get(2)?,
                name: row.get(3)?,
                path: row.get(4)?,
                promoted_reason: row.get(5)?,
                observation_count: row.get(6)?,
                created_at,
                last_invoked_at,
                status: row.get(9)?,
                correction_count: row.get(10)?,
                confirmation_count: row.get(11)?,
                stale: seconds_since(now, &reference_ts) >= self.stale_after_secs,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// The bounded neighborhood around one skill: its candidate, the
    /// observations that backed it, and any corrections/confirmations
    /// it's since received. Never returns more than that one skill's
    /// slice - there's no "whole graph" query in this store on purpose.
    pub fn skill_neighborhood(&self, skill_id: i64) -> anyhow::Result<SkillNeighborhood> {
        let (skill_name, promoted_reason, candidate_id): (String, String, i64) = self
            .conn
            .query_row(
                "SELECT name, promoted_reason, candidate_id FROM skills WHERE id = ?1",
                params![skill_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|_| anyhow::anyhow!("no such skill: {skill_id}"))?;

        let (candidate_title, rep_count): (String, i64) = self.conn.query_row(
            "SELECT title, rep_count FROM skill_candidates WHERE id = ?1",
            params![candidate_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;

        let mut obs_stmt = self.conn.prepare(
            "SELECT summary, project FROM observations WHERE candidate_id = ?1 ORDER BY id",
        )?;
        let observations = obs_stmt
            .query_map(params![candidate_id], |r| {
                Ok(ObservationRef {
                    summary: r.get(0)?,
                    project: r.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut corr_stmt = self
            .conn
            .prepare("SELECT kind, note FROM corrections WHERE skill_id = ?1 ORDER BY id")?;
        let corrections = corr_stmt
            .query_map(params![skill_id], |r| {
                Ok(CorrectionRef {
                    kind: r.get(0)?,
                    note: r.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(SkillNeighborhood {
            skill_id,
            skill_name,
            promoted_reason,
            candidate_id,
            candidate_title,
            rep_count,
            observations,
            corrections,
        })
    }

    /// Marks a skill as just having been invoked/followed - the signal
    /// `stale` in `list_skills` is judged against. Call this whenever the
    /// skill was actually used, regardless of whether it also gets
    /// feedback via `record_skill_feedback`.
    pub fn mark_skill_used(&self, skill_id: i64) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE skills SET last_invoked_at = ?1 WHERE id = ?2",
            params![now, skill_id],
        )?;
        if rows == 0 {
            anyhow::bail!("no such skill: {skill_id}");
        }
        Ok(())
    }

    /// Moves a skill's file out of the live skills directory into an
    /// archived subfolder, so it stops being loadable as an active skill.
    /// Nothing is deleted; `restore_skill` reverses it exactly. Always an
    /// explicit call; the `stale` flag never triggers this on its own.
    pub fn archive_skill(&self, skill_id: i64, skills_dir: &Path) -> anyhow::Result<String> {
        let (slug, path, status) = self.skill_slug_path_status(skill_id)?;
        if status == "archived" {
            anyhow::bail!("skill {skill_id} is already archived");
        }
        let archived_dir = skills_dir.join(".myelin-archived").join(&slug);
        self.relocate_skill(skill_id, &path, &archived_dir, "archived")
    }

    /// Reverses `archive_skill` - moves the file back under the live
    /// skills directory and marks it `active` again.
    pub fn restore_skill(&self, skill_id: i64, skills_dir: &Path) -> anyhow::Result<String> {
        let (slug, path, status) = self.skill_slug_path_status(skill_id)?;
        if status == "active" {
            anyhow::bail!("skill {skill_id} is already active");
        }
        let active_dir = skills_dir.join(&slug);
        self.relocate_skill(skill_id, &path, &active_dir, "active")
    }

    fn skill_slug_path_status(&self, skill_id: i64) -> anyhow::Result<(String, String, String)> {
        self.conn
            .query_row(
                "SELECT slug, path, status FROM skills WHERE id = ?1",
                params![skill_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|_| anyhow::anyhow!("no such skill: {skill_id}"))
    }

    fn relocate_skill(
        &self,
        skill_id: i64,
        current_path: &str,
        new_dir: &Path,
        new_status: &str,
    ) -> anyhow::Result<String> {
        std::fs::create_dir_all(new_dir)?;
        let new_path = new_dir.join("SKILL.md");
        std::fs::rename(current_path, &new_path)?;

        // Clean up the now-empty source directory. remove_dir (not
        // remove_dir_all) only succeeds if it's actually empty, so this
        // is a no-op rather than a hazard if anything unexpected is
        // still in there.
        if let Some(old_dir) = Path::new(current_path).parent() {
            let _ = std::fs::remove_dir(old_dir);
        }

        let new_path_str = new_path.to_string_lossy().to_string();
        self.conn.execute(
            "UPDATE skills SET path = ?1, status = ?2 WHERE id = ?3",
            params![new_path_str, new_status, skill_id],
        )?;
        Ok(new_path_str)
    }

    /// Records feedback on a promoted skill. `kind` is "correction" (the
    /// skill's instructions were wrong/incomplete — appended directly into
    /// the live SKILL.md, so it actually improves) or "confirmation" (it
    /// worked as written — logged for confidence, doesn't touch the file).
    pub fn record_skill_feedback(
        &self,
        skill_id: i64,
        kind: &str,
        note: &str,
    ) -> anyhow::Result<FeedbackResult> {
        if kind != "correction" && kind != "confirmation" {
            anyhow::bail!("kind must be 'correction' or 'confirmation', got '{kind}'");
        }
        let path: String = self
            .conn
            .query_row(
                "SELECT path FROM skills WHERE id = ?1",
                params![skill_id],
                |r| r.get(0),
            )
            .map_err(|_| anyhow::anyhow!("no such skill: {skill_id}"))?;

        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO corrections (skill_id, created_at, kind, note) VALUES (?1, ?2, ?3, ?4)",
            params![skill_id, now, kind, note],
        )?;

        if kind == "correction" {
            crate::skillfile::append_correction(Path::new(&path), note)?;
        }

        let correction_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM corrections WHERE skill_id = ?1 AND kind = 'correction'",
            params![skill_id],
            |r| r.get(0),
        )?;
        let confirmation_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM corrections WHERE skill_id = ?1 AND kind = 'confirmation'",
            params![skill_id],
            |r| r.get(0),
        )?;

        Ok(FeedbackResult {
            skill_id,
            correction_count,
            confirmation_count,
        })
    }

    /// Stages a heuristic-flagged, already-redacted excerpt from
    /// `myelin ingest-session` for later review by a live agent session -
    /// nothing here is treated as confirmed evidence until a caller
    /// decides to call `record_observation` off the back of it.
    pub fn stage_pending_review(
        &self,
        session_id: &str,
        project: Option<&str>,
        heuristic_reason: &str,
        excerpt: &str,
    ) -> anyhow::Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO pending_reviews (created_at, session_id, project, heuristic_reason, excerpt, status)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
            params![now, session_id, project, heuristic_reason, excerpt],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_pending_review(&self, limit: i64) -> anyhow::Result<Vec<PendingReviewView>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, session_id, project, heuristic_reason, excerpt
             FROM pending_reviews WHERE status = 'pending' ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(PendingReviewView {
                id: row.get(0)?,
                created_at: row.get(1)?,
                session_id: row.get(2)?,
                project: row.get(3)?,
                heuristic_reason: row.get(4)?,
                excerpt: row.get(5)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Clears a staged item from the queue - used both when it wasn't
    /// worth acting on and after successfully turning it into a real
    /// observation, since this store doesn't track that link explicitly.
    pub fn dismiss_pending_review(&self, id: i64) -> anyhow::Result<()> {
        let rows = self.conn.execute(
            "UPDATE pending_reviews SET status = 'dismissed' WHERE id = ?1 AND status = 'pending'",
            params![id],
        )?;
        if rows == 0 {
            anyhow::bail!("no pending review with id {id}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A fresh, isolated (db_path, skills_dir) pair under the OS temp dir.
    /// Not cleaned up afterward (harmless clutter in /tmp) — simplest thing
    /// that avoids a tempfile dependency while staying collision-free
    /// under parallel test execution.
    fn scratch_dirs(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("myelin-test-{label}-{nanos}"));
        (root.join("myelin.db"), root.join("skills"))
    }

    fn open_test_store(label: &str) -> (Store, std::path::PathBuf) {
        let (db_path, skills_dir) = scratch_dirs(label);
        let store = Store::open(&db_path, StoreConfig::default()).unwrap();
        (store, skills_dir)
    }

    fn obs(title: &str, summary: &str) -> NewObservation {
        NewObservation {
            title: title.to_string(),
            summary: summary.to_string(),
            project: None,
            context_signal: None,
            high_stakes: false,
        }
    }

    fn high_stakes_obs(title: &str, summary: &str) -> NewObservation {
        let mut input = obs(title, summary);
        input.high_stakes = true;
        input
    }

    #[test]
    fn promoting_past_the_cap_evicts_the_oldest_active_skill() {
        let (db_path, skills_dir) = scratch_dirs("evict-oldest");
        let store = Store::open(
            &db_path,
            StoreConfig {
                max_active_skills: 1,
                ..StoreConfig::default()
            },
        )
        .unwrap();

        store
            .record_observation(
                high_stakes_obs("rotate leaked api key", "revoke and reissue"),
                &skills_dir,
            )
            .unwrap();
        let result_b = store
            .record_observation(
                high_stakes_obs("fix flaky ci test", "add a retry with backoff"),
                &skills_dir,
            )
            .unwrap();

        let skills = store.list_skills().unwrap();
        let a = skills
            .iter()
            .find(|s| s.name == "rotate leaked api key")
            .unwrap();
        let b = skills
            .iter()
            .find(|s| s.name == "fix flaky ci test")
            .unwrap();
        assert_eq!(a.status, "archived");
        assert_eq!(b.status, "active");
        assert_eq!(result_b.evicted_skills.len(), 1);
        assert_eq!(result_b.evicted_skills[0].skill_id, a.id);
    }

    #[test]
    fn eviction_picks_the_least_recently_used_not_the_newest() {
        let (db_path, skills_dir) = scratch_dirs("evict-lru");
        let store = Store::open(
            &db_path,
            StoreConfig {
                max_active_skills: 2,
                ..StoreConfig::default()
            },
        )
        .unwrap();

        store
            .record_observation(
                high_stakes_obs("rotate leaked api key", "revoke and reissue"),
                &skills_dir,
            )
            .unwrap();
        store
            .record_observation(
                high_stakes_obs("fix flaky ci test", "add a retry with backoff"),
                &skills_dir,
            )
            .unwrap();

        let a_id = store
            .list_skills()
            .unwrap()
            .into_iter()
            .find(|s| s.name == "rotate leaked api key")
            .unwrap()
            .id;
        // Mark A used so its reference timestamp is newer than B's -
        // B is now the genuinely least-recently-used of the two.
        store.mark_skill_used(a_id).unwrap();

        let result_c = store
            .record_observation(
                high_stakes_obs("migrate database schema", "run the migration script"),
                &skills_dir,
            )
            .unwrap();

        let skills = store.list_skills().unwrap();
        let a = skills
            .iter()
            .find(|s| s.name == "rotate leaked api key")
            .unwrap();
        let b = skills
            .iter()
            .find(|s| s.name == "fix flaky ci test")
            .unwrap();
        let c = skills
            .iter()
            .find(|s| s.name == "migrate database schema")
            .unwrap();
        assert_eq!(a.status, "active", "just-used skill should not be evicted");
        assert_eq!(
            b.status, "archived",
            "least-recently-used skill should be evicted"
        );
        assert_eq!(
            c.status, "active",
            "just-promoted skill should not evict itself"
        );
        assert_eq!(result_c.evicted_skills[0].skill_id, b.id);
    }

    #[test]
    fn cap_zero_disables_auto_eviction() {
        let (db_path, skills_dir) = scratch_dirs("evict-disabled");
        let store = Store::open(
            &db_path,
            StoreConfig {
                max_active_skills: 0,
                ..StoreConfig::default()
            },
        )
        .unwrap();

        for (title, summary) in [
            ("rotate leaked api key", "revoke and reissue"),
            ("fix flaky ci test", "add a retry with backoff"),
            ("migrate database schema", "run the migration script"),
        ] {
            store
                .record_observation(high_stakes_obs(title, summary), &skills_dir)
                .unwrap();
        }

        let skills = store.list_skills().unwrap();
        assert_eq!(skills.len(), 3);
        assert!(skills.iter().all(|s| s.status == "active"));
    }

    #[test]
    fn default_config_does_not_evict_at_low_skill_counts() {
        let (store, skills_dir) = open_test_store("evict-default-low-count");
        store
            .record_observation(
                high_stakes_obs("rotate leaked api key", "revoke and reissue"),
                &skills_dir,
            )
            .unwrap();
        store
            .record_observation(
                high_stakes_obs("fix flaky ci test", "add a retry with backoff"),
                &skills_dir,
            )
            .unwrap();

        let skills = store.list_skills().unwrap();
        assert_eq!(skills.len(), 2);
        assert!(skills.iter().all(|s| s.status == "active"));
    }

    #[test]
    fn manual_promote_also_respects_the_cap() {
        let (db_path, skills_dir) = scratch_dirs("evict-manual-promote");
        let store = Store::open(
            &db_path,
            StoreConfig {
                promotion_reps: 100, // keep record_observation from auto-promoting
                max_active_skills: 1,
                ..StoreConfig::default()
            },
        )
        .unwrap();

        let candidate_a = store
            .record_observation(
                obs("rotate leaked api key", "revoke and reissue"),
                &skills_dir,
            )
            .unwrap()
            .candidate_id;
        let candidate_b = store
            .record_observation(
                obs("fix flaky ci test", "add a retry with backoff"),
                &skills_dir,
            )
            .unwrap()
            .candidate_id;

        store.promote_candidate(candidate_a, &skills_dir).unwrap();
        let outcome_b = store.promote_candidate(candidate_b, &skills_dir).unwrap();

        assert_eq!(outcome_b.evicted.len(), 1);
        let skills = store.list_skills().unwrap();
        assert_eq!(
            skills.iter().filter(|s| s.status == "active").count(),
            1,
            "manual promotion path must enforce the cap too, not just the automatic one"
        );
    }

    #[test]
    fn auto_evicted_skill_file_actually_moves_to_the_archived_dir() {
        let (db_path, skills_dir) = scratch_dirs("evict-file-move");
        let store = Store::open(
            &db_path,
            StoreConfig {
                max_active_skills: 1,
                ..StoreConfig::default()
            },
        )
        .unwrap();

        store
            .record_observation(
                high_stakes_obs("rotate leaked api key", "revoke and reissue"),
                &skills_dir,
            )
            .unwrap();
        store
            .record_observation(
                high_stakes_obs("fix flaky ci test", "add a retry with backoff"),
                &skills_dir,
            )
            .unwrap();

        let a = store
            .list_skills()
            .unwrap()
            .into_iter()
            .find(|s| s.name == "rotate leaked api key")
            .unwrap();
        assert_eq!(a.status, "archived");
        assert!(a.path.contains(".myelin-archived"));
        assert!(std::path::Path::new(&a.path).exists());
    }

    #[test]
    fn tokenize_lowercases_and_drops_short_tokens() {
        let tokens = tokenize("Run DB Migration on X");
        assert!(tokens.contains("run"));
        assert!(tokens.contains("migration"));
        assert!(!tokens.contains("db")); // len <= 2, filtered out
        assert!(!tokens.contains("x"));
    }

    #[test]
    fn jaccard_of_identical_sets_is_one() {
        let a: HashSet<String> = ["run", "migration", "service"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(jaccard(&a, &a.clone()), 1.0);
    }

    #[test]
    fn jaccard_of_disjoint_sets_is_zero() {
        let a: HashSet<String> = ["run", "migration"].iter().map(|s| s.to_string()).collect();
        let b: HashSet<String> = ["deploy", "service"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(jaccard(&a, &b), 0.0);
    }

    #[test]
    fn first_observation_creates_a_new_warming_candidate() {
        let (store, skills_dir) = open_test_store("new-candidate");
        let result = store
            .record_observation(
                obs("apply db migration hotfix", "run migrate.sh"),
                &skills_dir,
            )
            .unwrap();
        assert_eq!(result.rep_count, 1);
        assert!(!result.promoted);
        assert!(result.skill_path.is_none());
    }

    #[test]
    fn similar_observation_increments_the_same_candidate() {
        let (store, skills_dir) = open_test_store("increment-reps");
        let first = store
            .record_observation(
                obs(
                    "apply db migration hotfix",
                    "run migrate.sh then restart service",
                ),
                &skills_dir,
            )
            .unwrap();
        let second = store
            .record_observation(
                obs(
                    "apply db migration hotfix across services",
                    "run migrate.sh then restart service",
                ),
                &skills_dir,
            )
            .unwrap();
        assert_eq!(second.candidate_id, first.candidate_id);
        assert_eq!(second.rep_count, 2);
    }

    #[test]
    fn dissimilar_observation_creates_a_separate_candidate() {
        let (store, skills_dir) = open_test_store("separate-candidate");
        let first = store
            .record_observation(
                obs("apply db migration hotfix", "run migrate.sh"),
                &skills_dir,
            )
            .unwrap();
        let second = store
            .record_observation(
                obs("rotate leaked api key", "revoke and reissue the key"),
                &skills_dir,
            )
            .unwrap();
        assert_ne!(first.candidate_id, second.candidate_id);
    }

    #[test]
    fn reps_threshold_promotes_and_writes_a_real_skill_file() {
        let (store, skills_dir) = open_test_store("reps-promotion");
        let title = "apply db migration hotfix";
        let summary = "run migrate.sh then restart service then verify health";
        store
            .record_observation(obs(title, summary), &skills_dir)
            .unwrap();
        store
            .record_observation(obs(title, summary), &skills_dir)
            .unwrap();
        let third = store
            .record_observation(obs(title, summary), &skills_dir)
            .unwrap();

        assert!(third.promoted);
        let path = third.skill_path.unwrap();
        assert!(std::path::Path::new(&path).exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("name:"));
        assert!(content.contains(title));

        let skills = store.list_skills().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].promoted_reason, "reps");
        assert_eq!(skills[0].observation_count, 3);
    }

    #[test]
    fn high_stakes_promotes_on_the_first_observation() {
        let (store, skills_dir) = open_test_store("high-stakes-promotion");
        let mut input = obs("roll out CVE patch", "bump the dependency, run the scanner");
        input.high_stakes = true;
        input.context_signal = Some("security ticket: needs to land in all repos this week".into());

        let result = store.record_observation(input, &skills_dir).unwrap();
        assert!(result.promoted);
        assert_eq!(result.rep_count, 1);

        let skills = store.list_skills().unwrap();
        assert_eq!(skills[0].promoted_reason, "context_signal");
    }

    #[test]
    fn manual_promote_works_and_rejects_double_promotion() {
        let (store, skills_dir) = open_test_store("manual-promote");
        let result = store
            .record_observation(obs("one-off thing", "did it once"), &skills_dir)
            .unwrap();
        assert!(!result.promoted); // only 1 rep, no high_stakes -> still warming

        store
            .promote_candidate(result.candidate_id, &skills_dir)
            .unwrap();
        let err = store
            .promote_candidate(result.candidate_id, &skills_dir)
            .unwrap_err();
        assert!(err.to_string().contains("already promoted"));
    }

    #[test]
    fn correction_appends_to_the_live_skill_file_confirmation_does_not() {
        let (store, skills_dir) = open_test_store("feedback");
        let mut input = obs("rotate leaked api key", "revoke and reissue");
        input.high_stakes = true;
        let result = store.record_observation(input, &skills_dir).unwrap();
        let skill_id = store.list_skills().unwrap()[0].id;
        let path = result.skill_path.unwrap();

        let before = std::fs::read_to_string(&path).unwrap();
        store
            .record_skill_feedback(skill_id, "confirmation", "worked as written")
            .unwrap();
        let after_confirmation = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after_confirmation);

        let feedback = store
            .record_skill_feedback(skill_id, "correction", "also invalidate cached tokens")
            .unwrap();
        assert_eq!(feedback.correction_count, 1);
        assert_eq!(feedback.confirmation_count, 1);
        let after_correction = std::fs::read_to_string(&path).unwrap();
        assert!(after_correction.contains("## Corrections"));
        assert!(after_correction.contains("also invalidate cached tokens"));
    }

    #[test]
    fn feedback_rejects_invalid_kind_and_unknown_skill() {
        let (store, skills_dir) = open_test_store("feedback-errors");
        let mut input = obs("rotate leaked api key", "revoke and reissue");
        input.high_stakes = true;
        store.record_observation(input, &skills_dir).unwrap();
        let skill_id = store.list_skills().unwrap()[0].id;

        let bad_kind = store
            .record_skill_feedback(skill_id, "bogus", "note")
            .unwrap_err();
        assert!(bad_kind.to_string().contains("kind must be"));

        let bad_id = store
            .record_skill_feedback(999_999, "confirmation", "note")
            .unwrap_err();
        assert!(bad_id.to_string().contains("no such skill"));
    }

    #[test]
    fn a_freshly_promoted_skill_is_not_stale() {
        let (db_path, skills_dir) = scratch_dirs("fresh-not-stale");
        let store = Store::open(
            &db_path,
            StoreConfig {
                stale_after_secs: 3600,
                ..StoreConfig::default()
            },
        )
        .unwrap();
        let mut input = obs("rotate leaked api key", "revoke and reissue");
        input.high_stakes = true;
        store.record_observation(input, &skills_dir).unwrap();

        let skills = store.list_skills().unwrap();
        assert!(!skills[0].stale);
        assert!(skills[0].last_invoked_at.is_none());
    }

    #[test]
    fn a_skill_older_than_the_stale_window_is_flagged_and_marking_used_clears_it() {
        let (db_path, skills_dir) = scratch_dirs("stale-window");
        let store = Store::open(
            &db_path,
            StoreConfig {
                stale_after_secs: 60,
                ..StoreConfig::default()
            },
        )
        .unwrap();
        let mut input = obs("rotate leaked api key", "revoke and reissue");
        input.high_stakes = true;
        store.record_observation(input, &skills_dir).unwrap();
        let skill_id = store.list_skills().unwrap()[0].id;

        // Backdate created_at well past the 60s window - direct access to
        // `conn` works here since this test module is nested inside the
        // same file as `Store`.
        let two_hours_ago = (Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        store
            .conn
            .execute(
                "UPDATE skills SET created_at = ?1 WHERE id = ?2",
                params![two_hours_ago, skill_id],
            )
            .unwrap();
        assert!(store.list_skills().unwrap()[0].stale);

        store.mark_skill_used(skill_id).unwrap();
        // last_invoked_at is now "just now", well inside the 60s window.
        assert!(!store.list_skills().unwrap()[0].stale);
        assert!(store.list_skills().unwrap()[0].last_invoked_at.is_some());
    }

    #[test]
    fn mark_skill_used_rejects_unknown_skill() {
        let (store, _skills_dir) = open_test_store("mark-used-unknown");
        let err = store.mark_skill_used(999_999).unwrap_err();
        assert!(err.to_string().contains("no such skill"));
    }

    #[test]
    fn pending_review_lifecycle() {
        let (store, _skills_dir) = open_test_store("pending-review");

        let id = store
            .stage_pending_review(
                "sess-1",
                Some("myelin"),
                "multi-step-sequence",
                "redacted excerpt",
            )
            .unwrap();

        let queue = store.list_pending_review(50).unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].id, id);
        assert_eq!(queue[0].heuristic_reason, "multi-step-sequence");

        store.dismiss_pending_review(id).unwrap();
        assert!(store.list_pending_review(50).unwrap().is_empty());
    }

    #[test]
    fn list_pending_review_respects_the_limit() {
        let (store, _skills_dir) = open_test_store("pending-review-limit");
        for i in 0..3 {
            store
                .stage_pending_review("sess-1", None, "reason", &format!("excerpt {i}"))
                .unwrap();
        }
        assert_eq!(store.list_pending_review(200).unwrap().len(), 3);
        assert_eq!(store.list_pending_review(2).unwrap().len(), 2);
    }

    #[test]
    fn list_candidates_only_returns_warming_and_respects_the_limit() {
        let (store, skills_dir) = open_test_store("candidates-limit");
        // Three dissimilar single observations stay "warming" (default
        // promotion_reps is 3, none high_stakes).
        for (title, summary) in [
            ("rotate leaked api key", "revoke and reissue"),
            ("fix flaky ci test", "add a retry with backoff"),
            ("migrate database schema", "run the migration script"),
        ] {
            store
                .record_observation(obs(title, summary), &skills_dir)
                .unwrap();
        }
        assert_eq!(store.list_candidates(200).unwrap().len(), 3);
        assert_eq!(store.list_candidates(2).unwrap().len(), 2);
    }

    #[test]
    fn dismissing_unknown_or_already_dismissed_review_errors() {
        let (store, _skills_dir) = open_test_store("pending-review-errors");
        let err = store.dismiss_pending_review(999_999).unwrap_err();
        assert!(err.to_string().contains("no pending review"));

        let id = store
            .stage_pending_review("sess-1", None, "correction-language", "excerpt")
            .unwrap();
        store.dismiss_pending_review(id).unwrap();
        let err = store.dismiss_pending_review(id).unwrap_err();
        assert!(err.to_string().contains("no pending review"));
    }

    #[test]
    fn archive_then_restore_a_skill_round_trips_the_file_and_status() {
        let (store, skills_dir) = open_test_store("archive-restore");
        let mut input = obs("rotate leaked api key", "revoke and reissue");
        input.high_stakes = true;
        let result = store.record_observation(input, &skills_dir).unwrap();
        let original_path = result.skill_path.unwrap();
        let skill_id = store.list_skills().unwrap()[0].id;
        assert_eq!(store.list_skills().unwrap()[0].status, "active");
        assert!(std::path::Path::new(&original_path).exists());

        let archived_path = store.archive_skill(skill_id, &skills_dir).unwrap();
        assert!(!std::path::Path::new(&original_path).exists());
        assert!(std::path::Path::new(&archived_path).exists());
        assert!(archived_path.contains(".myelin-archived"));
        assert_eq!(store.list_skills().unwrap()[0].status, "archived");
        // Regression: relocate_skill used to leave the now-empty original
        // directory behind instead of cleaning it up.
        let original_dir = std::path::Path::new(&original_path).parent().unwrap();
        assert!(!original_dir.exists(), "empty source dir should be removed");

        let restored_path = store.restore_skill(skill_id, &skills_dir).unwrap();
        assert!(!std::path::Path::new(&archived_path).exists());
        assert!(std::path::Path::new(&restored_path).exists());
        assert_eq!(restored_path, original_path);
        assert_eq!(store.list_skills().unwrap()[0].status, "active");
        let archived_dir = std::path::Path::new(&archived_path).parent().unwrap();
        assert!(
            !archived_dir.exists(),
            "empty archived dir should be removed"
        );
    }

    #[test]
    fn archive_and_restore_reject_redundant_state_changes_and_unknown_ids() {
        let (store, skills_dir) = open_test_store("archive-restore-errors");
        let mut input = obs("rotate leaked api key", "revoke and reissue");
        input.high_stakes = true;
        store.record_observation(input, &skills_dir).unwrap();
        let skill_id = store.list_skills().unwrap()[0].id;

        let err = store.restore_skill(skill_id, &skills_dir).unwrap_err();
        assert!(err.to_string().contains("already active"));

        store.archive_skill(skill_id, &skills_dir).unwrap();
        let err = store.archive_skill(skill_id, &skills_dir).unwrap_err();
        assert!(err.to_string().contains("already archived"));

        let err = store.archive_skill(999_999, &skills_dir).unwrap_err();
        assert!(err.to_string().contains("no such skill"));
    }
}
