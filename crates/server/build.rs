fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Compile state manager proto
    tonic_build::configure()
        .file_descriptor_set_path("proto/state_manager_descriptor.bin")
        .compile_protos(&["proto/state_manager.proto"], &["proto"])?;

    // Compile miden RPC proto
    tonic_build::configure()
        .file_descriptor_set_path("proto/miden_rpc_descriptor.bin")
        .compile_protos(&["proto/miden_rpc.proto"], &["proto"])?;

    Ok(())
}
