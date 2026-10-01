//! The Miden implementation of [`ProposalExecutor`]: one attempt reproduces the stored request
//! at the chain tip, executes it with the collected signatures and Guardian's acknowledgment,
//! proves it remotely, seals its inputs and sends it once.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use guardian_shared::hex::{FromHex, IntoHex};
use guardian_shared::retry::{RPC_TRANSPORT_SIGNALS, StructuredEvidence, is_transient_error_with};
use guardian_shared::{FromJson, SignatureScheme, ToJson};
use miden_client::rpc::NodeRpcClient;
use miden_client::rpc::encryption::SealedTransactionInputs;
use miden_client::transaction::TransactionProver;
use miden_protocol::account::Account;
use miden_protocol::account::auth::Signature as AccountSignature;
use miden_protocol::block::BlockNumber;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak;
use miden_protocol::transaction::{
    ExecutedTransaction, ProvenTransaction, TransactionInputs, TransactionSummary,
};
use miden_protocol::utils::serde::Deserializable;
use miden_protocol::{Felt, Word};
use miden_standards::account::auth::Eip712TransactionSummary;
use miden_tx::auth::UnreachableAuth;
use miden_tx::{TransactionExecutor, TransactionExecutorError};

use super::aborts::Abort;
use super::chain::{ChainViewError, build_chain_view};
use super::foreign::ForeignAccountUnavailable;
use super::request::{StoredRequest, approval_expiration_block};
use super::sealing::seal_for_submission;
use super::store::ExecutionDataStore;
use super::threshold::{InvokedProcedure, effective_threshold};
use crate::config::execution::ExecutionConfig;
use crate::delta_object::CosignerSignature;
use crate::error::GuardianError;
use crate::network::miden::account_inspector::MidenAccountInspector;
use crate::services::execute_proposal::{
    ExecutedTransactionInfo, ExecutionAttempt, ExecutionInput, GuardianAck, ProposalExecutor,
    ProvenTransactionInfo, SignatureSelection, SubmissionOutcome,
};
use crate::services::execution_codec::TransactionRequestEnvelope;
use crate::storage::execution::{ExpirationBound, ForeignAccountUnavailableReason};
use crate::storage::{ExecutionFailure, ExecutionFailureCode};

const PROVER_BACKOFF_START: Duration = Duration::from_secs(1);
const PROVER_BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Executes Guardian-executable proposals against a Miden node and remote prover.
pub struct MidenExecutor {
    rpc: Arc<dyn NodeRpcClient>,
    prover: Arc<dyn TransactionProver + Send + Sync>,
    config: ExecutionConfig,
}

impl MidenExecutor {
    pub fn new(
        rpc: Arc<dyn NodeRpcClient>,
        prover: Arc<dyn TransactionProver + Send + Sync>,
        config: ExecutionConfig,
    ) -> Self {
        Self {
            rpc,
            prover,
            config,
        }
    }
}

/// The signatures an execution attaches as advice, keyed as the multisig auth procedure reads
/// them.
struct SelectedSignatures {
    selection: SignatureSelection,
    advice: Vec<(Word, Vec<Felt>)>,
}

fn select(
    input: &ExecutionInput,
    account: &Account,
    summary: &TransactionSummary,
) -> SelectedSignatures {
    let inspector = MidenAccountInspector::new(account);
    let cosigners: BTreeSet<String> = inspector
        .extract_pubkeys()
        .into_iter()
        .map(|commitment| commitment.to_lowercase())
        .collect();
    let required = effective_threshold(account, invoked_procedure(&input.proposal_payload))
        .unwrap_or(usize::MAX);

    let mut seen = BTreeSet::new();
    let mut advice = Vec::new();
    let mut ignored = 0u32;
    for signature in &input.cosigner_signatures {
        let signer = signature.signer_id.to_lowercase();
        let entry = if !cosigners.contains(&signer) {
            Err("not a registered cosigner")
        } else if !seen.insert(signer.clone()) {
            Err("a duplicate of an earlier signature from the same cosigner")
        } else {
            verified_advice(signature, &signer, summary)
                .ok_or("not a valid signature over the signed summary")
        };
        match entry {
            Ok(entry) => advice.push(entry),
            Err(reason) => {
                ignored += 1;
                tracing::info!(
                    account_id = %input.account_id,
                    signer_id = %signature.signer_id,
                    reason,
                    "ignoring a cosigner signature"
                );
            }
        }
    }
    SelectedSignatures {
        selection: SignatureSelection {
            required,
            valid: advice.len(),
            ignored,
        },
        advice,
    }
}

/// The advice entry for `signature` when it is `signer`'s valid signature over `summary`. An
/// EIP-712 signature signs the summary's typed-data digest, so it is verified against that and
/// keyed as the auth procedure reads it, exactly as the SDKs build it for local execution.
fn verified_advice(
    signature: &CosignerSignature,
    signer: &str,
    summary: &TransactionSummary,
) -> Option<(Word, Vec<Felt>)> {
    let commitment = <Word as FromHex>::from_hex(signer).ok()?;
    let message = summary.to_commitment();
    match &signature.signature {
        guardian_shared::ProposalSignature::Falcon { signature } => {
            let parsed = SignatureScheme::Falcon
                .parse_signature_hex(signature)
                .ok()?;
            let AccountSignature::Falcon512Poseidon2(falcon) = &parsed else {
                return None;
            };
            let public_key = falcon.public_key();
            (public_key.to_commitment() == commitment && public_key.verify(message, falcon))
                .then_some(())?;
            SignatureScheme::Falcon
                .build_signature_advice_entry(commitment, message, &parsed, None)
                .ok()
        }
        guardian_shared::ProposalSignature::Ecdsa {
            signature,
            public_key,
            message_format,
        } => {
            let public_key_hex = public_key.as_deref()?;
            let parsed = SignatureScheme::Ecdsa.parse_signature_hex(signature).ok()?;
            let AccountSignature::EcdsaK256Keccak(ecdsa) = &parsed else {
                return None;
            };
            let key_bytes = hex::decode(public_key_hex.trim_start_matches("0x")).ok()?;
            let key = ecdsa_k256_keccak::PublicKey::read_from_bytes(&key_bytes).ok()?;
            (key.to_commitment() == commitment).then_some(())?;
            match message_format {
                guardian_shared::EcdsaMessageFormat::Raw => {
                    key.verify(message, ecdsa).then_some(())?;
                    SignatureScheme::Ecdsa
                        .build_signature_advice_entry(
                            commitment,
                            message,
                            &parsed,
                            Some(public_key_hex),
                        )
                        .ok()
                }
                guardian_shared::EcdsaMessageFormat::Eip712 => {
                    key.verify_prehash(summary.eip712_hash().into_bytes(), ecdsa)
                        .then_some(())?;
                    Some(summary.eip712_signature_advice(&key, ecdsa))
                }
            }
        }
    }
}

fn summary_of(payload: &serde_json::Value) -> Result<TransactionSummary, String> {
    let tx_summary = payload
        .get("tx_summary")
        .ok_or_else(|| "proposal carries no tx_summary".to_string())?;
    TransactionSummary::from_json(tx_summary)
}

fn invoked_procedure(payload: &serde_json::Value) -> InvokedProcedure {
    InvokedProcedure::of_proposal_type(
        payload
            .get("metadata")
            .and_then(|metadata| metadata.get("proposal_type"))
            .and_then(serde_json::Value::as_str),
    )
}

/// A node that has not reached a needed block is behind; one that cannot be read is
/// unavailable; one whose chain data does not authenticate against itself is inconsistent.
/// None of them says anything about the proposal, so none is a binding mismatch.
fn chain_view_failure(error: ChainViewError) -> ExecutionFailure {
    let code = match &error {
        ChainViewError::ChainBehind { .. } => ExecutionFailureCode::ChainBehind,
        ChainViewError::Rpc(_) => ExecutionFailureCode::NodeUnavailable,
        ChainViewError::Inconsistent(_) => ExecutionFailureCode::ChainInconsistent,
    };
    ExecutionFailure::new(code, error.to_string())
}

async fn chain_tip(rpc: &dyn NodeRpcClient) -> Result<BlockNumber, ExecutionFailure> {
    rpc.get_block_header_by_number(None, false)
        .await
        .map(|(header, _)| header.block_num())
        .map_err(|e| {
            ExecutionFailure::new(
                ExecutionFailureCode::NodeUnavailable,
                format!("reading the chain tip failed: {e}"),
            )
        })
}

/// An error with every source beneath it: a prover reports only "failed to prove transaction"
/// at the top, and the cause an operator needs sits further down.
fn with_sources(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn is_transient(error: &(dyn std::error::Error + 'static)) -> bool {
    is_transient_error_with(
        error,
        |_| StructuredEvidence::Indeterminate,
        &[RPC_TRANSPORT_SIGNALS.as_slice(), &["timed out"]].concat(),
    )
}

#[async_trait]
impl ProposalExecutor for MidenExecutor {
    fn select_signatures(
        &self,
        input: &ExecutionInput,
    ) -> Result<SignatureSelection, GuardianError> {
        let account = Account::from_json(&input.state_json).map_err(GuardianError::InvalidDelta)?;
        let summary = summary_of(&input.proposal_payload).map_err(GuardianError::InvalidDelta)?;
        Ok(select(input, &account, &summary).selection)
    }

    async fn prepare(
        &self,
        input: ExecutionInput,
    ) -> Result<Box<dyn ExecutionAttempt>, ExecutionFailure> {
        let codec =
            |message: String| ExecutionFailure::new(ExecutionFailureCode::RequestCodec, message);
        let envelope: TransactionRequestEnvelope = input
            .proposal_payload
            .get("transaction_request")
            .cloned()
            .ok_or_else(|| codec("proposal carries no transaction request".to_string()))
            .and_then(|value| serde_json::from_value(value).map_err(|e| codec(e.to_string())))?;
        let bytes = envelope
            .verified_bytes(|serializer_id| self.config.admits_serializer(serializer_id))
            .map_err(|rejection| {
                ExecutionFailure::new(rejection.failure_code(), rejection.to_string())
            })?;
        let request = StoredRequest::read_from_bytes(&bytes).map_err(|e| codec(e.to_string()))?;
        let summary = summary_of(&input.proposal_payload).map_err(codec)?;
        request.check_against(&summary).map_err(|reason| {
            ExecutionFailure::new(
                ExecutionFailureCode::RequestInvalid(reason),
                format!("stored request is not Guardian-executable: {reason:?}"),
            )
        })?;
        let account = Account::from_json(&input.state_json).map_err(codec)?;
        let selected = select(&input, &account, &summary);

        if self.rpc.has_genesis_commitment().is_none() {
            let (genesis, _) = self
                .rpc
                .get_block_header_by_number(Some(BlockNumber::GENESIS), false)
                .await
                .map_err(|e| {
                    ExecutionFailure::new(ExecutionFailureCode::NodeUnavailable, e.to_string())
                })?;
            self.rpc
                .set_genesis_commitment(genesis.commitment())
                .await
                .map_err(|e| {
                    ExecutionFailure::new(ExecutionFailureCode::NodeUnavailable, e.to_string())
                })?;
        }
        let approval_expiration = approval_expiration_block(&summary);
        let tip = chain_tip(self.rpc.as_ref()).await?;
        if u64::from(tip.as_u32()) >= approval_expiration {
            return Err(ExecutionFailure::new(
                ExecutionFailureCode::ExpirationReached(ExpirationBound::Approval),
                format!("approval expired at block {approval_expiration}; the tip is {tip}"),
            ));
        }

        let unsigned = request
            .execution_inputs(&account, [])
            .map_err(|e| match e {
                super::request::RequestInputsError::Invalid(reason) => ExecutionFailure::new(
                    ExecutionFailureCode::RequestInvalid(reason),
                    format!("{reason:?}"),
                ),
                super::request::RequestInputsError::Malformed(message) => codec(message),
            })?;
        let mut tracked: BTreeSet<BlockNumber> = request.block_numbers().clone();
        tracked.extend(
            unsigned
                .input_notes
                .iter()
                .filter_map(|note| note.location().map(|location| location.block_num())),
        );
        let assembly = std::time::Instant::now();
        let view = build_chain_view(self.rpc.as_ref(), &tracked)
            .await
            .inspect(|_| {
                metrics::histogram!(crate::metrics::names::EXECUTION_CHAIN_VIEW_DURATION_SECONDS)
                    .record(assembly.elapsed().as_secs_f64());
            })
            .map_err(chain_view_failure)?;
        let notes: Vec<_> = unsigned
            .input_notes
            .iter()
            .map(|note| note.note().clone())
            .collect();
        let store = ExecutionDataStore::new(account.clone(), view, self.rpc.clone(), &notes);

        let reference = store.chain().reference_block();
        let executor: TransactionExecutor<'_, '_, _, UnreachableAuth> =
            TransactionExecutor::new(&store);
        let reproduced = match executor
            .execute_transaction(
                account.id(),
                reference,
                unsigned.input_notes,
                unsigned.tx_args,
            )
            .await
        {
            Err(TransactionExecutorError::Unauthorized(summary)) => *summary,
            Err(error) => return Err(execution_failure(&store, &error)),
            Ok(_) => {
                return Err(ExecutionFailure::new(
                    ExecutionFailureCode::BindingMismatch,
                    "the stored request executed without the cosigners' signatures",
                ));
            }
        };
        if reproduced.to_commitment() != summary.to_commitment() {
            log_output_note_difference(&summary, &reproduced);
            return Err(ExecutionFailure::new(
                ExecutionFailureCode::BindingMismatch,
                format!(
                    "reproduced summary {} differs from the signed {}",
                    reproduced.to_commitment(),
                    summary.to_commitment()
                ),
            ));
        }

        Ok(Box::new(MidenAttempt {
            rpc: self.rpc.clone(),
            prover: self.prover.clone(),
            account,
            request,
            summary_payload: reproduced.to_json(),
            summary,
            signature_advice: selected.advice,
            requires_ack: invoked_procedure(&input.proposal_payload).requires_guardian_ack(),
            store,
            executed: None,
            proven: None,
            sealed: None,
        }))
    }

    async fn chain_tip(&self) -> Result<u32, String> {
        chain_tip(self.rpc.as_ref())
            .await
            .map(|tip| tip.as_u32())
            .map_err(|failure| failure.message)
    }
}

/// Maps an execution error onto a stable cause. The foreign-account cause is read from the data
/// store because the executor reports it only as an opaque data-store error. A vault shortfall
/// is a fee shortfall: the base state is the one the proposer executed against and every input
/// note is pinned, so only the fee, computed at the new reference block, can have grown.
fn execution_failure(
    store: &ExecutionDataStore,
    error: &TransactionExecutorError,
) -> ExecutionFailure {
    if let Some(unavailable) = store.foreign_failure() {
        let reason = match unavailable {
            ForeignAccountUnavailable::Private { .. } => ForeignAccountUnavailableReason::Private,
            ForeignAccountUnavailable::Unavailable { .. } => {
                ForeignAccountUnavailableReason::Unavailable
            }
        };
        return ExecutionFailure::new(
            ExecutionFailureCode::ForeignAccountUnavailable(reason),
            format!("{unavailable:?}"),
        );
    }
    match Abort::of(error) {
        Some(Abort::ApprovalExpired) => ExecutionFailure::new(
            ExecutionFailureCode::ExpirationReached(ExpirationBound::Approval),
            "the multisig approval expired at or before the reference block",
        ),
        Some(Abort::VaultShortfall) => ExecutionFailure::new(
            ExecutionFailureCode::InsufficientFee,
            "the account cannot pay the transaction fee at the reference block",
        ),
        None => ExecutionFailure::new(
            ExecutionFailureCode::BindingMismatch,
            format!("the stored request does not execute: {error}"),
        ),
    }
}

/// Names the output notes that differ between the signed and the reproduced summary, which is
/// where a changed fee shows up.
fn log_output_note_difference(signed: &TransactionSummary, reproduced: &TransactionSummary) {
    let ids = |summary: &TransactionSummary| -> BTreeSet<String> {
        summary
            .output_notes()
            .iter()
            .map(|note| note.id().to_hex())
            .collect()
    };
    let (signed_ids, reproduced_ids) = (ids(signed), ids(reproduced));
    tracing::warn!(
        only_signed = ?signed_ids.difference(&reproduced_ids).collect::<Vec<_>>(),
        only_reproduced = ?reproduced_ids.difference(&signed_ids).collect::<Vec<_>>(),
        account_delta_matches = signed.account_delta().to_commitment() == reproduced.account_delta().to_commitment(),
        "the reproduced transaction differs from the signed one"
    );
}

struct MidenAttempt {
    rpc: Arc<dyn NodeRpcClient>,
    prover: Arc<dyn TransactionProver + Send + Sync>,
    account: Account,
    request: StoredRequest,
    summary: TransactionSummary,
    summary_payload: serde_json::Value,
    signature_advice: Vec<(Word, Vec<Felt>)>,
    requires_ack: bool,
    store: ExecutionDataStore,
    executed: Option<ExecutedTransaction>,
    proven: Option<ProvenTransaction>,
    sealed: Option<SealedTransactionInputs>,
}

impl MidenAttempt {
    async fn ensure_not_expired(&self, expiration: BlockNumber) -> Result<(), ExecutionFailure> {
        let tip = chain_tip(self.rpc.as_ref()).await?;
        if expiration <= tip {
            return Err(ExecutionFailure::new(
                ExecutionFailureCode::ExpirationReached(ExpirationBound::Transaction),
                format!("the transaction expires at block {expiration}; the tip is {tip}"),
            ));
        }
        Ok(())
    }

    fn executed(&self) -> &ExecutedTransaction {
        self.executed
            .as_ref()
            .expect("the attempt executes before it proves, seals or submits")
    }
}

#[async_trait]
impl ExecutionAttempt for MidenAttempt {
    fn reference_block(&self) -> u32 {
        self.store.chain().reference_block().as_u32()
    }

    fn summary_payload(&self) -> &serde_json::Value {
        &self.summary_payload
    }

    fn requires_guardian_ack(&self) -> bool {
        self.requires_ack
    }

    async fn execute(
        &mut self,
        ack: Option<GuardianAck>,
    ) -> Result<ExecutedTransactionInfo, ExecutionFailure> {
        let message = self.summary.to_commitment();
        let mut advice = self.signature_advice.clone();
        if let Some(ack) = ack {
            let commitment = <Word as FromHex>::from_hex(&ack.commitment_hex).map_err(|_| {
                ExecutionFailure::new(
                    ExecutionFailureCode::BindingMismatch,
                    "Guardian's acknowledgment commitment is not a word",
                )
            })?;
            let signature = ack
                .scheme
                .parse_signature_hex(&ack.signature_hex)
                .map_err(|e| ExecutionFailure::new(ExecutionFailureCode::BindingMismatch, e))?;
            let entry = ack
                .scheme
                .build_signature_advice_entry(
                    commitment,
                    message,
                    &signature,
                    Some(ack.public_key_hex.as_str()),
                )
                .map_err(|e| ExecutionFailure::new(ExecutionFailureCode::BindingMismatch, e))?;
            advice.push(entry);
        }
        let inputs = self
            .request
            .execution_inputs(&self.account, advice)
            .map_err(|e| {
                ExecutionFailure::new(ExecutionFailureCode::RequestCodec, e.to_string())
            })?;
        let reference = self.store.chain().reference_block();
        let executor: TransactionExecutor<'_, '_, _, UnreachableAuth> =
            TransactionExecutor::new(&self.store);
        let executed = executor
            .execute_transaction(
                self.account.id(),
                reference,
                inputs.input_notes,
                inputs.tx_args,
            )
            .await
            .map_err(|error| execution_failure(&self.store, &error))?;

        if executed.input_notes().commitment() != self.summary.input_notes().commitment()
            || executed.output_notes().commitment() != self.summary.output_notes().commitment()
        {
            return Err(ExecutionFailure::new(
                ExecutionFailureCode::BindingMismatch,
                "the executed transaction's notes differ from the signed summary",
            ));
        }
        self.ensure_not_expired(executed.expiration_block_num())
            .await?;
        let final_account_commitment = executed.final_account().to_commitment().into_hex();
        self.executed = Some(executed);
        Ok(ExecutedTransactionInfo {
            final_account_commitment,
        })
    }

    async fn prove(&mut self) -> Result<ProvenTransactionInfo, ExecutionFailure> {
        let expiration = self.executed().expiration_block_num();
        let inputs: TransactionInputs = self.executed().tx_inputs().clone();
        let mut backoff = PROVER_BACKOFF_START;
        let proving = std::time::Instant::now();
        let record_duration = || {
            metrics::histogram!(crate::metrics::names::EXECUTION_PROVING_DURATION_SECONDS)
                .record(proving.elapsed().as_secs_f64());
        };
        let proven = loop {
            match self.prover.prove(inputs.clone()).await {
                Ok(proven) => {
                    record_duration();
                    break proven;
                }
                Err(error) if is_transient(&error) => {
                    metrics::counter!(crate::metrics::names::EXECUTION_PROVER_RETRIES_TOTAL)
                        .increment(1);
                    tracing::warn!(
                        error = %with_sources(&error),
                        retry_in = ?backoff,
                        "transient prover failure; retrying under the held reservation"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(PROVER_BACKOFF_CAP);
                    self.ensure_not_expired(expiration)
                        .await
                        .inspect_err(|_| record_duration())
                        .map_err(|stop| match stop.code {
                            ExecutionFailureCode::ExpirationReached(_) => ExecutionFailure {
                                message: format!(
                                    "{}; the prover stayed unreachable until then, last error: {}",
                                    stop.message,
                                    with_sources(&error)
                                ),
                                code: stop.code,
                            },
                            _ => stop,
                        })?;
                }
                Err(error) => {
                    record_duration();
                    return Err(ExecutionFailure::new(
                        ExecutionFailureCode::ProvingFailed,
                        format!(
                            "the prover refused the transaction: {}",
                            with_sources(&error)
                        ),
                    ));
                }
            }
        };
        let info = ProvenTransactionInfo {
            transaction_id: proven.id().to_hex(),
            reference_block: self.reference_block(),
            expiration_block: proven.expiration_block_num().as_u32(),
        };
        self.proven = Some(proven);
        Ok(info)
    }

    async fn seal(&mut self) -> Result<(), ExecutionFailure> {
        let proven = self
            .proven
            .as_ref()
            .expect("the attempt proves before it seals");
        let sealed = seal_for_submission(
            self.rpc.as_ref(),
            self.store.chain(),
            proven.id(),
            self.executed().tx_inputs(),
        )
        .await
        .map_err(|e| ExecutionFailure::new(ExecutionFailureCode::SealingFailed, e.to_string()))?;
        self.sealed = Some(sealed);
        Ok(())
    }

    async fn submit(&mut self) -> SubmissionOutcome {
        let proven = self
            .proven
            .as_ref()
            .expect("the attempt proves before it submits");
        let sealed = self
            .sealed
            .take()
            .expect("the attempt seals before it submits, and submits once");
        match self.rpc.submit_proven_transaction(proven, sealed).await {
            Ok(_) => SubmissionOutcome::Accepted,
            Err(error) if error.is_indeterminate_submission() => SubmissionOutcome::Unknown {
                reason: error.to_string(),
            },
            Err(error) => SubmissionOutcome::Rejected {
                reason: error.to_string(),
            },
        }
    }
}

impl std::fmt::Debug for MidenAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MidenAttempt")
            .field("account_id", &self.account.id())
            .field("reference_block", &self.reference_block())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::is_transient;

    #[derive(Debug, thiserror::Error)]
    #[error("failed to prove transaction")]
    struct ProveFailed(#[source] std::io::Error);

    fn wrapped(cause: &str) -> ProveFailed {
        ProveFailed(std::io::Error::other(cause.to_string()))
    }

    #[test]
    fn a_transport_failure_under_the_generic_prover_message_is_retried() {
        for cause in [
            "connection error: i/o timeout",
            "transport error",
            "status: DeadlineExceeded, message: \"deadline exceeded\"",
            "the prover is unavailable",
        ] {
            assert!(is_transient(&wrapped(cause)), "{cause}");
        }
    }

    #[test]
    fn a_permanent_prover_failure_is_not_retried() {
        for cause in ["invalid transaction inputs", "proof verification failed"] {
            assert!(!is_transient(&wrapped(cause)), "{cause}");
        }
    }
}

#[cfg(test)]
mod chain_failure_tests {
    use miden_client::rpc::RpcError;
    use miden_protocol::block::BlockNumber;

    use super::{ChainViewError, ExecutionFailureCode, chain_view_failure};

    #[test]
    fn a_chain_view_failure_names_the_node_not_the_proposal() {
        let cases = [
            (
                ChainViewError::ChainBehind {
                    tip: BlockNumber::from(3),
                    required: BlockNumber::from(9),
                },
                ExecutionFailureCode::ChainBehind,
            ),
            (
                ChainViewError::Rpc(RpcError::ExpectedDataMissing("header".to_string())),
                ExecutionFailureCode::NodeUnavailable,
            ),
            (
                ChainViewError::Inconsistent("MMR delta does not apply".to_string()),
                ExecutionFailureCode::ChainInconsistent,
            ),
        ];
        for (error, code) in cases {
            assert_eq!(chain_view_failure(error).code, code);
        }
    }
}
