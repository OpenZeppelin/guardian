// build.rs

use std::{env, error::Error, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=masm/");
    println!("cargo::rerun-if-env-changed=BUILD_GENERATED_FILES_IN_SRC");

    let crate_dir = env::var("CARGO_MANIFEST_DIR")?;
    let masm_dir = Path::new(&crate_dir).join("masm");

    println!("cargo::rustc-env=OZ_MASM_DIR={}", masm_dir.display());

    Ok(())
}
