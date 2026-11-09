use guardian_prod_benchmarks::config::{Credential, ReadMode, RoleConfig};
use guardian_prod_benchmarks::workload::{
    Role, reader_fetches_state, role_for_user, watched_writer,
};

fn roles(readers: u32, writers: u32) -> RoleConfig {
    RoleConfig {
        readers,
        writers,
        reader_poll_interval_ms: 2_000,
        read_mode: ReadMode::NonceThenStateOnChange,
        writer_push_interval_ms: 10_000,
        cosigners_per_writer: 2,
    }
}

#[test]
fn readers_watch_writer_accounts_up_to_the_cosigner_limit() {
    let roles = roles(7, 2);
    let writers = [0, 1];
    let watched: Vec<Option<u32>> = (0..6)
        .map(|rank| watched_writer(&roles, &writers, rank))
        .collect();
    assert_eq!(watched, [Some(0), Some(1), Some(0), Some(1), None, None]);
    assert_eq!(
        watched_writer(&roles, &[], 0),
        None,
        "no writer in the shard"
    );
}

#[test]
fn writers_come_first_then_readers_then_idle_accounts() {
    let roles = roles(3, 2);
    let assigned: Vec<Role> = (0..7).map(|user| role_for_user(&roles, user)).collect();
    assert_eq!(
        assigned,
        [
            Role::Writer,
            Role::Writer,
            Role::Reader,
            Role::Reader,
            Role::Reader,
            Role::Idle,
            Role::Idle,
        ]
    );
}

#[test]
fn nonce_poll_fetches_state_only_on_first_poll_or_change() {
    let mode = ReadMode::NonceThenStateOnChange;
    assert!(reader_fetches_state(mode, None, 4));
    assert!(!reader_fetches_state(mode, Some(4), 4));
    assert!(reader_fetches_state(mode, Some(4), 5));
}

#[test]
fn full_state_mode_fetches_every_poll() {
    let mode = ReadMode::FullState;
    assert!(reader_fetches_state(mode, Some(4), 4));
    assert!(reader_fetches_state(mode, None, 0));
}

#[test]
fn target_profiles_load_with_roles() {
    for (name, mode, credential) in [
        (
            "target-317-mix.toml",
            ReadMode::NonceThenStateOnChange,
            "session",
        ),
        (
            "target-317-mix-wallet.toml",
            ReadMode::NonceThenStateOnChange,
            "wallet",
        ),
        ("target-317-full-state.toml", ReadMode::FullState, "session"),
    ] {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("profiles")
            .join(name);
        let config = guardian_prod_benchmarks::config::RunConfig::load_from_path(&path)
            .expect("profile loads");
        assert_eq!(config.credential.as_str(), credential, "{name}");
        if let Credential::Session { network } = &config.credential {
            assert_eq!(network, "testnet");
        }
        let roles = config.roles.expect("roles configured");
        assert_eq!((roles.readers, roles.writers), (20_000, 100));
        assert_eq!(roles.read_mode, mode);
        assert_eq!(roles.cosigners_per_writer, 3);
    }
}
