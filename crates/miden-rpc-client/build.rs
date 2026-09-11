use std::fs;
use std::path::{Path, PathBuf};

use miden_node_proto_build::rpc_api_descriptor;

const RPC_SUBDIR: &str = "rpc";
const WRAPPER_NAME: &str = "rpc_generated.rs";

fn main() {
    println!("cargo::rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let rpc_out = out_dir.join(RPC_SUBDIR);
    fs::create_dir_all(&rpc_out).expect("failed to create proto codegen directory");

    tonic_prost_build::configure()
        .build_server(true)
        .out_dir(&rpc_out)
        .compile_fds_with_config(rpc_api_descriptor(), tonic_prost_build::Config::new())
        .expect("failed to compile Miden node RPC protos");

    generate_wrapper(&out_dir, &rpc_out);
}

/// Produces a wrapper file that exposes each generated `<package>.rs` file as a
/// `pub mod <package>` so the library can `include!` a single stable path.
fn generate_wrapper(out_dir: &Path, rpc_out: &Path) {
    let mut mod_names: Vec<String> = fs::read_dir(rpc_out)
        .expect("failed to read proto codegen directory")
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            name.strip_suffix(".rs").map(str::to_owned)
        })
        .collect();
    mod_names.sort();

    let mut wrapper = String::new();
    for mod_name in &mod_names {
        wrapper.push_str(&format!(
            "#[allow(clippy::doc_markdown, clippy::struct_field_names, clippy::trivially_copy_pass_by_ref, clippy::large_enum_variant)]\n\
             pub mod {mod_name} {{ include!(concat!(env!(\"OUT_DIR\"), \"/{RPC_SUBDIR}/{mod_name}.rs\")); }}\n"
        ));
    }

    fs::write(out_dir.join(WRAPPER_NAME), wrapper).expect("failed to write proto wrapper");
}
