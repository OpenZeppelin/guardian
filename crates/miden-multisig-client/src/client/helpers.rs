//! Internal helper functions for GUARDIAN client interactions.

use crate::guardian_endpoint::verify_endpoint_commitment;
use guardian_client::GuardianClient;
#[cfg(test)]
use guardian_shared::FromJson;
use guardian_shared::SignatureScheme;
use guardian_shared::ToJson;
use miden_client::account::Account;
use miden_client::rpc::domain::account::GetAccountRequest;
use miden_client::rpc::{GrpcClient, GrpcError, NodeRpcClient, RpcError};
use miden_client::store::TransactionFilter;
use miden_client::transaction::{
    TransactionRequest, TransactionResult, TransactionStatus, TransactionSummary,
};
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::block::BlockNumber;
use miden_protocol::transaction::TransactionId;
use miden_protocol::utils::serde::Serializable;

use super::MultisigClient;
use crate::account::MultisigAccount;
use crate::builder::create_miden_client;
use crate::error::{MultisigError, Result, error_chain};
use crate::execution::{MidenRpcRetryPolicy, build_final_transaction_request};
use crate::keystore::word_from_hex;
use crate::proposal::{Proposal, TransactionType};
use crate::transaction::word_to_hex;

/// True for note-less storage-config transactions, whose post-submit state miden-client persists
/// incorrectly for private accounts, so local state must be rebuilt from the proven delta instead.
fn rebuilds_local_state_from_delta(transaction_type: &TransactionType) -> bool {
    match transaction_type {
        TransactionType::AddCosigner { .. }
        | TransactionType::RemoveCosigner { .. }
        | TransactionType::UpdateSigners { .. }
        | TransactionType::UpdateProcedureThreshold { .. }
        | TransactionType::SwitchGuardian { .. } => true,
        TransactionType::P2ID { .. }
        | TransactionType::ConsumeNotes { .. }
        | TransactionType::Custom => false,
    }
}

enum SubmissionOutcome {
    Accepted(BlockNumber),
    Reconciled(BlockNumber),
}

impl SubmissionOutcome {
    fn height(&self) -> BlockNumber {
        match self {
            Self::Accepted(height) | Self::Reconciled(height) => *height,
        }
    }
}

fn rebuild_account(mut base_account: Account, tx_result: &TransactionResult) -> Result<Account> {
    let account_delta = tx_result.account_delta();
    if account_delta.is_full_state() {
        Account::try_from(account_delta).map_err(|error| {
            MultisigError::MidenClient(format!(
                "failed to build account from full state delta: {error}"
            ))
        })
    } else {
        base_account.apply_delta(account_delta).map_err(|error| {
            MultisigError::MidenClient(format!(
                "failed to apply transaction delta to account: {error}"
            ))
        })?;
        Ok(base_account)
    }
}

impl MultisigClient {
    /// Creates a GUARDIAN client (unauthenticated).
    pub(crate) async fn create_guardian_client(&self) -> Result<GuardianClient> {
        GuardianClient::connect(&self.guardian_endpoint)
            .await
            .map_err(|e| MultisigError::GuardianConnection(e.to_string()))
    }

    /// Creates an authenticated GUARDIAN client.
    pub(crate) async fn create_authenticated_guardian_client(&self) -> Result<GuardianClient> {
        let client = self.create_guardian_client().await?;
        Ok(client.with_signer(self.key_manager.clone()))
    }

    pub(crate) async fn get_on_chain_account_commitment(
        &self,
        account_id: AccountId,
    ) -> Result<Word> {
        let rpc_client = GrpcClient::new(&self.miden_endpoint, 10_000);
        let (_, proof) = rpc_client
            .get_account(account_id, GetAccountRequest::new())
            .await
            .map_err(|e| match e {
                RpcError::RequestError {
                    error_kind: GrpcError::NotFound,
                    ..
                } => {
                    MultisigError::MidenClient(format!("account {} not found on chain", account_id))
                }
                other => MultisigError::from(other),
            })?;

        Ok(proof.account_witness().state_commitment())
    }

    pub(crate) async fn try_get_on_chain_account_commitment(
        &self,
        account_id: AccountId,
    ) -> Result<Option<Word>> {
        let rpc_client = GrpcClient::new(&self.miden_endpoint, 10_000);
        match rpc_client
            .get_account(account_id, GetAccountRequest::new())
            .await
        {
            Ok((_, proof)) => {
                let commitment = proof.account_witness().state_commitment();
                if commitment == Word::default() {
                    Ok(None)
                } else {
                    Ok(Some(commitment))
                }
            }
            Err(RpcError::RequestError {
                error_kind: GrpcError::NotFound,
                ..
            }) => Ok(None),
            Err(error) => Err(MultisigError::from(error)),
        }
    }

    /// Returns a reference to the current account, or error if none loaded.
    pub(crate) fn require_account(&self) -> Result<&MultisigAccount> {
        self.account
            .as_ref()
            .ok_or_else(|| MultisigError::MissingConfig("no account loaded".to_string()))
    }

    pub(crate) fn ensure_proposal_account_id(
        proposal_account_id: &str,
        expected_account_id: &AccountId,
    ) -> Result<()> {
        if proposal_account_id.eq_ignore_ascii_case(&expected_account_id.to_string()) {
            return Ok(());
        }

        Err(MultisigError::InvalidConfig(format!(
            "proposal is for account {} instead of {}",
            proposal_account_id, expected_account_id
        )))
    }

    /// Gets the GUARDIAN acknowledgment signature for a transaction.
    ///
    /// This pushes the delta to GUARDIAN and retrieves the server's signature.
    pub(crate) async fn get_guardian_ack_signature(
        &mut self,
        account: &MultisigAccount,
        nonce: u64,
        tx_summary: &TransactionSummary,
        tx_summary_commitment: Word,
    ) -> Result<crate::execution::SignatureAdvice> {
        let account_id = account.id();
        let prev_commitment = format!(
            "0x{}",
            hex::encode(Serializable::to_bytes(&account.commitment()))
        );

        // Push delta to GUARDIAN to get acknowledgment signature
        let mut guardian_client = self.create_authenticated_guardian_client().await?;
        let delta_payload = tx_summary.to_json();

        let push_response = guardian_client
            .push_delta(&account_id, nonce, &prev_commitment, &delta_payload)
            .await
            .map_err(|e| MultisigError::GuardianServer(format!("failed to push delta: {}", e)))?;

        // Get GUARDIAN ack signature
        let ack_sig = push_response.ack_sig.ok_or_else(|| {
            MultisigError::GuardianServer(
                "GUARDIAN did not return acknowledgment signature".to_string(),
            )
        })?;
        let ack_scheme = push_response
            .delta
            .as_ref()
            .and_then(|delta| delta.ack_scheme.as_deref())
            .ok_or_else(|| {
                MultisigError::GuardianServer(
                    "GUARDIAN did not return acknowledgment scheme".to_string(),
                )
            })
            .and_then(|ack_scheme| {
                SignatureScheme::from(ack_scheme).map_err(MultisigError::GuardianServer)
            })?;

        let (guardian_commitment_hex, raw_pubkey) = guardian_client
            .get_pubkey(Some(ack_scheme.as_str()))
            .await
            .map_err(|e| {
                MultisigError::GuardianServer(format!("failed to get GUARDIAN commitment: {}", e))
            })?;

        let guardian_commitment =
            word_from_hex(&guardian_commitment_hex).map_err(MultisigError::HexDecode)?;
        let expected_guardian_commitment = account.guardian_commitment()?;
        if guardian_commitment != expected_guardian_commitment {
            return Err(MultisigError::GuardianServer(format!(
                "GUARDIAN public key commitment {} does not match account commitment {}",
                word_to_hex(&guardian_commitment),
                word_to_hex(&expected_guardian_commitment)
            )));
        }

        let ack_signature = ack_scheme
            .parse_signature_hex(&ack_sig)
            .map_err(MultisigError::Signature)?;
        ack_scheme
            .build_signature_advice_entry(
                guardian_commitment,
                tx_summary_commitment,
                &ack_signature,
                push_response
                    .delta
                    .as_ref()
                    .and_then(|delta| delta.ack_pubkey.as_deref())
                    .or(raw_pubkey.as_deref()),
            )
            .map_err(MultisigError::Signature)
    }

    /// Verifies that a proposals metadata reconstructs the same tx_summary commitment.
    pub(crate) async fn verify_proposal_summary_binding(
        &mut self,
        proposal: &Proposal,
    ) -> Result<()> {
        let tx_summary_commitment = proposal.tx_summary.to_commitment();

        let proposal_id_commitment = word_to_hex(&tx_summary_commitment);
        if !proposal.id.eq_ignore_ascii_case(&proposal_id_commitment) {
            return Err(MultisigError::InvalidConfig(format!(
                "proposal id {} does not match tx_summary commitment {}",
                proposal.id, proposal_id_commitment
            )));
        }

        // Custom proposal types (issue #266) have no per-type reconstruction
        // recipe; the id ↔ tx_summary commitment match above is the only
        // available integrity guarantee for an opaque proposal. Guard the one
        // piece of transport metadata that gates readiness: a malformed payload
        // must not declare fewer required signatures than the account threshold,
        // or it could mark a custom proposal ready with too few cosigners.
        if matches!(proposal.transaction_type, TransactionType::Custom) {
            let account_threshold = self.require_account()?.threshold()? as usize;
            let declared = proposal
                .metadata
                .required_signatures
                .unwrap_or(account_threshold);
            if declared < account_threshold {
                return Err(MultisigError::InvalidConfig(format!(
                    "custom proposal {} declares {} required signatures, below the account threshold {}",
                    proposal.id, declared, account_threshold
                )));
            }
            return Ok(());
        }

        let account = self.require_account()?.clone();
        let salt = proposal.metadata.salt()?;
        let signer_commitments = proposal.metadata.signer_commitments()?;

        let tx_request = build_final_transaction_request(
            &self.miden_client,
            &proposal.transaction_type,
            account.inner(),
            salt,
            Vec::new(),
            proposal.metadata.new_threshold,
            Some(signer_commitments.as_slice()),
            self.key_manager.scheme(),
        )
        .await?;

        let reconstructed = crate::transaction::execute_for_summary(
            &mut self.miden_client,
            account.id(),
            tx_request,
        )
        .await?;

        if reconstructed.to_commitment() != tx_summary_commitment {
            return Err(MultisigError::InvalidConfig(format!(
                "proposal {} metadata does not match tx_summary",
                proposal.id
            )));
        }

        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn proposal_id_from_delta_payload(delta_payload: &str) -> Result<String> {
        let payload_json: serde_json::Value = serde_json::from_str(delta_payload).map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to parse proposal delta payload: {}", e))
        })?;
        let tx_summary_json = payload_json.get("tx_summary").ok_or_else(|| {
            MultisigError::InvalidConfig("missing tx_summary in delta payload".to_string())
        })?;
        let tx_summary = TransactionSummary::from_json(tx_summary_json).map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to parse tx_summary: {}", e))
        })?;
        Ok(word_to_hex(&tx_summary.to_commitment()))
    }

    /// Finalizes a transaction by executing it on-chain and updating local state.
    ///
    /// This handles the common post-execution logic for all proposal types.
    ///
    /// Note-less storage-config changes on private accounts take a manual
    /// execute/prove/submit pipeline and rebuild the account from the proven
    /// delta: the standard submit path otherwise leaves stale local state
    /// (cleared storage-map entries linger) and stages SMT roots that block a
    /// corrective overwrite. Note-bearing transactions keep the standard path to
    /// preserve note tracking. A full-state delta (private accounts) converts
    /// directly into the account; an incremental delta is applied onto the base.
    ///
    /// For a `SwitchGuardian` the new endpoint is registered only when it
    /// actually differs from the current one. On an unchanged endpoint the
    /// pushed switch delta canonicalizes normally; re-registering would overwrite
    /// the pre-switch base and double-apply the delta. Post-submit `sync_state`
    /// failures are ignored because GUARDIAN may not have canonicalized yet.
    pub(crate) async fn finalize_transaction(
        &mut self,
        account_id: AccountId,
        tx_request: TransactionRequest,
        transaction_type: &TransactionType,
    ) -> Result<()> {
        self.finalize_transaction_with_retry(
            account_id,
            tx_request,
            transaction_type,
            MidenRpcRetryPolicy::disabled(),
        )
        .await
    }

    pub(crate) async fn finalize_transaction_with_retry(
        &mut self,
        account_id: AccountId,
        tx_request: TransactionRequest,
        transaction_type: &TransactionType,
        retry_policy: MidenRpcRetryPolicy,
    ) -> Result<()> {
        if let TransactionType::SwitchGuardian {
            new_endpoint,
            new_commitment,
        } = transaction_type
        {
            verify_endpoint_commitment(new_endpoint, *new_commitment).await?;
        }

        let new_guardian_endpoint =
            if let TransactionType::SwitchGuardian { new_endpoint, .. } = transaction_type {
                Some(new_endpoint.clone())
            } else {
                None
            };

        let base_account = self
            .miden_client
            .get_account(account_id)
            .await
            .map_err(MultisigError::from)?
            .ok_or_else(|| {
                MultisigError::MissingConfig("account not found before execution".to_string())
            })?;

        let tx_result = self
            .execute_transaction_with_retry(account_id, tx_request, retry_policy)
            .await?;
        let proven = self
            .miden_client
            .prove_transaction(&tx_result)
            .await
            .map_err(MultisigError::from)?;
        let submission = self
            .submit_with_reconciliation(proven, &tx_result, retry_policy)
            .await?;

        let rebuilt = rebuild_account(base_account, &tx_result)?;
        let updated_account = if rebuilds_local_state_from_delta(transaction_type)
            || matches!(submission, SubmissionOutcome::Reconciled(_))
        {
            self.add_or_update_account(&rebuilt, true).await?;
            rebuilt
        } else {
            let submission_height = submission.height();
            self.miden_client
                .apply_transaction(&tx_result, submission_height)
                .await
                .map_err(MultisigError::from)?;
            self.miden_client
                .get_account(account_id)
                .await
                .map_err(MultisigError::from)?
                .ok_or_else(|| {
                    MultisigError::MissingConfig("account not found after execution".to_string())
                })?
        };

        let _ = self.miden_client.sync_state().await;

        if let Some(endpoint) = new_guardian_endpoint {
            let switching_endpoint = endpoint != self.guardian_endpoint;
            self.guardian_endpoint = endpoint;
            self.account = Some(MultisigAccount::new(updated_account.clone()));

            if switching_endpoint {
                self.register_on_guardian().await.map_err(|e| {
                    MultisigError::GuardianServer(format!(
                        "transaction executed successfully but failed to register on new GUARDIAN: {}",
                        e
                    ))
                })?;
            }
        } else {
            let multisig_account = MultisigAccount::new(updated_account);
            self.account = Some(multisig_account);
        }

        Ok(())
    }

    async fn execute_transaction_with_retry(
        &mut self,
        account_id: AccountId,
        tx_request: TransactionRequest,
        retry_policy: MidenRpcRetryPolicy,
    ) -> Result<TransactionResult> {
        let deadline = retry_policy.deadline();
        let mut attempt = 0;

        loop {
            match self
                .miden_client
                .execute_transaction(account_id, tx_request.clone())
                .await
                .map_err(MultisigError::from)
            {
                Ok(result) => return Ok(result),
                Err(error)
                    if error.is_transient_miden_rpc() && tokio::time::Instant::now() < deadline =>
                {
                    let delay = retry_policy
                        .delay(attempt)
                        .min(deadline.saturating_duration_since(tokio::time::Instant::now()));
                    attempt += 1;
                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn submit_with_reconciliation(
        &mut self,
        proven: miden_protocol::transaction::ProvenTransaction,
        tx_result: &TransactionResult,
        retry_policy: MidenRpcRetryPolicy,
    ) -> Result<SubmissionOutcome> {
        let transaction_id = tx_result.id();
        let deadline = retry_policy.deadline();
        let mut attempt = 0;

        loop {
            match self
                .miden_client
                .submit_proven_transaction(proven.clone(), tx_result)
                .await
                .map_err(MultisigError::from)
            {
                Ok(height) => return Ok(SubmissionOutcome::Accepted(height)),
                Err(error)
                    if error.is_transient_miden_rpc()
                        || (attempt > 0 && error.is_miden_duplicate_submission()) =>
                {
                    if !error.is_miden_rate_limited() {
                        match self.reconcile_submission(transaction_id).await {
                            Ok(Some(height)) => {
                                return Ok(SubmissionOutcome::Reconciled(height));
                            }
                            Ok(None) => {}
                            Err(reconcile_error) if reconcile_error.is_transient_miden_rpc() => {}
                            Err(reconcile_error) => return Err(reconcile_error),
                        }
                    }

                    let now = tokio::time::Instant::now();
                    if now >= deadline {
                        return Err(error);
                    }
                    let delay = retry_policy
                        .delay(attempt)
                        .min(deadline.saturating_duration_since(now));
                    attempt += 1;
                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn reconcile_submission(
        &mut self,
        transaction_id: TransactionId,
    ) -> Result<Option<BlockNumber>> {
        if let Some(height) = self.stored_submission_height(transaction_id).await? {
            return Ok(Some(height));
        }

        let sync_result = self
            .miden_client
            .sync_state()
            .await
            .map_err(MultisigError::from);
        let reconciled = self.stored_submission_height(transaction_id).await?;
        if reconciled.is_some() {
            return Ok(reconciled);
        }
        sync_result?;
        Ok(None)
    }

    async fn stored_submission_height(
        &self,
        transaction_id: TransactionId,
    ) -> Result<Option<BlockNumber>> {
        let transaction = self
            .miden_client
            .get_transactions(TransactionFilter::Ids(vec![transaction_id]))
            .await
            .map_err(MultisigError::from)?
            .into_iter()
            .next();

        match transaction {
            Some(record) => match record.status {
                TransactionStatus::Pending => Ok(Some(record.details.submission_height)),
                TransactionStatus::Committed { block_number, .. } => Ok(Some(block_number)),
                TransactionStatus::Discarded(cause) => Err(MultisigError::TransactionExecution(
                    format!("transaction {transaction_id} was discarded: {cause:?}"),
                )),
            },
            None => Ok(None),
        }
    }

    /// Resets the miden-client by creating a new instance with a fresh database.
    pub async fn reset_miden_client(&mut self) -> Result<()> {
        self.miden_client =
            create_miden_client(&self.account_dir, &self.miden_endpoint, &self.prover_config)
                .await?;
        Ok(())
    }

    /// Adds an account to miden-client if it doesn't exist, or updates it if it does.
    pub(crate) async fn add_or_update_account(
        &mut self,
        account: &Account,
        imported: bool,
    ) -> Result<()> {
        let account_id = account.id();

        let existing = self
            .miden_client
            .get_account(account_id)
            .await
            .map_err(|e| {
                MultisigError::MidenClient(format!("failed to check account: {}", error_chain(&e)))
            })?;

        if existing.is_some() {
            self.miden_client
                .add_account(account, true)
                .await
                .map_err(|e| {
                    MultisigError::MidenClient(format!(
                        "failed to update account: {}",
                        error_chain(&e)
                    ))
                })?;
        } else {
            self.miden_client
                .add_account(account, imported)
                .await
                .map_err(|e| {
                    MultisigError::MidenClient(format!(
                        "failed to add account: {}",
                        error_chain(&e)
                    ))
                })?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use guardian_shared::FromJson;
    use guardian_shared::ToJson;
    use miden_protocol::account::AccountId;
    use miden_protocol::account::delta::{AccountDelta, AccountStorageDelta, AccountVaultDelta};
    use miden_protocol::transaction::{InputNotes, RawOutputNotes, TransactionSummary};
    use miden_protocol::{Felt, Word};

    use super::MultisigClient;

    fn tx_summary_json() -> serde_json::Value {
        let account_id = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let delta = AccountDelta::new(
            account_id,
            AccountStorageDelta::default(),
            AccountVaultDelta::default(),
            Felt::ZERO,
        )
        .unwrap();
        TransactionSummary::new(
            delta,
            InputNotes::new(Vec::new()).unwrap(),
            RawOutputNotes::new(Vec::new()).unwrap(),
            Word::default(),
        )
        .to_json()
    }

    #[test]
    fn proposal_id_from_delta_payload_returns_tx_summary_commitment() {
        let tx_summary = TransactionSummary::from_json(&tx_summary_json()).unwrap();
        let expected_id = crate::transaction::word_to_hex(&tx_summary.to_commitment());
        let delta_payload = serde_json::json!({
            "tx_summary": tx_summary_json(),
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": []
            }
        })
        .to_string();

        let proposal_id = MultisigClient::proposal_id_from_delta_payload(&delta_payload).unwrap();

        assert_eq!(proposal_id, expected_id);
    }

    #[test]
    fn proposal_id_from_delta_payload_rejects_missing_tx_summary() {
        let result = MultisigClient::proposal_id_from_delta_payload("{\"metadata\":{}}");

        assert!(result.is_err());
    }

    #[test]
    fn ensure_proposal_account_id_accepts_matching_account() {
        let account_id = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();

        let result = MultisigClient::ensure_proposal_account_id(
            "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b",
            &account_id,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn ensure_proposal_account_id_rejects_mismatched_account() {
        let account_id = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();

        let error = MultisigClient::ensure_proposal_account_id(
            "0x8a8a8a8a8a8a8a010a8a8a8a8a8a8a",
            &account_id,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid configuration: proposal is for account 0x8a8a8a8a8a8a8a010a8a8a8a8a8a8a instead of 0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b"
        );
    }
}
