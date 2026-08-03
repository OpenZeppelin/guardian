//! Live-network validation of chain-MMR acquisition (issue #254).
//!
//! These are `#[ignore]`d because they need outbound network access, following the same
//! convention as the Postgres tests. Run with:
//!
//! ```text
//! cargo test -p guardian-server --features proving --lib live_ -- --ignored --nocapture
//! ```
//!
//! Everything here is **read-only**: `get_block_header` and `sync_chain_mmr` are queries. No
//! account, no funds, and no writes are involved, which is why validating the cold-start recipe
//! needs no infrastructure of our own.
//!
//! Endpoint defaults to the public Miden testnet and can be overridden with
//! `GUARDIAN_TEST_RPC_ENDPOINT`.

use std::collections::BTreeMap;

use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::merkle::MerklePath;
use miden_protocol::transaction::{InputNotes, PartialBlockchain};
use miden_rpc_client::MidenRpcClient;

use super::blockchain::acquire_chain_mmr;
use super::build_chain_view;

const DEFAULT_ENDPOINT: &str = "https://rpc.testnet.miden.io";

fn endpoint() -> String {
    std::env::var("GUARDIAN_TEST_RPC_ENDPOINT").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string())
}

async fn connect() -> MidenRpcClient {
    let endpoint = endpoint();
    MidenRpcClient::connect(endpoint.clone())
        .await
        .unwrap_or_else(|e| panic!("could not reach {endpoint}: {e}"))
}

/// The claim this settles is the least-verified load-bearing one in the design: that seeding a
/// `PartialMmr` with the genesis commitment and applying `SyncChainMmr(0)`'s delta yields peaks
/// that hash to the reference header's `chain_commitment`.
///
/// It rests on the node mapping `current_client_block_height = 0` to a delta *from* a one-leaf
/// forest. If that reading is wrong, `build_chain_view`'s own gate rejects the result and this
/// test fails with the two commitments printed — which is the outcome worth knowing.
#[tokio::test]
#[ignore = "requires outbound access to a Miden RPC node"]
async fn live_cold_start_chain_mmr_matches_the_reference_block() {
    let mut rpc = connect().await;
    let input_notes = InputNotes::default();

    let view = build_chain_view(&mut rpc, &input_notes)
        .await
        .expect("cold-start chain view assembles and its peaks match the reference block");

    println!(
        "LIVE endpoint={} reference_block={} chain_commitment={}",
        endpoint(),
        view.reference_block.block_num().as_u32(),
        view.reference_block.chain_commitment()
    );

    assert!(
        view.reference_block.block_num().as_u32() > 0,
        "a live chain should be past genesis; got block 0"
    );
    assert_eq!(
        view.blockchain.chain_length(),
        view.reference_block.block_num(),
        "the assembled blockchain's length must match the reference block it was built against"
    );
}

/// `SyncNotes` already provides the snapshot-pinned primitive needed by a stateless executor.
/// Its paths are opened at forest `block_to + 1`, so for execution against reference block `N`
/// Guardian must request through `N - 1`: the reference header commits to exactly that `N`-leaf
/// forest. This test exercises the off-by-one against real note-bearing blocks and follows the
/// endpoint's block-number pagination.
#[tokio::test]
#[ignore = "requires outbound access to a Miden RPC node"]
async fn live_sync_notes_paths_track_against_the_execution_reference_forest() {
    let mut rpc = connect().await;
    let (mut partial_mmr, reference_block) = acquire_chain_mmr(&mut rpc)
        .await
        .expect("cold-start chain MMR assembles");
    let reference = reference_block.block_num().as_u32();

    if reference < 2 {
        eprintln!("LIVE chain too short at block {reference} to sync historical notes; skipping");
        return;
    }

    let block_to = reference - 1;
    // One broad account-target tag is enough to exercise real paths. SyncNotes is tag/range
    // based, so sampling many tags from genesis can return a very large response even though a
    // transaction only needs proofs for its exact input notes.
    let note_tags = vec![0];
    let mut block_from = block_to.saturating_sub(10_000);
    let mut tracked = BTreeMap::<BlockNumber, BlockHeader>::new();

    loop {
        let response = rpc
            .sync_notes(block_from, block_to, note_tags.clone())
            .await
            .expect("SyncNotes accepts a reference-pinned range");
        let page = response
            .pagination_info
            .expect("SyncNotes returns pagination information");

        assert!(
            page.block_num >= block_from && page.block_num <= block_to,
            "SyncNotes page ended outside the requested range: from={block_from}, to={block_to}, end={}",
            page.block_num
        );

        for block in response.blocks {
            let header_proto = block
                .block_header
                .expect("SyncNotes block contains a block header");
            let header: BlockHeader = header_proto
                .try_into()
                .expect("SyncNotes block header is valid");
            let path_proto = block
                .mmr_path
                .expect("SyncNotes block contains an MMR path");
            let path: MerklePath = path_proto.try_into().expect("SyncNotes MMR path is valid");

            partial_mmr
                .track(header.block_num().as_usize(), header.commitment(), &path)
                .unwrap_or_else(|e| {
                    panic!(
                        "SyncNotes path for block {} did not track against reference block {reference}: {e}",
                        header.block_num().as_u32()
                    )
                });
            tracked.insert(header.block_num(), header);
        }

        if page.block_num == block_to {
            break;
        }
        block_from = page.block_num + 1;
    }

    assert!(
        !tracked.is_empty(),
        "SyncNotes found no testnet notes for the sampled account-target tag"
    );

    let tracked_count = tracked.len();
    let blockchain = PartialBlockchain::new(partial_mmr, tracked.into_values().collect::<Vec<_>>())
        .expect("SyncNotes paths form a partial blockchain at the execution reference forest");

    assert_eq!(blockchain.chain_length(), reference_block.block_num());
    println!(
        "LIVE tracked {tracked_count} SyncNotes blocks against execution reference {reference} (block_to={block_to})"
    );
}

/// **Stage 1 of live validation — needs no account and no funds.**
///
/// Assembles the witness from **live testnet chain data** rather than `MockChain`, executes a
/// locally-created multisig account's transaction against it, and proves it through the
/// **remote prover**. The account does not exist on chain, which is fine: nothing in execution
/// or proving consults the chain for the account — that is the node's job at submission.
///
/// So this isolates the one composition that was still untested: does a witness built from real
/// chain data, rather than a test harness's, produce a proof at all? Submission is Stage 2 and
/// is the only part that needs a funded, Guardian-registered account.
///
/// Set `GUARDIAN_TX_PROVER_URL` to a reachable prover to run it.
#[cfg(feature = "e2e")]
#[tokio::test]
#[ignore = "requires outbound access to a Miden RPC node and a remote prover"]
async fn live_prove_a_guardian_assembled_witness() {
    use miden_confidential_contracts::multisig_guardian::{
        MultisigGuardianBuilder, MultisigGuardianConfig,
    };
    use miden_protocol::Word;
    use miden_protocol::account::auth::AuthSecretKey;
    use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
    use miden_protocol::transaction::{InputNotes, TransactionArgs};
    use miden_remote_prover_client::RemoteTransactionProver;
    use miden_tx::auth::{BasicAuthenticator, SigningInputs, TransactionAuthenticator};
    use miden_tx::{TransactionExecutor, TransactionExecutorError};

    use super::ExecutionDataStore;

    let Ok(prover_url) = std::env::var("GUARDIAN_TX_PROVER_URL") else {
        eprintln!("LIVE GUARDIAN_TX_PROVER_URL not set; skipping remote-prover validation");
        return;
    };

    let mut rpc = connect().await;

    // Version skew is the prime suspect for a prover rejection: our dependency line is Miden
    // 0.15 and the public networks have been running 0.16 prereleases.
    let status = rpc.get_status().await.expect("node reports status");
    eprintln!("LIVE node version={} chain_tip={}", status.version, status.chain_tip);

    // The piece under test: chain data from the live network, not a harness.
    let input_notes = InputNotes::default();
    let view = build_chain_view(&mut rpc, &input_notes)
        .await
        .expect("live chain view assembles");

    let cosigner = SecretKey::new();
    let guardian = SecretKey::new();
    let config = MultisigGuardianConfig::new(
        1,
        vec![cosigner.public_key().to_commitment()],
        guardian.public_key().to_commitment(),
    );
    let account = MultisigGuardianBuilder::new(config)
        .build_existing()
        .expect("multisig account builds");

    let store = ExecutionDataStore::new(
        account.clone(),
        view.reference_block.clone(),
        view.blockchain,
        &[],
    )
    .expect("execution data store builds over live chain data");

    let salt = Word::from([9u32, 9, 9, 9]);
    let executor: TransactionExecutor<'_, '_, _, BasicAuthenticator> =
        TransactionExecutor::new(&store);

    let summary = match executor
        .execute_transaction(
            account.id(),
            view.reference_block.block_num(),
            InputNotes::default(),
            TransactionArgs::default().with_auth_args(salt),
        )
        .await
    {
        Err(TransactionExecutorError::Unauthorized(effects)) => effects,
        Ok(_) => panic!("unsigned execution must not authorize"),
        Err(other) => panic!("execution against live chain data failed: {other:?}"),
    };

    let message = summary.as_ref().to_commitment();
    let signing_inputs = SigningInputs::TransactionSummary(summary);
    let cosigner_sig = BasicAuthenticator::new(&[AuthSecretKey::Falcon512Poseidon2(
        cosigner.clone(),
    )])
    .get_signature(cosigner.public_key().to_commitment().into(), &signing_inputs)
    .await
    .expect("cosigner signs");
    let guardian_sig = BasicAuthenticator::new(&[AuthSecretKey::Falcon512Poseidon2(
        guardian.clone(),
    )])
    .get_signature(guardian.public_key().to_commitment().into(), &signing_inputs)
    .await
    .expect("guardian signs");

    let mut signed = TransactionArgs::default().with_auth_args(salt);
    signed.add_signature(cosigner.public_key().to_commitment().into(), message, cosigner_sig);
    signed.add_signature(guardian.public_key().to_commitment().into(), message, guardian_sig);

    let executed = executor
        .execute_transaction(
            account.id(),
            view.reference_block.block_num(),
            InputNotes::default(),
            signed,
        )
        .await
        .expect("signed execution against live chain data authorizes");

    let tx_inputs: miden_protocol::transaction::TransactionInputs = executed.into();
    {
        use miden_protocol::utils::serde::Serializable;
        eprintln!("LIVE witness serialized bytes={}", tx_inputs.to_bytes().len());
    }
    // FR-020 exists because of this: the client library's default timeout is 10s
    // (`tx_prover.rs:45`), which is below real proving times and shows up as an intermittent
    // "failed to prove transaction" rather than anything that names a timeout.
    let prover = RemoteTransactionProver::new(&prover_url)
        .with_timeout(std::time::Duration::from_secs(300));
    let proven = match prover.prove(&tx_inputs).await {
        Ok(proven) => proven,
        Err(e) => {
            // The outermost Display is just "failed to prove transaction"; the gRPC status is in
            // the source chain, so walk it.
            eprintln!("LIVE prover error (debug): {e:?}");
            let mut source: Option<&(dyn std::error::Error + 'static)> =
                std::error::Error::source(&e);
            let mut depth = 1;
            while let Some(err) = source {
                eprintln!("LIVE   caused by [{depth}]: {err}");
                source = err.source();
                depth += 1;
            }
            panic!("remote prover at {prover_url} failed; see the chain above");
        }
    };

    println!(
        "LIVE remote prover={} reference_block={} proven_account={} expiration={}",
        prover_url,
        view.reference_block.block_num().as_u32(),
        proven.account_id().to_hex(),
        proven.expiration_block_num().as_u32()
    );
    assert_eq!(proven.account_id(), account.id());
}
