use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .file_descriptor_set_path("proto/guardian_descriptor.bin")
        .compile_protos(&["proto/guardian.proto"], &["proto"])?;

    let git_sha = std::env::var("GUARDIAN_GIT_SHA")
        .ok()
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GUARDIAN_GIT_SHA={git_sha}");
    println!("cargo:rerun-if-env-changed=GUARDIAN_GIT_SHA");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");

    Ok(())
}
