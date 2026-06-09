//! Generate the Guardian OpenAPI specifications (issue #241).
//!
//! Writes four JSON specs into the target directory (default `docs/`):
//!   - `openapi.json`            — combined client + dashboard (+ evm)
//!   - `openapi-client.json`     — client API only
//!   - `openapi-dashboard.json`  — dashboard API only
//!   - `openapi-evm.json`        — EVM API only (requires `--features evm`)
//!
//! Build with `--features evm` so the combined and EVM specs are complete:
//!
//! ```sh
//! cargo run --features evm --bin gen-openapi -- docs
//! ```
//!
//! Pass `--check <dir>` to verify the committed specs are up to date
//! (used by CI); it exits non-zero and lists stale files instead of
//! writing.
use std::path::Path;
use std::process::ExitCode;

/// `(file name, pretty-printed JSON)` for every spec this build produces.
fn specs() -> Vec<(&'static str, String)> {
    let json = |doc: utoipa::openapi::OpenApi| {
        doc.to_pretty_json()
            .expect("OpenAPI spec must serialize to JSON")
    };
    // `mut` is only used under the `evm` feature; without it the vec is final.
    #[cfg_attr(not(feature = "evm"), allow(unused_mut))]
    let mut out = vec![
        ("openapi.json", json(server::openapi::openapi())),
        (
            "openapi-client.json",
            json(server::openapi::client_openapi()),
        ),
        (
            "openapi-dashboard.json",
            json(server::openapi::dashboard_openapi()),
        ),
    ];
    #[cfg(feature = "evm")]
    out.push(("openapi-evm.json", json(server::openapi::evm_openapi())));
    out
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (check, dir) = match args.as_slice() {
        [flag, dir] if flag == "--check" => (true, dir.clone()),
        [dir] => (false, dir.clone()),
        [] => (false, "docs".to_string()),
        _ => {
            eprintln!("usage: gen-openapi [--check] [DIR]");
            return ExitCode::from(2);
        }
    };

    if !cfg!(feature = "evm") {
        eprintln!(
            "warning: built without the `evm` feature; the combined and EVM specs will be \
             incomplete. Run with `--features evm` to generate the committed files."
        );
    }

    let dir = Path::new(&dir);
    let mut stale = Vec::new();

    for (name, json) in specs() {
        let path = dir.join(name);
        // Match the trailing newline `writeln!` adds below.
        let expected = format!("{json}\n");
        if check {
            match std::fs::read_to_string(&path) {
                Ok(found) if found == expected => {}
                _ => stale.push(path.display().to_string()),
            }
        } else if let Err(e) = std::fs::write(&path, &expected) {
            eprintln!("failed to write {}: {e}", path.display());
            return ExitCode::FAILURE;
        } else {
            eprintln!("wrote {}", path.display());
        }
    }

    if check && !stale.is_empty() {
        eprintln!("OpenAPI specs are stale; regenerate with:");
        eprintln!(
            "  cargo run --features evm --bin gen-openapi -- {}",
            dir.display()
        );
        for f in &stale {
            eprintln!("  - {f}");
        }
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
