//! Read-only checks against a live 0.17 node. They default to devnet; set
//! `GUARDIAN_LIVE_MIDEN_RPC_URL` to point them elsewhere (for example a local node).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_protocol::block::BlockNumber;

use super::{ForeignAccounts, build_chain_view};

const TIMEOUT_MS: u64 = 30_000;

async fn live_rpc() -> Arc<dyn NodeRpcClient> {
    let endpoint = match std::env::var("GUARDIAN_LIVE_MIDEN_RPC_URL") {
        Ok(url) => Endpoint::try_from(url.as_str()).expect("valid endpoint"),
        Err(_) => Endpoint::devnet(),
    };
    let rpc = GrpcClient::new(&endpoint, TIMEOUT_MS);
    let (genesis, _) = rpc
        .get_block_header_by_number(Some(BlockNumber::GENESIS), false)
        .await
        .expect("genesis header");
    rpc.set_genesis_commitment(genesis.commitment())
        .await
        .expect("genesis commitment");
    Arc::new(rpc)
}

#[tokio::test]
#[ignore = "live network; run with --ignored and GUARDIAN_LIVE_MIDEN_RPC_URL"]
async fn live_chain_view_tracks_a_bound_block_behind_the_pruning_window() {
    let rpc = live_rpc().await;
    let (tip, _) = rpc.get_block_header_by_number(None, false).await.unwrap();
    let bound = BlockNumber::from(tip.block_num().as_u32().saturating_sub(120));

    let started = Instant::now();
    let view = build_chain_view(rpc.as_ref(), &BTreeSet::from([bound]))
        .await
        .expect("chain view at the live tip");
    let elapsed = started.elapsed();

    assert!(view.reference_block() >= tip.block_num());
    assert!(view.authenticates(bound));
    assert_eq!(
        view.protocol_config().to_commitment(),
        view.reference_header().protocol_config_commitment()
    );
    eprintln!(
        "chain view at block {} tracking block {bound}: {elapsed:?}",
        view.reference_block()
    );
}

#[tokio::test]
#[ignore = "live network; run with --ignored and GUARDIAN_LIVE_MIDEN_RPC_URL"]
async fn live_fee_faucet_loads_as_a_foreign_account_at_the_reference_block() {
    let rpc = live_rpc().await;
    let view = build_chain_view(rpc.as_ref(), &BTreeSet::new())
        .await
        .expect("chain view");
    let fee_faucet = view.protocol_config().fee_asset_id().faucet_id();
    let foreign = ForeignAccounts::new(rpc.clone(), view.reference_block());

    let inputs = foreign
        .inputs(fee_faucet, view.reference_block())
        .await
        .expect("the public fee faucet is served at the reference block");
    assert_eq!(inputs.id(), fee_faucet);
}

#[tokio::test]
#[ignore = "live network; run with --ignored and GUARDIAN_LIVE_MIDEN_RPC_URL"]
async fn live_transaction_encryption_key_verifies_against_the_tip_validators() {
    let rpc = live_rpc().await;
    let view = build_chain_view(rpc.as_ref(), &BTreeSet::new())
        .await
        .expect("chain view");
    let attested = rpc
        .get_transaction_encryption_key()
        .await
        .expect("the node serves an encryption key");
    attested
        .verify(
            view.genesis_commitment(),
            view.reference_header().validator_config(),
        )
        .expect("an attestation from a tip validator verifies");
}
