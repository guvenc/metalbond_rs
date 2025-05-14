use anyhow::Result;
use clap::Parser;
use metalbond::{Config, DefaultNetworkClient, MetalBond};
use std::sync::Arc;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct ServerCli {
    /// Listen address. e.g. :4711
    #[arg(long, default_value = "[::]:4711")]
    listen: String,

    /// Enable debug logging
    #[arg(short, long, default_value_t = false)]
    verbose: bool,

    /// Keepalive Interval in seconds
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    keepalive: Option<u64>,

    /// HTTP Server listen address. e.g. :4712
    #[arg(long)]
    http: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = ServerCli::parse();

    // Setup Logging
    let log_level = if cli.verbose { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(format!("metalbond={}", log_level)))?;
    let subscriber = FmtSubscriber::builder().with_env_filter(filter).finish();
    tracing::subscriber::set_global_default(subscriber)?;

    tracing::info!("MetalBond Server Version: {}", env!("CARGO_PKG_VERSION"));

    // ---- Configuration ----
    let mut config = Config::default();
    if let Some(secs) = cli.keepalive {
        config.keepalive_interval = std::time::Duration::from_secs(secs);
    }

    // Create appropriate client (dummy on non-Linux)
    #[cfg(not(any(feature = "netlink-support", target_os = "linux")))]
    let client: Arc<dyn metalbond::client::Client> = Arc::new(DefaultNetworkClient::new().await?);

    #[cfg(any(feature = "netlink-support", target_os = "linux"))]
    let client: Arc<dyn metalbond::client::Client> = Arc::new({
        // Create a basic NetlinkClientConfig with default values
        let netlink_config = metalbond::NetlinkClientConfig {
            vni_table_map: std::collections::HashMap::new(),
            link_name: "lo".to_string(), // Default to loopback interface
            ipv4_only: false,
        };
        DefaultNetworkClient::new(netlink_config).await?
    });

    // Create and start the MetalBond instance
    let mut metalbond = MetalBond::new(config, client, true);

    // Start HTTP server if configured
    if let Some(http_addr) = cli.http {
        // Clone the RouteTable directly and wrap in Arc
        let route_table = Arc::new(metalbond.route_table_view.clone());
        tokio::spawn(async move {
            if let Err(e) = metalbond::http_server::run_http_server(http_addr, route_table).await {
                tracing::error!("HTTP server failed: {}", e);
            }
        });
    }

    // Start core tasks
    metalbond.start();

    // Start the main server listener
    metalbond.start_server(cli.listen).await?;

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutdown signal received.");
    metalbond.shutdown().await;

    Ok(())
}
