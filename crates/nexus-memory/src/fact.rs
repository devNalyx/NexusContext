//! The declarative memory types. Facts are curated, code-anchored and
//! review-gated; they are never raw transcript, code structure or preference
//! data (proposal locked decision 1). Their exchange shape is OKF v0.3 (ADR
//! 0020), but the in-memory model here is deliberately independent of YAML.

use serde::{Deserialize, Serialize};

/// Pin the exchange version. OKF is pre-1.0 and minor versions may break
/// (§12), so a bundle declares exactly what it was written against.
pub const OKF_VERSION: &str = "0.3";

/// Hard default for injected context. A caller may lower it, never raise it
/// past the daemon's configured cap (see `gate.rs`).
pub const DEFAULT_INJECTION_BUDGET_TOKENS: u64 = 800;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    Decision,
    Incident,
    Lesson,
    DeadEnd,
    Owner,
}

impl FactKind {
    /// OKF `type` values are producer-chosen; these are ours (ADR 0020).
    pub fn as_okf_type(self) -> &'static str {
        match self {
            Self::Decision => "Decision",
            Self::Incident => "Incident",
            Self::Lesson => "Lesson",
            Self::DeadEnd => "Dead End",
            Self::Owner => "Owner",
        }
    }

    pub fn from_okf_type(value: &str) -> Option<Self> {
        match value.trim() {
            "Decision" => Some(Self::Decision),
            "Incident" => Some(Self::Incident),
            "Lesson" => Some(Self::Lesson),
            "Dead End" | "DeadEnd" => Some(Self::DeadEnd),
            "Owner" => Some(Self::Owner),
            // OKF §11: unknown types are tolerated, but tolerated is not
            // trusted — an unknown type is not imported as a fact.
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FactStatus {
    Draft,
    #[default]
    Stable,
    Deprecated,
}

impl FactStatus {
    pub fn as_okf(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Stable => "stable",
            Self::Deprecated => "deprecated",
        }
    }

    pub fn from_okf(value: &str) -> Self {
        // OKF §5.4: a value outside the vocabulary is NOT stable.
        match value {
            "draft" => Self::Draft,
            "deprecated" => Self::Deprecated,
            "stable" => Self::Stable,
            _ => Self::Draft,
        }
    }
}

/// A code anchor. `hash` is the file fingerprint recorded at curation time;
/// the watcher flips the fact to stale when the file changes (P2.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub path: String,
    pub symbol: Option<String>,
    pub line: Option<u32>,
    pub hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    pub resource: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub id: String,
    pub repo_path: String,
    pub kind: FactKind,
    pub title: String,
    pub body: String,
    pub status: FactStatus,
    pub superseded_by: Option<String>,
    pub anchors: Vec<Anchor>,
    pub sources: Vec<Source>,
    pub generated_by: String,
    pub generated_at: i64,
    pub verified_by: Option<String>,
    pub verified_at: Option<i64>,
    pub stale_after: Option<i64>,
    /// Watcher/file-hash truth, kept in SQLite rather than derived from the
    /// OKF frontmatter so an exported bundle can never "un-stale" a fact.
    pub anchor_stale: bool,
}

impl Fact {
    pub fn is_stale(&self, now: i64) -> bool {
        if self.anchor_stale {
            return true;
        }
        // OKF §5.5: an unparseable/absolute instant must never be treated as
        // fresh. Parsed values are stored as unix seconds; a malformed value
        // is rejected at import time rather than silently becoming None.
        self.stale_after.is_some_and(|deadline| now >= deadline)
    }
}

/// OKF §5.3 trust tiers. Only `human:` actors confer the top tier, and only
/// when the verification is current (`verified.at >= generated.at`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustTier {
    Unverified,
    MachineConfirmed,
    HumanReviewed,
}

impl TrustTier {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::MachineConfirmed => "machine-confirmed",
            Self::HumanReviewed => "human-reviewed",
        }
    }
}

pub fn trust_tier(fact: &Fact) -> TrustTier {
    let (Some(actor), Some(at)) = (&fact.verified_by, fact.verified_at) else {
        return TrustTier::Unverified;
    };
    if at < fact.generated_at {
        return TrustTier::Unverified;
    }
    if actor.starts_with("human:") && actor.len() > "human:".len() {
        TrustTier::HumanReviewed
    } else {
        TrustTier::MachineConfirmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact() -> Fact {
        Fact {
            id: "f1".into(),
            repo_path: "/repo".into(),
            kind: FactKind::Decision,
            title: "t".into(),
            body: "b".into(),
            status: FactStatus::Stable,
            superseded_by: None,
            anchors: vec![],
            sources: vec![],
            generated_by: "agent/1".into(),
            generated_at: 100,
            verified_by: None,
            verified_at: None,
            stale_after: None,
            anchor_stale: false,
        }
    }

    #[test]
    fn a_human_verification_after_generation_is_human_reviewed() {
        let mut f = fact();
        f.verified_by = Some("human:alice".into());
        f.verified_at = Some(100);
        assert_eq!(trust_tier(&f), TrustTier::HumanReviewed);
    }

    #[test]
    fn a_human_verification_before_regeneration_does_not_count() {
        let mut f = fact();
        f.verified_by = Some("human:alice".into());
        f.verified_at = Some(99);
        f.generated_at = 100;
        assert_eq!(trust_tier(&f), TrustTier::Unverified);
    }

    #[test]
    fn team_and_process_actors_are_only_machine_confirmed() {
        let mut f = fact();
        f.verified_by = Some("team:finance".into());
        f.verified_at = Some(100);
        assert_eq!(trust_tier(&f), TrustTier::MachineConfirmed);
        f.verified_by = Some("process:nightly".into());
        assert_eq!(trust_tier(&f), TrustTier::MachineConfirmed);
    }

    #[test]
    fn empty_human_actor_is_not_a_review() {
        let mut f = fact();
        f.verified_by = Some("human:".into());
        f.verified_at = Some(100);
        assert_eq!(trust_tier(&f), TrustTier::MachineConfirmed);
    }

    #[test]
    fn both_staleness_sources_are_honoured() {
        let mut f = fact();
        f.stale_after = Some(200);
        assert!(!f.is_stale(199));
        assert!(f.is_stale(200));
        let mut f = fact();
        f.anchor_stale = true;
        assert!(f.is_stale(0));
    }

    #[test]
    fn unknown_okf_status_is_not_stable() {
        assert_eq!(FactStatus::from_okf("surprise"), FactStatus::Draft);
        assert_eq!(FactStatus::from_okf("stable"), FactStatus::Stable);
    }
}
