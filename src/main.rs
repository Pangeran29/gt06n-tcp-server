use std::sync::Arc;

use gt06n_tcp_server::config::Config;
use gt06n_tcp_server::db::build_event_handler;
use gt06n_tcp_server::server::Gt06TcpServer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env();

    let env_filter = EnvFilter::try_new(config.log_filter.clone())
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .compact()
        .init();

    let event_handler = build_event_handler(&config)
        .await
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;

    let server = Gt06TcpServer::bind(config.clone(), Arc::clone(&event_handler)).await?;
    tracing::info!(bind_addr = %server.local_addr()?, "GT06N TCP server started");
    server.run().await?;

    Ok(())
}
