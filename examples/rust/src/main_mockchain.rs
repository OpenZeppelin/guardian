mod falcon;
mod multisig;

use miden_client::account::Account;
use miden_client::keystore::FilesystemKeyStore;
use miden_client::testing::MockChainBuilder;
use miden_client::transaction::TransactionExecutorError;
use miden_client::vm::{AdviceInputs, AdviceMap};
use miden_client::{Deserializable, Felt, Serializable, Word};

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

/// Converts a hex-encoded public key commitment (with or without 0x prefix) into a `Word`.
pub fn commitment_from_hex(hex_commitment: &str) -> Result<Word, String> {
    let trimmed = hex_commitment.strip_prefix("0x").unwrap_or(hex_commitment);
    let bytes = hex::decode(trimmed)
        .map_err(|err| format!("Failed to decode commitment hex '{hex_commitment}': {err}"))?;

    Word::read_from_bytes(&bytes)
        .map_err(|err| format!("Failed to deserialize commitment word '{hex_commitment}': {err}"))
}

#[tokio::main]
async fn main() -> ClientResult<()> {
    println!("=== PSM Multi-Client E2E Flow (MockChain) ===\n");

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

    let server_commitment_hex = match client1.get_pubkey().await {
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

    let server_commitment =
        commitment_from_hex(&server_commitment_hex).expect("Failed to parse server commitment");
    println!();

    println!("Step 2: Creating multisig PSM account...");

    let init_seed = [0xff; 32];
    let account = multisig::create_multisig_psm_account(
        &client1_commitment_hex,
        &client2_commitment_hex,
        &server_commitment_hex,
        init_seed,
    );

    let account_id = account.id();
    println!("  ✓ Account ID: {}", account_id);
    println!("  ✓ Multisig: 2-of-2 with PSM");
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

    match client1
        .configure(&account_id, auth_config, initial_state, "Filesystem")
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

    let mut client2 = PsmClient::connect(psm_endpoint.clone())
        .await
        .expect("Failed to connect")
        .with_auth(client2_auth);

    let retrieved_account = match client2.get_state(&account_id).await {
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

        let (config_hash, config_values) =
            multisig::build_multisig_config_advice(3, &signer_commitments);

        let tx_script = match multisig::build_update_signers_script() {
            Ok(script) => script,
            Err(err) => {
                println!("  ✗ Failed to build transaction script: {}", err);
                return Ok(());
            }
        };

        let mock_chain = match MockChainBuilder::with_accounts([account.clone()]) {
            Ok(builder) => builder.build().unwrap(),
            Err(err) => {
                println!("  ✗ Failed to create MockChain: {}", err);
                return Ok(());
            }
        };

        let mut advice_map = AdviceMap::default();
        advice_map.insert(config_hash, config_values.clone());
        let advice_inputs = AdviceInputs {
            map: advice_map,
            ..Default::default()
        };

        let tx_context_init = match mock_chain.build_tx_context(account.id(), &[], &[]) {
            Ok(builder) => match builder
                .tx_script(tx_script.clone())
                .tx_script_args(config_hash)
                .extend_advice_inputs(advice_inputs.clone())
                .auth_args(salt)
                .build()
            {
                Ok(ctx) => ctx,
                Err(err) => {
                    println!("  ✗ Failed to build transaction context: {}", err);
                    return Ok(());
                }
            },
            Err(err) => {
                println!("  ✗ Failed to create transaction context: {}", err);
                return Ok(());
            }
        };

        let tx_summary = match tx_context_init.execute().await {
            Err(TransactionExecutorError::Unauthorized(tx_effects)) => {
                println!("  ✓ Transaction summary created");
                tx_effects
            }
            Ok(_) => {
                println!("  ✗ Expected Unauthorized error but transaction succeeded");
                return Ok(());
            }
            Err(err) => {
                println!("  ✗ Simulation failed: {:?}", err);
                return Ok(());
            }
        };
        println!();

        println!("Step 6: Push transaction summary to PSM...");

        let tx_summary_json = tx_summary.to_json();
        let prev_commitment = format!("0x{}", hex::encode(account.commitment().as_bytes()));

        let (_new_commitment, ack_sig) = match client2
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

                let mut final_advice_map = AdviceMap::default();
                final_advice_map.insert(config_hash, config_values);
                for (key, value) in signature_advice {
                    final_advice_map.insert(key, value);
                }
                let final_advice_inputs = AdviceInputs {
                    map: final_advice_map,
                    ..Default::default()
                };

                let tx_result = match mock_chain.build_tx_context(account.id(), &[], &[]) {
                    Ok(builder) => match builder
                        .tx_script(tx_script)
                        .tx_script_args(config_hash)
                        .add_signature(server_commitment.into(), tx_message, ack_signature)
                        .add_signature(
                            signer_commitments[0].into(),
                            tx_message,
                            cosigner1_signature,
                        )
                        .add_signature(
                            signer_commitments[1].into(),
                            tx_message,
                            cosigner2_signature,
                        )
                        .extend_advice_inputs(final_advice_inputs)
                        .auth_args(salt)
                        .build()
                    {
                        Ok(ctx) => match ctx.execute().await {
                            Ok(result) => result,
                            Err(e) => {
                                println!("  ✗ Execution failed: {:?}", e);
                                return Ok(());
                            }
                        },
                        Err(err) => {
                            println!("  ✗ Build failed: {}", err);
                            return Ok(());
                        }
                    },
                    Err(err) => {
                        println!("  ✗ Context creation failed: {}", err);
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
