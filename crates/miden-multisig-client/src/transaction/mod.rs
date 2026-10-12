//! Transaction building and execution for multisig operations.

mod auth_args;
mod builder;
mod configuration;
mod consume;
mod guardian;
mod payment;

pub use auth_args::{
    MAX_APPROVAL_EXPIRATION_DELTA, TransactionRequestBuilderExt, multisig_auth_args,
    proposal_auth_args, proposer_auth_args, summary_approval_expiration_block_num, summary_salt,
    synced_fee_faucet_id,
};
pub use builder::{ProposalBuilder, ProposalOptions};
pub use configuration::{
    build_update_procedure_threshold_transaction_request, build_update_signers_transaction_request,
};
pub(crate) use consume::ensure_notes_authenticated;
pub use consume::{
    build_consume_notes_transaction_request, build_consume_notes_transaction_request_from_notes,
};
pub use guardian::build_update_guardian_transaction_request;
pub use payment::build_p2id_transaction_request;

use miden_client::ClientError;
use miden_client::transaction::{TransactionExecutorError, TransactionRequest, TransactionSummary};
use miden_protocol::account::AccountId;
use miden_protocol::block::BlockNumber;
use miden_protocol::{Felt, Word};

use crate::MidenSdkClient;
use crate::error::{MultisigError, Result};

/// Deserializes a producer-supplied transaction request bytes (issue #266 producer
/// API). The bytes are the serialized form of a Miden `TransactionRequest`.
pub fn deserialize_transaction_request(bytes: &[u8]) -> Result<TransactionRequest> {
    use miden_client::Deserializable;
    TransactionRequest::read_from_bytes(bytes).map_err(|e| {
        MultisigError::InvalidConfig(format!("failed to decode transaction request: {e}"))
    })
}

/// Executes a multisig request at the chain tip to get its summary (expects the
/// Unauthorized error). Proposers derive a new proposal's summary with it, and
/// cosigners and the executor reproduce it, whatever block they have synced to.
///
/// Since protocol 0.17 a multisig summary binds the block its auth args name
/// (the bound block), not the block the transaction executes against, so it
/// reproduces at any later tip once the bound block is in the transaction's
/// partial blockchain. The request declares it (see
/// [`TransactionRequestBuilderExt`]), and foreign accounts, the fee faucet
/// among them, load at the tip. Executing at the bound block instead would load
/// them there, which a node prunes about 50 blocks later (issue #462).
pub async fn execute_for_summary_at_tip(
    client: &mut MidenSdkClient,
    account_id: AccountId,
    request: TransactionRequest,
) -> Result<TransactionSummary> {
    prepare_tip_execution(client, &request).await?;
    match client.execute_transaction(account_id, request).await {
        Ok(_) => Err(MultisigError::UnexpectedSuccess),
        Err(ClientError::TransactionExecutorError(TransactionExecutorError::Unauthorized(
            summary,
        ))) => Ok(*summary),
        Err(ClientError::TransactionExecutorError(err)) => {
            Err(MultisigError::TransactionExecution(err.to_string()))
        }
        Err(err) => Err(MultisigError::from(err)),
    }
}

/// Gets `client` ready to execute `request` at the chain tip: checks the
/// request declares the block its multisig auth args bind, and syncs to that
/// block (see [`sync_to_block`]).
pub(crate) async fn prepare_tip_execution(
    client: &mut MidenSdkClient,
    request: &TransactionRequest,
) -> Result<()> {
    match declared_bound_block(request)? {
        Some(bound_block_num) => sync_to_block(client, bound_block_num).await,
        None => Ok(()),
    }
}

/// Syncs `client` once when its sync height is below `bound_block_num`, the
/// block a proposal binds. Execution at a tip below it fails with "requested
/// block N is after transaction reference block M", and a store that has never
/// synced (a cosigner that has only just pulled the account) holds no header
/// to rebuild the request from.
///
/// This does not make a store that is already past the bound block current:
/// the entry points that re-execute a proposal sync the chain first, since an
/// execution loads foreign accounts, the fee faucet among them, at the store's
/// sync height, and a node prunes that state about 50 blocks later.
pub(crate) async fn sync_to_block(
    client: &mut MidenSdkClient,
    bound_block_num: BlockNumber,
) -> Result<()> {
    if sync_height(client).await? >= bound_block_num {
        return Ok(());
    }
    client.sync_state().await.map_err(|e| {
        MultisigError::miden_client_with_context(
            format!("failed to sync to block {bound_block_num} the proposal binds"),
            e,
        )
    })?;
    let synced = sync_height(client).await?;
    if synced < bound_block_num {
        return Err(MultisigError::ChainBehindBoundBlock {
            synced,
            bound_block_num,
        });
    }
    Ok(())
}

/// Syncs `client` to the chain tip before a proposal is re-executed, so the
/// execution's reference block is current: foreign accounts, the fee faucet
/// among them, load at the store's sync height, and a node prunes that state
/// about 50 blocks later.
pub(crate) async fn sync_chain(client: &mut MidenSdkClient) -> Result<()> {
    client.sync_state().await.map_err(|e| {
        MultisigError::miden_client_with_context(
            "failed to sync the Miden client before re-executing the proposal",
            e,
        )
    })?;
    Ok(())
}

/// Whether a failed re-execution came from chain state this client can catch
/// up with rather than from the proposal itself: a node that has not reached
/// the bound block yet, or account state the node pruned because the store
/// was not synced recently. Either clears on a later attempt, which syncs
/// first.
pub(crate) fn is_stale_chain_error(error: &MultisigError) -> bool {
    matches!(error, MultisigError::ChainBehindBoundBlock { .. })
        || error.to_string().contains("has been pruned")
}

async fn sync_height(client: &MidenSdkClient) -> Result<BlockNumber> {
    client
        .get_sync_height()
        .await
        .map_err(|e| MultisigError::miden_client_with_context("failed to read the sync height", e))
}

/// The block `request`'s multisig auth args bind, after checking the request
/// declares it. `None` for a request without multisig auth args, which has no
/// bound block to declare.
pub(crate) fn declared_bound_block(request: &TransactionRequest) -> Result<Option<BlockNumber>> {
    let Some(bound_block_num) = request_bound_block_num(request) else {
        return Ok(None);
    };
    if !request.block_numbers().contains(&bound_block_num) {
        return Err(MultisigError::BoundBlockNotDeclared { bound_block_num });
    }
    Ok(Some(bound_block_num))
}

/// The block a multisig request's summary binds, read from the auth-args
/// preimage the request carries in its advice map (the first element of
/// [`MultisigAuthArgs`](miden_standards::account::auth::MultisigAuthArgs)'s
/// `[BLOCK_WORD, SALT, CONVERSION_INFO]`). `None` when the request carries no
/// such preimage.
fn request_bound_block_num(request: &TransactionRequest) -> Option<BlockNumber> {
    const AUTH_ARGS_NUM_ELEMENTS: usize = 12;

    let auth_arg = (*request.auth_arg())?;
    let preimage = request.advice_map().get(&auth_arg)?;
    if preimage.len() != AUTH_ARGS_NUM_ELEMENTS {
        return None;
    }
    u32::try_from(preimage[0].as_canonical_u64())
        .ok()
        .map(BlockNumber::from)
}

/// Generates a random salt word.
pub fn generate_salt() -> Word {
    let mut bytes = [0u8; 32];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut bytes);

    let mut felts = [Felt::ZERO; 4];
    for (i, chunk) in bytes.chunks(8).enumerate() {
        let mut arr = [0u8; 8];
        arr.copy_from_slice(chunk);
        felts[i] = guardian_shared::felt::felt_from_u64_reduced(u64::from_le_bytes(arr));
    }
    felts.into()
}

/// Converts a Word to hex string with 0x prefix.
pub fn word_to_hex(word: &Word) -> String {
    let bytes: Vec<u8> = word
        .iter()
        .flat_map(|felt| felt.as_canonical_u64().to_le_bytes())
        .collect();
    format!("0x{}", hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_transaction_request_rejects_garbage_bytes() {
        let err = deserialize_transaction_request(&[0xde, 0xad, 0xbe, 0xef])
            .expect_err("garbage bytes must not deserialize");
        assert!(
            err.to_string()
                .contains("failed to decode transaction request")
        );
    }

    mod declared_bound_block {
        use miden_client::transaction::TransactionRequestBuilder;
        use miden_protocol::crypto::SequentialCommit;
        use miden_standards::account::auth::MultisigAuthArgs;

        use super::super::{TransactionRequestBuilderExt, declared_bound_block};
        use super::*;

        fn auth_args() -> MultisigAuthArgs {
            MultisigAuthArgs::new(BlockNumber::from(11), Word::from([1u32, 2, 3, 4]))
        }

        #[test]
        fn a_request_built_with_the_extension_declares_its_bound_block() {
            let request = TransactionRequestBuilder::new()
                .multisig_auth_args(&auth_args())
                .build()
                .expect("request builds");

            assert_eq!(
                declared_bound_block(&request).expect("the bound block is declared"),
                Some(BlockNumber::from(11))
            );
        }

        /// Auth args attached by hand without the declaration execute only at the
        /// bound block itself; at any later tip the auth procedure cannot read the
        /// bound block. Refused by name instead of failing inside the VM.
        #[test]
        fn a_request_that_binds_a_block_without_declaring_it_is_refused() {
            let auth_args = auth_args();
            let commitment = auth_args.to_commitment();
            let request = TransactionRequestBuilder::new()
                .auth_arg(commitment)
                .extend_advice_map([(commitment, auth_args.to_elements())])
                .build()
                .expect("request builds");

            match declared_bound_block(&request) {
                Err(MultisigError::BoundBlockNotDeclared { bound_block_num }) => {
                    assert_eq!(bound_block_num, BlockNumber::from(11));
                }
                other => panic!("expected BoundBlockNotDeclared, got {other:?}"),
            }
        }

        #[test]
        fn a_request_without_multisig_auth_args_has_no_bound_block() {
            let request = TransactionRequestBuilder::new()
                .build()
                .expect("request builds");

            assert_eq!(
                declared_bound_block(&request).expect("nothing to declare"),
                None
            );
        }
    }

    #[test]
    fn deserialize_transaction_request_rejects_empty_bytes() {
        let err =
            deserialize_transaction_request(&[]).expect_err("empty bytes must not deserialize");
        assert!(
            err.to_string()
                .contains("failed to decode transaction request")
        );
    }

    /// Guards the locally assembled transaction kernel against network drift.
    /// Dependency changes must preserve the live network's proof commitment.
    #[test]
    fn transaction_kernel_commitment_matches_network() {
        use miden_protocol::transaction::TransactionKernel;

        const EXPECTED_KERNEL_COMMITMENT: &str =
            "0x12b6033c1334140b2d553b871260ff87b62275a7eccd3829975ffd58f9f2a80a";

        let actual = word_to_hex(&TransactionKernel.to_commitment());
        assert_eq!(
            actual, EXPECTED_KERNEL_COMMITMENT,
            "transaction kernel commitment drifted from the network kernel; a transitive \
             hashing crate (e.g. Plonky3 `p3-*`) likely changed in Cargo.lock"
        );
    }
}
