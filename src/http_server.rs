use crate::routetable::RouteTable;
use crate::types::{Destination, NextHop, Vni};
use anyhow::Result;
use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use std::collections::BTreeMap; // Use BTreeMap for sorted output
                                // Removed unused: use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

#[derive(Serialize)]
struct JsonRoutes {
    date: String,
    #[serde(serialize_with = "crate::http_server::serialize_vnet_map::serialize")]
    // Corrected path to function
    vnet: BTreeMap<Vni, BTreeMap<Destination, Vec<NextHop>>>, // Use BTreeMap for sorting
    metalbond_version: String,
}

// Custom serializer because Destination uses IpNet which doesn't directly serialize well as map keys
mod serialize_vnet_map {
    use super::*;
    use serde::ser::{SerializeMap, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S>(
        map: &BTreeMap<Vni, BTreeMap<Destination, Vec<NextHop>>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map_ser = serializer.serialize_map(Some(map.len()))?;
        for (k, v) in map {
            map_ser.serialize_entry(&k.to_string(), &SerializeDestMap(v))?;
        }
        map_ser.end()
    }

    struct SerializeDestMap<'a>(&'a BTreeMap<Destination, Vec<NextHop>>);

    impl Serialize for SerializeDestMap<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut map_ser = serializer.serialize_map(Some(self.0.len()))?;
            for (k, v) in self.0 {
                map_ser.serialize_entry(&k.to_string(), v)?;
            }
            map_ser.end()
        }
    }
}

#[derive(Template)]
#[template(path = "index.html", escape = "html")]
struct IndexTemplate {
    date: String,
    vnet: BTreeMap<Vni, BTreeMap<Destination, Vec<NextHop>>>, // Use BTreeMap for sorting
    metalbond_version: String,
}

// Helper to get current timestamp string
fn now_string() -> String {
    // Using chrono crate for formatting
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

async fn get_route_data(State(route_table): State<Arc<RouteTable>>) -> JsonRoutes {
    let mut vnet_map: BTreeMap<Vni, BTreeMap<Destination, Vec<NextHop>>> = BTreeMap::new();

    for vni in route_table.get_vnis().await {
        let dest_map = route_table.get_destinations_by_vni(vni).await;
        // Convert HashMap to BTreeMap for sorting destinations
        let mut sorted_dest_map = BTreeMap::new();
        for (dest, mut hops) in dest_map {
            // Sort hops consistently by string representation
            hops.sort_by_key(|a| a.to_string());
            sorted_dest_map.insert(dest, hops); // Destination needs Ord - IpNet doesn't have it, use wrapper or string key
        }

        if !sorted_dest_map.is_empty() {
            vnet_map.insert(vni, sorted_dest_map);
        }
    }

    JsonRoutes {
        date: now_string(),
        vnet: vnet_map,
        metalbond_version: env!("CARGO_PKG_VERSION").to_string(), // Get version from Cargo.toml
    }
}

async fn main_handler(state: State<Arc<RouteTable>>) -> Result<Html<String>, AppError> {
    let data = get_route_data(state).await;
    let template = IndexTemplate {
        date: data.date,
        vnet: data.vnet,
        metalbond_version: data.metalbond_version,
    };
    let html = template.render()?;
    Ok(Html(html))
}

async fn json_handler(state: State<Arc<RouteTable>>) -> Result<Json<JsonRoutes>, AppError> {
    let data = get_route_data(state).await;
    Ok(Json(data))
}

async fn yaml_handler(state: State<Arc<RouteTable>>) -> Result<Yaml<JsonRoutes>, AppError> {
    let data = get_route_data(state).await;
    Ok(Yaml(data))
}

// Custom response type for YAML
struct Yaml<T>(T);

impl<T> IntoResponse for Yaml<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        match serde_yaml::to_string(&self.0) {
            Ok(yaml) => (StatusCode::OK, [("content-type", "text/yaml")], yaml).into_response(),
            Err(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to serialize YAML: {}", err),
            )
                .into_response(),
        }
    }
}

// Centralized error handling for Axum handlers
struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!("HTTP handler error: {:?}", self.0);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Internal server error: {}", self.0),
        )
            .into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

pub async fn run_http_server(listen_addr: String, route_table: Arc<RouteTable>) -> Result<()> {
    let app = Router::new()
        .route("/", get(main_handler))
        .route("/routes.json", get(json_handler))
        .route("/routes.yaml", get(yaml_handler))
        .with_state(route_table);

    let listener = TcpListener::bind(&listen_addr).await?;
    tracing::info!("HTTP server listening on {}", listen_addr);
    axum::serve(listener, app).await?;
    Ok(())
}

// ---- Askama Ord/PartialOrd implementations for map keys ----
// Needed because IpNet doesn't implement Ord, but BTreeMap requires it.
// We compare based on the string representation.

impl PartialOrd for Destination {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Destination {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.prefix
            .addr()
            .cmp(&other.prefix.addr())
            .then_with(|| self.prefix.prefix_len().cmp(&other.prefix.prefix_len()))
    }
}

impl PartialOrd for NextHop {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NextHop {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Define a consistent ordering based on fields
        self.target_address
            .cmp(&other.target_address)
            .then_with(|| self.target_vni.cmp(&other.target_vni))
            .then_with(|| self.hop_type.cmp(&other.hop_type)) // Assuming generated enum has Ord
            .then_with(|| self.nat_port_range_from.cmp(&other.nat_port_range_from))
            .then_with(|| self.nat_port_range_to.cmp(&other.nat_port_range_to))
    }
}
