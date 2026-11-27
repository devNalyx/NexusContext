pub mod config;
pub mod error;
pub mod paths;
pub mod registry;
pub mod stats;
pub mod text;

pub use config::{
    Config, ConfigIndexConfig, LspConfig, SourceConfig, ToolsConfig, ToolsPreset, VaultIndexConfig,
    WatcherConfig,
};
pub use error::{Error, Result};
pub use paths::{harden_dir_owner_only, project_hash, Paths};
pub use registry::{ProjectEntry, Registry};
pub use stats::{ToolCallStats, UsageStats};
pub use text::truncate_to_byte_boundary;
