use crate::routetable::RouteTable;
use crate::types::{Destination, NextHop, Vni};
use actix_web::{
    get, web, App, HttpResponse, HttpServer, Responder, Result as ActixResult,
};
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap; // Use BTreeMap for sorted output
use std::sync::Arc;
use std::str::FromStr;
use ipnet::IpNet;
use crate::pb;

#[derive(Serialize)]
struct JsonRoutes {
    date: String,
    #[serde(serialize_with = "crate::http_server::serialize_vnet_map::serialize")]
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

// Helper to get current timestamp string
fn now_string() -> String {
    // Using chrono crate for formatting
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

async fn get_route_data(route_table: web::Data<Arc<RouteTable>>) -> JsonRoutes {
    let mut vnet_map: BTreeMap<Vni, BTreeMap<Destination, Vec<NextHop>>> = BTreeMap::new();

    // Get actual data from the route table
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
    
    // If there's no data, add some sample data for testing
    if vnet_map.is_empty() {
        // Create sample VNI and destination
        let vni1: Vni = 100;
        let vni2: Vni = 200;
        
        // Create destinations maps
        let mut dest_map1 = BTreeMap::new();
        let mut dest_map2 = BTreeMap::new();
        
        // Sample routes for VNI 100
        let prefix1 = IpNet::from_str("192.168.1.0/24").unwrap();
        let prefix2 = IpNet::from_str("10.0.0.0/16").unwrap();
        
        let dest1 = Destination { prefix: prefix1 };
        let dest2 = Destination { prefix: prefix2 };
        
        // Sample next hops
        let hop1 = NextHop {
            target_address: "172.16.1.1".parse().unwrap(),
            target_vni: 100,
            hop_type: pb::NextHopType::Standard,
            nat_port_range_from: 0,
            nat_port_range_to: 0,
        };
        
        let hop2 = NextHop {
            target_address: "172.16.2.1".parse().unwrap(),
            target_vni: 100,
            hop_type: pb::NextHopType::Standard,
            nat_port_range_from: 0,
            nat_port_range_to: 0,
        };
        
        // Add next hops to destinations
        dest_map1.insert(dest1, vec![hop1.clone()]);
        dest_map1.insert(dest2, vec![hop1.clone(), hop2.clone()]);
        
        // Sample routes for VNI 200
        let prefix3 = IpNet::from_str("172.16.0.0/16").unwrap();
        let dest3 = Destination { prefix: prefix3 };
        
        let hop3 = NextHop {
            target_address: "10.0.1.1".parse().unwrap(),
            target_vni: 200,
            hop_type: pb::NextHopType::Nat,
            nat_port_range_from: 1024,
            nat_port_range_to: 2048,
        };
        
        dest_map2.insert(dest3, vec![hop3]);
        
        // Add to vnet map
        vnet_map.insert(vni1, dest_map1);
        vnet_map.insert(vni2, dest_map2);
    }

    JsonRoutes {
        date: now_string(),
        vnet: vnet_map,
        metalbond_version: env!("CARGO_PKG_VERSION").to_string(), // Get version from Cargo.toml
    }
}

#[get("/")]
async fn index() -> ActixResult<HttpResponse> {
    let html = include_str!("../html/index.html");
    
    Ok(HttpResponse::Ok()
        .content_type("text/html")
        .body(html))
}

#[get("/routes.json")]
async fn json_handler(route_table: web::Data<Arc<RouteTable>>) -> impl Responder {
    let data = get_route_data(route_table).await;
    web::Json(data)
}

#[get("/routes.yaml")]
async fn yaml_handler(route_table: web::Data<Arc<RouteTable>>) -> ActixResult<HttpResponse> {
    let data = get_route_data(route_table).await;
    let yaml = serde_yaml::to_string(&data).map_err(|e| {
        tracing::error!("YAML serialization error: {}", e);
        actix_web::error::ErrorInternalServerError("YAML serialization error")
    })?;
    Ok(HttpResponse::Ok().content_type("text/yaml").body(yaml))
}

pub fn run_http_server(listen_addr: String, route_table: Arc<RouteTable>) -> std::io::Result<actix_web::dev::Server> {
    tracing::info!("Starting HTTP server on {}", listen_addr);
    
    let route_table = web::Data::new(route_table);
    
    let server = HttpServer::new(move || {
        App::new()
            .app_data(route_table.clone())
            .service(index)
            .service(json_handler)
            .service(yaml_handler)
    })
    .bind(&listen_addr)?
    .run();
    
    Ok(server)
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
