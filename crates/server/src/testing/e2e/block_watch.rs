//! End-to-end confirmation of block watch (issue #585): the release sweep
//! learns that an account moved from a committed block instead of probing
//! it, and releases it with the same detectors and the same proof.
//!
//! The fixtures are the release sweep suite's (this module is its child):
//! a real multisig account guarded by this server, onboarded through
//! `/configure`, whose guardian switch executed on a mock chain and never
//! reached this server. The node's block feed is scripted; everything
//! else (metadata, storage, the consumer, the detectors, the release write
//! and its audit row) is the real server code over the filesystem backend.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::*;
use crate::coordination::{BlockWatchStore, InMemoryBlockWatchStore};
use crate::jobs::block_watch::testing::{ScriptedBlockSourceFactory, block};
use crate::jobs::block_watch::{
    BlockWatchHandle, BlockWatchSignals, BlockWatchStatus, Consumer, Stopped,
};
use crate::jobs::release_sweep::{ReleaseSweeper, SweepState, VisitOutcome};
use crate::network::NetworkType;
use crate::storage::LeaseFence;

/// The block the switch transaction landed in.
const SWITCH_BLOCK: u32 = 1_201;

/// One block watch holder: a consumer over the scripted chain and the
/// release sweeper that visits what it records.
struct Holder {
    sweeper: ReleaseSweeper,
    signals: Arc<BlockWatchSignals>,
    handle: BlockWatchHandle,
}

impl Holder {
    fn new(
        state: &AppState,
        store: Arc<InMemoryBlockWatchStore>,
        factory: Arc<ScriptedBlockSourceFactory>,
        confirmations: u32,
    ) -> Self {
        Self {
            sweeper: ReleaseSweeper::new(
                state.clone(),
                confirmations,
                Duration::from_secs(60),
                Arc::new(SweepState::default()),
            ),
            signals: Arc::new(BlockWatchSignals::default()),
            handle: BlockWatchHandle {
                config: crate::block_watch::BlockWatchConfig::default(),
                network: NetworkType::MidenLocal,
                store,
                factory,
                status: Arc::new(BlockWatchStatus::default()),
                durable_observations: true,
            },
        }
    }

    /// Run the consumer until the cursor reaches `block`, then stop it.
    async fn consume_through(&self, state: &AppState, block: u32) {
        let cancel = CancellationToken::new();
        let consumer = Consumer::new(
            state.clone(),
            self.handle.clone(),
            self.signals.clone(),
            LeaseFence {
                lease_name: crate::coordination::RELEASE_SWEEP_LEASE.to_string(),
                holder_id: "single-process".to_string(),
                fence_token: 0,
            },
            false,
        );
        let task = tokio::spawn(consumer.run(cancel.clone()));
        let store = &self.handle.store;
        for _ in 0..2_000 {
            if store
                .cursor(NetworkType::MidenLocal)
                .await
                .unwrap()
                .is_some_and(|cursor| cursor.last_block >= block)
            {
                cancel.cancel();
                assert_eq!(task.await.unwrap(), Stopped::Cancelled);
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("block watch never reached block {block}");
    }

    /// Visit every due observation once, as the paced loop's observation
    /// turns do, deleting the checked ones.
    async fn visit_observations(&self) -> Vec<(String, u32, VisitOutcome)> {
        let store = &self.handle.store;
        let mut visited = Vec::new();
        let mut deferred = Vec::new();
        while let Some(observation) = store
            .next_due_observation(chrono::Utc::now())
            .await
            .unwrap()
            .filter(|observation| {
                !deferred.contains(&(observation.account_id.clone(), observation.block_num))
            })
        {
            let outcome = self
                .sweeper
                .visit_observed(
                    &observation.account_id,
                    &observation.on_chain_commitment,
                    observation.block_num,
                )
                .await
                .expect("visit succeeds");
            if outcome == VisitOutcome::Checked {
                store
                    .delete_observation(&observation.account_id, observation.block_num)
                    .await
                    .unwrap();
            } else {
                deferred.push((observation.account_id.clone(), observation.block_num));
            }
            visited.push((observation.account_id, observation.block_num, outcome));
        }
        visited
    }
}

/// A chain at `cursor + 1..` with the switch transaction landing in
/// `SWITCH_BLOCK`, and a store whose cursor stands just before it.
async fn switch_chain(
    fixture: &UnannouncedSwitch,
) -> (
    Arc<InMemoryBlockWatchStore>,
    Arc<ScriptedBlockSourceFactory>,
) {
    let factory = Arc::new(ScriptedBlockSourceFactory::new());
    factory.chain.push_empty(SWITCH_BLOCK - 1, SWITCH_BLOCK - 1);
    factory.chain.push(block(
        SWITCH_BLOCK,
        &[(
            fixture.account_id_hex.as_str(),
            fixture.executed_commitment.as_str(),
        )],
    ));
    let store = Arc::new(InMemoryBlockWatchStore::new());
    store
        .set_cursor(
            NetworkType::MidenLocal,
            SWITCH_BLOCK - 2,
            None,
            &LeaseFence {
                lease_name: crate::coordination::RELEASE_SWEEP_LEASE.to_string(),
                holder_id: "single-process".to_string(),
                fence_token: 0,
            },
        )
        .await
        .unwrap();
    (store, factory)
}

/// Install the chain view like `install_chain`, keeping the client so the
/// test can move the chain tip. The node's reads answer at the block the
/// switch lands in, as a node that served that block would: block watch
/// does not take a read from before the block it observed for an answer.
fn install_chain_kept(
    fixture: &mut UnannouncedSwitch,
    on_chain: &Account,
    publish: bool,
) -> Arc<IntegrationMockNetworkClient> {
    let miden_client = MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal);
    let mut client = IntegrationMockNetworkClient::new(miden_client);
    client.register_account(fixture.account_id_hex.clone(), commitment_hex(on_chain));
    if publish {
        client.publish_state(fixture.account_id_hex.clone(), on_chain.to_json());
    }
    let client = Arc::new(client);
    client.set_chain_tip(SWITCH_BLOCK);
    fixture.state.network_client = client.clone();
    client
}

#[tokio::test]
async fn test_block_watch_releases_a_public_switch_seen_in_a_block_once_confirmed() {
    // US1 scenario 1: a public account with no candidate in flight moves,
    // in a block, to a state whose published storage holds a foreign
    // guardian key. The observation opens the streak; a re-check at a
    // later block confirms it, crediting the block the move was seen in.
    let mut fixture = unannounced_switch().await;
    let executed = fixture.executed_account.clone();
    let chain = install_chain_kept(&mut fixture, &executed, true);
    let (store, factory) = switch_chain(&fixture).await;
    let holder = Holder::new(&fixture.state, store.clone(), factory, 2);

    holder.consume_through(&fixture.state, SWITCH_BLOCK).await;
    assert_eq!(store.count_pending_observations().await.unwrap(), 1);

    let visits = holder.visit_observations().await;
    assert_eq!(
        visits,
        vec![(
            fixture.account_id_hex.clone(),
            SWITCH_BLOCK,
            VisitOutcome::Waiting
        )],
        "the observation stays while the streak is open"
    );
    assert!(
        fixture.released_at().await.is_none(),
        "one storage read confirms nothing"
    );
    assert_eq!(
        holder.sweeper.sweep_state().streak(&fixture.account_id_hex),
        Some(1)
    );

    // The re-check reads the binding again, one block later.
    chain.advance_chain();
    assert_eq!(
        holder
            .sweeper
            .visit_account(&fixture.account_id_hex)
            .await
            .unwrap(),
        VisitOutcome::Checked
    );
    assert!(fixture.released_at().await.is_some());
    let events = fixture.release_events();
    assert_eq!(events.len(), 1);
    let payload = &events[0].payload;
    assert_eq!(payload["detected_by"], "chain_sweep");
    assert_eq!(payload["source"], "block_watch");
    assert_eq!(payload["observed_block_num"], SWITCH_BLOCK);
    assert_eq!(
        payload["new_guardian_commitment"],
        fixture.new_guardian_commitment_hex
    );
    assert_eq!(payload["on_chain_commitment"], fixture.executed_commitment);
    assert_eq!(payload["stored_commitment"], fixture.pre_switch_commitment);
    // The kept observation is settled once the account is released.
    let visits = holder.visit_observations().await;
    assert_eq!(
        visits,
        vec![(
            fixture.account_id_hex.clone(),
            SWITCH_BLOCK,
            VisitOutcome::Checked
        )]
    );
    assert_eq!(store.count_pending_observations().await.unwrap(), 0);
    assert_eq!(store.count_pending_observations().await.unwrap(), 0);
}

#[tokio::test]
async fn test_block_watch_releases_a_private_switch_at_the_block_its_proposal_executed() {
    // US1 scenario 2: a private account whose switch proposal is pending
    // here. The block shows it at the proposal's post-state: released at
    // once, with that block as the switch block, and the proposal
    // finalized. Nothing is read from published storage (there is none).
    let mut fixture = unannounced_switch_for(AccountType::Private).await;
    let proposal_id = fixture.push_switch_proposal().await;
    let executed = fixture.executed_account.clone();
    install_chain_kept(&mut fixture, &executed, false);
    let (store, factory) = switch_chain(&fixture).await;
    let holder = Holder::new(&fixture.state, store.clone(), factory, 2);

    holder.consume_through(&fixture.state, SWITCH_BLOCK).await;
    let visits = holder.visit_observations().await;
    assert_eq!(visits.len(), 1);
    assert_eq!(visits[0].2, VisitOutcome::Checked);

    assert!(fixture.released_at().await.is_some());
    let events = fixture.release_events();
    assert_eq!(events.len(), 1);
    let payload = &events[0].payload;
    assert_eq!(payload["detected_by"], "proposal_match");
    assert_eq!(payload["proposal_id"], proposal_id);
    assert_eq!(payload["switch_commitment"], fixture.executed_commitment);
    assert_eq!(payload["switch_block_num"], SWITCH_BLOCK);
    assert_eq!(payload["source"], "block_watch");
    assert_eq!(payload["observed_block_num"], SWITCH_BLOCK);
    assert!(
        fixture
            .state
            .storage
            .pull_pending_proposals(&fixture.account_id_hex)
            .await
            .unwrap()
            .is_empty(),
        "the executed switch proposal is finalized"
    );
}

#[tokio::test]
async fn test_block_watch_ignores_own_landings_busy_accounts_and_strangers() {
    // US1 scenarios 3 to 5: the account's own state landing, its own
    // candidate landing, and an account this server does not guard are
    // each one lookup in the block and nothing else. A switch that lands
    // while the candidate is in flight is observed, and waits for the
    // push path (PR #586 review, I2).
    let mut fixture = unannounced_switch().await;
    let executed = fixture.executed_account.clone();
    install_chain_kept(&mut fixture, &executed, true);
    let stranger = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
    let factory = Arc::new(ScriptedBlockSourceFactory::new());
    // Block 10: this server's own state lands, and a stranger moves.
    factory.chain.push(block(
        10,
        &[
            (
                fixture.account_id_hex.as_str(),
                fixture.pre_switch_commitment.as_str(),
            ),
            (stranger, "0xstranger"),
        ],
    ));
    let store = Arc::new(InMemoryBlockWatchStore::new());
    store
        .set_cursor(
            NetworkType::MidenLocal,
            9,
            None,
            &LeaseFence {
                lease_name: crate::coordination::RELEASE_SWEEP_LEASE.to_string(),
                holder_id: "single-process".to_string(),
                fence_token: 0,
            },
        )
        .await
        .unwrap();
    let holder = Holder::new(&fixture.state, store.clone(), factory.clone(), 1);
    holder.consume_through(&fixture.state, 10).await;
    assert_eq!(store.count_pending_observations().await.unwrap(), 0);

    // Block 11: the candidate of this server's lands: the push path owns
    // it.
    let (nonce, candidate_post_state) = fixture.push_stuck_candidate().await;
    assert!(
        fixture
            .state
            .metadata
            .get(&fixture.account_id_hex)
            .await
            .unwrap()
            .unwrap()
            .has_pending_candidate
    );
    factory.chain.push(block(
        11,
        &[(
            fixture.account_id_hex.as_str(),
            candidate_post_state.as_str(),
        )],
    ));
    holder.consume_through(&fixture.state, 11).await;
    assert_eq!(
        store.count_pending_observations().await.unwrap(),
        0,
        "its own candidate landing is left to the push path"
    );

    // Block 12: the switch lands instead, the candidate still in flight.
    // No candidate explains the move, so it is observed; its visit waits
    // while the candidate is in flight.
    factory.chain.push(block(
        12,
        &[(
            fixture.account_id_hex.as_str(),
            fixture.executed_commitment.as_str(),
        )],
    ));
    holder.consume_through(&fixture.state, 12).await;
    assert_eq!(store.count_pending_observations().await.unwrap(), 1);
    assert_eq!(
        holder.visit_observations().await,
        vec![(fixture.account_id_hex.clone(), 12, VisitOutcome::Waiting)]
    );
    assert!(fixture.released_at().await.is_none());
    assert!(fixture.release_events().is_empty());
    assert_eq!(store.count_pending_observations().await.unwrap(), 1);

    // Once the candidate is parked the account is the sweep's again: the
    // kept observation runs the detectors.
    fixture.park_diverged_candidate(nonce).await;
    let observation = store
        .next_due_observation(chrono::Utc::now() + chrono::Duration::hours(2))
        .await
        .unwrap()
        .expect("the observation was kept");
    let outcome = holder
        .sweeper
        .visit_observed(
            &observation.account_id,
            &observation.on_chain_commitment,
            observation.block_num,
        )
        .await
        .expect("visit succeeds");
    assert_eq!(outcome, VisitOutcome::Checked);
    assert!(
        fixture.released_at().await.is_some(),
        "the switch is released from the kept observation"
    );
}

#[tokio::test]
async fn test_block_watch_successor_resumes_from_the_cursor_and_releases_what_it_inherited() {
    // US3 scenario 4 / SC-005: holder A records the switch observation and
    // loses the lease before visiting it. Holder B, with fresh replica-local
    // state over the same store, resumes after A's cursor (no block twice)
    // and releases the account from the observation it inherited.
    let mut fixture = unannounced_switch_for(AccountType::Private).await;
    fixture.push_switch_proposal().await;
    let executed = fixture.executed_account.clone();
    install_chain_kept(&mut fixture, &executed, false);
    let (store, factory) = switch_chain(&fixture).await;

    let holder_a = Holder::new(&fixture.state, store.clone(), factory.clone(), 2);
    holder_a.consume_through(&fixture.state, SWITCH_BLOCK).await;
    assert_eq!(store.count_pending_observations().await.unwrap(), 1);

    factory.chain.push_empty(SWITCH_BLOCK + 1, SWITCH_BLOCK + 3);
    let holder_b = Holder::new(&fixture.state, store.clone(), factory.clone(), 2);
    holder_b
        .consume_through(&fixture.state, SWITCH_BLOCK + 3)
        .await;
    let opens = factory.opens();
    assert_eq!(
        opens.iter().map(|(_, from)| *from).collect::<Vec<_>>(),
        vec![SWITCH_BLOCK - 1, SWITCH_BLOCK + 1],
        "B starts after A's cursor"
    );
    let visits = holder_b.visit_observations().await;
    assert_eq!(visits.len(), 1);
    assert!(fixture.released_at().await.is_some());
    assert_eq!(
        fixture.release_events()[0].payload["observed_block_num"],
        SWITCH_BLOCK
    );
}
