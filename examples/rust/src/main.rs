use miden_keystore::{FilesystemKeyStore, KeyStore};
use miden_lib::account::wallets::BasicWallet;
use miden_lib::transaction::TransactionKernel;
use miden_objects::account::{
    AccountBuilder, AccountComponent, AccountDelta, AccountStorageMode, AccountType,
    StorageMap, StorageSlot,
    delta::{AccountStorageDelta, AccountVaultDelta},
};
use miden_objects::assembly::diagnostics::NamedSource;
use miden_objects::crypto::dsa::rpo_falcon512::{PublicKey, SecretKey};
use miden_objects::crypto::hash::rpo::Rpo256;
use miden_objects::utils::{Deserializable, Serializable};
use miden_objects::{Felt, Word};
use miden_objects::account::Account;
use miden_rpc_client::MidenRpcClient;
use miden_processor::ExecutionOptions;
use private_state_manager_client::{
    Auth, AuthConfig, ClientResult, FalconRpoSigner, MidenFalconRpoAuth, PsmClient,
    auth::miden_falcon_rpo::IntoWord,
};
use private_state_manager_client::auth_config::AuthType;
use rand_chacha::ChaCha20Rng;
use tempfile::TempDir;

// Load Multisig Auth MASM code from file (includes PSM verification)
const MULTISIG_AUTH: &str = include_str!("../masm/multisig.masm");
const PSM_LIB: &str = include_str!("../masm/psm.masm");

/// Generate a Falcon keypair and return (full_pubkey_hex, commitment_hex, secret_key)
fn generate_falcon_keypair(
    keystore: &FilesystemKeyStore<ChaCha20Rng>,
) -> (String, String, SecretKey) {
    let pubkey_commitment_word = keystore
        .generate_key()
        .expect("Failed to generate key");
    let secret_key = keystore
        .get_key(pubkey_commitment_word)
        .expect("Failed to get key");

    // Verify the commitment matches what we'll send later
    let actual_pubkey = secret_key.public_key();
    let actual_commitment = actual_pubkey.to_commitment();
    assert_eq!(
        pubkey_commitment_word, actual_commitment,
        "Keystore commitment doesn't match derived public key commitment!"
    );

    // Return both full public key (for auth) and commitment (for account storage)
    use private_state_manager_shared::hex::IntoHex;
    let full_pubkey_hex = (&actual_pubkey).into_hex();
    let commitment_hex = format!("0x{}", hex::encode(pubkey_commitment_word.to_bytes()));

    (full_pubkey_hex, commitment_hex, secret_key)
}

/// Create a multisig PSM account with 2-of-2 threshold
fn create_multisig_psm_account(
    client1_pubkey_hex: &str,
    client2_pubkey_hex: &str,
    psm_server_pubkey_hex: &str,
    init_seed: [u8; 32],
) -> miden_objects::account::Account {
    // Convert pubkey commitments (Word) from hex to Word
    // The client sends public key commitments (32 bytes), not full keys
    let psm_pubkey_bytes = hex::decode(&psm_server_pubkey_hex[2..])
        .expect("Failed to decode PSM pubkey");
    let psm_commitment_word = Word::read_from_bytes(&psm_pubkey_bytes)
        .expect("Failed to convert PSM commitment to Word");

    let client1_pubkey_bytes = hex::decode(&client1_pubkey_hex[2..])
        .expect("Failed to decode client1 pubkey");
    let client1_commitment_word = Word::read_from_bytes(&client1_pubkey_bytes)
        .expect("Failed to convert client1 commitment to Word");

    let client2_pubkey_bytes = hex::decode(&client2_pubkey_hex[2..])
        .expect("Failed to decode client2 pubkey");
    let client2_commitment_word = Word::read_from_bytes(&client2_pubkey_bytes)
        .expect("Failed to convert client2 commitment to Word");

    // Build multisig auth component with storage slots
    // Storage layout for multisig.masm:
    // Slot 0: [threshold, num_approvers, 0, 0]
    // Slot 1: Public keys map (client1, client2)
    // Slot 2: Executed transactions map (empty initially)
    // Slot 3: Procedure thresholds map (empty initially)
    // Slot 4: PSM selector [1,0,0,0] = ON
    // Slot 5: PSM public key map

    // Slot 0: Multisig config - require 2 out of 2 signatures
    let slot_0 = StorageSlot::Value(Word::from([2u32, 2, 0, 0]));

    // Slot 1: Client public key commitments map
    let mut client_pubkeys_map = StorageMap::new();
    let _ = client_pubkeys_map.insert(
        Word::from([0u32, 0, 0, 0]), // index 0 - client1
        client1_commitment_word,
    );
    let _ = client_pubkeys_map.insert(
        Word::from([1u32, 0, 0, 0]), // index 1 - client2
        client2_commitment_word,
    );
    let slot_1 = StorageSlot::Map(client_pubkeys_map);

    // Slot 2: Executed transactions map (empty)
    let slot_2 = StorageSlot::Map(StorageMap::new());

    // Slot 3: Procedure thresholds map (empty)
    let slot_3 = StorageSlot::Map(StorageMap::new());

    // Slot 4: PSM selector [1,0,0,0] = ON
    let slot_4 = StorageSlot::Value(Word::from([1u32, 0, 0, 0]));

    // Slot 5: PSM public key commitment map (single entry at index 0)
    let mut psm_key_map = StorageMap::new();
    let _ = psm_key_map.insert(
        Word::from([0u32, 0, 0, 0]), // index 0
        psm_commitment_word,
    );
    let slot_5 = StorageSlot::Map(psm_key_map);

    // Create PSM library with openzeppelin::psm namespace
    let base_assembler = TransactionKernel::assembler();
    let psm_library = base_assembler
        .clone()
        .assemble_library([NamedSource::new("openzeppelin::psm", PSM_LIB)])
        .expect("Failed to compile PSM library");

    // Add PSM library to assembler for multisig auth compilation
    let assembler = base_assembler
        .with_dynamic_library(psm_library)
        .expect("Failed to add PSM library to assembler");

    let auth_component = AccountComponent::compile(
        MULTISIG_AUTH.to_string(),
        assembler,
        vec![slot_0, slot_1, slot_2, slot_3, slot_4, slot_5],
    )
    .expect("Failed to compile auth component")
    .with_supports_all_types();

    // Create account with both clients as cosigners
    AccountBuilder::new(init_seed)
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Private)
        .with_auth_component(auth_component)
        .with_component(BasicWallet)
        .build()
        .expect("Failed to build account")
}

#[tokio::main]
async fn main() -> ClientResult<()> {
    println!("=== PSM Multi-Client E2E Flow ===\n");

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let keystore = FilesystemKeyStore::<ChaCha20Rng>::new(temp_dir.path().to_path_buf())
        .expect("Failed to create keystore");

    // =========================================================================
    // Setup: Generate keys for both clients
    // =========================================================================
    println!("Setup: Generating keys...");

    let (client1_full_pubkey_hex, client1_commitment_hex, client1_secret_key) = generate_falcon_keypair(&keystore);
    let (client2_full_pubkey_hex, client2_commitment_hex, client2_secret_key) = generate_falcon_keypair(&keystore);

    println!("  ✓ Client 1 commitment: {}...", &client1_commitment_hex);
    println!("  ✓ Client 2 commitment: {}...", &client2_commitment_hex);
    println!();

    // =========================================================================
    // Step 1: Connect to PSM and get server's public key
    // =========================================================================
    println!("Step 1: Connect to PSM and get server's public key...");

    let client1_signer = FalconRpoSigner::new(client1_secret_key.clone());
    let client1_auth = Auth::FalconRpoSigner(client1_signer);

    let psm_endpoint = "http://localhost:50051".to_string();
    let mut client1 = match PsmClient::connect(psm_endpoint.clone()).await {
        Ok(client) => client.with_auth(client1_auth),
        Err(e) => {
            println!("  ✗ Failed to connect: {}", e);
            println!("  Hint: Start PSM server with: cargo run --package private-state-manager-server --bin server");
            return Ok(());
        }
    };

    let server_ack_pubkey = match client1.get_pubkey().await {
        Ok(pubkey) => {
            println!("  ✓ Connected to PSM server");
            pubkey
        }
        Err(e) => {
            println!("  ✗ Failed to get server pubkey: {}", e);
            return Ok(());
        }
    };

    // Compute the commitment from the server's full public key
    // The server returns the full key for signature verification, but we need
    // to store only the commitment in the account
    let server_pubkey_bytes = hex::decode(&server_ack_pubkey[2..])
        .expect("Failed to decode server public key");
    let server_pubkey = PublicKey::read_from_bytes(&server_pubkey_bytes)
        .expect("Failed to deserialize server public key");
    let server_commitment = server_pubkey.to_commitment();
    let server_commitment_hex = format!("0x{}", hex::encode(server_commitment.to_bytes()));

    println!("  ✓ Server commitment: {}...", &server_commitment_hex);
    println!();

    // =========================================================================
    // Step 2: Create multisig PSM account with server's pubkey commitment
    // =========================================================================
    println!("Step 2: Creating multisig PSM account with PSM auth...");

    let init_seed = [0xff; 32];
    let account = create_multisig_psm_account(
        &client1_commitment_hex,
        &client2_commitment_hex,
        &server_commitment_hex,
        init_seed,
    );

    let account_id = account.id();
    println!("  ✓ Account ID: {}", account_id);
    println!("  ✓ Commitment: 0x{}", hex::encode(account.commitment().as_bytes()));
    println!("  ✓ Multisig: 2-of-2 (client1, client2)");
    println!("  ✓ PSM auth enabled with server's pubkey");
    println!();

    // =========================================================================
    // Step 3: Client 1 - Configure account in PSM
    // =========================================================================
    println!("Step 3: Client 1 - Configure account in PSM...");

    // Configure with both cosigners (use full public keys for auth, not commitments)
    // The server needs full keys to verify signatures
    let auth_config = AuthConfig {
        auth_type: Some(AuthType::MidenFalconRpo(MidenFalconRpoAuth {
            cosigner_pubkeys: vec![client1_full_pubkey_hex.clone(), client2_full_pubkey_hex.clone()],
        })),
    };

    // Create state with serialized account
    let account_bytes = account.to_bytes();
    let account_base64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &account_bytes);

    let initial_state = serde_json::json!({
        "data": account_base64,
        "account_id": account_id.to_string(),
    });

    match client1.configure(&account_id, auth_config, initial_state, "Filesystem").await {
        Ok(response) => {
            println!("  ✓ {}", response.message);
        }
        Err(e) => {
            println!("  ✗ Configuration failed: {}", e);
            return Ok(());
        }
    };
    println!();

    // =========================================================================
    // Step 4: Client 2 - Pull state from PSM
    // =========================================================================
    println!("Step 4: Client 2 - Pull state from PSM...");

    // Client 2 connects with their key
    let client2_signer = FalconRpoSigner::new(client2_secret_key.clone());
    let client2_auth = Auth::FalconRpoSigner(client2_signer);

    let mut client2 = PsmClient::connect(psm_endpoint.clone()).await
        .expect("Failed to connect")
        .with_auth(client2_auth);

    let retrieved_account = match client2.get_state(&account_id).await {
        Ok(response) => {
            println!("  ✓ {}", response.message);
            if let Some(state) = response.state {
                println!("    Commitment: {}", state.commitment);
                println!("    Updated at: {}", state.updated_at);

                let state_value: serde_json::Value = serde_json::from_str(&state.state_json)
                    .expect("Failed to parse state_json");

                // Deserialize account
                if let Some(data_str) = state_value["data"].as_str() {
                    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data_str)
                        .expect("Failed to decode account data");
                    match Account::read_from_bytes(&bytes) {
                        Ok(account) => {
                            println!("    ✓ Deserialized account");
                            Some(account)
                        }
                        Err(e) => {
                            println!("    ✗ Failed to deserialize: {}", e);
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
        Err(e) => {
            println!("  ✗ Failed to get state: {}", e);
            None
        }
    };
    println!();

    // =========================================================================
    // Step 5: Client 2 - Create delta to add a new cosigner
    // =========================================================================
    if let Some(account) = retrieved_account {
        println!("Step 5: Client 2 - Create delta to add a new cosigner...");
        println!("  ✓ Account retrieved from PSM");
        println!("    Account ID: {}", account.id());
        println!("    Current nonce: {}", account.nonce());

        // Generate a new cosigner keypair
        println!("  Generating new cosigner keypair...");
        let new_cosigner_secret = SecretKey::new();
        let new_cosigner_pubkey = new_cosigner_secret.public_key();
        let new_cosigner_commitment = new_cosigner_pubkey.to_commitment();
        use private_state_manager_shared::hex::IntoHex;
        let new_cosigner_full_pubkey_hex = (&new_cosigner_pubkey).into_hex();
        let new_cosigner_commitment_hex = format!("0x{}", hex::encode(new_cosigner_commitment.to_bytes()));
        println!("  ✓ New cosigner commitment: {}...", &new_cosigner_commitment_hex);

        // Create a delta to add the new cosigner (index 2) and update the threshold config
        println!("  Creating delta to add new cosigner and update to 3-of-3 multisig...");
        let mut storage_delta = AccountStorageDelta::default();

        // Update slot 0: Change from 2-of-2 to 3-of-3
        storage_delta.set_item(0, Word::from([3u32, 3, 0, 0]));

        // Add new cosigner to slot 1 map at index 2
        // Note: We only need to add the new entry; existing entries remain unchanged
        storage_delta.set_map_item(
            1, // slot index for pubkeys map
            Word::from([2u32, 0, 0, 0]), // index 2 for the new cosigner
            new_cosigner_commitment,
        );

        let new_nonce = Felt::new(account.nonce().as_int() + 1);
        let new_delta = AccountDelta::new(
            account_id,
            storage_delta,
            AccountVaultDelta::default(),
            new_nonce,
        ).expect("Failed to create delta");

        println!("  ✓ Created delta with nonce: {}", new_nonce);
        println!("  ✓ Delta adds cosigner at index 2");
        println!("  ✓ Updates multisig to 3-of-3");
        println!();

        // =========================================================================
        // Step 6: Update PSM configuration with new cosigner
        // =========================================================================
        println!("Step 6: Update PSM configuration to include new cosigner...");

        // We need to update the PSM auth config to include the new cosigner
        // so PSM knows about all three signers
        let _updated_auth_config = AuthConfig {
            auth_type: Some(AuthType::MidenFalconRpo(MidenFalconRpoAuth {
                cosigner_pubkeys: vec![
                    client1_full_pubkey_hex.clone(),
                    client2_full_pubkey_hex.clone(),
                    new_cosigner_full_pubkey_hex.clone(),
                ],
            })),
        };

        // Note: In a real implementation, you would call an update_auth endpoint
        // For now, we'll note this as a required step
        println!("  ⚠️  Note: PSM auth config should be updated to include new cosigner");
        println!("     Current cosigners: 2, New cosigners: 3");
        println!();

        // =========================================================================
        // Step 7: Client 2 - Push delta to PSM
        // =========================================================================
        println!("Step 7: Client 2 - Push delta to PSM...");

        // Serialize delta to JSON using the expected format
        use private_state_manager_shared::ToJson;
        let delta_payload = new_delta.to_json();
        let prev_commitment = format!("0x{}", hex::encode(account.commitment().as_bytes()));

        let (new_commitment, ack_sig) = match client2
            .push_delta(&account_id, new_nonce.as_int(), prev_commitment, delta_payload)
            .await
        {
            Ok(response) => {
                println!("  ✓ {}", response.message);
                if let Some(delta) = response.delta {
                    println!("    New commitment: {}", delta.new_commitment);
                    println!("    Server ack signature: {}...", &delta.ack_sig[0..20]);
                    (delta.new_commitment, delta.ack_sig)
                } else {
                    println!("  ✗ No delta in response");
                    return Ok(());
                }
            }
            Err(e) => {
                println!("  ✗ Failed to push delta: {}", e);
                return Ok(());
            }
        };
        println!();

        // =========================================================================
        // Step 8: Client 2 - Verify ack signature and prepare on-chain transaction
        // =========================================================================
        println!("Step 8: Client 2 - Verify PSM ack signature...");

        // Verify the server's signature on the new commitment
        use private_state_manager_client::verify_commitment_signature;

        match verify_commitment_signature(&new_commitment, &server_ack_pubkey, &ack_sig) {
            Ok(true) => {
                println!("  ✓ PSM server signature is VALID");
                println!("  ✓ Delta authenticated by PSM");
                println!();

                // Apply delta locally
                let mut updated_account = account.clone();
                updated_account.apply_delta(&new_delta)
                    .expect("Failed to apply delta");

                println!("  Account state after delta:");
                println!("    New commitment: 0x{}", hex::encode(updated_account.commitment().as_bytes()));
                println!("    New nonce: {}", updated_account.nonce());
                println!();

                // =========================================================================
                // Step 9: Demonstrate transaction submission to Miden testnet
                // =========================================================================
                println!("Step 9: Preparing for Miden testnet submission...");

                println!("  For private accounts, transactions require:");
                println!("    1. Transaction construction with delta");
                println!("    2. Zero-knowledge proof generation");
                println!("    3. Submission to network");

                // Connect to testnet
                let miden_node_url = "https://rpc.testnet.miden.io:443";
                println!("\n  Connecting to Miden testnet: {}", miden_node_url);

                match MidenRpcClient::connect(miden_node_url.to_string()).await {
                    Ok(mut rpc_client) => {
                        println!("  ✓ Connected to Miden testnet");

                        // Get the latest block reference for transaction context
                        match rpc_client.get_block_header(None, false).await {
                            Ok(response) => {
                                if let Some(block_header) = response.block_header {
                                    println!("  ✓ Got latest block: #{}", block_header.block_num);
                                    // Block header fields available for transaction construction

                                println!("\n  Transaction details:");
                                println!("    • Account ID: {}", account_id);
                                println!("    • Current nonce: {}", account.nonce());
                                println!("    • New nonce: {}", updated_account.nonce());
                                println!("    • Delta changes:");
                                println!("      - Threshold: 2-of-2 → 3-of-3");
                                println!("      - New cosigner at index 2");
                                println!("    • New commitment: 0x{}", hex::encode(updated_account.commitment().as_bytes()));

                                // Show authentication requirements
                                println!("\n  Authentication requirements:");
                                println!("    • Need 2 signatures (current threshold)");
                                println!("    • Client 1 ✓ (has secret key)");
                                println!("    • Client 2 ✓ (has secret key)");

                                // Create example signatures
                                let tx_message = updated_account.id().into_word();
                                let client1_sig = client1_secret_key.sign(tx_message);
                                let client2_sig = client2_secret_key.sign(tx_message);

                                println!("\n  Generated signatures:");
                                println!("    • Client 1: {}...", hex::encode(&client1_sig.to_bytes()[..32]));
                                println!("    • Client 2: {}...", hex::encode(&client2_sig.to_bytes()[..32]));

                                println!("\n  Full transaction flow would:");
                                println!("    1. Create TransactionRequest with:");
                                println!("       - Account state and delta");
                                println!("       - Block reference #{}", block_header.block_num);
                                println!("       - Authentication signatures");
                                println!();
                                println!("    2. Execute transaction kernel:");
                                println!("       - Verify authentication (2-of-2 multisig)");
                                println!("       - Apply delta to account state");
                                println!("       - Update account commitment");
                                println!();
                                println!("    3. Generate zero-knowledge proof:");
                                println!("       - Prove valid state transition");
                                println!("       - Prove signature validity");
                                println!("       - This is computationally expensive (~10-30 seconds)");
                                println!();
                                println!("    4. Submit proven transaction:");
                                println!("       ```rust");
                                println!("       let tx_id = rpc_client.submit_proven_transaction(");
                                println!("           proven_tx");
                                println!("       ).await?;");
                                println!("       ```");

                                    // Check if account exists on-chain (for demo purposes)
                                    println!("\n  Checking if account exists on testnet...");
                                    match rpc_client.get_account_commitment(&account_id).await {
                                        Ok(commitment) => {
                                            println!("    ✓ Account found on testnet");
                                            println!("    On-chain commitment: {}", commitment);
                                            println!("    Ready for transaction submission!");
                                        }
                                        Err(_) => {
                                            println!("    ℹ Account not found on testnet");
                                            println!("    Would need to be created first via faucet or note consumption");
                                        }
                                    }
                                } else {
                                    println!("  ✗ No block header returned");
                                }
                            }
                            Err(e) => {
                                println!("  ✗ Failed to get latest block: {}", e);
                            }
                        }

                        println!("\n  PSM's value in this flow:");
                        println!("    ✅ Delta validated BEFORE expensive proof generation");
                        println!("    ✅ PSM signature provides validation evidence");
                        println!("    ✅ Prevents wasted computation on invalid state changes");
                        println!("    ✅ Enables secure multi-party coordination");
                        println!("    ✅ The new 3-of-3 configuration is ready for on-chain submission");
                    }
                    Err(e) => {
                        println!("  ✗ Failed to connect to Miden testnet: {}", e);
                    }
                }
            }
            Ok(false) => {
                println!("  ✗ PSM server signature is INVALID");
            }
            Err(e) => {
                println!("  ✗ Signature verification error: {}", e);
            }
        }
    } else {
        println!("  ✗ Failed to retrieve account from PSM");
    }

    println!("\n=== Multi-client E2E flow completed! ===");
    Ok(())
}
