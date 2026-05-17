use gt06n_tcp_server::config::Config;
use gt06n_tcp_server::subscription_maintenance::run_subscription_maintenance_from_config;
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

    run_subscription_maintenance_from_config(&config)
        .await
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;

    Ok(())
}
