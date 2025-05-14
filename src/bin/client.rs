use anyhow::{anyhow, Context, Result};
use clap::Parser;
use metalbond::pb;
use metalbond::{Config, Destination, MetalBond, NextHop, Vni};

// Import NetlinkClient and NetlinkClientConfig - these are always available on Linux,
// or when the netlink-support feature is explicitly enabled
#[cfg(any(feature = "netlink-support", target_os = "linux"))]
use metalbond::{NetlinkClient, NetlinkClientConfig};

// Import DummyClient only when netlink support is not active
#[cfg(not(any(feature = "netlink-support", target_os = "linux")))]
use metalbond::client::DummyClient;

// Import HashMap conditionally since it's only used with NetlinkClient
#[cfg(any(feature = "netlink-support", target_os = "linux"))]
use std::collections::HashMap;

use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct ClientCli {
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
    #[arg(long, required_if_eq("install_routes", ""))]
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = ClientCli::parse();

    // Setup Logging
    let log_level = if cli.verbose { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(format!("metalbond={}", log_level)))?;
    let subscriber = FmtSubscriber::builder().with_env_filter(filter).finish();
    tracing::subscriber::set_global_default(subscriber)?;

    tracing::info!("MetalBond Client Version: {}", env!("CARGO_PKG_VERSION"));
    
    #[cfg(target_os = "linux")]
    tracing::info!("Running on Linux with netlink support enabled");
    
    #[cfg(all(not(target_os = "linux"), feature = "netlink-support"))]
    tracing::info!("Running with netlink support enabled");
    
    #[cfg(not(any(feature = "netlink-support", target_os = "linux")))]
    tracing::info!("Running without netlink support (using DummyClient)");

    // ---- Configuration ----
    let mut config = Config::default();
    if let Some(secs) = cli.keepalive {
        config.keepalive_interval = Duration::from_secs(secs);
    }

    // Setup OS Client based on platform and command line options
    // Use NetlinkClient when on Linux or when netlink-support feature is enabled
    let os_client: Arc<dyn metalbond::client::Client> = if !cli.install_routes.is_empty() {
        #[cfg(any(feature = "netlink-support", target_os = "linux"))]
        {
            let tun_name = cli
                .tun
                .ok_or_else(|| anyhow!("--tun <DEVICE> is required when using --install-routes"))?;

            let mut vni_table_map = HashMap::new();
            for mapping in cli.install_routes {
                let parts: Vec<&str> = mapping.split('#').collect();
                if parts.len() != 2 {
                    return Err(anyhow!("Malformed VNI Table mapping: {}", mapping));
                }
                let vni = Vni::from_str(parts[0])
                    .with_context(|| format!("Cannot parse VNI: {}", parts[0]))?;
                let table_id = i32::from_str(parts[1])
                    .with_context(|| format!("Cannot parse table ID: {}", parts[1]))?;
                if table_id <= 0 {
                    return Err(anyhow!(
                        "Invalid table ID {} in mapping {}",
                        table_id,
                        mapping
                    ));
                }
                vni_table_map.insert(vni, table_id);
            }
            tracing::info!("VNI to Route Table mapping: {:?}", vni_table_map);

            let nl_config = NetlinkClientConfig {
                vni_table_map,
                link_name: tun_name,
                ipv4_only: cli.ipv4_only,
            };
            Arc::new(
                NetlinkClient::new(nl_config)
                    .await
                    .context("Cannot create Netlink Client")?,
            )
        }

        #[cfg(not(any(feature = "netlink-support", target_os = "linux")))]
        {
            tracing::warn!("Install routes specified but netlink support is not available! Using DummyClient instead.");
            Arc::new(
                DummyClient::new()
                    .await
                    .context("Cannot create Dummy Client")?
            )
        }
    } else {
        // If no routes need to be installed, use NetlinkClient when on Linux
        // or when netlink-support feature is enabled
        #[cfg(any(feature = "netlink-support", target_os = "linux"))]
        {
            tracing::info!("Using NetlinkClient");
            let nl_config = NetlinkClientConfig {
                vni_table_map: HashMap::new(),
                link_name: cli.tun.unwrap_or_else(|| "metalbond0".to_string()),
                ipv4_only: cli.ipv4_only,
            };
            Arc::new(
                NetlinkClient::new(nl_config)
                    .await
                    .context("Cannot create Netlink Client")?,
            )
        }

        #[cfg(not(any(feature = "netlink-support", target_os = "linux")))]
        {
            tracing::info!("Using DummyClient (netlink support not available)");
            Arc::new(
                DummyClient::new()
                    .await
                    .context("Cannot create Dummy Client")?
            )
        }
    };

    // Create MetalBond instance
    let mut metalbond = MetalBond::new(config, os_client, false);

    // Start HTTP server if configured
    if let Some(http_addr) = cli.http {
        // Clone the RouteTable directly and wrap in Arc
        let route_table = Arc::new(metalbond.route_table_view.clone());
        tokio::spawn(async move {
            match metalbond::http_server::run_http_server(http_addr, route_table) {
                Ok(server) => {
                    // Start the server and await its completion
                    if let Err(e) = server.await {
                        tracing::error!("HTTP server failed: {}", e);
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to start HTTP server: {}", e);
                }
            }
        });
    }

    // Start core tasks
    metalbond.start();

    // Add server peers for connection manager to handle
    for server_addr in cli.server {
        if let Err(e) = metalbond.add_peer(&server_addr).await {
            tracing::error!("Failed to add peer {}: {}", server_addr, e);
        }
    }

    // Wait for connections to establish
    tracing::info!("Waiting a few seconds for initial connections...");
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Process initial subscriptions
    for vni in cli.subscribe {
        metalbond.subscribe(vni).await?;
    }

    // Process initial announcements
    for ann_str in cli.announce {
        // Format: vni#prefix#destHop[#routeType][#fromPort#toPort]
        let parts: Vec<&str> = ann_str.split('#').collect();
        if !(3..=6).contains(&parts.len()) {
            return Err(anyhow!(
                "Malformed announcement: '{}', expected format vni#prefix#destHop[#type][#from#to]",
                ann_str
            ));
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
                return Err(anyhow!(
                    "Malformed NAT announcement: '{}', expected vni#prefix#destHop#NAT#from#to",
                    ann_str
                ));
            }
            from_port = u16::from_str(parts[4]).context("Invalid NAT from port")?;
            to_port = u16::from_str(parts[5]).context("Invalid NAT to port")?;
        } else if parts.len() > 4 {
            // Non-NAT type should not have port range
            return Err(anyhow!(
                "Unexpected port range for non-NAT announcement: '{}'",
                ann_str
            ));
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
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutdown signal received.");
    metalbond.shutdown().await;

    Ok(())
}
