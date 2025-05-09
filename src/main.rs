mod client;
mod http_server;
mod metalbond;
mod netlink;
mod peer;
mod routetable;
mod types;
mod pb; // Include the protobuf module

use crate::client::{Client, DummyClient};
use crate::metalbond::MetalBond;
use crate::netlink::{NetlinkClient, NetlinkClientConfig};
use crate::types::{Config, Destination, NextHop, Vni};
use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run MetalBond Server
    Server {
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
    },
    /// Run MetalBond Client
    Client {
        /// Server address(es). You may define multiple servers.
        #[arg(long, required = true)]
        server: Vec<String>,
        /// Subscribe to VNIs
        #[arg(long)]
        subscribe: Vec<Vni>,
        /// Announce Prefixes in VNIs (e.g. 23#10.0.23.0/24#2001:db8::1#[STD|LB|NAT]#[FROM#TO]
        #[arg(long)]
        announce: Vec<String>,
        /// Enable debug logging
        #[arg(short, long, default_value_t = false)]
        verbose: bool,
        /// Install routes via netlink. VNI to route table mapping (e.g. 23#100 installs routes of VNI 23 to route table 100)
        #[arg(long)]
        install_routes: Vec<String>,
        /// ip6tnl tun device name (required if --install-routes is used)
        #[arg(long, required_if_eq("install_routes", ""))] // This isn't quite right, needs validator if install_routes is non-empty
        tun: Option<String>,
        /// Receive only IPv4 routes
        #[arg(long, name = "ipv4-only", default_value_t = false)]
        ipv4_only: bool,
        /// Keepalive Interval in seconds
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        keepalive: Option<u64>,
        /// HTTP Server listen address. e.g. :4712
        #[arg(long)]
        http: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Setup Logging
    let log_level = if match &cli.command {
        Commands::Server { verbose, .. } => *verbose,
        Commands::Client { verbose, .. } => *verbose,
    } {
        "debug"
    } else {
        "info"
    };
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(format!("metalbond_rs={}", log_level)))?;
    let subscriber = FmtSubscriber::builder().with_env_filter(filter).finish();
    tracing::subscriber::set_global_default(subscriber)?;

    tracing::info!("MetalBond_rs Version: {}", env!("CARGO_PKG_VERSION"));

    // ---- Configuration ----
    let mut config = Config::default();
    let http_listen_addr: Option<String>;

    match &cli.command {
        Commands::Server {
            keepalive, http, ..
        } => {
            if let Some(secs) = keepalive {
                config.keepalive_interval = Duration::from_secs(*secs);
            }
            http_listen_addr = http.clone();
        }
        Commands::Client {
            keepalive, http, ..
        } => {
            if let Some(secs) = keepalive {
                config.keepalive_interval = Duration::from_secs(*secs);
            }
            http_listen_addr = http.clone();
        }
    }

    // ---- Client/Server Specific Setup ---
    match cli.command {
        Commands::Server { listen, .. } => {
            let client: Arc<dyn Client> = Arc::new(DummyClient::new());
            let mut metalbond = MetalBond::new(config, client, true);

            // Start HTTP server if configured
            if let Some(http_addr) = http_listen_addr {
                let state_clone = Arc::clone(&metalbond.state);
                tokio::spawn(async move {
                    if let Err(e) = http_server::run_http_server(http_addr, state_clone).await {
                        tracing::error!("HTTP server failed: {}", e);
                    }
                });
            }

            // Start core tasks (distributor, etc.)
            metalbond.start();

            // Start the main server listener
            metalbond.start_server(listen).await?;

            // Wait for shutdown signal
            signal::ctrl_c().await?;
            tracing::info!("Shutdown signal received.");
            metalbond.shutdown().await;
        }
        Commands::Client {
            server,
            subscribe,
            announce,
            install_routes,
            tun,
            ipv4_only,
            ..
        } => {
            // Setup OS Client (Netlink or Dummy)
            let os_client: Arc<dyn Client> = if !install_routes.is_empty() {
                let tun_name = tun.ok_or_else(|| {
                    anyhow!("--tun <DEVICE> is required when using --install-routes")
                })?;

                let mut vni_table_map = HashMap::new();
                for mapping in install_routes {
                    let parts: Vec<&str> = mapping.split('#').collect();
                    if parts.len() != 2 {
                        return Err(anyhow!("Malformed VNI Table mapping: {}", mapping));
                    }
                    let vni = Vni::from_str(parts[0])
                        .with_context(|| format!("Cannot parse VNI: {}", parts[0]))?;
                    let table_id = i32::from_str(parts[1])
                        .with_context(|| format!("Cannot parse table ID: {}", parts[1]))?;
                    if table_id <= 0 { // Basic validation, kernel table 0 often invalid/special
                        return Err(anyhow!("Invalid table ID {} in mapping {}", table_id, mapping));
                    }
                    vni_table_map.insert(vni, table_id);
                }
                tracing::info!("VNI to Route Table mapping: {:?}", vni_table_map);

                let nl_config = NetlinkClientConfig {
                    vni_table_map,
                    link_name: tun_name,
                    ipv4_only,
                };
                Arc::new(
                    NetlinkClient::new(nl_config)
                        .await
                        .context("Cannot create Netlink Client")?,
                )
            } else {
                Arc::new(DummyClient::new())
            };

            // Create MetalBond instance
            let mut metalbond = MetalBond::new(config, os_client, false);

            // Start HTTP server if configured
            if let Some(http_addr) = http_listen_addr {
                let state_clone = Arc::clone(&metalbond.state);
                tokio::spawn(async move {
                    if let Err(e) = http_server::run_http_server(http_addr, state_clone).await {
                        tracing::error!("HTTP server failed: {}", e);
                    }
                });
            }


            // Start core tasks (distributor, connection manager)
            metalbond.start();


            // Add server peers for connection manager to handle
            for server_addr in server {
                if let Err(e) = metalbond.add_peer(&server_addr).await {
                    tracing::error!("Failed to add peer {}: {}", server_addr, e);
                    // Decide if fatal or continue
                    // return Err(e); // Make it fatal for now
                }
            }

            // TODO: Wait for connections to establish? The Go code waits.
            tracing::info!("Waiting a few seconds for initial connections...");
            tokio::time::sleep(Duration::from_secs(5)).await; // Simple wait

            // Process initial subscriptions
            for vni in subscribe {
                metalbond.subscribe(vni).await?;
            }

            // Process initial announcements
            for ann_str in announce {
                // Format: vni#prefix#destHop[#routeType][#fromPort#toPort]
                let parts: Vec<&str> = ann_str.split('#').collect();
                if !(3..=6).contains(&parts.len()) {
                    return Err(anyhow!("Malformed announcement: '{}', expected format vni#prefix#destHop[#type][#from#to]", ann_str));
                }

                let vni = Vni::from_str(parts[0]).context("Invalid VNI")?;
                let dest = Destination::from_str(parts[1]).context("Invalid prefix")?;
                let hop_ip = IpAddr::from_str(parts[2]).context("Invalid nexthop address")?;

                let mut hop_type = pb::NextHopType::Standard;
                let mut from_port = 0u16;
                let mut to_port = 0u16;

                if parts.len() > 3 {
                    hop_type = pb::next_hop_type_from_str(parts[3]);
                }

                if hop_type == pb::NextHopType::Nat {
                    if parts.len() != 6 {
                        return Err(anyhow!("Malformed NAT announcement: '{}', expected vni#prefix#destHop#NAT#from#to", ann_str));
                    }
                    from_port = u16::from_str(parts[4]).context("Invalid NAT from port")?;
                    to_port = u16::from_str(parts[5]).context("Invalid NAT to port")?;
                } else if parts.len() > 4 {
                    // Non-NAT type should not have port range
                    return Err(anyhow!("Unexpected port range for non-NAT announcement: '{}'", ann_str));
                }


                let nh = NextHop {
                    target_address: hop_ip,
                    target_vni: 0, // TargetVNI seems unused in announcements? Go code sets it to 0.
                    hop_type,
                    nat_port_range_from: from_port,
                    nat_port_range_to: to_port,
                };

                metalbond.announce_route(vni, dest, nh).await?;
            }

            // Wait for shutdown signal
            signal::ctrl_c().await?;
            tracing::info!("Shutdown signal received.");
            metalbond.shutdown().await;
        }
    }

    Ok(())
}