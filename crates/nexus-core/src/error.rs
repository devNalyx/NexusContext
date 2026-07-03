use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to read config file at {path}: {source}")]
    ConfigRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse config: {0}")]
    ConfigParse(#[from] toml::de::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
