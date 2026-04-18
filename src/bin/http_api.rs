use gt06n_tcp_server::api::HttpApiServer;
use gt06n_tcp_server::config::Config;
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

    let server = HttpApiServer::from_config(&config)
        .await
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;

    tracing::info!(bind_addr = %server.bind_addr(), "HTTP API server started");

    server
        .run()
        .await
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;

    Ok(())
}
