fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .compile_protos(&["proto/guardian_module_v1.proto"], &["proto"])?;
    Ok(())
}
