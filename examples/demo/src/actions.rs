use miden_client::ClientError;
use miden_objects::account::Signature as AccountSignature;
use miden_objects::crypto::dsa::rpo_falcon512::Signature as RpoFalconSignature;
use miden_objects::transaction::TransactionSummary;
use miden_objects::utils::Serializable;
use miden_objects::{Felt, Word};
use private_state_manager_client::{
    verify_commitment_signature, AuthConfig, FromJson, MidenFalconRpoAuth, ToJson,
};
use private_state_manager_shared::hex::FromHex;
use rand::RngCore;
use rustyline::DefaultEditor;

use crate::display::{
    print_account_info, print_connection_status, print_full_hex, print_info,
    print_keypair_generated, print_section, print_storage_overview, print_success, print_waiting,
    shorten_hex,
};
use crate::falcon::generate_falcon_keypair;
use crate::helpers::commitment_from_hex;
use crate::menu::prompt_input;
use crate::multisig::create_multisig_psm_account;
use crate::state::SessionState;

pub async fn action_generate_keypair(state: &mut SessionState) -> Result<(), String> {
    print_waiting("Generating Falcon keypair");

    let keystore = state.get_keystore();
    let (commitment_hex, secret_key) = generate_falcon_keypair(keystore)?;

    state.set_keypair(commitment_hex.clone(), secret_key);

    print_keypair_generated(&commitment_hex);
    print_success("Keypair generated and added to keystore");

    Ok(())
}

pub async fn action_view_proposals(state: &mut SessionState) -> Result<(), String> {
    use crate::proposals::{extract_proposal_metadata, count_signatures, get_signers};

    print_section("View Pending Proposals");

    let account = state.get_account()?;
    let account_id = account.id();

    // Fetch all proposals from server
    let psm_client = state.get_psm_client_mut()?;
    let proposals_response = psm_client
        .get_delta_proposals(&account_id)
        .await
        .map_err(|e| format!("Failed to fetch proposals: {}", e))?;

    let proposals = proposals_response.proposals;
    if proposals.is_empty() {
        print_info("No pending proposals found for this account");
        return Ok(());
    }

    print_info(&format!("\nFound {} pending proposal(s):", proposals.len()));
    println!();

    for (idx, proposal) in proposals.iter().enumerate() {
        let metadata = extract_proposal_metadata(proposal);
        let signature_count = count_signatures(proposal);
        let signers = get_signers(proposal);

        println!("  [{}] Proposal (nonce: {})", idx + 1, proposal.nonce);
        println!("      Type: {}", metadata.proposal_type);
        println!("      Signatures: {}", signature_count);

        // Show signer list if any
        if !signers.is_empty() {
            println!("      Signers:");
            for signer in &signers {
                println!("        - {}", shorten_hex(signer));
            }
        }

        // Show the proposal details based on type
        if metadata.proposal_type == "add_cosigner" {
            if let Some(new_threshold) = metadata.new_threshold {
                let current_threshold = new_threshold - 1;
                let current_signers = metadata.signers_required_hex.len();
                let new_signers = metadata.signer_commitments_hex.len();

                println!("      Current config: {}-of-{}", current_threshold, current_signers);
                println!("      New config:     {}-of-{}", new_threshold, new_signers);
            }
        }
        println!();
    }

    print_info("\nUse option [8] to sign a proposal");
    print_info("Use option [9] to finalize a proposal");

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

    let user_commitment = state.get_commitment_hex()?;

    cosigner_commitments.push(user_commitment.to_string());

    println!("\nYour commitment: {}", shorten_hex(user_commitment));
    println!("\nEnter commitments for other cosigners:");

    for i in 1..num_cosigners {
        let commitment = prompt_input(editor, &format!("  Cosigner {} commitment: ", i + 1))?;

        // Validate commitment format (should be 32 bytes = 64 hex chars + optional 0x prefix)
        let commitment_stripped = commitment.strip_prefix("0x").unwrap_or(&commitment);
        if commitment_stripped.len() != 64 {
            return Err(format!(
                "Invalid commitment length for cosigner {}: expected 64 hex chars, got {}",
                i + 1,
                commitment_stripped.len()
            ));
        }

        hex::decode(commitment_stripped)
            .map_err(|_| format!("Invalid commitment hex for cosigner {}", i + 1))?;

        let commitment_with_prefix = if commitment.starts_with("0x") {
            commitment
        } else {
            format!("0x{}", commitment)
        };

        cosigner_commitments.push(commitment_with_prefix);
    }

    let psm_client = state.get_psm_client_mut()?;
    print_waiting("Fetching PSM server commitment");

    let psm_commitment_hex = psm_client
        .get_pubkey()
        .await
        .map_err(|e| format!("Failed to get PSM commitment: {}", e))?;

    println!("PSM Commitment: {}", shorten_hex(&psm_commitment_hex));

    print_waiting("Creating multisig account");

    let mut rng = state.create_rng();
    let mut init_seed = [0u8; 32];
    rng.fill_bytes(&mut init_seed);

    let cosigner_refs: Vec<&str> = cosigner_commitments.iter().map(|s| s.as_str()).collect();
    let account =
        create_multisig_psm_account(threshold, &cosigner_refs, &psm_commitment_hex, init_seed);

    print_waiting("Adding account to Miden client");

    let account_id = account.id();
    let miden_client = state.get_miden_client_mut()?;
    miden_client
        .add_account(&account, false)
        .await
        .map_err(|e| e.to_string())?;

    miden_client
        .sync_state()
        .await
        .map_err(|e| format!("Failed to sync client state: {}", e))?;

    state.set_account(account);
    state.cosigner_commitments = cosigner_commitments;

    print_success(&format!(
        "Account created: {}",
        shorten_hex(&account_id.to_string())
    ));

    Ok(())
}


pub async fn action_configure_psm(state: &mut SessionState) -> Result<(), String> {
    print_section("Configure Account in PSM");

    let account = state.get_account()?;
    let account_id = account.id();

    let cosigner_commitments = state.cosigner_commitments.clone();
    if cosigner_commitments.is_empty() {
        return Err("No cosigner commitments found. Create account first.".to_string());
    }

    use base64::Engine;
    use miden_client::Serializable;
    let account_bytes = account.to_bytes();
    let account_base64 = base64::engine::general_purpose::STANDARD.encode(&account_bytes);

    let auth_config = AuthConfig {
        auth_type: Some(
            private_state_manager_client::auth_config::AuthType::MidenFalconRpo(
                MidenFalconRpoAuth {
                    cosigner_commitments,
                },
            ),
        ),
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
    let account_id =
        AccountId::from_hex(&account_id_hex).map_err(|e| format!("Invalid account ID: {}", e))?;

    print_waiting("Configuring PSM authentication");
    state.configure_psm_auth()?;

    print_waiting("Fetching account from PSM");

    let psm_client = state.get_psm_client_mut()?;
    let account_state_response = psm_client
        .get_state(&account_id)
        .await
        .map_err(|e| format!("Failed to get account state: {}", e))?;

    print_waiting("Deserializing account data");

    let state_json = account_state_response
        .state
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
    miden_client
        .add_account(&account, false)
        .await
        .map_err(|e| e.to_string())?;

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
    let delta_response = psm_client
        .get_delta_since(&account_id, current_nonce)
        .await
        .map_err(|e| format!("Failed to get deltas: {}", e))?;

    if let Some(merged_delta) = delta_response.merged_delta {
        println!("\nReceived merged delta:");
        println!(
            "  Delta payload: {} bytes",
            merged_delta.delta_payload.len()
        );

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

    let _prev_commitment = format!("0x{}", hex::encode(account.commitment().as_bytes()));

    // Step 1: Prompt for new cosigner commitment
    print_info("Enter the new cosigner's commitment:");
    let new_cosigner_commitment_hex = prompt_input(editor, "  New cosigner commitment: ")?;

    // Validate commitment format
    let commitment_stripped = new_cosigner_commitment_hex
        .strip_prefix("0x")
        .unwrap_or(&new_cosigner_commitment_hex);
    if commitment_stripped.len() != 64 {
        return Err(format!(
            "Invalid commitment length: expected 64 hex chars, got {}",
            commitment_stripped.len()
        ));
    }

    let new_cosigner_commitment_hex = if new_cosigner_commitment_hex.starts_with("0x") {
        new_cosigner_commitment_hex
    } else {
        format!("0x{}", new_cosigner_commitment_hex)
    };

    let new_cosigner_commitment = commitment_from_hex(&new_cosigner_commitment_hex)?;

    // Get current cosigner commitments from storage
    let storage = account.storage();
    let config_word = storage
        .get_item(0)
        .map_err(|e| format!("Failed to get multisig config: {}", e))?;

    let current_threshold = config_word[0].as_int();
    let current_num_cosigners = config_word[1].as_int();

    print_info(&format!(
        "Current config: {}-of-{}",
        current_threshold, current_num_cosigners
    ));
    print_info(&format!(
        "New config will be: {}-of-{}",
        current_threshold + 1,
        current_num_cosigners + 1
    ));

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

    print_info(&format!(
        "Extracted {} existing commitments from account storage",
        existing_commitments_hex.len()
    ));

    let mut signer_commitments: Vec<Word> = existing_commitments_hex
        .iter()
        .map(|hex| commitment_from_hex(hex))
        .collect::<Result<Vec<_>, _>>()?;

    // Add the new cosigner
    signer_commitments.push(new_cosigner_commitment);

    // Update stored commitments for future use
    state.cosigner_commitments = existing_commitments_hex.clone();
    state
        .cosigner_commitments
        .push(new_cosigner_commitment_hex.clone());

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

    miden_client
        .sync_state()
        .await
        .map_err(|e| format!("Failed to sync client state: {}", e))?;

    let tx_summary = match miden_client
        .new_transaction(account_id, tx_request.clone())
        .await
    {
        Err(ClientError::TransactionExecutorError(
            miden_client::transaction::TransactionExecutorError::Unauthorized(summary),
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

    // Step 3: Push proposal to PSM server and automatically sign with own key
    print_waiting("Pushing proposal to PSM server");

    // Store multisig metadata alongside TransactionSummary for later finalization
    let signer_commitments_hex: Vec<String> = signer_commitments
        .iter()
        .map(|w| format!("0x{}", hex::encode(w.as_bytes())))
        .collect();

    let salt_hex = format!("0x{}", hex::encode(salt.as_bytes()));

    let delta_payload = serde_json::json!({
        "tx_summary": tx_summary.to_json(),
        "new_threshold": new_threshold,
        "signer_commitments_hex": signer_commitments_hex,
        "signers_required_hex": signer_commitments_hex.clone(),
        "salt_hex": salt_hex,
    });

    // Get values from state before borrowing psm_client mutably
    let user_secret_key = state.get_secret_key()?.clone();
    let user_commitment_hex = state.get_commitment_hex()?.to_string();

    // Push the proposal to PSM server
    let psm_client = state.get_psm_client_mut()?;
    let proposal_response = psm_client
        .push_delta_proposal(&account_id, current_nonce, &delta_payload)
        .await
        .map_err(|e| format!("Failed to push proposal to PSM: {}", e))?;

    if !proposal_response.success {
        return Err(format!("Failed to create proposal: {}", proposal_response.message));
    }

    let proposal_commitment = proposal_response.commitment;
    print_success("Proposal created on PSM server");
    print_full_hex("\nProposal ID (Commitment)", &proposal_commitment);

    // Automatically sign with own key
    print_waiting("Signing proposal with your key");

    // Sign the proposal commitment
    let commitment_word = commitment_from_hex(&proposal_commitment)?;
    let user_signature_raw = user_secret_key.sign(commitment_word);
    let user_signature_hex = format!("0x{}", hex::encode(user_signature_raw.to_bytes()));

    // Sign the proposal on the server
    let sign_response = psm_client
        .sign_delta_proposal(
            &account_id,
            &proposal_commitment,
            "falcon",
            &user_signature_hex,
        )
        .await
        .map_err(|e| format!("Failed to sign proposal: {}", e))?;

    if !sign_response.success {
        return Err(format!("Failed to sign proposal: {}", sign_response.message));
    }

    print_success(&format!(
        "Automatically signed with your key ({})",
        shorten_hex(&user_commitment_hex)
    ));

    print_info(&format!(
        "\nSignatures collected: 1/{}",
        current_num_cosigners
    ));
    print_info("\nNext steps:");
    print_info("  1. Share the Proposal ID above with other cosigners");
    print_info("  2. Other cosigners use option [8] 'Sign a proposal'");
    print_info(&format!(
        "  3. Once you have {}/{} signatures, use option [9] 'Finalize a proposal'",
        current_num_cosigners, current_num_cosigners
    ));

    Ok(())
}


pub async fn action_sign_transaction(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    use crate::proposals::{extract_proposal_metadata, has_signer_signed, count_signatures, ProposalMetadata};
    use miden_client::Serializable;

    print_section("Sign a Proposal");

    let account = state.get_account()?;
    let account_id = account.id();

    // Get values from state before borrowing psm_client mutably
    let user_secret_key = state.get_secret_key()?.clone();
    let user_commitment_hex = state.get_commitment_hex()?.to_string();

    // Fetch all proposals from server
    print_waiting("Fetching proposals from PSM server");
    let psm_client = state.get_psm_client_mut()?;
    let proposals_response = psm_client
        .get_delta_proposals(&account_id)
        .await
        .map_err(|e| format!("Failed to fetch proposals: {}", e))?;

    let proposals = proposals_response.proposals;
    if proposals.is_empty() {
        print_info("No pending proposals found for this account");
        return Ok(());
    }

    // Display proposals list
    print_info(&format!("\nFound {} pending proposal(s):", proposals.len()));
    println!();

    // Store proposal metadata and commitments for later use
    let mut proposal_info: Vec<(ProposalMetadata, Option<String>)> = Vec::new();

    for (idx, proposal) in proposals.iter().enumerate() {
        let metadata = extract_proposal_metadata(proposal);
        let tx_commitment = metadata.get_tx_commitment();
        let signature_count = count_signatures(proposal);

        println!("  [{}] Proposal (nonce: {})", idx + 1, proposal.nonce);
        println!("      Type: {}", metadata.proposal_type);
        println!("      Signatures: {}/{}", signature_count, metadata.signers_required_hex.len());

        // We don't have timestamp and proposer info in the current client protocol
        // This would need to be added to the proto or stored in delta_payload

        // Show the tx commitment if available
        if let Some(ref comm) = tx_commitment {
            println!("      TX Commitment: {}", shorten_hex(comm));
        }

        println!();
        proposal_info.push((metadata, tx_commitment));
    }

    // Prompt for selection
    let selection = prompt_input(editor, "Select proposal to sign (number): ")?;
    let idx = selection.parse::<usize>()
        .map_err(|_| "Invalid selection".to_string())?
        .checked_sub(1)
        .ok_or("Invalid selection (use 1-based index)".to_string())?;

    if idx >= proposals.len() {
        return Err("Selection out of range".to_string());
    }

    let selected_proposal = &proposals[idx];
    let (_metadata, tx_commitment) = &proposal_info[idx];
    let tx_commitment = tx_commitment.as_ref()
        .ok_or("Could not extract transaction commitment from proposal".to_string())?;

    // Check if already signed by this user
    if has_signer_signed(selected_proposal, &user_commitment_hex) {
        print_info(&format!(
            "You have already signed this proposal (commitment: {})",
            shorten_hex(&user_commitment_hex)
        ));
        return Ok(());
    }

    // The proposal ID is the transaction summary commitment
    let proposal_id = tx_commitment.clone();

    print_info(&format!("\nProposal ID: {}", shorten_hex(&proposal_id)));
    print_waiting("Signing proposal with your key");

    // Parse the TX commitment to Word for signing
    let commitment_word = commitment_from_hex(&proposal_id)?;
    let user_signature_raw = user_secret_key.sign(commitment_word);
    let user_signature_hex = format!("0x{}", hex::encode(user_signature_raw.to_bytes()));

    // Sign the proposal on the server
    let sign_response = psm_client
        .sign_delta_proposal(
            &account_id,
            &proposal_id,
            "falcon",
            &user_signature_hex,
        )
        .await
        .map_err(|e| format!("Failed to sign proposal: {}", e))?;

    if !sign_response.success {
        return Err(format!("Failed to sign proposal: {}", sign_response.message));
    }

    print_success(&format!(
        "Successfully signed proposal with your key ({})",
        shorten_hex(&user_commitment_hex)
    ));

    // Check updated signature count
    if let Some(updated_delta) = sign_response.delta {
        let updated_metadata = extract_proposal_metadata(&updated_delta);
        let updated_sig_count = count_signatures(&updated_delta);

        print_info(&format!(
            "Signatures collected: {}/{}",
            updated_sig_count,
            updated_metadata.signers_required_hex.len()
        ));

        if updated_metadata.is_ready(updated_sig_count) {
            print_success("\n✓ All signatures collected!");
            print_info("This proposal can now be finalized using option [9]");
        } else {
            print_info("\nWaiting for more signatures...");
        }
    }

    Ok(())
}

pub async fn action_finalize_pending_transaction(state: &mut SessionState) -> Result<(), String> {
    use crate::multisig::build_signature_advice_entry;
    use crate::proposals::{count_signatures, extract_proposal_metadata, get_signers};

    print_section("Finalize Proposal");

    state.configure_psm_auth()?;

    let account = state.get_account()?;
    let account_id = account.id();

    print_waiting("Fetching pending proposals from PSM");
    let psm_client = state.get_psm_client_mut()?;
    let proposals_response = psm_client
        .get_delta_proposals(&account_id)
        .await
        .map_err(|e| format!("Failed to get proposals: {}", e))?;

    let proposals = &proposals_response.proposals;

    if proposals.is_empty() {
        print_info("No pending proposals found");
        return Err("No proposals to finalize".to_string());
    }

    println!("\nPending Proposals:");
    for (idx, proposal) in proposals.iter().enumerate() {
        let metadata = extract_proposal_metadata(proposal);
        let signature_count = count_signatures(proposal);
        let signers = get_signers(proposal);

        println!("  [{}] Proposal (nonce: {})", idx + 1, proposal.nonce);
        println!("      Type: {}", metadata.proposal_type);
        println!("      Signatures: {}", signature_count);

        if !signers.is_empty() {
            println!("      Signers:");
            for signer in &signers {
                println!("        - {}", shorten_hex(signer));
            }
        }
    }

    print!("\nSelect proposal number to finalize: ");
    std::io::Write::flush(&mut std::io::stdout()).map_err(|e| format!("Failed to flush: {}", e))?;
    let mut choice = String::new();
    std::io::stdin()
        .read_line(&mut choice)
        .map_err(|e| format!("Failed to read input: {}", e))?;

    let idx: usize = choice
        .trim()
        .parse()
        .map_err(|_| "Invalid proposal number".to_string())?;

    if idx == 0 || idx > proposals.len() {
        return Err("Invalid proposal number".to_string());
    }

    let proposal = &proposals[idx - 1];
    let metadata = extract_proposal_metadata(proposal);
    let signature_count = count_signatures(proposal);

    print_info(&format!("\nFinalizing proposal (nonce: {})", proposal.nonce));
    print_info(&format!("Type: {}", metadata.proposal_type));
    print_info(&format!("Signatures collected: {}", signature_count));

    let delta_payload_json = proposal.delta_payload.as_ref();
    let payload_wrapper: serde_json::Value = serde_json::from_str(delta_payload_json)
        .map_err(|e| format!("Failed to parse delta payload: {}", e))?;

    let tx_summary_value = payload_wrapper
        .get("tx_summary")
        .ok_or("Missing tx_summary in delta payload")?;

    let tx_summary = TransactionSummary::from_json(tx_summary_value)
        .map_err(|e| format!("Failed to deserialize transaction summary: {}", e))?;

    let tx_summary_commitment = tx_summary.to_commitment();
    let tx_summary_commitment_hex = format!("0x{}", hex::encode(tx_summary_commitment.as_bytes()));

    let mut signature_advice = Vec::new();

    let ack_sig: &str = proposal.ack_sig.as_ref();

    if ack_sig.is_empty() {
        return Err("PSM ack signature is empty".to_string());
    }

    print_info("Building signature advice from collected signatures");

    let psm_commitment_hex = psm_client
        .get_pubkey()
        .await
        .map_err(|e| format!("Failed to get PSM commitment: {}", e))?;

    let ack_sig_with_prefix = if ack_sig.starts_with("0x") {
        ack_sig.to_string()
    } else {
        format!("0x{}", ack_sig)
    };

    verify_commitment_signature(
        &tx_summary_commitment_hex,
        &psm_commitment_hex,
        &ack_sig_with_prefix,
    )
    .map_err(|e| format!("PSM signature verification failed: {}", e))?;

    let ack_signature = RpoFalconSignature::from_hex(&ack_sig_with_prefix)
        .map_err(|e| format!("Failed to parse PSM signature: {}", e))?;

    let psm_commitment = commitment_from_hex(&psm_commitment_hex)?;
    signature_advice.push(build_signature_advice_entry(
        psm_commitment,
        tx_summary_commitment,
        &AccountSignature::from(ack_signature),
    ));

    let signers = get_signers(proposal);
    if let Some(ref status) = proposal.status {
        if let Some(ref status_oneof) = status.status {
            use private_state_manager_client::delta_status::Status;
            if let Status::Pending(ref pending) = status_oneof {
                for cosigner_sig in &pending.cosigner_sigs {
                    let sig_json: serde_json::Value =
                        serde_json::from_str(&cosigner_sig.signature).map_err(|e| {
                            format!("Failed to parse cosigner signature JSON: {}", e)
                        })?;

                    let sig_hex = sig_json
                        .get("signature")
                        .and_then(|v| v.as_str())
                        .ok_or("Missing signature field")?;

                    let sig = RpoFalconSignature::from_hex(sig_hex)
                        .map_err(|e| format!("Invalid cosigner signature: {}", e))?;

                    let commitment = commitment_from_hex(&cosigner_sig.signer_id)?;
                    signature_advice.push(build_signature_advice_entry(
                        commitment,
                        tx_summary_commitment,
                        &AccountSignature::from(sig),
                    ));
                }
            }
        }
    }

    print_success(&format!(
        "Built signature advice with {} signatures (PSM + {} cosigners)",
        signature_advice.len(),
        signers.len()
    ));

    print_waiting("Building final transaction request");

    use crate::multisig::build_update_signers_transaction_request;

    let salt = metadata.salt();
    let signer_commitments = metadata.signer_commitments();
    let new_threshold = metadata
        .new_threshold
        .ok_or("Missing new_threshold in proposal metadata")?;

    let (final_tx_request, _final_config_hash) = build_update_signers_transaction_request(
        new_threshold,
        &signer_commitments,
        salt,
        signature_advice,
    )
    .map_err(|e| format!("Failed to build final transaction request: {}", e))?;

    print_waiting("Executing transaction locally");
    let miden_client = state.get_miden_client_mut()?;
    let tx_result = miden_client
        .new_transaction(account_id, final_tx_request)
        .await
        .map_err(|e| format!("Transaction execution failed: {}", e))?;

    let new_nonce = tx_result.account_delta().nonce_delta().as_int();

    print_success(&format!("✓ Transaction executed! New nonce: {}", new_nonce));

    print_waiting("Proving and submitting transaction to Miden node");
    miden_client
        .submit_transaction(tx_result.clone())
        .await
        .map_err(|e| format!("Failed to submit transaction: {}", e))?;

    print_success("✓ Transaction submitted to Miden node!");

    print_info(&format!(
        "New configuration: {}-of-{}",
        new_threshold,
        metadata.signer_commitments_hex.len()
    ));

    print_waiting("Updating local account state");

    let current_account = state.get_account_mut()?;
    current_account
        .apply_delta(tx_result.account_delta())
        .map_err(|e| format!("Failed to apply account delta: {}", e))?;

    print_success("Local account state updated");

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
        println!(
            "\n  Current Account: {}",
            shorten_hex(&account_id.to_string())
        );
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

