# Quickstart: verifying block watch

**Feature**: 012-block-streaming

## 1. Does the node serve the feed? (no server needed)

Drop this file at `crates/miden-rpc-client/examples/block_probe.rs` (do not
commit it), build with `cargo build -p miden-rpc-client --example
block_probe`, run `./target/debug/examples/block_probe
https://rpc.devnet.miden.io`. It prints the node version, the tip, one
`GetBlockByNumber`, then opens `BlockSubscription` three blocks behind the
tip and reads 12 items (replay + live) within 90 s. Run on 2026-11-02:
devnet and testnet (both miden-node 0.18.0) replayed 4 blocks instantly and
then delivered live blocks every 3 s; empty blocks ~400 B, blocks with 1–2
account updates 1.8–4 KB; the first item carries `protocol_config`
(~3 KB).

```rust
use std::time::{Duration, Instant};
use guardian_shared::retry::RpcReadMode;
use miden_rpc_client::{MidenRpcClient, account, blockchain, rpc};
use prost::Message;
use tonic::Request;

#[tokio::main]
async fn main() {
    let endpoint = std::env::args().nth(1).unwrap_or_else(|| "https://rpc.devnet.miden.io".to_string());
    let mut client = MidenRpcClient::connect(endpoint.clone()).await.expect("connect");
    let tip = client.get_chain_tip(RpcReadMode::SingleAttempt).await.expect("tip");
    let status = client.get_status().await.map(|s| format!("{s:?}")).unwrap_or_else(|e| format!("status err: {e}"));
    println!("endpoint={endpoint} tip={tip}\nstatus={}", status.chars().take(200).collect::<String>());

    let started = Instant::now();
    match client.client_mut().get_block_by_number(Request::new(rpc::GetBlockByNumberRequest { block_num: tip, include_proof: Some(false) })).await {
        Ok(resp) => {
            let resp = resp.into_inner();
            println!("GetBlockByNumber({tip}) ok in {:?}: encoded_len={}", started.elapsed(), resp.encoded_len());
            if let Some(block) = resp.block.as_ref() { describe(block); }
        }
        Err(status) => println!("GetBlockByNumber({tip}) ERR {:?} {}", status.code(), status.message()),
    }

    let from = tip.saturating_sub(3);
    let started = Instant::now();
    let mut stream = match client.client_mut().block_subscription(Request::new(rpc::BlockSubscriptionRequest { block_from: from })).await {
        Ok(resp) => resp.into_inner(),
        Err(status) => { println!("BlockSubscription(from={from}) ERR {:?} {}", status.code(), status.message()); return; }
    };
    println!("BlockSubscription(from={from}) opened in {:?}", started.elapsed());
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut n = 0;
    while Instant::now() < deadline && n < 12 {
        match tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), stream.message()).await {
            Ok(Ok(Some(item))) => {
                n += 1;
                let header = item.block.as_ref().and_then(|b| b.header.as_ref());
                println!("  item #{n} at +{:?}: block_num={:?} ts={:?} committed_tip={} proto_cfg={} encoded_len={}",
                    started.elapsed(), header.and_then(|h| h.block_num.map(|b| b.block_num)), header.map(|h| h.timestamp),
                    item.committed_chain_tip, item.protocol_config.is_some(), item.encoded_len());
                if let Some(block) = item.block.as_ref() { describe(block); }
            }
            Ok(Ok(None)) => { println!("  stream ended"); break; }
            Ok(Err(status)) => { println!("  stream ERR {:?} {}", status.code(), status.message()); break; }
            Err(_) => { println!("  no item within deadline ({n} received)"); break; }
        }
    }
}

fn describe(block: &blockchain::SignedBlock) {
    let Some(body) = block.body.as_ref() else { return };
    let public = body.updated_accounts.iter()
        .filter(|u| matches!(u.details.as_ref().and_then(|d| d.update.as_ref()), Some(account::account_update_details::Update::Public(_))))
        .count();
    println!("    updated_accounts={} (public={public}) txs={} note_batches={} nullifiers={} sigs={}",
        body.updated_accounts.len(), body.transactions.len(), body.output_note_batches.len(), body.created_nullifiers.len(), block.signatures.len());
}
```

## 2. Unit and scripted-node suites

```bash
cd guardian
cargo test -p miden-rpc-client                                   # decode, get_block, stream wrappers
cargo test -p guardian-server --lib -- jobs::block_watch network::miden::block_feed \
  coordination::block_watch_store jobs::release_sweep builder::block_watch
cargo test -p guardian-server --lib --features integration,e2e testing::e2e::switch_guardian_release_sweep
PQ_LIB_DIR=... DATABASE_URL=postgres://...:5433/guardian_test ./scripts/test-postgres.sh
cargo clippy --all-targets --all-features -- -D warnings
```

The Postgres benchmark (SC-003) is
`jobs::block_watch::tests::postgres::a_block_of_five_thousand_updates_is_processed_within_a_second`
(~45 ms per 5,000-update block on a local Postgres 16).

## 3. Local chain, two replicas, a real switch (as run on 2026-11-02)

Local chain: `git clone --depth 1 --branch v0.17.3
https://github.com/0xMiden/miden-node`, then `docker compose up -d sequencer
ntx-builder tx-prover funding-service` with every `MIDEN_*_IMAGE` set to
`ghcr.io/0xmiden/<name>:v0.17.3` (it embeds miden-protocol 0.17.1, Guardian's
pin). RPC on `127.0.0.1:57291`, base fee 7, funding through `POST
http://127.0.0.1:50401/request-funds {"account_id", "amount"}`.

A real switch the old guardian never hears about: with the multisig SDK,
create a 1-of-1 account on guardian A, fund it, deploy it with a
consume-notes proposal through A, create a `SwitchGuardian` proposal on A
(pending there), then `export_proposal_to_string` →
`import_proposal_from_string` → `execute_imported_proposal` (the offline
path, which never pushes to A). Guardian A, with
`GUARDIAN_RELEASE_SWEEP_ROTATION_SECONDS=604800`, logged
`block_watch_observation_recorded` for the switch block and released the
account 17 ms later with `detected_by=proposal_match source=block_watch
observed_block_num=<switch block> switch_block_num=<switch block>`.

Fault injection against the real node: a small byte proxy between the
server (`GUARDIAN_MIDEN_RPC_ENDPOINT=http://127.0.0.1:<proxy>`) and the
node, which can cut every connection (reconnect from `cursor + 1`, reason
`status_unknown`) or freeze only the stream's connection (after
`GUARDIAN_BLOCK_WATCH_STALL_SECONDS` a `block_watch_stall`, reopen from
`cursor + 1`). The public endpoints route on the request's authority, so
the proxy only works against a node reached directly.

The original plan for this section:

1. Start a local miden-node (see the local chain recipe; the compose stack
   in `docs/guides/horizontal-scaling` plus the node) and two Guardian
   replicas sharing Postgres, with `GUARDIAN_BLOCK_WATCH_ENABLED=true`,
   `GUARDIAN_RELEASE_SWEEP_ROTATION_SECONDS=604800` (so only block watch can
   release within the test window).
2. Confirm attach: `event=block_watch_attached transport=stream` on the
   lease holder; `GET /dashboard/info` `.backend.block_watch.lag_blocks`
   near 0.
3. Onboard a multisig account through `examples/demo`, then execute a
   guardian switch to a second Guardian **without** the push reaching the
   first one (the offline switch path, or point the client's push at the
   second Guardian only).
4. Within a few blocks the first Guardian logs
   `event=block_watch_observation_recorded` for the account, then
   `event=release_sweep_released detected_by=chain_sweep|proposal_match
   source=block_watch observed_block_num=N`; the account shows
   `released_at` and the `accounts.release` audit row carries
   `observed_block_num`.
5. Failover: `docker stop` the lease holder mid-run; within one lease TTL
   (30 s) the other replica logs `block_watch_attached from_block=<cursor+1>`;
   no block is processed twice (`guardian_block_watch_blocks_total` on both
   replicas sums to the chain's progress).
6. Fallback: run with `GUARDIAN_BLOCK_WATCH_TRANSPORT=poll` and repeat step
   4; then with a node that refuses the subscription (or an iptables drop of
   the stream) to see `block_watch_transport from=stream to=poll`.

## 4. Devnet

Point one Guardian at `https://rpc.devnet.miden.io` (`GUARDIAN_NETWORK`
devnet). Expect `transport=stream` and 3 s cadence. Leave it 24 h and
confirm zero `guardian_block_watch_reconnects_total{reason=
status_resource_exhausted}` (SC-004) and `lag_blocks` staying at 0–1. Only
then enable in testnet and, with sign-off, prod.
