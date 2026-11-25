//! NexusContext declarative memory.
//!
//! Curated facts only: decisions, incidents, lessons, dead ends, owners.
//! SQLite is canonical; OKF v0.3 is the portable exchange contract (ADR 0020).
//! No raw transcript, code structure, preferences — and no LLM in the daemon.

pub mod fact;
pub mod gate;
pub mod injection;
pub mod ledger;
pub mod okf;
pub mod recall;
pub mod store;

pub use fact::{
    trust_tier, Anchor, Fact, FactKind, FactStatus, Source, TrustTier,
    DEFAULT_INJECTION_BUDGET_TOKENS, OKF_VERSION,
};
pub use gate::{admit, Admission, Exclusion};
pub use injection::{render_injection, Injection, SkillSignpost};
pub use ledger::{InjectionRecord, Ledger, UsageTokens};
pub use okf::{discover_bundles, BundleRoute};
pub use recall::{estimate_tokens, recall, recall_with_lower_trust, RecallResult};
pub use store::Store;
