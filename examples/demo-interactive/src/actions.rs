use miden_client::ClientError;
use miden_objects::account::Signature as AccountSignature;
use miden_objects::crypto::dsa::rpo_falcon512::Signature as RpoFalconSignature;
use miden_objects::{Felt, Word};
use private_state_manager_client::{verify_commitment_signature, AuthConfig, MidenFalconRpoAuth, ToJson};
use private_state_manager_shared::hex::FromHex;
use rand::RngCore;
use rustyline::DefaultEditor;

use crate::display::{
    print_section, print_success, print_waiting, print_keypair_generated,
    print_account_info, print_storage_overview, print_connection_status, print_full_hex,
    print_info, shorten_hex,
};
use crate::falcon::generate_falcon_keypair;
use crate::helpers::{add_account_and_sync, commitment_from_hex};
use crate::menu::prompt_input;
use crate::multisig::create_multisig_psm_account;
use crate::state::SessionState;

pub async fn action_generate_keypair(state: &mut SessionState) -> Result<(), String> {
    print_waiting("Generating Falcon keypair");

    let keystore = state.get_keystore();
    let (pubkey_hex, commitment_hex, secret_key) = generate_falcon_keypair(keystore)?;

    state.set_keypair(pubkey_hex.clone(), commitment_hex.clone(), secret_key);

    print_keypair_generated(&pubkey_hex, &commitment_hex);
    print_success("Keypair generated and added to keystore");

    Ok(())
}

pub async fn action_create_account(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Create Multisig Account");

    let threshold: u64 = prompt_input(editor, "Enter threshold (e.g., 2): ")?
        .parse()
        .map_err(|_| "Invalid threshold")?;

    let num_cosigners: usize = prompt_input(editor, "Enter number of cosigners (including you): ")?
        .parse()
        .map_err(|_| "Invalid number")?;

    if num_cosigners < threshold as usize {
        return Err("Number of cosigners must be >= threshold".to_string());
    }

    let mut cosigner_commitments = Vec::new();
    let mut cosigner_pubkeys = Vec::new();

    let user_commitment = state.get_commitment_hex()?;
    let user_pubkey = state.get_pubkey_hex()?;

    cosigner_commitments.push(user_commitment.to_string());
    cosigner_pubkeys.push(user_pubkey.to_string());

    println!("\nYour public key: {}", shorten_hex(user_pubkey));
    println!("Your commitment (derived): {}", shorten_hex(user_commitment));
    println!("\nEnter public keys for other cosigners (commitments will be derived automatically):");

    use crate::falcon::pubkey_to_commitment;

    for i in 1..num_cosigners {
        let pubkey = prompt_input(editor, &format!("  Cosigner {} public key: ", i + 1))?;

        // Validate public key format
        hex::decode(pubkey.strip_prefix("0x").unwrap_or(&pubkey))
            .map_err(|_| format!("Invalid public key hex for cosigner {}", i + 1))?;

        // Derive commitment from public key
        let commitment = pubkey_to_commitment(&pubkey)?;

        println!("    Derived commitment: {}", shorten_hex(&commitment));

        cosigner_commitments.push(commitment);
        cosigner_pubkeys.push(pubkey);
    }

    let psm_client = state.get_psm_client_mut()?;
    print_waiting("Fetching PSM server public key");

    let psm_pubkey_hex = psm_client.get_pubkey()
        .await
        .map_err(|e| format!("Failed to get PSM pubkey: {}", e))?;

    println!("PSM Public Key: {}", shorten_hex(&psm_pubkey_hex));

    print_waiting("Creating multisig account");

    let mut rng = state.create_rng();
    let mut init_seed = [0u8; 32];
    rng.fill_bytes(&mut init_seed);

    let cosigner_refs: Vec<&str> = cosigner_commitments.iter().map(|s| s.as_str()).collect();
    let account = create_multisig_psm_account(
        threshold,
        &cosigner_refs,
        &psm_pubkey_hex,
        init_seed,
    );

    print_waiting("Adding account to Miden client");

    let miden_client = state.get_miden_client_mut()?;
    add_account_and_sync(miden_client, &account).await?;

    let account_id = account.id();
    state.set_account(account);
    state.set_cosigner_pubkeys(cosigner_pubkeys);
    state.cosigner_commitments = cosigner_commitments;

    print_success(&format!("Account created: {}", shorten_hex(&account_id.to_string())));

    Ok(())
}

pub async fn action_configure_psm(state: &mut SessionState) -> Result<(), String> {
    print_section("Configure Account in PSM");

    let account = state.get_account()?;
    let account_id = account.id();

    let cosigner_pubkeys = state.get_cosigner_pubkeys().to_vec();
    if cosigner_pubkeys.is_empty() {
        return Err("No cosigner public keys found. Create account first.".to_string());
    }

    use base64::Engine;
    use miden_client::Serializable;
    let account_bytes = account.to_bytes();
    let account_base64 = base64::engine::general_purpose::STANDARD.encode(&account_bytes);

    let auth_config = AuthConfig {
        auth_type: Some(private_state_manager_client::auth_config::AuthType::MidenFalconRpo(
            MidenFalconRpoAuth {
                cosigner_pubkeys,
            },
        )),
    };

    let initial_state = serde_json::json!({
        "data": account_base64,
        "account_id": account_id.to_string(),
    });

    print_waiting("Configuring PSM authentication");
    state.configure_psm_auth()?;

    print_waiting("Configuring account in PSM");

    let psm_client = state.get_psm_client_mut()?;

    let response = psm_client
        .configure(&account_id, auth_config, initial_state, "Filesystem")
        .await
        .map_err(|e| format!("PSM configuration failed: {}", e))?;

    print_success(&format!("Account configured in PSM: {}", response.message));
    print_full_hex("  Account ID", &account_id.to_string());

    Ok(())
}

pub async fn action_pull_from_psm(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    use miden_client::account::Account;
    use miden_client::Deserializable;
    use miden_objects::account::AccountId;

    print_section("Pull Account from PSM");

    let account_id_hex = prompt_input(editor, "Enter account ID: ")?;
    let account_id = AccountId::from_hex(&account_id_hex)
        .map_err(|e| format!("Invalid account ID: {}", e))?;

    print_waiting("Configuring PSM authentication");
    state.configure_psm_auth()?;

    print_waiting("Fetching account from PSM");

    let psm_client = state.get_psm_client_mut()?;
    let account_state_response = psm_client.get_state(&account_id)
        .await
        .map_err(|e| format!("Failed to get account state: {}", e))?;

    print_waiting("Deserializing account data");

    let state_json = account_state_response.state
        .ok_or_else(|| "No state returned from PSM".to_string())?
        .state_json;

    let state_value: serde_json::Value = serde_json::from_str(&state_json)
        .map_err(|e| format!("Failed to parse state JSON: {}", e))?;

    let account_base64 = state_value["data"]
        .as_str()
        .ok_or_else(|| "Missing 'data' field in state".to_string())?;

    use base64::Engine;
    let account_bytes = base64::engine::general_purpose::STANDARD
        .decode(account_base64)
        .map_err(|e| format!("Failed to decode account data: {}", e))?;

    let account = Account::read_from_bytes(&account_bytes)
        .map_err(|e| format!("Failed to deserialize account: {}", e))?;

    print_waiting("Adding account to Miden client");

    let miden_client = state.get_miden_client_mut()?;
    add_account_and_sync(miden_client, &account).await?;

    // Extract commitments from account storage so they're available for add_cosigner
    use crate::account_inspector::AccountInspector;
    let inspector = AccountInspector::new(&account);
    let commitments = inspector.extract_cosigner_commitments();
    state.cosigner_commitments = commitments;

    state.set_account(account);

    print_success("Account pulled successfully and added to local client");
    print_full_hex("  Account ID", &account_id.to_string());

    Ok(())
}

pub async fn action_pull_deltas_from_psm(state: &mut SessionState) -> Result<(), String> {
    print_section("Pull Deltas from PSM");

    let account = state.get_account()?;
    let account_id = account.id();
    let current_nonce = account.nonce().as_int();

    print_waiting("Configuring PSM authentication");
    state.configure_psm_auth()?;

    print_waiting(&format!("Fetching deltas since nonce {}", current_nonce));

    let psm_client = state.get_psm_client_mut()?;
    let delta_response = psm_client.get_delta_since(&account_id, current_nonce)
        .await
        .map_err(|e| format!("Failed to get deltas: {}", e))?;

    if let Some(merged_delta) = delta_response.merged_delta {
        println!("\nReceived merged delta:");
        println!("  Delta payload: {} bytes", merged_delta.delta_payload.len());

        print_success("Deltas pulled successfully");
        print_info("Note: Apply delta functionality not yet implemented");
    } else {
        print_info("No new deltas found");
    }

    Ok(())
}

pub async fn action_add_cosigner(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    use crate::multisig::build_update_signers_transaction_request;

    print_section("Add Cosigner (Update to N+1)");

    let account = state.get_account()?;
    let account_id = account.id();
    let current_nonce = account.nonce().as_int();

    let prev_commitment = format!("0x{}", hex::encode(account.commitment().to_bytes()));

    // Step 1: Prompt for new cosigner public key (derive commitment)
    print_info("Enter the new cosigner's public key:");
    let new_cosigner_pubkey_hex = prompt_input(editor, "  New cosigner public key: ")?;

    // Derive commitment from public key
    use crate::falcon::pubkey_to_commitment;
    let new_cosigner_commitment_hex = pubkey_to_commitment(&new_cosigner_pubkey_hex)?;
    let new_cosigner_commitment = commitment_from_hex(&new_cosigner_commitment_hex)?;

    println!("  Derived commitment: {}", shorten_hex(&new_cosigner_commitment_hex));

    // Get current cosigner commitments from storage
    let storage = account.storage();
    let config_word = storage.get_item(0)
        .map_err(|e| format!("Failed to get multisig config: {}", e))?;

    let current_threshold = config_word[0].as_int();
    let current_num_cosigners = config_word[1].as_int();

    print_info(&format!("Current config: {}-of-{}", current_threshold, current_num_cosigners));
    print_info(&format!("New config will be: {}-of-{}", current_threshold + 1, current_num_cosigners + 1));

    // Extract existing commitments from account storage (Slot 1)
    use crate::account_inspector::AccountInspector;
    let inspector = AccountInspector::new(account);
    let existing_commitments_hex = inspector.extract_cosigner_commitments();

    if existing_commitments_hex.len() != current_num_cosigners as usize {
        return Err(format!(
            "Extracted commitments mismatch: found {}, expected {}",
            existing_commitments_hex.len(),
            current_num_cosigners
        ));
    }

    print_info(&format!("Extracted {} existing commitments from account storage", existing_commitments_hex.len()));

    let mut signer_commitments: Vec<Word> = existing_commitments_hex
        .iter()
        .map(|hex| commitment_from_hex(hex))
        .collect::<Result<Vec<_>, _>>()?;

    // Add the new cosigner
    signer_commitments.push(new_cosigner_commitment);

    // Update stored commitments for future use
    state.cosigner_commitments = existing_commitments_hex.clone();
    state.cosigner_commitments.push(new_cosigner_commitment_hex.clone());

    let new_threshold = current_threshold + 1;

    // Step 2: Build and simulate transaction
    print_waiting("Building update_signers transaction");

    let salt = Word::from([
        Felt::new(rand::random()),
        Felt::new(0),
        Felt::new(0),
        Felt::new(0),
    ]);

    let (tx_request, _config_hash) = build_update_signers_transaction_request(
        new_threshold,
        &signer_commitments,
        salt,
        vec![], // No signatures yet - this is for simulation
    )
    .map_err(|e| format!("Failed to build transaction request: {}", e))?;

    print_waiting("Simulating transaction to get summary");

    let miden_client = state.get_miden_client_mut()?;
    let tx_summary = match miden_client.new_transaction(account_id, tx_request.clone()).await {
        Err(ClientError::TransactionExecutorError(
            miden_client::transaction::TransactionExecutorError::Unauthorized(summary)
        )) => {
            print_success("Transaction summary created via simulation");
            summary
        }
        Ok(_) => {
            return Err("Expected Unauthorized error but transaction succeeded".to_string());
        }
        Err(e) => {
            return Err(format!("Simulation failed: {}", e));
        }
    };

    // Step 3: Store pending transaction and automatically sign with own key
    print_waiting("Storing pending transaction");

    let tx_summary_json = serde_json::to_string(&tx_summary.to_json())
        .map_err(|e| format!("Failed to serialize tx summary: {}", e))?;
    let tx_summary_commitment = tx_summary.to_commitment();
    let tx_summary_commitment_hex = format!("0x{}", hex::encode(tx_summary_commitment.to_bytes()));

    // Get PSM commitment for later
    let psm_client = state.get_psm_client_mut()?;
    let psm_pubkey_hex = psm_client.get_pubkey()
        .await
        .map_err(|e| format!("Failed to get PSM pubkey: {}", e))?;
    let psm_commitment_hex = psm_pubkey_hex.clone();

    // Automatically sign with own key
    let user_secret_key = state.get_secret_key()?;
    let user_signature_raw = user_secret_key.sign(tx_summary_commitment);
    let user_commitment_hex = state.get_commitment_hex()?;

    // Convert to AccountSignature and then to hex
    let user_signature = AccountSignature::from(user_signature_raw);
    let user_signature_hex = format!("0x{}", hex::encode(user_signature.to_bytes()));

    use crate::pending_tx::PendingTransaction;
    use std::collections::HashMap;
    use miden_client::Serializable;

    let mut collected_signatures = HashMap::new();
    collected_signatures.insert(user_commitment_hex.to_string(), user_signature_hex.clone());

    let salt_hex = format!("0x{}", hex::encode(salt.to_bytes()));

    // All commitments for the final transaction (including new cosigner)
    let signer_commitments_hex: Vec<String> = signer_commitments.iter()
        .map(|w| format!("0x{}", hex::encode(w.to_bytes())))
        .collect();

    // Only existing commitments need to sign (NOT the new cosigner being added)
    let signers_required_hex = existing_commitments_hex.clone();

    // Store the new cosigner's public key for later use in configure_auth
    let signer_pubkeys_hex = vec![new_cosigner_pubkey_hex];

    let pending_tx = PendingTransaction {
        tx_summary_json,
        tx_summary_commitment_hex: tx_summary_commitment_hex.clone(),
        new_threshold,
        signer_commitments_hex,
        signer_pubkeys_hex, // Just the NEW cosigner's pubkey (we'll get existing ones from PSM later)
        signers_required_hex,
        salt_hex,
        psm_commitment_hex,
        current_nonce,
        prev_commitment,
        collected_signatures,
    };

    state.pending_tx_store.save(&pending_tx)
        .map_err(|e| format!("Failed to save pending transaction: {}", e))?;

    print_success("Transaction summary created and stored locally");
    print_success(&format!("Automatically signed with your key ({})", shorten_hex(user_commitment_hex)));
    print_full_hex("\nTransaction Summary Commitment", &tx_summary_commitment_hex);

    print_info(&format!("\nSignatures collected: 1/{}", current_num_cosigners));
    print_info("\nNext steps:");
    print_info("  1. Share the commitment above with other cosigners");
    print_info("  2. Other cosigners use option [7] 'Sign pending transaction'");
    print_info(&format!("  3. Once you have {}/{} signatures, use option [8] 'Finalize pending transaction'", current_num_cosigners, current_num_cosigners));

    Ok(())
}

pub async fn action_sign_transaction(state: &mut SessionState) -> Result<(), String> {
    print_section("Sign Pending Transaction");

    // Load pending transaction
    let pending_tx = state.pending_tx_store.load()
        .map_err(|e| format!("Failed to load pending transaction: {}", e))?;

    print_full_hex("Transaction Summary Commitment", &pending_tx.tx_summary_commitment_hex);

    // Check if already signed
    let user_commitment_hex = state.get_commitment_hex()?;
    if pending_tx.collected_signatures.contains_key(user_commitment_hex) {
        print_info(&format!("You have already signed this transaction (commitment: {})", shorten_hex(user_commitment_hex)));
        print_info(&format!("Signatures collected: {}/{}", pending_tx.collected_signatures.len(), pending_tx.signers_required_hex.len()));
        return Ok(());
    }

    // Automatically sign with own key
    print_waiting("Signing transaction with your key");

    let user_secret_key = state.get_secret_key()?;
    let tx_summary_commitment = pending_tx.tx_summary_commitment();
    let user_signature_raw = user_secret_key.sign(tx_summary_commitment);

    // Convert to AccountSignature and then to hex
    use miden_client::Serializable;
    let user_signature = AccountSignature::from(user_signature_raw);
    let user_signature_hex = format!("0x{}", hex::encode(user_signature.to_bytes()));

    // Add signature to pending transaction
    state.pending_tx_store.add_signature(user_commitment_hex.to_string(), user_signature_hex)
        .map_err(|e| format!("Failed to add signature: {}", e))?;

    print_success(&format!("Signed with your key ({})", shorten_hex(user_commitment_hex)));

    // Reload to show updated count
    let updated_pending_tx = state.pending_tx_store.load()
        .map_err(|e| format!("Failed to reload pending transaction: {}", e))?;

    print_info(&format!("Signatures collected: {}/{}", updated_pending_tx.collected_signatures.len(), updated_pending_tx.signers_required_hex.len()));

    if updated_pending_tx.collected_signatures.len() >= updated_pending_tx.signers_required_hex.len() {
        print_success("\n✓ All signatures collected!");
        print_info("The proposer can now use option [8] 'Finalize pending transaction'");
    }

    Ok(())
}

pub async fn action_finalize_pending_transaction(state: &mut SessionState) -> Result<(), String> {
    use crate::multisig::build_signature_advice_entry;

    print_section("Finalize Pending Transaction");

    // Load pending transaction
    let pending_tx = state.pending_tx_store.load()
        .map_err(|e| format!("Failed to load pending transaction: {}", e))?;

    print_info(&format!("Signatures collected: {}/{}", pending_tx.collected_signatures.len(), pending_tx.signers_required_hex.len()));

    // Check if we have all signatures from cosigners
    if pending_tx.collected_signatures.len() < pending_tx.signers_required_hex.len() {
        return Err(format!(
            "Not enough signatures collected. Need {}, have {}",
            pending_tx.signers_required_hex.len(),
            pending_tx.collected_signatures.len()
        ));
    }

    // Step 1: Push to PSM to get PSM signature
    print_waiting("Configuring PSM authentication");
    state.configure_psm_auth()?;

    print_waiting("Pushing transaction summary to PSM");

    let account = state.get_account()?;
    let account_id = account.id();

    // Parse the tx_summary JSON string back to a serde_json::Value
    let tx_summary_payload: serde_json::Value = serde_json::from_str(&pending_tx.tx_summary_json)
        .map_err(|e| format!("Failed to parse tx summary JSON: {}", e))?;

    let psm_client = state.get_psm_client_mut()?;
    let push_response = psm_client.push_delta(
        &account_id,
        pending_tx.current_nonce,
        pending_tx.prev_commitment.clone(),
        tx_summary_payload,
    )
    .await
    .map_err(|e| format!("Failed to push delta to PSM: {}", e))?;

    print_success(&format!("Delta pushed to PSM: {}", push_response.message));

    // Get PSM acknowledgment signature
    let ack_sig = push_response
        .ack_sig
        .or_else(|| push_response.delta.as_ref().map(|d| d.ack_sig.clone()))
        .ok_or_else(|| "Missing ack signature in PSM response".to_string())?;

    if ack_sig.is_empty() {
        return Err("PSM ack signature is empty".to_string());
    }

    print_success("Received PSM acknowledgment signature");

    // Step 2: Build signature advice with PSM + all collected signatures
    let mut signature_advice = Vec::new();

    let tx_summary_commitment = pending_tx.tx_summary_commitment();
    let tx_summary_commitment_hex = &pending_tx.tx_summary_commitment_hex;

    // Add PSM signature
    let psm_pubkey_hex = psm_client.get_pubkey()
        .await
        .map_err(|e| format!("Failed to get PSM pubkey: {}", e))?;

    let ack_sig_with_prefix = if ack_sig.starts_with("0x") {
        ack_sig.clone()
    } else {
        format!("0x{}", ack_sig)
    };

    // Verify PSM signature
    verify_commitment_signature(tx_summary_commitment_hex, &psm_pubkey_hex, &ack_sig_with_prefix)
        .map_err(|e| format!("PSM signature verification failed: {}", e))?;

    let ack_signature = RpoFalconSignature::from_hex(&ack_sig_with_prefix)
        .map_err(|e| format!("Failed to parse PSM signature: {}", e))?;

    let psm_commitment = pending_tx.psm_commitment();
    signature_advice.push(build_signature_advice_entry(
        psm_commitment,
        tx_summary_commitment,
        &AccountSignature::from(ack_signature),
    ));

    // Add all collected cosigner signatures (only from signers_required, not including new cosigner)
    for (i, commitment_hex) in pending_tx.signers_required_hex.iter().enumerate() {
        let sig_hex = pending_tx.collected_signatures.get(commitment_hex)
            .ok_or_else(|| format!("Missing signature for cosigner {} (commitment: {})", i + 1, shorten_hex(commitment_hex)))?;

        let sig = RpoFalconSignature::from_hex(sig_hex)
            .map_err(|e| format!("Invalid signature from cosigner {}: {}", i + 1, e))?;

        let commitment = commitment_from_hex(commitment_hex)?;
        signature_advice.push(build_signature_advice_entry(
            commitment,
            tx_summary_commitment,
            &AccountSignature::from(sig),
        ));
    }

    print_success(&format!("Built signature advice with {} signatures (PSM + {} cosigners)",
        signature_advice.len(), pending_tx.signers_required_hex.len()));

    // Step 3: Build final transaction and execute on-chain
    print_waiting("Building final transaction request");

    use crate::multisig::build_update_signers_transaction_request;

    let salt = pending_tx.salt();
    let signer_commitments = pending_tx.signer_commitments();
    let (final_tx_request, _final_config_hash) = build_update_signers_transaction_request(
        pending_tx.new_threshold,
        &signer_commitments,
        salt,
        signature_advice,
    )
    .map_err(|e| format!("Failed to build final transaction request: {}", e))?;

    print_waiting("Executing transaction on-chain");

    let miden_client = state.get_miden_client_mut()?;
    let tx_result = miden_client.new_transaction(account_id, final_tx_request).await
        .map_err(|e| format!("Transaction execution failed: {}", e))?;

    let new_nonce = tx_result.account_delta().nonce_delta().as_int();

    print_success(&format!("✓ Transaction finalized! New nonce: {}", new_nonce));
    print_info(&format!("New configuration: {}-of-{}", pending_tx.new_threshold, pending_tx.signer_commitments_hex.len()));

    // Step 4: Update PSM auth configuration with new cosigner list
    print_waiting("Updating PSM authentication configuration");

    // First, get current public keys from PSM metadata
    let psm_client = state.get_psm_client_mut()?;
    let state_response = psm_client.get_state(&account_id)
        .await
        .map_err(|e| format!("Failed to get PSM state: {}", e))?;

    // Extract current public keys from PSM response (if available in metadata)
    // Note: PSM doesn't expose metadata in get_state, so we'll need to build the list ourselves
    // For now, we'll use a simpler approach: extract all commitments from the updated account state
    // and match them with the public keys we have

    // Parse the updated account state from PSM
    let state_json: serde_json::Value = serde_json::from_str(&state_response.state.unwrap().state_json)
        .map_err(|e| format!("Failed to parse state JSON: {}", e))?;

    // Extract updated commitments from storage slot 1
    use crate::account_inspector::AccountInspector;
    use private_state_manager_shared::FromJson;
    let updated_account = miden_objects::account::Account::from_json(&state_json)
        .map_err(|e| format!("Failed to parse account: {}", e))?;
    let inspector = AccountInspector::new(&updated_account);
    let updated_commitments = inspector.extract_cosigner_commitments();

    // Build complete pubkey list: we only have the NEW cosigner's pubkey
    // For a production system, we should retrieve this from PSM metadata or store it locally
    // For the demo, we'll note that configure_auth will fail if we don't have all pubkeys

    print_info(&format!("Note: This is a demo limitation - we only have the new cosigner's public key"));
    print_info(&format!("In production, retrieve all public keys from PSM metadata or local storage"));
    print_info(&format!("Skipping PSM auth config update for now"));

    // TODO: Implement proper public key retrieval from PSM or local storage
    // For now, comment out the configure_auth call
    //
    // use private_state_manager_client::{AuthConfig, MidenFalconRpoAuth};
    // let auth_config = AuthConfig {
    //     auth_type: Some(private_state_manager_client::auth_config::AuthType::MidenFalconRpo(
    //         MidenFalconRpoAuth {
    //             cosigner_pubkeys: all_cosigner_pubkeys,
    //         }
    //     )),
    // };
    //
    // psm_client.configure_auth(&account_id, auth_config)
    //     .await
    //     .map_err(|e| format!("Failed to update PSM auth config: {}", e))?;
    //
    // print_success("PSM authentication configuration updated successfully");

    // Clear pending transaction
    state.pending_tx_store.clear()
        .map_err(|e| format!("Failed to clear pending transaction: {}", e))?;

    print_info("Pending transaction cleared");
    print_info("Note: Use 'Pull deltas from PSM' or sync with node to get updated account state");

    Ok(())
}

pub async fn action_show_account(state: &SessionState) -> Result<(), String> {
    let account = state.get_account()?;

    print_account_info(account);
    print_storage_overview(account);

    Ok(())
}

pub async fn action_show_status(state: &SessionState) -> Result<(), String> {
    print_connection_status(state.is_psm_connected(), state.is_miden_connected());

    if state.has_account() {
        let account_id = state.get_account_id()?;
        println!("\n  Current Account: {}", shorten_hex(&account_id.to_string()));
    } else {
        println!("\n  No account loaded");
    }

    if state.has_keypair() {
        let commitment = state.get_commitment_hex()?;
        println!("  Your Commitment: {}", shorten_hex(commitment));
    } else {
        println!("  No keypair generated");
    }

    Ok(())
}
