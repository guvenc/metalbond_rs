fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/metalbond.proto");

    let mut config = prost_build::Config::new();
    // Enable serde support for all generated types
    config.type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]");

    config.compile_protos(&["proto/metalbond.proto"], &["proto/"])?;
    Ok(())
}
