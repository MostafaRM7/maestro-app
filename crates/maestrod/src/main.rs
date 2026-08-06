use maestrod::{DaemonConfig, DaemonPaths, DaemonServer};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("maestrod=info")),
        )
        .with_target(false)
        .init();

    let server = DaemonServer::bind(DaemonPaths::discover()?, DaemonConfig::default()).await?;
    let shutdown = server.shutdown_handle();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            shutdown.request();
        }
    });
    server.run().await?;
    Ok(())
}
