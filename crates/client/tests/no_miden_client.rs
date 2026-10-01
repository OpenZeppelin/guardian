//! Requesting and observing Guardian execution must need no Miden capability: no node client,
//! no transaction building, no proving. The base client may name protocol types, but neither it
//! nor the shared crate it builds on may depend on the crates that would bring those in.

const FORBIDDEN: [&str; 4] = [
    "miden-client",
    "miden-tx",
    "miden-multisig-client",
    "miden-rpc-client",
];

/// The crate names a manifest's `[dependencies]` table declares.
fn dependencies(manifest: &str) -> Vec<String> {
    manifest
        .split("\n[")
        .find(|section| section.starts_with("dependencies]"))
        .expect("the manifest has a [dependencies] table")
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim().to_string()))
        .filter(|name| !name.is_empty() && !name.starts_with('#'))
        .collect()
}

#[test]
fn neither_the_base_client_nor_the_shared_crate_depends_on_a_miden_client() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for manifest in [root.join("Cargo.toml"), root.join("../shared/Cargo.toml")] {
        let text = std::fs::read_to_string(&manifest).expect("the manifest reads");
        let declared = dependencies(&text);
        assert!(
            declared.iter().any(|name| name == "miden-protocol"),
            "{}: the parse found no dependencies, so it proves nothing: {declared:?}",
            manifest.display()
        );
        for forbidden in FORBIDDEN {
            assert!(
                !declared.iter().any(|name| name == forbidden),
                "{} depends on {forbidden}",
                manifest.display()
            );
        }
    }
}
