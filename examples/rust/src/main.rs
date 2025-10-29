mod miden;

// miden-client is available for full on-chain submission (see ON_CHAIN_SUBMISSION.md)
use miden_keystore::{FilesystemKeyStore, KeyStore};
use miden_lib::account::wallets::BasicWallet;
use miden_lib::transaction::TransactionKernel;
use miden_objects::account::{
    Account, AccountBuilder, AccountComponent, AccountDelta, AccountStorageMode, AccountType,
    StorageMap, StorageSlot,
    delta::{AccountStorageDelta, AccountVaultDelta},
};
use miden_objects::assembly::diagnostics::NamedSource;
use miden_objects::crypto::dsa::rpo_falcon512::{PublicKey, SecretKey};
use miden_objects::transaction::{InputNotes, OutputNotes, TransactionSummary};
use miden_objects::utils::{Deserializable, Serializable};
use miden_objects::{Felt, Word, ZERO};
use miden_rpc_client::MidenRpcClient;
use private_state_manager_client::{
    Auth, AuthConfig, ClientResult, FalconRpoSigner, MidenFalconRpoAuth, PsmClient,
};
use private_state_manager_shared::hex::IntoHex;
use private_state_manager_shared::ToJson;
use private_state_manager_client::auth_config::AuthType;
use rand_chacha::ChaCha20Rng;
use tempfile::TempDir;
use private_state_manager_client::verify_commitment_signature;

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
    // Step 5: Client 2 - Create TransactionSummary
    // =========================================================================
    if let Some(account) = retrieved_account {
        println!("Step 5: Client 2 - Create TransactionSummary for 3-of-3 + new cosigner...");
        println!("  ✓ Account retrieved from PSM");
        println!("    Account ID: {}", account.id());
        println!("    Current nonce: {}", account.nonce());

        // Generate a new cosigner keypair
        println!("  Generating new cosigner keypair...");
        let new_cosigner_secret = SecretKey::new();
        let new_cosigner_pubkey = new_cosigner_secret.public_key();
        let new_cosigner_commitment = new_cosigner_pubkey.to_commitment();
        let _new_cosigner_full_pubkey_hex = (&new_cosigner_pubkey).into_hex();
        let new_cosigner_commitment_hex = format!("0x{}", hex::encode(new_cosigner_commitment.to_bytes()));
        println!("  ✓ New cosigner commitment: {}...", &new_cosigner_commitment_hex);

        // Create a delta to add the new cosigner (index 2) and update the threshold config
        println!("  Creating delta to add new cosigner and update to 3-of-3 multisig...");
        let mut storage_delta = AccountStorageDelta::default();

        // Update slot 0: Change from 2-of-2 to 3-of-3
        storage_delta.set_item(0, Word::from([3u32, 3, 0, 0]));

        // Add new cosigner to slot 1 map at index 2
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

        // Build a TransactionSummary: no input/output notes for this config change; pick a salt
        let tx_salt = Word::from([Felt::new(42u64), ZERO, ZERO, ZERO]);
        let tx_summary = TransactionSummary::new(
            new_delta.clone(),
            InputNotes::new(Vec::new()).expect("inputs"),
            OutputNotes::new(Vec::new()).expect("outputs"),
            tx_salt,
        );

        println!("  ✓ Created TransactionSummary with nonce: {}", new_nonce);
        println!("  ✓ Delta adds cosigner at index 2");
        println!("  ✓ Updates multisig to 3-of-3");

        let original_commitment = tx_summary.to_commitment();
        println!("  ✓ TX summary commitment: 0x{}", hex::encode(original_commitment.to_bytes()));
        println!();

        // =========================================================================
        // Step 6: Client 2 - Push TransactionSummary to PSM; server signs TX summary commitment
        // =========================================================================
        println!("Step 6: Push TransactionSummary; expect ack signature over TX_SUMMARY_COMMITMENT...");

        // Serialize TransactionSummary to JSON using the expected format
        let tx_summary_json = tx_summary.to_json();
        let prev_commitment = format!("0x{}", hex::encode(account.commitment().as_bytes()));

        let (_new_commitment, ack_sig) = match client2
            .push_delta(&account_id, new_nonce.as_int(), prev_commitment, tx_summary_json)
            .await
        {
            Ok(response) => {
                println!("  ✓ {}", response.message);
                // ack_sig can be at top level or in delta
                let ack_sig = response.ack_sig
                    .or_else(|| response.delta.as_ref().map(|d| d.ack_sig.clone()))
                    .unwrap_or_default();

                if let Some(delta) = response.delta {
                    println!("    New commitment: {}", delta.new_commitment);
                    if !ack_sig.is_empty() {
                        println!("    PSM ack signature: {}...", &ack_sig[0..20.min(ack_sig.len())]);
                        (delta.new_commitment, ack_sig)
                    } else {
                        println!("  ✗ Missing ack signature in response");
                        return Ok(());
                    }
                } else {
                    println!("  ✗ No delta in response");
                    return Ok(());
                }
            }
            Err(e) => {
                println!("  ✗ Failed to push TransactionSummary: {}", e);
                return Ok(());
            }
        };
        println!();

        // =========================================================================
        // Step 7: Verify PSM ack signature over the TX summary commitment; update local account
        // =========================================================================
        println!("Step 7: Verify PSM ack over TX summary commitment...");

        // Compute TX summary commitment hex for verification
        let tx_summary_commitment_hex = format!("0x{}", hex::encode(tx_summary.to_commitment().to_bytes()));

        // Ensure the ack_sig has 0x prefix for verify_commitment_signature
        let ack_sig_with_prefix = if ack_sig.starts_with("0x") {
            ack_sig.clone()
        } else {
            format!("0x{}", ack_sig)
        };

        println!("  Debug: TX commitment for verification: {}", &tx_summary_commitment_hex);
        println!("  Debug: Server pubkey: {}...", &server_ack_pubkey[..40.min(server_ack_pubkey.len())]);
        println!("  Debug: ACK signature: {}...", &ack_sig_with_prefix[..40.min(ack_sig_with_prefix.len())]);

        match verify_commitment_signature(&tx_summary_commitment_hex, &server_ack_pubkey, &ack_sig_with_prefix) {
            Ok(true) => {
                println!("  ✓ PSM signature VALID over TX summary commitment");
                // Apply the same delta locally to reflect the change
                let mut updated_account = account.clone();
                updated_account.apply_delta(&new_delta).expect("apply delta");
                println!("  New local commitment: 0x{}", hex::encode(updated_account.commitment().as_bytes()));
                println!("  New local nonce: {}", updated_account.nonce());
                println!();

                // =========================================================================
                // Step 8: Connect to Miden testnet and show readiness for on-chain submission
                // =========================================================================
                println!("Step 8: Connecting to Miden testnet for potential on-chain submission...");

                let client = create_client(&keystore)?;

                // Connect to Miden testnet RPC
                let miden_node_url = std::env::var("MIDEN_NODE_URL")
                    .unwrap_or_else(|_| "https://rpc.testnet.miden.io:443".to_string());
                println!("  Connecting to Miden node: {}", miden_node_url);

                let mut rpc_client = match MidenRpcClient::connect(miden_node_url.clone()).await {
                    Ok(client) => {
                        println!("  ✓ Connected to Miden testnet RPC");
                        client
                    }
                    Err(e) => {
                        println!("  ✗ Failed to connect to Miden node: {}", e);
                        println!("  Note: PSM validation still succeeded - transaction can be submitted when node is available");
                        return Ok(());
                    }
                };

                // Get latest block info
                let block_header = match rpc_client.get_block_header(None, false).await {
                    Ok(response) => {
                        if let Some(header) = response.block_header {
                            println!("  ✓ Latest block: #{}", header.block_num);
                            println!("    Timestamp: {}", header.timestamp);
                            Some(header)
                        } else {
                            println!("  ✗ No block header in response");
                            None
                        }
                    }
                    Err(e) => {
                        println!("  ✗ Failed to get block header: {}", e);
                        None
                    }
                };

                if let Some(_block_header) = block_header {
                    println!("\n  === Step 9: On-Chain Submission with miden-client ===");
                    println!();
                    println!("  Transaction is PSM-validated and ready for on-chain submission!");
                    println!();
                    println!("  To complete full on-chain submission using miden-client:");
                    println!("  ───────────────────────────────────────────────────────────");
                    println!();
                    println!("  1. Setup miden-client:");
                    println!("     ```rust");
                    println!("     use miden_client::{{ClientBuilder, Endpoint}};");
                    println!("     use miden_client::crypto::RpoRandomCoin;");
                    println!();
                    println!("     let endpoint = Endpoint::new(");
                    println!("         \"https\".into(),");
                    println!("         \"rpc.testnet.miden.io\".into(),");
                    println!("         Some(443)");
                    println!("     );");
                    println!();
                    println!("     let mut client = ClientBuilder::new()");
                    println!("         .sqlite_store(\"/path/to/db\")");
                    println!("         .tonic_rpc_client(&endpoint, Some(10_000))");
                    println!("         .filesystem_keystore(\"/path/to/keys\")");
                    println!("         .rng(Box::new(RpoRandomCoin::new(seed)))");
                    println!("         .build()");
                    println!("         .await?;");
                    println!("     ```");
                    println!();
                    println!("  2. Deploy account on-chain (if new):");
                    println!("     • New accounts must be deployed before transacting");
                    println!("     • Use faucet or existing account to deploy");
                    println!();
                    println!("  3. Create and execute transaction:");
                    println!("     ```rust");
                    println!("     // Import PSM-validated account");
                    println!("     client.import_account(&account, seed, &auth_info).await?;");
                    println!();
                    println!("     // Sync with network");
                    println!("     client.sync_state().await?;");
                    println!();
                    println!("     // Build transaction with PSM signature in advice");
                    println!("     let tx_request = TransactionRequest::new()");
                    println!("         .with_custom_script(tx_script)");
                    println!("         .with_advice_inputs(psm_sig_advice);");
                    println!();
                    println!("     // Execute (validates PSM sig in MASM)");
                    println!("     let tx_result = client");
                    println!("         .new_transaction(account_id, tx_request)");
                    println!("         .await?;");
                    println!("     ```");
                    println!();
                    println!("  4. Prove and submit (10-30 seconds):");
                    println!("     ```rust");
                    println!("     client.submit_transaction(tx_result).await?;");
                    println!("     println!(\"✓ Transaction submitted to network!\");");
                    println!("     ```");
                    println!();
                    println!("  ═══════════════════════════════════════════════");
                    println!("  ✅ PSM Validation Complete!");
                    println!("  ═══════════════════════════════════════════════");
                    println!("  What we accomplished:");
                    println!("  1. ✅ Created multisig account with PSM verification");
                    println!("  2. ✅ PSM validated and signed TransactionSummary");
                    println!("  3. ✅ Verified PSM signature locally");
                    println!("  4. ✅ Connected to Miden testnet RPC");
                    println!();
                    println!("  Key Achievement:");
                    println!("  PSM pre-validation prevents wasted computation!");
                    println!("  Only valid transactions proceed to expensive proving.");
                    println!();
                    println!("  See ON_CHAIN_SUBMISSION.md for complete integration guide.");
                    println!("  ═══════════════════════════════════════════════");
                }

                println!("\n=== PSM Multi-Client E2E Summary ===");
                println!("✅ Account created with 2-of-2 multisig + PSM verification");
                println!("✅ TransactionSummary created to add 3rd signer and update to 3-of-3");
                println!("✅ PSM validated and signed the TX summary commitment");
                println!("✅ Connected to Miden testnet RPC successfully");
                println!("\nKey PSM Benefits Demonstrated:");
                println!("• Pre-validation before expensive proof generation");
                println!("• Multi-party coordination with signature verification");
                println!("• State consistency across clients");
                println!("• Ready for on-chain submission when needed");
            }
            Ok(false) => {
                println!("  ✗ PSM signature INVALID");
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