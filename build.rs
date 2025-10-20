// Build script to generate Rust code from protobuf definitions

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .compile(
            &["proto/liquidity.proto"],
            &["proto"],
        )?;
    Ok(())
}
