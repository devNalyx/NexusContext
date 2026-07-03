use anyhow::Result;
use nexus_core::{Config, Paths};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let paths = Paths::resolve();
    std::fs::create_dir_all(&paths.config_dir)?;
    std::fs::create_dir_all(&paths.data_dir)?;

    let config = Config::load(&paths.config_file())?;

    tracing::info!(
        config_dir = %paths.config_dir.display(),
        data_dir = %paths.data_dir.display(),
        embeddings_configured = config.embeddings.endpoint.is_some(),
        "nexusd starting"
    );

    if config.embeddings.endpoint.is_none() {
        tracing::info!(
            "no embeddings endpoint configured - semantic search disabled, \
             structural/graph tools remain fully available"
        );
    }

    // Phase 0 scaffold: MCP stdio server, ingestion engine, and control socket
    // land in later phases. For now the daemon just proves it starts cleanly
    // and shuts down on signal.
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");

    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = std::env::var("NEXUS_LOG_LEVEL")
        .map(EnvFilter::new)
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt().with_env_filter(filter).init();
}
