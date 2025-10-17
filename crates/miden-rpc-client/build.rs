fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Compile all miden-node proto files
    // We use proto_path to make all generated code use crate::proto:: prefix
    tonic_build::configure()
        .build_server(false) // We only need the client
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(
            &["proto/rpc.proto"],
            &["proto"],
        )?;
    Ok(())
}
