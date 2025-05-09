// Include the `metalbond` module, which is generated from metalbond.proto.
pub mod metalbond {
    #![allow(clippy::all)] // Disable clippy warnings for generated code
    include!(concat!(env!("OUT_DIR"), "/metalbond.rs"));

    // Helper to convert string to NextHopType enum
    pub fn next_hop_type_from_str(s: &str) -> NextHopType {
        match s.to_lowercase().as_str() {
            "std" | "standard" => NextHopType::Standard,
            "lb" | "loadbalancer" | "loadbalancer_target" => NextHopType::LoadbalancerTarget,
            "nat" => NextHopType::Nat,
            _ => NextHopType::Standard, // Default
        }
    }
}

// Re-export for easier use
pub use metalbond::*;