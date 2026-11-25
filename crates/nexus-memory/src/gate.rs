//! Admission gate. OKF trust tiers are advisory signals and never access
//! control (OKF §5.3); NC turns them into an admission decision for *auto*
//! injection. This is where "without losing confidence" is enforced.

use crate::fact::{trust_tier, Fact, TrustTier};
use nexus_core::Config;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exclusion {
    Stale,
    NotStable,
    Unverified,
    MachineConfirmed,
    AnchorOutsideRoots,
}

impl Exclusion {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stale => "stale",
            Self::NotStable => "not_stable",
            Self::Unverified => "unverified",
            Self::MachineConfirmed => "machine_confirmed",
            Self::AnchorOutsideRoots => "anchor_outside_allowed_roots",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Admission {
    pub tier: TrustTier,
    pub include: bool,
    pub exclusions: Vec<Exclusion>,
}

/// Auto-inject only facts that are current, stable, anchored inside
/// `allowed_roots`, and carrying a current `human:` verification. Everything
/// else is still readable through explicit recall, but never pushed into a
/// harness unasked.
pub fn admit(fact: &Fact, config: &Config, now: i64) -> Admission {
    let mut exclusions = Vec::new();

    if fact.is_stale(now) {
        exclusions.push(Exclusion::Stale);
    }
    if fact.status != crate::fact::FactStatus::Stable {
        exclusions.push(Exclusion::NotStable);
    }
    if fact.anchors.iter().any(|anchor| {
        let raw = Path::new(&anchor.path);
        // OKF bundles carry repo-relative anchors so they stay portable;
        // resolve against the fact's repo before checking allowed_roots.
        let resolved = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            Path::new(&fact.repo_path).join(raw)
        };
        !config.is_path_allowed(&resolved)
    }) {
        exclusions.push(Exclusion::AnchorOutsideRoots);
    }

    let tier = trust_tier(fact);
    match tier {
        TrustTier::Unverified => exclusions.push(Exclusion::Unverified),
        TrustTier::MachineConfirmed => exclusions.push(Exclusion::MachineConfirmed),
        TrustTier::HumanReviewed => {}
    }

    Admission {
        tier,
        include: exclusions.is_empty(),
        exclusions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fact::{Anchor, FactKind, FactStatus};

    fn fact() -> Fact {
        Fact {
            id: "f".into(),
            repo_path: "/repo".into(),
            kind: FactKind::Decision,
            title: "t".into(),
            body: "b".into(),
            status: FactStatus::Stable,
            superseded_by: None,
            anchors: vec![Anchor {
                path: "/repo/src/lib.rs".into(),
                symbol: None,
                line: None,
                hash: None,
            }],
            sources: vec![],
            generated_by: "agent/1".into(),
            generated_at: 100,
            verified_by: Some("human:alice".into()),
            verified_at: Some(100),
            stale_after: None,
            anchor_stale: false,
        }
    }

    #[test]
    fn a_current_human_reviewed_stable_fact_is_injected() {
        let a = admit(&fact(), &Config::default(), 200);
        assert!(a.include);
        assert_eq!(a.tier, TrustTier::HumanReviewed);
    }

    #[test]
    fn machine_confirmed_and_unverified_are_never_auto_injected() {
        let mut f = fact();
        f.verified_by = Some("process:nightly".into());
        let a = admit(&f, &Config::default(), 200);
        assert!(!a.include);
        assert!(a.exclusions.contains(&Exclusion::MachineConfirmed));

        f.verified_at = Some(99);
        let a = admit(&f, &Config::default(), 200);
        assert!(a.exclusions.contains(&Exclusion::Unverified));
    }

    #[test]
    fn stale_and_deprecated_facts_are_excluded() {
        let mut f = fact();
        f.stale_after = Some(150);
        assert!(admit(&f, &Config::default(), 200)
            .exclusions
            .contains(&Exclusion::Stale));
        f.stale_after = None;
        f.status = FactStatus::Deprecated;
        assert!(admit(&f, &Config::default(), 200)
            .exclusions
            .contains(&Exclusion::NotStable));
    }

    #[test]
    fn anchors_outside_allowed_roots_are_excluded() {
        let dir = std::env::temp_dir().join(format!("nc-gate-{}", std::process::id()));
        let inside = dir.join("inside");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(inside.join("a.rs"), "x").unwrap();
        std::fs::write(outside.join("b.rs"), "x").unwrap();

        let config = Config {
            allowed_roots: vec![inside.to_string_lossy().to_string()],
            ..Default::default()
        };
        let mut f = fact();
        f.anchors[0].path = outside.join("b.rs").to_string_lossy().to_string();
        assert!(admit(&f, &config, 200)
            .exclusions
            .contains(&Exclusion::AnchorOutsideRoots));
        f.anchors[0].path = inside.join("a.rs").to_string_lossy().to_string();
        assert!(admit(&f, &config, 200).include);

        // OKF anchors are repo-relative on purpose; the gate must resolve
        // them against the fact's repo, not the daemon's working directory.
        f.repo_path = inside.to_string_lossy().to_string();
        f.anchors[0].path = "a.rs".into();
        assert!(admit(&f, &config, 200).include);
        // No open DB handle here, but keep cleanup robust on Windows.
        let _ = std::fs::remove_dir_all(dir);
    }
}
