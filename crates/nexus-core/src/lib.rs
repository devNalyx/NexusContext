pub mod config;
pub mod error;
pub mod paths;
pub mod registry;
pub mod stats;

pub use config::{Config, EmbeddingsConfig, EmbeddingsPolicy, WatcherConfig};
pub use error::{Error, Result};
pub use paths::{project_hash, Paths};
pub use registry::{ProjectEntry, Registry};
pub use stats::{ToolCallStats, UsageStats};
