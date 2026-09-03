//! Transaction building and execution for multisig operations.

mod builder;
mod configuration;
mod consume;
mod guardian;
mod payment;

pub use builder::ProposalBuilder;
pub use configuration::{
    build_update_procedure_threshold_transaction_request, build_update_signers_transaction_request,
};
pub use consume::{
    build_consume_notes_transaction_request, build_consume_notes_transaction_request_from_notes,
};
pub(crate) use fee::MaybeFeeConversionInfo;
pub use guardian::build_update_guardian_transaction_request;
pub use payment::build_p2id_transaction_request;

mod fee {
    use miden_client::Word;
    use miden_client::transaction::TransactionRequestBuilder;
    use miden_standards::account::auth::{FeeConversionInfo, commit_fee_conversion_info};

    /// Sets the auth args of a multisig request, committing fee conversion info when there is any.
    ///
    /// Since protocol 0.16 the multisig auth procedures pay the transaction fee and read the
    /// conversion info from the auth args, so a request that passes the salt through bare aborts
    /// in `fee::pay_fee` with `ERR_FEE_CONVERSION_INFO_MISSING` wherever the verification base fee
    /// is non-zero. Committing keeps the salt's other role intact: the commitment is what the
    /// summary binds, and distinct salts still produce distinct commitments.
    ///
    /// The commitment and its advice preimage are applied by hand rather than through
    /// [`TransactionRequestBuilder::fee_conversion_info`]. That method additionally marks the
    /// request as *declaring* conversion info, and miden-client refuses such a request before
    /// execution for every auth component outside `AuthSingleSig`/`AuthMultisig` — which includes
    /// the `AuthGuardedMultisig` component these accounts use. The auth arg and advice entries
    /// come out the same either way; the serialized request differs by the declaration flag.
    /// The TypeScript SDK sets the same two fields the same way.
    ///
    /// `None` reproduces the pre-fee behaviour and is only valid on a chain that charges nothing.
    pub(crate) trait MaybeFeeConversionInfo {
        fn maybe_fee_conversion_info(
            self,
            fee_conversion_info: Option<FeeConversionInfo>,
            salt: Word,
        ) -> Self;
    }

    impl MaybeFeeConversionInfo for TransactionRequestBuilder {
        fn maybe_fee_conversion_info(
            self,
            fee_conversion_info: Option<FeeConversionInfo>,
            salt: Word,
        ) -> Self {
            match fee_conversion_info {
                Some(info) => {
                    let (auth_arg, preimage) = commit_fee_conversion_info(info, salt);
                    self.auth_arg(auth_arg)
                        .extend_advice_map([(auth_arg, preimage)])
                }
                None => self.auth_arg(salt),
            }
        }
    }
}

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use miden_client::ClientError;
use miden_client::transaction::{
    ChainAnchor, TransactionExecutorError, TransactionRequest, TransactionSummary,
};
use miden_protocol::account::AccountId;
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

/// Serializes a [`ChainAnchor`] to base64 for the proposal wire payload.
pub fn chain_anchor_to_base64(anchor: &ChainAnchor) -> String {
    use miden_client::Serializable;
    BASE64.encode(anchor.to_bytes())
}

/// Deserializes a [`ChainAnchor`] from its base64 wire form. `ChainAnchor`
/// deserialization validates the header/chain consistency internally, so a
/// decoded anchor only needs its block commitment checked against the signed
/// transaction summary before it is safe to execute against.
pub fn chain_anchor_from_base64(anchor_b64: &str) -> Result<ChainAnchor> {
    use miden_client::Deserializable;
    let bytes = BASE64
        .decode(anchor_b64)
        .map_err(|e| MultisigError::InvalidConfig(format!("invalid chain_anchor base64: {e}")))?;
    ChainAnchor::read_from_bytes(&bytes)
        .map_err(|e| MultisigError::InvalidConfig(format!("invalid chain_anchor: {e}")))
}

/// Captures a [`ChainAnchor`] for the request at the current sync height and
/// executes the transaction against it to get its summary (expects the
/// Unauthorized error). The anchor is returned alongside the summary so the
/// proposer can ship it with the signed data; cosigners and the executor then
/// reproduce the summary — which binds the reference block commitment since
/// protocol 0.16 — with [`execute_for_summary_at`] regardless of their own
/// sync height.
pub async fn execute_for_summary(
    client: &mut MidenSdkClient,
    account_id: AccountId,
    request: TransactionRequest,
) -> Result<(TransactionSummary, ChainAnchor)> {
    let anchor = client
        .chain_anchor_for_request(&request)
        .await
        .map_err(|e| MultisigError::MidenClient(format!("failed to capture chain anchor: {e}")))?;
    let summary = execute_for_summary_at(client, account_id, request, anchor.clone()).await?;
    Ok((summary, anchor))
}

/// Executes a transaction at the given [`ChainAnchor`]'s reference block to
/// get its summary (expects Unauthorized error).
pub async fn execute_for_summary_at(
    client: &mut MidenSdkClient,
    account_id: AccountId,
    request: TransactionRequest,
    anchor: ChainAnchor,
) -> Result<TransactionSummary> {
    match client
        .execute_transaction_at(account_id, request, anchor)
        .await
    {
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

    /// The commitment must reach the request through `auth_arg` + `extend_advice_map`, never
    /// through `TransactionRequestBuilder::fee_conversion_info`.
    ///
    /// That method also flags the request as declaring conversion info, and miden-client rejects
    /// such a request before execution for every auth component except `AuthSingleSig` and
    /// `AuthMultisig`. These accounts run `AuthGuardedMultisig`, so a declaring request builds
    /// fine and is then rejected during account validation, before execution.
    #[test]
    fn committed_fee_conversion_info_does_not_declare_itself() {
        use miden_client::transaction::TransactionRequestBuilder;
        use miden_protocol::account::{AccountId, AccountType};
        use miden_standards::account::auth::{FeeConversionInfo, commit_fee_conversion_info};

        let faucet = AccountId::builder()
            .account_type(AccountType::Public)
            .build_with_seed([7u8; 32]);
        let info = FeeConversionInfo::one_to_one(faucet);
        let salt = Word::from([1u32, 2, 3, 4]);
        let (expected_auth_arg, expected_preimage) = commit_fee_conversion_info(info, salt);

        let request = TransactionRequestBuilder::new()
            .maybe_fee_conversion_info(Some(info), salt)
            .build()
            .expect("request should build");

        assert!(
            !request.declares_fee_conversion_info(),
            "declaring the conversion info makes miden-client reject the guarded account"
        );
        assert_eq!(*request.auth_arg(), Some(expected_auth_arg));
        assert_eq!(
            request
                .advice_map()
                .get(&expected_auth_arg)
                .map(|values| values.to_vec()),
            Some(expected_preimage),
            "the preimage must be present or `load_conversion_info` finds nothing"
        );
    }

    /// Pins the commitment to a literal digest, which is the one value both SDKs must agree on.
    ///
    /// Everything else about cross-SDK reconstruction is derivable — the faucet comes from the
    /// proposal's anchor and the rate is fixed — so this hash is the whole of the shared
    /// contract. The TypeScript side asserts the same four felts for the same inputs in
    /// `packages/miden-multisig-client/src/transaction/feeAuth.test.ts`, where it reaches them
    /// through `Poseidon2.hashElements` rather than `merge`. Asserting it only there would leave
    /// a swap of the operand order invisible to `cargo test`: the two Rust tests above compute
    /// their expectation with the same `commit_fee_conversion_info` the code under test calls,
    /// so they move with it. Change either side and this fails; update it only against a digest
    /// recomputed independently, never by copying whatever the code now returns.
    #[test]
    fn fee_conversion_commitment_matches_the_typescript_vector() {
        use miden_protocol::Hasher;
        use miden_protocol::account::{AccountId, AccountType};
        use miden_standards::account::auth::{FeeConversionInfo, commit_fee_conversion_info};

        let conversion_info = Word::from([0xdeadu32, 0xbeef, 1, 1]);
        let salt = Word::from([11u32, 22, 33, 44]);

        assert_eq!(
            Hasher::merge(&[conversion_info, salt]),
            Word::from(&[
                Felt::new_unchecked(8229881116367716759),
                Felt::new_unchecked(4629940889584181757),
                Felt::new_unchecked(3690706593687614873),
                Felt::new_unchecked(14641540369284480384),
            ]),
            "the auth-arg digest diverged from the TypeScript SDK's pinned vector"
        );

        let faucet = AccountId::builder()
            .account_type(AccountType::Public)
            .build_with_seed([7u8; 32]);
        let info = FeeConversionInfo::one_to_one(faucet);
        let (auth_arg, preimage) = commit_fee_conversion_info(info, salt);

        assert_eq!(
            auth_arg,
            Hasher::merge(&[info.to_word(), salt]),
            "the commitment must hash CONVERSION_INFO before SALT"
        );
        assert_eq!(
            preimage,
            [salt.as_elements(), info.to_word().as_elements()].concat(),
            "load_conversion_info pops the salt first, so the preimage is SALT ++ CONVERSION_INFO"
        );
    }

    /// Pins the felt layout of the conversion-info word itself.
    ///
    /// The digest vector above fixes the hash but feeds it a hand-written word, so it says
    /// nothing about how a *faucet* becomes that word. Rust delegates to
    /// `FeeConversionInfo::to_word`, while the TypeScript SDK hand-rolls the same four felts in
    /// `conversionInfoWord`. Only this assertion couples them: without it a reordering inside
    /// `to_word` would carry Rust along silently, leave TypeScript behind, and still leave both
    /// suites green while the two SDKs committed different auth args for the same proposal.
    #[test]
    fn conversion_info_word_layout_matches_the_typescript_sdk() {
        use miden_protocol::account::{AccountId, AccountType};
        use miden_standards::account::auth::FeeConversionInfo;

        let faucet = AccountId::builder()
            .account_type(AccountType::Public)
            .build_with_seed([7u8; 32]);

        assert_eq!(
            FeeConversionInfo::one_to_one(faucet).to_word(),
            Word::from([
                faucet.suffix(),
                faucet.prefix().as_felt(),
                Felt::new_unchecked(1),
                Felt::new_unchecked(1),
            ]),
            "conversionInfoWord builds [suffix, prefix, 1, 1]; to_word must agree felt for felt"
        );

        // `one_to_one` sets both rate slots to ONE, which leaves a numerator/denominator
        // swap indistinguishable. A distinct-rate case separates them. Native fees never
        // take this branch, but the layout the two SDKs share is the same either way.
        let skewed = FeeConversionInfo::new(faucet, 2, 3).expect("a 2/3 rate is representable");
        assert_eq!(
            skewed.to_word(),
            Word::from([
                faucet.suffix(),
                faucet.prefix().as_felt(),
                Felt::new_unchecked(2),
                Felt::new_unchecked(3),
            ]),
            "the numerator precedes the denominator"
        );
    }

    /// Without conversion info the salt stays the auth arg, and nothing is committed.
    #[test]
    fn absent_fee_conversion_info_passes_the_salt_through() {
        use miden_client::transaction::TransactionRequestBuilder;

        let salt = Word::from([9u32, 8, 7, 6]);

        let request = TransactionRequestBuilder::new()
            .maybe_fee_conversion_info(None, salt)
            .build()
            .expect("request should build");

        assert!(!request.declares_fee_conversion_info());
        assert_eq!(*request.auth_arg(), Some(salt));
    }

    #[test]
    fn deserialize_transaction_request_rejects_garbage_bytes() {
        let err = deserialize_transaction_request(&[0xde, 0xad, 0xbe, 0xef])
            .expect_err("garbage bytes must not deserialize");
        assert!(
            err.to_string()
                .contains("failed to decode transaction request")
        );
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
            "0xeb141480ed70ab3d2bf3bb1ec8e84358c41ca11045aecbbd95881c5a2f95ca43";

        let actual = word_to_hex(&TransactionKernel.to_commitment());
        assert_eq!(
            actual, EXPECTED_KERNEL_COMMITMENT,
            "transaction kernel commitment drifted from the network kernel; a transitive \
             hashing crate (e.g. Plonky3 `p3-*`) likely changed in Cargo.lock"
        );
    }
}
