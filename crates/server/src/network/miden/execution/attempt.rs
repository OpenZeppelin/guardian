//! The Miden implementation of [`ProposalExecutor`]: one attempt reproduces the stored request
//! at the chain tip, executes it with the collected signatures and Guardian's acknowledgment,
//! proves it remotely, seals its inputs and sends it once.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use guardian_shared::hex::{FromHex, IntoHex};
use guardian_shared::retry::{
    RPC_TRANSPORT_SIGNALS, StructuredEvidence, grpc_code_evidence, is_transient_error_with,
};
use guardian_shared::{
    EcdsaMessageFormat, FromJson, ProposalSignature, SignatureScheme, ToJson,
    parse_ecdsa_public_key_hex,
};
use miden_client::rpc::NodeRpcClient;
use miden_client::rpc::encryption::SealedTransactionInputs;
use miden_client::transaction::TransactionProver;
use miden_protocol::account::Account;
use miden_protocol::account::auth::Signature as AccountSignature;
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::transaction::{
    ExecutedTransaction, ProvenTransaction, TransactionInputs, TransactionSummary,
    TransactionVerifier,
};
use miden_protocol::{Felt, Word};
use miden_standards::account::auth::Eip712TransactionSummary;
use miden_tx::auth::UnreachableAuth;
use miden_tx::{TransactionExecutor, TransactionExecutorError};
use serde::Deserialize;

use super::aborts::Abort;
use super::chain::{ChainViewError, build_chain_view_from, fetch_genesis};
use super::foreign::ForeignAccountUnavailable;
use super::request::{StoredRequest, approval_expiration_block};
use super::sealing::seal_for_submission;
use super::store::ExecutionDataStore;
use super::threshold::{InvokedProcedure, effective_threshold};
use crate::delta_object::CosignerSignature;
use crate::error::GuardianError;
use crate::metrics::execution::{record_chain_view, record_prover_retry, record_proving};
use crate::network::miden::account_inspector::MidenAccountInspector;
use crate::services::execute_proposal::{
    ExecutedTransactionInfo, ExecutionAttempt, ExecutionInput, GuardianAck, ProposalExecutor,
    ProvenTransactionInfo, SignatureSelection, SubmissionOutcome,
};
use crate::services::execution_codec::TransactionRequestEnvelope;
use crate::services::proposal_signature::{proposal_tx_summary, verify_proposal_signature};
use crate::storage::execution::{ExpirationBound, ForeignAccountUnavailableReason};
use crate::storage::{ExecutionFailure, ExecutionFailureCode};

const PROVER_BACKOFF_START: Duration = Duration::from_secs(1);
const PROVER_BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Executes Guardian-executable proposals against a Miden node and remote prover.
pub struct MidenExecutor {
    rpc: Arc<dyn NodeRpcClient>,
    prover: Arc<dyn TransactionProver + Send + Sync>,
    genesis: tokio::sync::OnceCell<BlockHeader>,
}

impl MidenExecutor {
    pub fn new(
        rpc: Arc<dyn NodeRpcClient>,
        prover: Arc<dyn TransactionProver + Send + Sync>,
    ) -> Self {
        Self {
            rpc,
            prover,
            genesis: tokio::sync::OnceCell::new(),
        }
    }

    /// The node's genesis header, read once and pinned on the RPC client so later reads are
    /// verified against it.
    async fn genesis(&self) -> Result<&BlockHeader, ExecutionFailure> {
        self.genesis
            .get_or_try_init(|| async {
                let genesis = fetch_genesis(self.rpc.as_ref()).await?;
                if self.rpc.has_genesis_commitment().is_none() {
                    self.rpc
                        .set_genesis_commitment(genesis.commitment())
                        .await?;
                }
                Ok(genesis)
            })
            .await
            .map_err(chain_view_failure)
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
        } else if seen.contains(&signer) {
            Err("a duplicate of an earlier valid signature from the same cosigner")
        } else {
            verified_advice(signature, &signer, summary)
                .ok_or("not a valid signature over the signed summary")
                .inspect(|_| {
                    seen.insert(signer.clone());
                })
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

/// The advice entry for `signature` when it is `signer`'s valid signature over `summary`, under
/// the same acceptance rule as signing. An EIP-712 signature signs the summary's typed-data
/// digest, so it is keyed as the auth procedure reads it, exactly as the SDKs build it for local
/// execution.
fn verified_advice(
    signature: &CosignerSignature,
    signer: &str,
    summary: &TransactionSummary,
) -> Option<(Word, Vec<Felt>)> {
    verify_proposal_signature(summary, &signature.signature)
        .ok()?
        .eq_ignore_ascii_case(signer)
        .then_some(())?;
    let commitment = <Word as FromHex>::from_hex(signer).ok()?;
    let message = summary.to_commitment();
    match &signature.signature {
        ProposalSignature::Falcon { signature } => {
            let parsed = SignatureScheme::Falcon
                .parse_signature_hex(signature)
                .ok()?;
            SignatureScheme::Falcon
                .build_signature_advice_entry(commitment, message, &parsed, None)
                .ok()
        }
        ProposalSignature::Ecdsa {
            signature,
            public_key,
            message_format,
        } => {
            let parsed = SignatureScheme::Ecdsa.parse_signature_hex(signature).ok()?;
            match message_format {
                EcdsaMessageFormat::Raw => SignatureScheme::Ecdsa
                    .build_signature_advice_entry(
                        commitment,
                        message,
                        &parsed,
                        public_key.as_deref(),
                    )
                    .ok(),
                EcdsaMessageFormat::Eip712 => {
                    let AccountSignature::EcdsaK256Keccak(ecdsa) = &parsed else {
                        return None;
                    };
                    let key = parse_ecdsa_public_key_hex(public_key.as_deref()?).ok()?;
                    Some(summary.eip712_signature_advice(&key, ecdsa))
                }
            }
        }
    }
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
    match &error {
        ChainViewError::ChainBehind { .. } => {
            ExecutionFailure::new(ExecutionFailureCode::ChainBehind, error.to_string())
        }
        ChainViewError::Rpc(_) => ExecutionFailure::with_logged_cause(
            ExecutionFailureCode::NodeUnavailable,
            "Guardian could not read the chain from the node",
            &error,
        ),
        ChainViewError::Inconsistent(_) => ExecutionFailure::with_logged_cause(
            ExecutionFailureCode::ChainInconsistent,
            "the node served chain data that does not authenticate",
            &error,
        ),
    }
}

async fn chain_tip(rpc: &dyn NodeRpcClient) -> Result<BlockNumber, ExecutionFailure> {
    rpc.get_block_header_by_number(None, false)
        .await
        .map(|(header, _)| header.block_num())
        .map_err(|e| {
            ExecutionFailure::with_logged_cause(
                ExecutionFailureCode::NodeUnavailable,
                "Guardian could not read the chain tip from the node",
                &e,
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
        |cause| {
            cause
                .downcast_ref::<tonic::Status>()
                .map(|status| grpc_code_evidence(status.code() as i32))
                .unwrap_or(StructuredEvidence::Indeterminate)
        },
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
        let summary = proposal_tx_summary(&input.proposal_payload)?;
        Ok(select(input, &account, &summary).selection)
    }

    async fn prepare(
        &self,
        input: ExecutionInput,
    ) -> Result<Box<dyn ExecutionAttempt>, ExecutionFailure> {
        let started = std::time::Instant::now();
        let codec = |message: &'static str, cause: &dyn std::fmt::Display| {
            ExecutionFailure::with_logged_cause(ExecutionFailureCode::RequestCodec, message, cause)
        };
        let envelope = input
            .proposal_payload
            .get("transaction_request")
            .ok_or_else(|| {
                ExecutionFailure::new(
                    ExecutionFailureCode::RequestCodec,
                    "proposal carries no transaction request",
                )
            })
            .and_then(|value| {
                TransactionRequestEnvelope::deserialize(value)
                    .map_err(|e| codec("the stored request envelope is malformed", &e))
            })?;
        let bytes = envelope.verified_bytes().map_err(|rejection| {
            ExecutionFailure::new(rejection.failure_code(), rejection.to_string())
        })?;
        let request = StoredRequest::decode(&bytes).map_err(|e| {
            codec(
                "stored request does not decode with this server's miden-client, which may not \
                 be the version that serialized it",
                &e,
            )
        })?;
        let summary = proposal_tx_summary(&input.proposal_payload)
            .map_err(|e| codec("the signed transaction summary does not decode", &e))?;
        request.check_against(&summary).map_err(|reason| {
            ExecutionFailure::new(
                ExecutionFailureCode::RequestInvalid(reason),
                format!("stored request is not Guardian-executable: {reason:?}"),
            )
        })?;
        let account = Account::from_json(&input.state_json)
            .map_err(|e| codec("the account state does not decode", &e))?;
        let selected = select(&input, &account, &summary);
        let decoded = started.elapsed();

        let genesis = self.genesis().await?;
        let approval_expiration = approval_expiration_block(&summary);
        let tip = chain_tip(self.rpc.as_ref()).await?;
        if u64::from(tip.as_u32()) >= approval_expiration {
            return Err(ExecutionFailure::new(
                ExecutionFailureCode::ExpirationReached(ExpirationBound::Approval),
                format!("approval expired at block {approval_expiration}; the tip is {tip}"),
            ));
        }

        let tip_read = started.elapsed() - decoded;

        let unsigned = request
            .execution_inputs(&account, [])
            .map_err(|e| match e {
                super::request::RequestInputsError::Invalid(reason) => ExecutionFailure::new(
                    ExecutionFailureCode::RequestInvalid(reason),
                    format!("{reason:?}"),
                ),
                super::request::RequestInputsError::Malformed(message) => {
                    codec("the stored request's inputs are malformed", &message)
                }
            })?;
        let bound = summary.block_number();
        let mut tracked = BTreeSet::from([bound]);
        for block in unsigned
            .input_notes
            .iter()
            .filter_map(|note| note.location().map(|location| location.block_num()))
        {
            if block > bound {
                return Err(ExecutionFailure::new(
                    ExecutionFailureCode::BindingMismatch,
                    format!(
                        "a pinned note claims inclusion at block {block}, after block {bound} the summary binds"
                    ),
                ));
            }
            tracked.insert(block);
        }
        let assembly = std::time::Instant::now();
        let view = build_chain_view_from(self.rpc.as_ref(), genesis, &tracked)
            .await
            .map_err(chain_view_failure)?;
        let chain_view = assembly.elapsed();
        record_chain_view(chain_view);
        let notes: Vec<_> = unsigned
            .input_notes
            .iter()
            .map(|note| note.note().clone())
            .collect();
        let store = ExecutionDataStore::new(account.clone(), view, self.rpc.clone(), &notes);

        let reference = store.chain().reference_block();
        let executor: TransactionExecutor<'_, '_, _, UnreachableAuth> =
            TransactionExecutor::new(&store);
        let reproducing = std::time::Instant::now();
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
        let unsigned_run = reproducing.elapsed();
        let foreign_fetch = store.foreign_fetch_time();
        tracing::debug!(
            account_id = %account.id(),
            decode_ms = decoded.as_millis(),
            tip_ms = tip_read.as_millis(),
            chain_view_ms = chain_view.as_millis(),
            unsigned_run_ms = unsigned_run.as_millis(),
            foreign_fetch_ms = foreign_fetch.as_millis(),
            vm_ms = unsigned_run.saturating_sub(foreign_fetch).as_millis(),
            total_ms = started.elapsed().as_millis(),
            "Guardian execution prepare breakdown"
        );
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
        return match unavailable {
            ForeignAccountUnavailable::Private { account_id } => ExecutionFailure::new(
                ExecutionFailureCode::ForeignAccountUnavailable(
                    ForeignAccountUnavailableReason::Private,
                ),
                format!(
                    "foreign account {} is private, so the node cannot serve it",
                    account_id.to_hex()
                ),
            ),
            ForeignAccountUnavailable::Unavailable { account_id, reason } => {
                tracing::warn!(account_id = %account_id.to_hex(), %reason, "foreign account unavailable");
                ExecutionFailure::new(
                    ExecutionFailureCode::ForeignAccountUnavailable(
                        ForeignAccountUnavailableReason::Unavailable,
                    ),
                    format!(
                        "Guardian could not read foreign account {} from the node",
                        account_id.to_hex()
                    ),
                )
            }
        };
    }
    if let Some(node_failure) = store.node_failure() {
        return ExecutionFailure::with_logged_cause(
            ExecutionFailureCode::NodeUnavailable,
            "Guardian could not read data the transaction needs from the node",
            &node_failure,
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
        None => ExecutionFailure::with_logged_cause(
            ExecutionFailureCode::BindingMismatch,
            "the stored request does not execute",
            error,
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

    /// The prover's answer is not trusted: a proof the node would refuse, or one of another
    /// transaction, is refused here, before the boundary, where the proposal can still be retried.
    /// Verifying the proof is CPU-bound, so it runs on the blocking pool rather than holding a
    /// runtime thread that request handlers and lease heartbeats share.
    async fn ensure_proves_the_executed_transaction(
        &self,
        proven: ProvenTransaction,
    ) -> Result<ProvenTransaction, ExecutionFailure> {
        let executed = self.executed();
        let refused =
            |message: String| ExecutionFailure::new(ExecutionFailureCode::ProvingFailed, message);
        if proven.id() != executed.id()
            || proven.account_id() != executed.account_id()
            || proven.account_update().final_state_commitment()
                != executed.final_account().to_commitment()
        {
            return Err(refused(format!(
                "the prover returned transaction {} for executed transaction {}",
                proven.id().to_hex(),
                executed.id().to_hex()
            )));
        }
        tokio::task::spawn_blocking(move || {
            TransactionVerifier::new(miden_protocol::MIN_PROOF_SECURITY_LEVEL)
                .verify(&proven)
                .map(|_deferred_precompiles_settle_in_the_batch| proven)
        })
        .await
        .map_err(|error| {
            ExecutionFailure::with_logged_cause(
                ExecutionFailureCode::ProvingFailed,
                "Guardian could not verify the returned proof",
                &error,
            )
        })?
        .map_err(|error| {
            ExecutionFailure::with_logged_cause(
                ExecutionFailureCode::ProvingFailed,
                "the returned proof does not verify",
                &error,
            )
        })
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
                .map_err(|e| {
                    ExecutionFailure::with_logged_cause(
                        ExecutionFailureCode::BindingMismatch,
                        "Guardian's acknowledgment signature does not parse",
                        &e,
                    )
                })?;
            let entry = ack
                .scheme
                .build_signature_advice_entry(
                    commitment,
                    message,
                    &signature,
                    Some(ack.public_key_hex.as_str()),
                )
                .map_err(|e| {
                    ExecutionFailure::with_logged_cause(
                        ExecutionFailureCode::BindingMismatch,
                        "Guardian's acknowledgment does not form signature advice",
                        &e,
                    )
                })?;
            advice.push(entry);
        }
        let inputs = self
            .request
            .execution_inputs(&self.account, advice)
            .map_err(|e| {
                ExecutionFailure::with_logged_cause(
                    ExecutionFailureCode::RequestCodec,
                    "the stored request's inputs are malformed",
                    &e,
                )
            })?;
        let reference = self.store.chain().reference_block();
        self.store.begin_execution();
        let executor: TransactionExecutor<'_, '_, _, UnreachableAuth> =
            TransactionExecutor::new(&self.store);
        let signatures = self.signature_advice.len();
        let executed = executor
            .execute_transaction(
                self.account.id(),
                reference,
                inputs.input_notes,
                inputs.tx_args,
            )
            .await
            .map_err(|error| match error {
                TransactionExecutorError::Unauthorized(_) => ExecutionFailure::new(
                    ExecutionFailureCode::InsufficientSignatures,
                    format!(
                        "the {signatures} valid signatures do not meet the threshold of every \
                         procedure this transaction calls; collect more and request execution \
                         again"
                    ),
                ),
                error => execution_failure(&self.store, &error),
            })?;

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
        let info = ExecutedTransactionInfo {
            final_account_commitment: executed.final_account().to_commitment().into_hex(),
            reference_block: self.reference_block(),
            expiration_block: executed.expiration_block_num().as_u32(),
        };
        self.executed = Some(executed);
        Ok(info)
    }

    async fn prove(&mut self) -> Result<ProvenTransactionInfo, ExecutionFailure> {
        let expiration = self.executed().expiration_block_num();
        let inputs: TransactionInputs = self.executed().tx_inputs().clone();
        let mut backoff = PROVER_BACKOFF_START;
        let proving = std::time::Instant::now();
        let record_duration = || record_proving(proving.elapsed());
        let proven = loop {
            match self.prover.prove(inputs.clone()).await {
                Ok(proven) => {
                    record_duration();
                    break proven;
                }
                Err(error) if is_transient(&error) => {
                    record_prover_retry();
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
                            ExecutionFailureCode::ExpirationReached(_) => {
                                tracing::warn!(
                                    error = %with_sources(&error),
                                    "the prover stayed unreachable until the transaction expired"
                                );
                                ExecutionFailure {
                                    message: format!(
                                        "{}; the prover stayed unreachable until then",
                                        stop.message
                                    ),
                                    code: stop.code,
                                }
                            }
                            _ => stop,
                        })?;
                }
                Err(error) => {
                    record_duration();
                    return Err(ExecutionFailure::with_logged_cause(
                        ExecutionFailureCode::ProvingFailed,
                        "the prover refused the transaction",
                        &with_sources(&error),
                    ));
                }
            }
        };
        let proven = self.ensure_proves_the_executed_transaction(proven).await?;
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
        .map_err(|e| {
            ExecutionFailure::with_logged_cause(
                ExecutionFailureCode::SealingFailed,
                "Guardian could not seal the transaction inputs for submission",
                &e,
            )
        })?;
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
            Err(error) => classify_submission_error(&error),
        }
    }
}

/// Past the boundary a rejection deletes the proposal, so only a node that answered and refused
/// the transaction is a rejection. A response that failed to arrive or to decode may still have
/// been accepted, and a node reporting the transaction as already known may hold it, so both are
/// left for the chain to settle.
fn classify_submission_error(error: &miden_client::rpc::RpcError) -> SubmissionOutcome {
    use miden_client::rpc::{GrpcError, RpcError};
    let refused = matches!(
        error,
        RpcError::RequestError { error_kind, .. } if !matches!(error_kind, GrpcError::AlreadyExists)
    ) && !error.is_indeterminate_submission();
    let reason = error.to_string();
    if refused {
        SubmissionOutcome::Rejected { reason }
    } else {
        SubmissionOutcome::Unknown { reason }
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

    #[derive(Debug, thiserror::Error)]
    #[error("failed to prove transaction")]
    struct ProveRefused(#[source] tonic::Status);

    #[test]
    fn the_prover_status_code_decides_over_its_message() {
        assert!(!is_transient(&ProveRefused(
            tonic::Status::invalid_argument("inputs unavailable: request timed out upstream")
        )));
        assert!(is_transient(&ProveRefused(tonic::Status::unavailable(
            "prover busy"
        ))));
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
            let failure = chain_view_failure(error);
            assert_eq!(failure.code, code);
            assert!(
                !failure.message.contains("header") && !failure.message.contains("MMR"),
                "{}",
                failure.message
            );
        }
    }
}

#[cfg(all(test, feature = "e2e"))]
mod execution_failure_tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use miden_client::testing::mock::MockRpcApi;
    use miden_testing::{Auth, MockChainBuilder};
    use miden_tx::TransactionExecutorError;

    use super::super::chain::build_chain_view;
    use super::{ExecutionDataStore, ExecutionFailureCode, execution_failure};

    async fn store() -> ExecutionDataStore {
        let mut builder = MockChainBuilder::new();
        let wallet = builder.add_existing_wallet(Auth::IncrNonce).unwrap();
        let rpc = Arc::new(MockRpcApi::new(builder.build().unwrap()));
        rpc.advance_blocks(2);
        let view = build_chain_view(rpc.as_ref(), &BTreeSet::new())
            .await
            .unwrap();
        ExecutionDataStore::new(wallet, view, rpc, &[])
    }

    #[tokio::test]
    async fn an_execution_stopped_by_a_node_read_is_node_unavailable() {
        let store = store().await;
        let error = TransactionExecutorError::MissingAuthenticator;
        assert_eq!(
            execution_failure(&store, &error).code,
            ExecutionFailureCode::BindingMismatch
        );

        store.record_node_failure("transport error: connection refused".to_string());
        let failure = execution_failure(&store, &error);
        assert_eq!(failure.code, ExecutionFailureCode::NodeUnavailable);
        assert!(!failure.message.contains("connection refused"));

        store.begin_execution();
        assert_eq!(
            execution_failure(&store, &error).code,
            ExecutionFailureCode::BindingMismatch
        );
    }
}

#[cfg(test)]
mod submission_classification_tests {
    use miden_client::rpc::{GrpcError, RpcEndpoint, RpcError};

    use super::{SubmissionOutcome, classify_submission_error};

    fn refused(kind: GrpcError) -> RpcError {
        RpcError::RequestError {
            endpoint: RpcEndpoint::SubmitProvenTx,
            error_kind: kind,
            endpoint_error: None,
            source: None,
        }
    }

    fn is_unknown(error: RpcError) -> bool {
        matches!(
            classify_submission_error(&error),
            SubmissionOutcome::Unknown { .. }
        )
    }

    #[test]
    fn only_a_node_that_answered_and_refused_is_a_rejection() {
        assert!(matches!(
            classify_submission_error(&refused(GrpcError::InvalidArgument)),
            SubmissionOutcome::Rejected { .. }
        ));
        assert!(is_unknown(refused(GrpcError::Unavailable)));
        assert!(is_unknown(refused(GrpcError::AlreadyExists)));
        assert!(is_unknown(RpcError::ConnectionError("reset".into())));
        assert!(is_unknown(RpcError::DeserializationError(
            "truncated".to_string()
        )));
        assert!(is_unknown(RpcError::InvalidResponse("garbled".to_string())));
        assert!(is_unknown(RpcError::ExpectedDataMissing(
            "block".to_string()
        )));
    }
}
