fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .file_descriptor_set_path("proto/guardian_descriptor.bin")
        .compile_protos(&["proto/guardian.proto"], &["proto"])?;

    Ok(())
}
