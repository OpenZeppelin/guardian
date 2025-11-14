fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure().build_server(true).compile_protos(
        &["../server/proto/state_manager.proto"],
        &["../server/proto"],
    )?;
    Ok(())
}
