//! Restic REST API server backed by 115 open platform cloud storage.

use clap::Parser;
use std::net::SocketAddr;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use restic_115::config::Config;
use restic_115::open115::Open115Client;
use restic_115::restic::create_router;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| config.log_level.clone().into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Starting restic-115");
    tracing::info!("Repository path: {}", config.repo_path);
    tracing::info!("Listen address: {}", config.listen_addr);

    let client = Open115Client::new(config.clone());

    let app = create_router(client).layer(TraceLayer::new_for_http());
    let addr: SocketAddr = config.listen_addr.parse()?;

    tracing::info!("Server listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
