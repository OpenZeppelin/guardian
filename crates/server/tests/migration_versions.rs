//! Diesel takes a migration's version from its directory name up to the first `_`, and runs only
//! one migration per version, so two directories sharing a version silently skip one of them.

#[test]
fn every_migration_has_its_own_version() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut versions = std::collections::BTreeMap::<String, Vec<String>>::new();
    for entry in std::fs::read_dir(&dir).expect("migrations directory") {
        let name = entry
            .expect("migration entry")
            .file_name()
            .into_string()
            .expect("utf-8 name");
        if let Some((version, _)) = name.split_once('_') {
            versions.entry(version.to_string()).or_default().push(name);
        }
    }
    let shared: Vec<_> = versions.values().filter(|names| names.len() > 1).collect();
    assert!(
        shared.is_empty(),
        "migrations sharing a version: {shared:?}"
    );
}
