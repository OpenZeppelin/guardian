mod falcon;
mod multisig;

use std::path::Path;
use std::sync::Arc;

use clap::{Parser, ValueEnum};
use miden_client::account::Account;
use miden_client::crypto::RpoRandomCoin;
use miden_client::keystore::FilesystemKeyStore;
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_client::{
    Client, ClientError, Deserializable, ExecutionOptions, Felt, Serializable, Word,
};
use miden_client_sqlite_store::SqliteStore;
use miden_objects::{MAX_TX_EXECUTION_CYCLES, MIN_TX_EXECUTION_CYCLES};

use miden_objects::account::auth::Signature as AccountSignature;
use miden_objects::crypto::dsa::rpo_falcon512::Signature as RawFalconSignature;

use private_state_manager_client::auth_config::AuthType;
use private_state_manager_client::{
    verify_commitment_signature, Auth, AuthConfig, ClientResult, FalconRpoSigner,
    MidenFalconRpoAuth, PsmClient,
};
use private_state_manager_shared::hex::FromHex;
use private_state_manager_shared::ToJson;

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use tempfile::TempDir;

#[derive(Debug, Clone, ValueEnum)]
enum Network {
    Local,
    Testnet,
}

#[derive(Parser, Debug)]
#[command(name = "psm-rust-example")]
#[command(about = "PSM Multi-Client E2E Flow Example")]
struct Args {
    /// Network to connect to
    #[arg(long, value_enum, default_value = "local")]
    network: Network,
}

fn commitment_from_hex(hex_commitment: &str) -> Result<Word, String> {
    let trimmed = hex_commitment.strip_prefix("0x").unwrap_or(hex_commitment);
    let bytes = hex::decode(trimmed)
        .map_err(|err| format!("Failed to decode commitment hex '{hex_commitment}': {err}"))?;

    Word::read_from_bytes(&bytes)
        .map_err(|err| format!("Failed to deserialize commitment word '{hex_commitment}': {err}"))
}

async fn create_miden_client(data_dir: &Path, endpoint: &Endpoint) -> Result<Client<()>, String> {
    let store_path = data_dir.join("miden-client.sqlite");
    let store = SqliteStore::new(store_path)
        .await
        .map_err(|err| format!("Failed to open SQLite store: {err}"))?;
    let store = Arc::new(store);

    let rng = Box::new(RpoRandomCoin::new(Word::default()));
    let exec_options = ExecutionOptions::new(
        Some(MAX_TX_EXECUTION_CYCLES),
        MIN_TX_EXECUTION_CYCLES,
        false,
        true,
    )
    .map_err(|err| format!("Failed to build execution options: {err}"))?;

    let grpc_client = GrpcClient::new(endpoint, 10_000);
    let rpc_client: Arc<dyn NodeRpcClient> = Arc::new(grpc_client);

    Client::new(
        rpc_client,
        rng,
        store,
        None,
        exec_options,
        Some(20),
        Some(256),
        None,
        None, // prover
    )
    .await
    .map_err(|err| format!("Failed to create Miden client: {err}"))
}

async fn add_account_and_sync(
    client: &mut Client<()>,
    account: &Account,
) -> Result<(), ClientError> {
    client.add_account(account, false).await?;
    client.sync_state().await?;
    Ok(())
}

#[tokio::main]
async fn main() -> ClientResult<()> {
    let args = Args::parse();

    println!("=== PSM Multi-Client E2E Flow ===\n");

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let rng = ChaCha20Rng::from_seed([42u8; 32]);
    let keystore = FilesystemKeyStore::with_rng(temp_dir.path().to_path_buf(), rng)
        .expect("Failed to create keystore");

    println!("Setup: Generating keys...");

    let (_client1_full_pubkey_hex, client1_commitment_hex, client1_secret_key) =
        falcon::generate_falcon_keypair(&keystore);
    let (_client2_full_pubkey_hex, client2_commitment_hex, client2_secret_key) =
        falcon::generate_falcon_keypair(&keystore);

    println!("  ✓ Client 1 commitment: {}...", &client1_commitment_hex);
    println!("  ✓ Client 2 commitment: {}...", &client2_commitment_hex);
    println!();

    let miden_endpoint = match args.network {
        Network::Local => {
            println!("Step 1: Connect to PSM and Miden node (local)...");
            Endpoint::new("http".to_string(), "localhost".to_string(), Some(57291))
        }
        Network::Testnet => {
            println!("Step 1: Connect to PSM and Miden node (testnet)...");
            Endpoint::new(
                "https".to_string(),
                "rpc.testnet.miden.io".to_string(),
                None,
            )
        }
    };

    let client1_signer = FalconRpoSigner::new(client1_secret_key.clone());
    let client1_auth = Auth::FalconRpoSigner(client1_signer);

    let psm_endpoint = "http://localhost:50051".to_string();
    let mut psm_client1 = match PsmClient::connect(psm_endpoint.clone()).await {
        Ok(client) => client.with_auth(client1_auth),
        Err(e) => {
            println!("  ✗ Failed to connect to PSM: {}", e);
            println!("  Hint: Start PSM server with: cargo run --package private-state-manager-server --bin server");
            return Ok(());
        }
    };

    let server_commitment_hex = match psm_client1.get_pubkey().await {
        Ok(commitment) => {
            println!("  ✓ Connected to PSM server");
            println!("  ✓ Server commitment: {}...", &commitment[..18]);
            commitment
        }
        Err(e) => {
            println!("  ✗ Failed to get server commitment: {}", e);
            return Ok(());
        }
    };

    let mut miden_client = match create_miden_client(temp_dir.path(), &miden_endpoint).await {
        Ok(client) => {
            println!("  ✓ Connected to Miden node");
            client
        }
        Err(e) => {
            println!("  ✗ Failed to create Miden client: {}", e);
            if matches!(args.network, Network::Local) {
                println!("  Hint: Start Miden node on port 57291");
            }
            return Ok(());
        }
    };

    // Check for kernel version mismatch between client library and node
    use miden_client::transaction::TransactionKernel;
    let grpc_client_check = GrpcClient::new(&miden_endpoint, 10_000);
    if let Ok((block_header, _)) = grpc_client_check
        .get_block_header_by_number(None, false)
        .await
    {
        let node_kernel = block_header.tx_kernel_commitment();
        let client_kernel: Word = TransactionKernel.to_commitment();
        if node_kernel != client_kernel {
            println!("  ✗ Kernel version mismatch!");
            println!(
                "    Node kernel:   0x{}",
                hex::encode(node_kernel.as_bytes())
            );
            println!(
                "    Client kernel: 0x{}",
                hex::encode(client_kernel.as_bytes())
            );
            println!(
                "    The Miden node is running a different kernel version than the client library."
            );
            println!("    Please ensure both use the same miden-lib version (currently: 0.12.4).");
            return Ok(());
        }
    }

    println!();

    println!("Step 2: Creating multisig PSM account...");

    // Use random seed to avoid conflicts with existing accounts
    let init_seed: [u8; 32] = rand::random();
    let account = multisig::create_multisig_psm_account(
        &client1_commitment_hex,
        &client2_commitment_hex,
        &server_commitment_hex,
        init_seed,
    );

    let account_id = account.id();
    println!("  ✓ Account ID: {}", account_id);
    println!("  ✓ Multisig: 2-of-2 with PSM");

    if let Err(e) = add_account_and_sync(&mut miden_client, &account).await {
        println!("  ✗ Failed to add account to Miden client: {}", e);
        return Ok(());
    }
    println!("  ✓ Account synced with Miden node");
    println!();

    println!("Step 3: Client 1 - Configure account in PSM...");

    let auth_config = AuthConfig {
        auth_type: Some(AuthType::MidenFalconRpo(MidenFalconRpoAuth {
            cosigner_commitments: vec![
                client1_commitment_hex.clone(),
                client2_commitment_hex.clone(),
            ],
        })),
    };

    let account_bytes = account.to_bytes();
    let account_base64 =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &account_bytes);

    let initial_state = serde_json::json!({
        "data": account_base64,
        "account_id": account_id.to_string(),
    });

    match psm_client1
        .configure(&account_id, auth_config, initial_state)
        .await
    {
        Ok(response) => {
            println!("  ✓ {}", response.message);
        }
        Err(e) => {
            println!("  ✗ Configuration failed: {}", e);
            return Ok(());
        }
    };
    println!();

    println!("Step 4: Client 2 - Pull state from PSM...");

    let client2_signer = FalconRpoSigner::new(client2_secret_key.clone());
    let client2_auth = Auth::FalconRpoSigner(client2_signer);

    let mut psm_client2 = PsmClient::connect(psm_endpoint.clone())
        .await
        .expect("Failed to connect")
        .with_auth(client2_auth);

    let retrieved_account = match psm_client2.get_state(&account_id).await {
        Ok(response) => {
            println!("  ✓ {}", response.message);
            if let Some(state) = response.state {
                let state_value: serde_json::Value =
                    serde_json::from_str(&state.state_json).expect("Failed to parse state_json");

                if let Some(data_str) = state_value["data"].as_str() {
                    let bytes = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        data_str,
                    )
                    .expect("Failed to decode account data");
                    match Account::read_from_bytes(&bytes) {
                        Ok(account) => Some(account),
                        Err(e) => {
                            println!("  ✗ Failed to deserialize: {}", e);
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

    if let Some(account) = retrieved_account {
        println!("Step 5: Client 2 - Simulate transaction (update to 3-of-3)...");
        let (_new_cosigner_full_pubkey_hex, new_cosigner_commitment_hex, _new_cosigner_secret_key) =
            falcon::generate_falcon_keypair(&keystore);

        let signer_commitments = match [
            &client1_commitment_hex,
            &client2_commitment_hex,
            &new_cosigner_commitment_hex,
        ]
        .into_iter()
        .map(|hex_commitment| commitment_from_hex(hex_commitment))
        .collect::<Result<Vec<_>, _>>()
        {
            Ok(commitments) => commitments,
            Err(err) => {
                println!("  ✗ Failed to parse signer commitments: {}", err);
                return Ok(());
            }
        };

        let salt = Word::from([Felt::new(42), Felt::new(0), Felt::new(0), Felt::new(0)]);

        let (tx_request, _config_hash) = match multisig::build_update_signers_transaction_request(
            3,
            &signer_commitments,
            salt,
            vec![],
        ) {
            Ok(req) => req,
            Err(err) => {
                println!("  ✗ Failed to build transaction request: {}", err);
                return Ok(());
            }
        };

        let tx_summary = match miden_client
            .execute_transaction(account.id(), tx_request)
            .await
        {
            Err(ClientError::TransactionExecutorError(
                miden_client::transaction::TransactionExecutorError::Unauthorized(tx_summary),
            )) => {
                println!("  ✓ Transaction summary created");
                tx_summary
            }
            Ok(_) => {
                println!("  ✗ Expected Unauthorized error but transaction succeeded");
                return Ok(());
            }
            Err(e) => {
                println!("  ✗ Simulation failed: {:?}", e);
                return Ok(());
            }
        };
        println!();

        println!("Step 6: Push transaction summary to PSM...");

        let tx_summary_json = tx_summary.to_json();
        let prev_commitment = format!("0x{}", hex::encode(account.commitment().as_bytes()));

        let (_new_commitment, ack_sig) = match psm_client2
            .push_delta(
                &account_id,
                account.nonce().as_int(),
                prev_commitment,
                tx_summary_json,
            )
            .await
        {
            Ok(response) => {
                println!("  ✓ {}", response.message);
                let ack_sig = response
                    .ack_sig
                    .or_else(|| response.delta.as_ref().map(|d| d.ack_sig.clone()))
                    .unwrap_or_default();

                if let Some(delta) = response.delta {
                    if !ack_sig.is_empty() {
                        (delta.new_commitment, ack_sig)
                    } else {
                        println!("  ✗ Missing ack signature");
                        return Ok(());
                    }
                } else {
                    println!("  ✗ No delta in response");
                    return Ok(());
                }
            }
            Err(e) => {
                println!("  ✗ Push failed: {}", e);
                return Ok(());
            }
        };
        println!();

        println!("Step 7: Execute transaction with signatures...");

        let tx_summary_commitment_hex =
            format!("0x{}", hex::encode(tx_summary.to_commitment().to_bytes()));

        let ack_sig_with_prefix = if ack_sig.starts_with("0x") {
            ack_sig.clone()
        } else {
            format!("0x{}", ack_sig)
        };

        let server_commitment =
            commitment_from_hex(&server_commitment_hex).expect("Failed to parse server commitment");

        match verify_commitment_signature(
            &tx_summary_commitment_hex,
            &server_commitment_hex,
            &ack_sig_with_prefix,
        ) {
            Ok(true) => {
                let tx_message = tx_summary.to_commitment();

                let ack_signature = match RawFalconSignature::from_hex(&ack_sig_with_prefix) {
                    Ok(sig) => AccountSignature::from(sig),
                    Err(err) => {
                        println!("  ✗ Failed to parse PSM signature: {}", err);
                        return Ok(());
                    }
                };

                let cosigner1_signature =
                    AccountSignature::from(client1_secret_key.sign(tx_message));
                let cosigner2_signature =
                    AccountSignature::from(client2_secret_key.sign(tx_message));

                let signature_advice = vec![
                    multisig::build_signature_advice_entry(
                        server_commitment,
                        tx_message,
                        &ack_signature,
                    ),
                    multisig::build_signature_advice_entry(
                        signer_commitments[0],
                        tx_message,
                        &cosigner1_signature,
                    ),
                    multisig::build_signature_advice_entry(
                        signer_commitments[1],
                        tx_message,
                        &cosigner2_signature,
                    ),
                ];

                let (final_tx_request, _final_config_hash) =
                    match multisig::build_update_signers_transaction_request(
                        3,
                        &signer_commitments,
                        salt,
                        signature_advice,
                    ) {
                        Ok(req) => req,
                        Err(err) => {
                            println!("  ✗ Failed to build final transaction request: {}", err);
                            return Ok(());
                        }
                    };

                let tx_result = match miden_client
                    .execute_transaction(account.id(), final_tx_request)
                    .await
                {
                    Ok(result) => result,
                    Err(e) => {
                        println!("  ✗ Execution failed: {}", e);
                        return Ok(());
                    }
                };

                println!(
                    "  ✓ Transaction executed (nonce: {})",
                    tx_result.account_delta().nonce_delta().as_int()
                );
            }
            Ok(false) => {
                println!("  ✗ Invalid PSM signature");
            }
            Err(e) => {
                println!("  ✗ Verification error: {}", e);
            }
        }
    }

    println!("\n=== Flow completed ===");
    Ok(())
}
