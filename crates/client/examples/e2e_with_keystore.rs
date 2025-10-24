use miden_keystore::{FilesystemKeyStore, KeyStore};
use miden_lib::account::{auth::AuthRpoFalcon512Multisig, wallets::BasicWallet};
use miden_objects::account::{
    AccountBuilder, AccountDelta,
    delta::{AccountStorageDelta, AccountVaultDelta},
};
use miden_objects::crypto::dsa::rpo_falcon512::PublicKey;
use miden_objects::utils::Serializable;
use miden_objects::{Felt, Word};
use private_state_manager_client::{
    Auth, AuthConfig, ClientResult, FalconRpoSigner, MidenFalconRpoAuth, PsmClient,
};
use private_state_manager_shared::ToJson;
use rand_chacha::ChaCha20Rng;
use tempfile::TempDir;

#[tokio::main]
async fn main() -> ClientResult<()> {
    println!("=== PSM E2E Example with Keystore ===\n");

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let keystore = FilesystemKeyStore::<ChaCha20Rng>::new(temp_dir.path().to_path_buf())
        .expect("Failed to create keystore");

    println!("1. Generating keys and creating multisig account...");
    let pub_key_1_word = keystore.generate_key().expect("Failed to generate key 1");
    let pub_key_2_word = keystore.generate_key().expect("Failed to generate key 2");
    let pub_key_3_word = keystore.generate_key().expect("Failed to generate key 3");

    let pub_key_1 = PublicKey::new(pub_key_1_word);
    let pub_key_2 = PublicKey::new(pub_key_2_word);
    let pub_key_3 = PublicKey::new(pub_key_3_word);

    let approvers = vec![pub_key_1, pub_key_2, pub_key_3];
    let threshold = 2u32;

    let multisig_component = AuthRpoFalcon512Multisig::new(threshold, approvers.clone())
        .expect("multisig component creation failed");

    let (account, _) = AccountBuilder::new([0xff; 32])
        .with_auth_component(multisig_component)
        .with_component(BasicWallet)
        .build()
        .expect("account building failed");

    let account_id = account.id();
    let initial_commitment = account.commitment();
    let initial_nonce = account.nonce();

    println!("  Account ID: {}", account_id);
    println!(
        "  Initial Commitment: 0x{}",
        hex::encode(initial_commitment.as_bytes())
    );
    println!("  Threshold: {}/{}", threshold, approvers.len());
    println!();

    println!("2. Preparing PSM client with auth...");
    let secret_key_1 = keystore.get_key(pub_key_1_word).expect("Failed to get key");
    let falcon_signer = FalconRpoSigner::new(secret_key_1);
    let auth = Auth::FalconRpoSigner(falcon_signer);

    let pubkey_1_hex = format!("0x{}", hex::encode(pub_key_1_word.to_bytes()));
    let pubkey_2_hex = format!("0x{}", hex::encode(pub_key_2_word.to_bytes()));
    let pubkey_3_hex = format!("0x{}", hex::encode(pub_key_3_word.to_bytes()));

    println!("  Authorized pubkeys:");
    println!("    1: {}", pubkey_1_hex);
    println!("    2: {}", pubkey_2_hex);
    println!("    3: {}", pubkey_3_hex);
    println!();

    let psm_endpoint = "http://localhost:50051".to_string();

    println!("3. Connecting to PSM at {}...", psm_endpoint);
    let mut client = match PsmClient::connect(psm_endpoint).await {
        Ok(client) => client.with_auth(auth),
        Err(e) => {
            println!("✗ Failed to connect: {}", e);
            println!("\nTo run this example:");
            println!("  1. Start your PSM server");
            println!("  2. Set PSM_ENDPOINT if needed");
            println!("  3. Re-run: cargo run --example e2e_with_keystore\n");
            return Ok(());
        }
    };

    println!("4. Configuring account on PSM...");
    let auth_config = AuthConfig {
        auth_type: Some(
            private_state_manager_client::auth_config::AuthType::MidenFalconRpo(
                MidenFalconRpoAuth {
                    cosigner_pubkeys: vec![
                        pubkey_1_hex.clone(),
                        pubkey_2_hex.clone(),
                        pubkey_3_hex.clone(),
                    ],
                },
            ),
        ),
    };

    let initial_state = account.to_json();

    match client
        .configure(&account_id, auth_config, initial_state, "Filesystem")
        .await
    {
        Ok(response) => println!("  ✓ {}", response.message),
        Err(e) => println!("  ✗ Configuration failed: {}", e),
    }
    println!();

    println!("5. Creating and pushing a delta adding a 4th approver...");
    let pub_key_4 = PublicKey::new(Word::from([4u32, 0, 0, 0]));
    let mut storage_delta = AccountStorageDelta::default();
    storage_delta.set_map_item(1, Word::from([3u32, 0, 0, 0]), Word::from(pub_key_4));
    storage_delta.set_item(0, Word::from([threshold, 4u32, 0, 0]));
    let nonce = initial_nonce.as_int() + 1;
    let nonce = Felt::new(nonce);

    let delta = AccountDelta::new(
        account_id,
        storage_delta,
        AccountVaultDelta::default(),
        nonce,
    )
    .expect("Failed to create delta");

    let delta_payload = delta.to_json();
    let prev_commitment = format!("0x{}", hex::encode(initial_commitment.as_bytes()));

    match client
        .push_delta(&account_id, nonce.as_int(), prev_commitment, delta_payload)
        .await
    {
        Ok(response) => {
            println!("  ✓ {}", response.message);
            if let Some(delta) = response.delta {
                println!("    New commitment: {}", delta.new_commitment);
                if !delta.ack_sig.is_empty() {
                    println!("    Server ack signature: {}...", &delta.ack_sig[0..20]);
                }
            }
        }
        Err(e) => println!("  ✗ Push delta failed: {}", e),
    }
    println!();

    println!("6. Retrieving delta from PSM...");
    match client.get_delta(&account_id, 1).await {
        Ok(response) => {
            println!("  ✓ {}", response.message);
            if let Some(delta) = response.delta {
                println!("    Nonce: {}", delta.nonce);
                println!("    Commitment: {}", delta.new_commitment);
            }
        }
        Err(e) => println!("  ✗ Get delta failed: {}", e),
    }
    println!();

    println!("7. Getting account state...");
    match client.get_state(&account_id).await {
        Ok(response) => {
            println!("  ✓ {}", response.message);
            if let Some(state) = response.state {
                println!("    Commitment: {}", state.commitment);
                println!("    Updated at: {}", state.updated_at);
            }
        }
        Err(e) => println!("  ✗ Get state failed: {}", e),
    }

    println!("\n=== End-to-end flow completed! ===");
    Ok(())
}
