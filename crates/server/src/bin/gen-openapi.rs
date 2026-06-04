//! Generate the Guardian OpenAPI specification (issue #241).
//!
//! Writes the JSON spec produced by [`server::openapi::openapi`] to the
//! path given as the first argument, or to stdout when no path is
//! given. Build with `--features evm` to include the EVM routes:
//!
//! ```sh
//! cargo run --features evm --bin gen-openapi -- docs/openapi.json
//! ```
use std::io::Write;

fn main() -> std::io::Result<()> {
    let spec = server::openapi::openapi();
    let json = spec
        .to_pretty_json()
        .expect("OpenAPI spec must serialize to JSON");

    match std::env::args().nth(1) {
        Some(path) => {
            let mut file = std::fs::File::create(&path)?;
            // Trailing newline keeps the committed file POSIX-clean.
            writeln!(file, "{json}")?;
            eprintln!("Wrote OpenAPI spec to {path}");
        }
        None => {
            println!("{json}");
        }
    }
    Ok(())
}
