fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .file_descriptor_set_path("proto/state_manager_descriptor.bin")
        .compile_protos(&["proto/state_manager.proto"], &["proto"])?;

    Ok(())
}
