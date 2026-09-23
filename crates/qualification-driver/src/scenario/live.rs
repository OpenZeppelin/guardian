use anyhow::anyhow;
use miden_client::rpc::Endpoint;
use miden_multisig_client::{AbandonStatus, MultisigClient, ProposalStatus};
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::asset::Asset;

use crate::manifest::{NetworkName, Scheme, Shape};
use crate::scenario::signers::{RunSigner, RunSigners};

use super::{ActionOutcome, Runner};

pub struct LiveContext {
    pub network: NetworkName,
    pub guardian_endpoint: String,
    pub account_dir: std::path::PathBuf,
    /// Where the treasury lock and spend ledger live. Deliberately not
    /// `account_dir`: that one is per run, and the lock and the cap only work
    /// while every spender in the run shares one directory, including the
    /// `fund` subprocess the TypeScript leg spawns.
    pub treasury_dir: std::path::PathBuf,
    /// A second GUARDIAN deployment, required only by the migration scenario.
    pub migration_endpoint: Option<String>,
}

/// The account a scenario is working on, shared by its actions.
///
/// Each cosigner keeps its own client and its own store: they are separate
/// parties, and one client standing in for several would make a 2-of-3 pass for
/// reasons no real deployment enjoys.
pub struct LiveSession {
    pub clients: Vec<MultisigClient>,
    /// Retained so a cosigner's key can be handed to the other SDK. The client
    /// does not expose its key, and it should not: only this harness has a
    /// reason to move key material between processes.
    pub signers: RunSigners,
    pub threshold: u32,
    pub scheme: Scheme,
    pub account_id: AccountId,
    pub proposal_id: Option<String>,
    pub exported_proposal: Option<String>,
    /// The signer being added, held aside so it cannot sign its own admission.
    pub incoming: Option<Vec<MultisigClient>>,
    /// Index of the signer a removal took away, kept so its eviction can be tested.
    pub departed: Option<usize>,
    pub expected_signers: Option<Vec<String>>,
    pub expected_threshold: Option<u32>,
    pub faucet: Option<AccountId>,
    pub treasury: Option<AccountId>,
    pub sent_amount: u64,
    pub balance_before_send: Option<u64>,
    /// Set once a GUARDIAN migration is proposed: completion is judged differently.
    pub migrating: bool,
    pub expected_procedure: Option<miden_multisig_client::ProcedureName>,
    pub expected_procedure_threshold: Option<u32>,
    pub transferred: u64,
    pub balance_seen: bool,
    /// The producer's serialized transaction request, kept verbatim. Preparing
    /// a custom execution re-executes these exact bytes to reproduce the signed
    /// commitment, so rebuilding them with a fresh salt would not match.
    pub custom_request: Option<Vec<u8>>,
    /// The nonce the custom proposal was pushed with, which is the candidate
    /// the abandon has to pin.
    pub custom_nonce: Option<u64>,
}

fn endpoint(network: NetworkName) -> Endpoint {
    match network {
        NetworkName::Devnet => Endpoint::devnet(),
        NetworkName::Testnet => Endpoint::testnet(),
    }
}

async fn build_cosigners(
    context: &LiveContext,
    signers: &RunSigners,
    run_tag: &str,
) -> anyhow::Result<Vec<MultisigClient>> {
    let mut clients = Vec::with_capacity(signers.signers.len());
    for (index, signer) in signers.signers.iter().enumerate() {
        let dir = context
            .account_dir
            .join(run_tag)
            .join(format!("cosigner-{index}"));
        std::fs::create_dir_all(&dir)?;

        let builder = MultisigClient::builder()
            .miden_endpoint(endpoint(context.network))
            .guardian_endpoint(context.guardian_endpoint.clone())
            .account_dir(&dir);

        let builder = match signer {
            RunSigner::Falcon(key) => builder.with_secret_key(key.clone()),
            RunSigner::Ecdsa(key) => builder.with_ecdsa_secret_key(key.clone()),
        };

        let mut client = builder
            .build()
            .await
            .map_err(|error| anyhow!("cannot build cosigner {index}: {error}"))?;
        client
            .reset_miden_client()
            .await
            .map_err(|error| anyhow!("cannot reset cosigner {index}: {error}"))?;
        clients.push(client);
    }
    Ok(clients)
}

fn commitments(clients: &[MultisigClient]) -> Vec<Word> {
    clients
        .iter()
        .map(|client| client.user_commitment())
        .collect()
}

/// Builds the multisig account locally. Nothing reaches the chain here: the
/// account exists on chain only once it transacts.
pub async fn create(runner: &Runner, shape: Shape, scheme: Scheme, run_tag: &str) -> ActionOutcome {
    let Some(context) = runner.live.as_ref() else {
        return ActionOutcome::failed_setup("the live context is not configured");
    };
    let Some((threshold, total)) = shape.threshold_and_total() else {
        return ActionOutcome::failed_setup(format!("{shape:?} is not a usable multisig shape"));
    };
    let Some(signers) = RunSigners::for_shape(threshold, total, scheme) else {
        return ActionOutcome::failed_setup(format!(
            "cannot generate a {threshold}-of-{total} {scheme:?} signer set"
        ));
    };

    let mut clients = match build_cosigners(context, &signers, run_tag).await {
        Ok(clients) => clients,
        Err(error) => return ActionOutcome::failed_setup(error.to_string()),
    };

    let signer_commitments = commitments(&clients);
    if let Err(error) = clients[0]
        .create_account(threshold, signer_commitments)
        .await
    {
        return ActionOutcome::failed_product(format!(
            "creating the multisig account failed: {error}"
        ));
    }

    let Some(account_id) = clients[0].account_id() else {
        return ActionOutcome::failed_product(
            "the account was created but the client reports no account id".to_string(),
        );
    };

    *runner.session.lock().await = Some(LiveSession {
        clients,
        signers,
        threshold,
        scheme,
        account_id,
        proposal_id: None,
        exported_proposal: None,
        incoming: None,
        departed: None,
        expected_signers: None,
        expected_threshold: None,
        faucet: None,
        treasury: None,
        sent_amount: 0,
        balance_before_send: None,
        migrating: false,
        expected_procedure: None,
        expected_procedure_threshold: None,
        transferred: 0,
        balance_seen: false,
        custom_request: None,
        custom_nonce: None,
    });
    ActionOutcome::Passed
}

/// Opens a session on the long-lived account instead of creating a fresh one.
///
/// Registers the account with GUARDIAN. A GUARDIAN call, not a chain
/// transaction, so it runs before the account is funded.
pub async fn register(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };

    match session.clients[0].push_account().await {
        Ok(()) => ActionOutcome::Passed,
        Err(error) => ActionOutcome::failed_product(format!(
            "registering account {} with GUARDIAN failed: {error}",
            session.account_id
        )),
    }
}

/// Reads the registered account back through GUARDIAN, which is what makes the
/// registration an observation rather than an assumption.
pub async fn verify_registration(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };

    match session.clients[0].get_deltas().await {
        Ok(_) => ActionOutcome::Passed,
        Err(error) => ActionOutcome::failed_product(format!(
            "GUARDIAN would not serve state for {}: {error}",
            session.account_id
        )),
    }
}

/// What one ephemeral account is funded with.
///
/// Enough to consume the note and execute a handful of proposals, and no more:
/// residue is accepted rather than swept, so an over-funded account is spend
/// the run never gets back.
const ACCOUNT_FUNDING: u64 = 200_000;

/// How long a funding note may take to appear before the run gives up on it.
const NOTE_ARRIVAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(180);

async fn wait_for_notes(
    client: &mut MultisigClient,
    deadline: std::time::Duration,
) -> Result<Vec<miden_multisig_client::ConsumableNote>, ActionOutcome> {
    let started = std::time::Instant::now();
    let mut last_error = None;
    let mut wait = std::time::Duration::from_secs(1);

    while started.elapsed() < deadline {
        if let Err(error) = client.sync().await {
            last_error = Some(error.to_string());
        } else {
            match client.list_consumable_notes().await {
                Ok(notes) if !notes.is_empty() => return Ok(notes),
                Ok(_) => {}
                Err(error) => last_error = Some(error.to_string()),
            }
        }
        // Chain events usually land within a few seconds, so early polls are
        // tight and back off rather than waiting a flat interval every time.
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(std::time::Duration::from_secs(5));
    }

    Err(ActionOutcome::EnvironmentBlocked {
        reason: format!(
            "the funding note did not reach the account within {}s{}",
            deadline.as_secs(),
            last_error
                .map(|error| format!("; last error: {error}"))
                .unwrap_or_default()
        ),
    })
}

/// Funds the account and proposes consuming that funding note.
///
/// This ordering is forced rather than chosen. On a fee-charging chain the
/// account cannot execute anything until it holds the fee asset, and the only
/// way in is to consume an inbound note, which for a multisig means a proposal.
/// The first proposal an account makes is therefore always this one.
pub async fn create_proposal(runner: &Runner) -> ActionOutcome {
    let Some(context) = runner.live.as_ref() else {
        return ActionOutcome::failed_setup("the live context is not configured");
    };
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };

    let funded = match crate::funding::service::fund_once(
        context.network,
        &context.treasury_dir,
        session.account_id,
        ACCOUNT_FUNDING,
    )
    .await
    {
        Ok(funded) => funded,
        Err(error) => {
            return ActionOutcome::failed_setup(format!(
                "cannot fund {}: {error}",
                session.account_id
            ));
        }
    };

    let Some(funded) = funded else {
        return ActionOutcome::Skipped {
            reason: "this chain charges nothing, so there is no funding note to consume"
                .to_string(),
        };
    };
    session.faucet = Some(funded.faucet);
    session.treasury = Some(funded.treasury);
    // Accumulated, not assigned. A scenario can fund more than once: this
    // action funds because the first proposal an account makes is always the
    // consume of its funding note, and `asset-transfer` funds for its own
    // reasons, so a scenario that runs both sends two notes. The consume
    // proposal then takes every note it can see, which is one or two depending
    // on whether the first had committed yet, and an assignment here recorded
    // only the second. The balance assertion is an upper bound, so under-
    // recording the total made it fail whenever both notes landed in time: a
    // race in the harness, reported against the product.
    session.transferred += funded.amount;

    // A submitted transfer is not yet a visible note: it has to be committed in
    // a block first. Polled against a deadline rather than slept on, so a
    // network that is merely slow is distinguishable from one that lost it.
    let client = &mut session.clients[0];
    let notes = match wait_for_notes(client, NOTE_ARRIVAL_DEADLINE).await {
        Ok(notes) => notes,
        Err(error) => return error,
    };

    let note_ids = notes.iter().map(|note| note.id).collect();
    match client
        .propose_transaction(miden_multisig_client::TransactionType::consume_notes(
            note_ids,
        ))
        .await
    {
        Ok(proposal) => {
            session.proposal_id = Some(proposal.id.clone());
            ActionOutcome::Passed
        }
        Err(error) => {
            ActionOutcome::failed_product(format!("creating the consume proposal failed: {error}"))
        }
    }
}

/// Collects signatures from the other cosigners until the threshold is met.
pub async fn sign_proposal(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(proposal_id) = session.proposal_id.clone() else {
        return ActionOutcome::failed_setup("no proposal has been created in this scenario");
    };

    let account_id = session.account_id;
    let threshold = session.threshold as usize;

    // The proposer's own signature is already on it, so the remaining cosigners
    // make up the difference.
    for index in 1..threshold {
        let Some(client) = session.clients.get_mut(index) else {
            return ActionOutcome::failed_setup(format!("cosigner {index} is missing"));
        };
        if let Err(error) = client.pull_account(account_id).await {
            return ActionOutcome::failed_product(format!(
                "cosigner {index} could not load the account: {error}"
            ));
        }
        if let Err(error) = client.sync().await {
            return ActionOutcome::failed_product(format!(
                "cosigner {index} could not sync: {error}"
            ));
        }
        if let Err(error) = client.sign_proposal(&proposal_id).await {
            return ActionOutcome::failed_product(format!(
                "cosigner {index} could not sign: {error}"
            ));
        }
    }

    ActionOutcome::Passed
}

/// Executes the proposal and confirms the account moved.
pub async fn execute_proposal(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(proposal_id) = session.proposal_id.clone() else {
        return ActionOutcome::failed_setup("no proposal has been created in this scenario");
    };

    let migrating = session.migrating;
    // Signatures collected off-channel live in the document and were never
    // pushed, so GUARDIAN's copy is short of the threshold and the online path
    // refuses. An offline-created proposal is not there at all. Either way the
    // document is the thing the cosigners actually signed, so execute from it;
    // the acknowledgement still comes from GUARDIAN.
    let document = match session
        .exported_proposal
        .as_deref()
        .map(miden_multisig_client::ExportedProposal::from_json)
    {
        Some(Ok(document)) => Some(document),
        Some(Err(error)) => {
            return ActionOutcome::failed_setup(format!(
                "the offline proposal is not readable: {error}"
            ));
        }
        None => None,
    };
    let client = &mut session.clients[0];
    let nonce_before = chain_nonce(client).await;
    // Read before executing: once the proposal leaves the pending set there is
    // nothing left to read it from, and completion has to be bound to it. An
    // offline document carries its own nonce, which is the one it was signed
    // at, so an offline proposal GUARDIAN never listed is still bound.
    let binding = if migrating {
        Binding::Migration
    } else if let Some(document) = &document {
        Binding::Nonce(document.nonce)
    } else {
        match proposal_nonce(client, &proposal_id).await {
            Ok(nonce) => Binding::Nonce(nonce),
            Err(reason) => return unbindable(reason),
        }
    };

    let executed = match &document {
        Some(document) => client.execute_imported_proposal(document).await,
        None => client.execute_proposal(&proposal_id).await,
    };

    if let Err(error) = executed {
        return ActionOutcome::failed_product(format!("executing the proposal failed: {error}"));
    }

    if let Err(error) = client.sync().await {
        return ActionOutcome::failed_product(format!("syncing after execution failed: {error}"));
    }

    match wait_for_execution(client, &proposal_id, binding).await {
        Completion::Confirmed => ActionOutcome::Passed,
        Completion::Discarded(reason) => ActionOutcome::failed_product(format!(
            "the proposal left the pending set without becoming canonical: {reason}"
        )),
        Completion::Pending(reason) => {
            // Still pending means either the chain never took the transaction or
            // GUARDIAN has not caught up, and only the account's own nonce
            // separates them. Corroboration, not the definition of done.
            let nonce_after = chain_nonce(client).await;
            if let (Some(before), Some(after)) = (nonce_before, nonce_after)
                && before == after
            {
                return ActionOutcome::failed_product(format!(
                    "the proposal was executed but the account nonce never moved past {before}, \
                     so nothing reached the chain"
                ));
            }
            ActionOutcome::EnvironmentBlocked {
                reason: format!("the executed proposal was {reason}"),
            }
        }
    }
}

async fn chain_nonce(client: &mut MultisigClient) -> Option<u64> {
    client.sync().await.ok()?;
    client.account().map(|account| account.nonce())
}

/// Signs with cosigners until `target` signatures stand on the proposal.
///
/// Creating a proposal in Rust already carries the proposer's signature, so
/// index 0 is not offered it again.
async fn collect_signatures(session: &mut LiveSession, target: usize) -> Result<(), ActionOutcome> {
    let proposal_id = session.proposal_id.clone().ok_or_else(|| {
        ActionOutcome::failed_setup("no proposal has been created in this scenario")
    })?;
    let account_id = session.account_id;

    let mut collected = 1;
    for index in 1..session.clients.len() {
        if collected >= target {
            break;
        }
        let client = &mut session.clients[index];
        if let Err(error) = client.pull_account(account_id).await {
            return Err(ActionOutcome::failed_product(format!(
                "cosigner {index} could not load the account: {error}"
            )));
        }
        if let Err(error) = client.sync().await {
            return Err(ActionOutcome::failed_product(format!(
                "cosigner {index} could not sync: {error}"
            )));
        }
        match client.sign_proposal(&proposal_id).await {
            Ok(_) => collected += 1,
            Err(error) => {
                return Err(ActionOutcome::failed_product(format!(
                    "cosigner {index} could not sign: {error}"
                )));
            }
        }
    }

    if collected < target {
        return Err(ActionOutcome::failed_product(format!(
            "collected {collected} signature(s) but {target} were needed"
        )));
    }
    Ok(())
}

async fn signature_count(client: &mut MultisigClient, proposal_id: &str) -> Option<usize> {
    client
        .list_proposals()
        .await
        .ok()?
        .into_iter()
        .find(|proposal| proposal.id == proposal_id)
        .map(|proposal| proposal.signatures.len())
}

fn vault_amount(account: &miden_protocol::account::Account, faucet: AccountId) -> u64 {
    account
        .vault()
        .assets()
        .filter_map(|asset| match asset {
            Asset::Fungible(fungible) if fungible.faucet_id() == faucet => {
                Some(fungible.amount().as_u64())
            }
            _ => None,
        })
        .sum()
}

/// A proposal one signature short of threshold must not execute, and must
/// survive the attempt so the remaining cosigner can still sign it.
pub async fn reject_below_threshold(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(proposal_id) = session.proposal_id.clone() else {
        return ActionOutcome::failed_setup("no proposal has been created in this scenario");
    };
    if session.threshold < 2 {
        return ActionOutcome::Skipped {
            reason: "a 1-of-n account has no below-threshold state".to_string(),
        };
    }

    let target = session.threshold as usize - 1;
    if let Err(outcome) = collect_signatures(session, target).await {
        return outcome;
    }

    let client = &mut session.clients[0];
    let nonce_before = chain_nonce(client).await;
    match client.execute_proposal(&proposal_id).await {
        Ok(()) => ActionOutcome::failed_product(format!(
            "a proposal with {target} of {} signatures executed",
            session.threshold
        )),
        Err(error) => {
            // Matched on the variant, not on words in the message. `signature`
            // appears in unrelated failures, so a substring match would accept a
            // refusal that had nothing to do with the count and report it as
            // threshold enforcement.
            if !matches!(
                error,
                miden_multisig_client::MultisigError::ProposalNotReady { .. }
            ) {
                return ActionOutcome::failed_product(format!(
                    "the proposal was refused, but not for being below threshold: {error}"
                ));
            }
            // The refusal here is the client's own pre-flight check, and
            // GUARDIAN acknowledges a delta without counting cosigner
            // signatures, so confirm nothing reached the chain rather than
            // assuming the refusal stopped it.
            if chain_nonce(client).await != nonce_before {
                return ActionOutcome::failed_product(
                    "the account nonce advanced after a below-threshold execution was refused"
                        .to_string(),
                );
            }
            match client.list_proposals().await {
                Ok(proposals) if proposals.iter().any(|entry| entry.id == proposal_id) => {
                    ActionOutcome::Passed
                }
                Ok(_) => ActionOutcome::failed_product(
                    "the refused proposal was discarded instead of staying pending".to_string(),
                ),
                Err(error) => ActionOutcome::failed_product(format!(
                    "listing proposals after the refusal failed: {error}"
                )),
            }
        }
    }
}

/// Signing twice with one key must not count twice toward the threshold.
pub async fn reject_duplicate_signature(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(proposal_id) = session.proposal_id.clone() else {
        return ActionOutcome::failed_setup("no proposal has been created in this scenario");
    };

    let client = &mut session.clients[0];
    let Some(before) = signature_count(client, &proposal_id).await else {
        return ActionOutcome::failed_product(
            "the proposal disappeared before the duplicate was attempted".to_string(),
        );
    };

    let outcome = client.sign_proposal(&proposal_id).await;
    // Not `unwrap_or(before)`. Falling back to the count from before the
    // attempt turns an unreadable listing into evidence that the count did not
    // change, which is the one thing this scenario exists to establish. Not
    // being able to look is not an answer.
    let Some(after) = signature_count(client, &proposal_id).await else {
        return ActionOutcome::failed_product(
            "the signature count could not be read after the duplicate was attempted, so \
             whether the duplicate was counted is unknown"
                .to_string(),
        );
    };

    match outcome {
        Ok(_) if after > before => ActionOutcome::failed_product(format!(
            "a duplicate signature was accepted; the count went from {before} to {after}"
        )),
        Ok(_) => ActionOutcome::Passed,
        Err(_) if after == before => ActionOutcome::Passed,
        Err(error) => ActionOutcome::failed_product(format!(
            "the duplicate was refused with {error} but still changed the count from {before} to {after}"
        )),
    }
}

/// A cosigner holding only its own key rebuilds the account from GUARDIAN.
///
/// The recovering cosigner is the one that has taken no part in the scenario,
/// and each cosigner has its own store, so nothing local can be standing in for
/// what GUARDIAN serves.
pub async fn recover_by_cosigner(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };

    let account_id = session.account_id;
    let last = session.clients.len() - 1;
    if last == 0 {
        return ActionOutcome::Skipped {
            reason: "a 1-of-1 account has no other cosigner to recover through".to_string(),
        };
    }

    let client = &mut session.clients[last];
    match client.recover_by_key().await {
        Ok(recovered) => {
            if recovered
                .iter()
                .any(|entry| entry.account_id == account_id.to_hex())
            {
                ActionOutcome::Passed
            } else {
                ActionOutcome::failed_product(format!(
                    "GUARDIAN did not offer {account_id} to a cosigner holding one of its keys"
                ))
            }
        }
        Err(error) => ActionOutcome::failed_product(format!(
            "the cosigner could not recover {account_id}: {error}"
        )),
    }
}

/// How long an executed proposal may take to leave the pending set.
const CANONICALIZATION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(180);

/// What the chain and GUARDIAN say about an executed proposal.
enum Completion {
    /// Confirmed on chain, agreed with GUARDIAN, and canonical in history.
    Confirmed,
    /// The proposal is gone but nothing canonical carries its state. A delta
    /// that canonicalization abandons is removed from the pending set exactly
    /// like one that succeeded, so this is the case a pending-set check alone
    /// reports as a pass.
    Discarded(String),
    /// Still pending when the deadline passed.
    Pending(String),
}

/// The nonce a proposal will land at, read while it is still listed.
///
/// Completion has to be bound to the proposal it was asked about, and once the
/// proposal leaves the pending set there is nothing left to read the nonce
/// from, so callers capture it before executing.
async fn proposal_nonce(client: &mut MultisigClient, proposal_id: &str) -> Result<u64, String> {
    client
        .list_proposals()
        .await
        .map_err(|error| format!("listing proposals failed: {error}"))?
        .into_iter()
        .find(|entry| entry.id == proposal_id)
        .map(|entry| entry.nonce)
        .ok_or_else(|| format!("proposal {proposal_id} is not listed"))
}

/// A proposal whose nonce cannot be read is refused before it is executed.
///
/// Completion cannot be confirmed without the nonce, so executing anyway spent
/// the transaction, waited out the whole canonicalization deadline and then
/// reported the product, for what was never more than a listing the harness
/// could not read.
fn unbindable(reason: String) -> ActionOutcome {
    ActionOutcome::failed_setup(format!(
        "the proposal nonce could not be read before executing, so completion could not be \
         bound to it; nothing was executed: {reason}"
    ))
}

/// What ties an executed proposal's completion to that proposal.
#[derive(Clone, Copy)]
enum Binding {
    /// The canonical delta must land at this nonce.
    Nonce(u64),
    /// A migration repoints the client at the GUARDIAN it just moved to, and
    /// that GUARDIAN has no history for an account it has only just been
    /// handed. The canonical delta stays with the one being left behind, so
    /// chain agreement is what completion means.
    Migration,
}

/// Waits for an executed proposal to be provably complete.
///
/// Completion is not "the proposal disappeared": canonicalization removes the
/// proposal whether it applied the delta or gave up on it. So this asks the SDK
/// the same questions a consumer would — does local state match the chain, and
/// does GUARDIAN's canonical history carry that state — instead of re-deriving
/// an answer from the pending list.
async fn wait_for_execution(
    client: &mut MultisigClient,
    proposal_id: &str,
    binding: Binding,
) -> Completion {
    let started = std::time::Instant::now();
    let mut wait = std::time::Duration::from_secs(1);
    let mut last = String::from("never answered");

    while started.elapsed() < CANONICALIZATION_DEADLINE {
        let _ = client.sync().await;

        // Presence alone is not enough: a proposal reported as finalized is
        // done, not pending. This listing comes from GUARDIAN rather than a
        // local cache, so it does not carry stale entries the way the
        // TypeScript one can, but the two must agree on what pending means.
        let still_pending = match client.list_proposals().await {
            Ok(proposals) => proposals
                .iter()
                .find(|entry| entry.id == proposal_id)
                .is_some_and(|entry| !matches!(entry.status, ProposalStatus::Finalized)),
            Err(error) => {
                last = error.to_string();
                true
            }
        };

        if !still_pending {
            match client.verify_state_commitment().await {
                Ok(verified) => {
                    let commitment = normalize_hex(&verified.on_chain_commitment_hex);
                    let wanted = match binding {
                        Binding::Migration => return Completion::Confirmed,
                        Binding::Nonce(wanted) => wanted,
                    };
                    match client.delta_history(Some(20), None).await {
                        Ok(page) => {
                            // Bound to the proposal, not merely to the account
                            // being self-consistent. Matching on the commitment
                            // alone answers "is this account in a state some
                            // canonical delta explains", which an account whose
                            // delta was discarded satisfies just as well: it
                            // never moved, so it still agrees with chain and the
                            // *previous* delta still carries that commitment.
                            // The nonce is what ties the answer to the delta
                            // under test.
                            let canonical = page.entries.iter().any(|entry| {
                                entry.nonce == wanted
                                    && entry.new_commitment.as_deref().is_some_and(|recorded| {
                                        normalize_hex(recorded) == commitment
                                    })
                            });
                            if canonical {
                                return Completion::Confirmed;
                            }
                            last = format!(
                                "no canonical delta at nonce {wanted} carries commitment \
                                 {commitment}"
                            );
                        }
                        Err(error) => last = format!("reading the delta history failed: {error}"),
                    }
                }
                Err(error) => last = format!("state commitment disagrees: {error}"),
            }
        }

        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(std::time::Duration::from_secs(5));

        if !still_pending && started.elapsed() >= CANONICALIZATION_DEADLINE {
            return Completion::Discarded(last);
        }
    }

    match client.list_proposals().await {
        Ok(proposals) if !proposals.iter().any(|entry| entry.id == proposal_id) => {
            Completion::Discarded(last)
        }
        _ => Completion::Pending(format!(
            "still pending after {}s; {last}",
            CANONICALIZATION_DEADLINE.as_secs()
        )),
    }
}

fn normalize_hex(value: &str) -> String {
    value.trim_start_matches("0x").to_ascii_lowercase()
}

/// Asserts the account's holdings, first as an empty baseline and then against
/// what the transfer delivered.
///
/// The second reading is bounded rather than exact: executing the consuming
/// transaction pays a fee out of the same asset, so the account keeps less than
/// it received.
pub async fn assert_balance(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };

    if !session.balance_seen {
        let Some(account) = session.clients[0].account() else {
            return ActionOutcome::failed_setup("the client holds no account to read".to_string());
        };
        let held = account.inner().vault().assets().count();
        if held > 0 {
            return ActionOutcome::failed_product(format!(
                "a freshly created account already holds {held} asset(s)"
            ));
        }
        session.balance_seen = true;
        return ActionOutcome::Passed;
    }

    let Some(faucet) = session.faucet else {
        return ActionOutcome::failed_setup("nothing was transferred to this account");
    };
    let transferred = session.transferred;

    if let Err(error) = session.clients[0].sync().await {
        return ActionOutcome::failed_product(format!(
            "syncing before the balance read failed: {error}"
        ));
    }

    let Some(account) = session.clients[0].account() else {
        return ActionOutcome::failed_setup("the client holds no account to read".to_string());
    };
    let balance = vault_amount(account.inner(), faucet);

    if balance == 0 {
        ActionOutcome::failed_product(format!(
            "the account holds nothing of faucet {faucet} after consuming the note"
        ))
    } else if balance > transferred {
        ActionOutcome::failed_product(format!(
            "the account holds {balance} but only {transferred} was transferred"
        ))
    } else {
        ActionOutcome::Passed
    }
}

/// Sends assets to the multisig account from the treasury.
pub async fn transfer_asset(runner: &Runner) -> ActionOutcome {
    let Some(context) = runner.live.as_ref() else {
        return ActionOutcome::failed_setup("the live context is not configured");
    };
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };

    match crate::funding::service::fund_once(
        context.network,
        &context.treasury_dir,
        session.account_id,
        ACCOUNT_FUNDING,
    )
    .await
    {
        Ok(Some(funded)) => {
            session.faucet = Some(funded.faucet);
            session.treasury = Some(funded.treasury);
            session.transferred += funded.amount;
            ActionOutcome::Passed
        }
        Ok(None) => ActionOutcome::Skipped {
            reason: "this chain charges nothing, so there is nothing to transfer".to_string(),
        },
        Err(error) => ActionOutcome::failed_setup(format!(
            "cannot transfer to {}: {error}",
            session.account_id
        )),
    }
}

/// Consumes the transferred note through a full proposal lifecycle.
pub async fn consume_note(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let threshold = session.threshold as usize;

    let notes = match wait_for_notes(&mut session.clients[0], NOTE_ARRIVAL_DEADLINE).await {
        Ok(notes) => notes,
        Err(outcome) => return outcome,
    };

    let note_ids = notes.iter().map(|note| note.id).collect();
    match session.clients[0]
        .propose_transaction(miden_multisig_client::TransactionType::consume_notes(
            note_ids,
        ))
        .await
    {
        Ok(proposal) => session.proposal_id = Some(proposal.id.clone()),
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "creating the consume proposal failed: {error}"
            ));
        }
    }

    if let Err(outcome) = collect_signatures(session, threshold).await {
        return outcome;
    }

    let proposal_id = session.proposal_id.clone().unwrap_or_default();
    let client = &mut session.clients[0];
    // Read before executing, for the same reason: completion is bound to this
    // proposal, and the proposal is gone by the time it is judged.
    let nonce = match proposal_nonce(client, &proposal_id).await {
        Ok(nonce) => nonce,
        Err(reason) => return unbindable(reason),
    };
    if let Err(error) = client.execute_proposal(&proposal_id).await {
        return ActionOutcome::failed_product(format!("consuming the note failed: {error}"));
    }
    if let Err(error) = client.sync().await {
        return ActionOutcome::failed_product(format!("syncing after the consume failed: {error}"));
    }

    match wait_for_execution(client, &proposal_id, Binding::Nonce(nonce)).await {
        Completion::Confirmed => ActionOutcome::Passed,
        Completion::Discarded(reason) => ActionOutcome::failed_product(format!(
            "the consuming proposal left the pending set without becoming canonical: {reason}"
        )),
        Completion::Pending(reason) => ActionOutcome::EnvironmentBlocked {
            reason: format!("the consuming proposal was {reason}"),
        },
    }
}

/// Serializes the pending proposal for transport over a side channel.
pub async fn export_proposal(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(proposal_id) = session.proposal_id.clone() else {
        return ActionOutcome::failed_setup("no proposal has been created in this scenario");
    };

    match session.clients[0]
        .export_proposal_to_string(&proposal_id)
        .await
    {
        Ok(json) => {
            session.exported_proposal = Some(json);
            ActionOutcome::Passed
        }
        Err(error) => {
            ActionOutcome::failed_product(format!("exporting the proposal failed: {error}"))
        }
    }
}

/// Signs the exported proposal on each cosigner's own client until the
/// threshold is met, passing the document along rather than any shared state.
///
/// Nothing reaches GUARDIAN here: the point of the offline path is that the
/// signatures travel inside the document.
pub async fn sign_proposal_external(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(document) = session.exported_proposal.clone() else {
        return ActionOutcome::failed_setup("no proposal has been exported in this scenario");
    };
    let threshold = session.threshold as usize;
    let account_id = session.account_id;

    let mut exported = match miden_multisig_client::ExportedProposal::from_json(&document) {
        Ok(exported) => exported,
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "the exported proposal is not readable: {error}"
            ));
        }
    };

    for index in 0..session.clients.len() {
        if exported.signatures.len() >= threshold {
            break;
        }
        let client = &mut session.clients[index];
        if index > 0
            && let Err(error) = client.pull_account(account_id).await
        {
            return ActionOutcome::failed_product(format!(
                "cosigner {index} could not load the account: {error}"
            ));
        }
        match client.sign_imported_proposal(&mut exported).await {
            Ok(()) => {}
            Err(error) => {
                let message = error.to_string().to_lowercase();
                if message.contains("already signed") {
                    continue;
                }
                return ActionOutcome::failed_product(format!(
                    "cosigner {index} could not sign offline: {error}"
                ));
            }
        }
    }

    if exported.signatures.len() < threshold {
        return ActionOutcome::failed_product(format!(
            "the offline document carries {} signature(s) but the threshold is {threshold}",
            exported.signatures.len()
        ));
    }

    match exported.to_json() {
        Ok(json) => {
            session.exported_proposal = Some(json);
            ActionOutcome::Passed
        }
        Err(error) => ActionOutcome::failed_product(format!(
            "re-serializing the signed proposal failed: {error}"
        )),
    }
}

/// Brings the externally signed document back into the executing client.
pub async fn import_proposal(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(document) = session.exported_proposal.clone() else {
        return ActionOutcome::failed_setup("no proposal has been signed offline in this scenario");
    };
    let threshold = session.threshold as usize;

    match session.clients[0]
        .import_proposal_from_string(&document)
        .await
    {
        Ok(imported) => {
            if imported.signatures.len() < threshold {
                return ActionOutcome::failed_product(format!(
                    "the imported proposal kept {} of {threshold} signatures",
                    imported.signatures.len()
                ));
            }
            session.proposal_id = Some(imported.id.clone());
            ActionOutcome::Passed
        }
        Err(error) => {
            ActionOutcome::failed_product(format!("importing the signed proposal failed: {error}"))
        }
    }
}

/// Reads the acknowledgement identity of the GUARDIAN an account would migrate
/// to. Scheme-specific: GUARDIAN holds one identity per signature scheme and the
/// account binds the one matching its own.
async fn migration_target_commitment(endpoint: &str, scheme: Scheme) -> anyhow::Result<Word> {
    let scheme = match scheme {
        Scheme::Falcon => "falcon",
        Scheme::Ecdsa => "ecdsa",
        other => anyhow::bail!("{other:?} has no GUARDIAN acknowledgement identity"),
    };
    let mut client = guardian_client::GuardianClient::connect(endpoint.to_string()).await?;
    let (commitment, _) = client.get_pubkey(Some(scheme)).await?;
    Ok(Word::try_from(commitment.as_str())?)
}

/// Builds a GUARDIAN migration proposal without contacting GUARDIAN.
///
/// Migration needs a second deployment to migrate to: the account's guardian
/// slot must actually change, and re-pointing it at the GUARDIAN it already
/// uses produces no state change for the transaction to commit. The endpoint is
/// supplied by the stack rather than assumed, so a run without one reports that
/// it could not test migration instead of testing something else.
pub async fn create_proposal_offline(runner: &Runner) -> ActionOutcome {
    let Some(context) = runner.live.as_ref() else {
        return ActionOutcome::failed_setup("the live context is not configured");
    };
    let Some(target) = context.migration_endpoint.clone() else {
        return ActionOutcome::EnvironmentBlocked {
            // Named as the driver actually reads it. The Rust driver speaks
            // gRPC, so it takes the gRPC endpoint; the TypeScript one takes
            // the HTTP endpoint. Pointing a reader at a variable nothing reads
            // sends them to configure the wrong thing.
            reason: "migration needs a second GUARDIAN deployment; set \
                     QUAL_GUARDIAN_MIGRATION_GRPC to one"
                .to_string(),
        };
    };

    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };

    let commitment = match migration_target_commitment(&target, session.scheme).await {
        Ok(commitment) => commitment,
        Err(error) => {
            return ActionOutcome::EnvironmentBlocked {
                reason: format!("the migration target at {target} is unreachable: {error}"),
            };
        }
    };

    let client = &mut session.clients[0];
    let current = client
        .account()
        .and_then(|account| account.guardian_commitment().ok());
    if current == Some(commitment) {
        return ActionOutcome::EnvironmentBlocked {
            reason: format!(
                "the migration target at {target} has the same identity as the current GUARDIAN"
            ),
        };
    }

    match client
        .create_proposal_offline(miden_multisig_client::TransactionType::SwitchGuardian {
            new_endpoint: target,
            new_commitment: commitment,
        })
        .await
    {
        Ok(exported) => match exported.to_json() {
            Ok(json) => {
                session.proposal_id = Some(exported.id.clone());
                session.exported_proposal = Some(json);
                session.migrating = true;
                ActionOutcome::Passed
            }
            Err(error) => ActionOutcome::failed_product(format!(
                "serializing the offline migration proposal failed: {error}"
            )),
        },
        Err(error) => ActionOutcome::failed_product(format!(
            "building the offline migration proposal failed: {error}"
        )),
    }
}

/// Rotates GUARDIAN through the pending set instead of around it.
///
/// The offline path is the air-gapped one: nothing reaches GUARDIAN until the
/// signed document is imported. This is the other half of the flow, and the
/// half a deployment actually uses when GUARDIAN is reachable: the proposal is
/// coordinated by GUARDIAN, cosigners sign it there, and only then does it
/// execute. Rotation is a first-class custody operation, so qualifying only the
/// air-gapped path leaves the common one untested.
///
/// The distinction is asserted rather than assumed: a proposal that never
/// reached GUARDIAN's pending set was created offline whatever the call was
/// named.
pub async fn switch_guardian_online(runner: &Runner) -> ActionOutcome {
    let Some(context) = runner.live.as_ref() else {
        return ActionOutcome::failed_setup("the live context is not configured");
    };
    let Some(target) = context.migration_endpoint.clone() else {
        return ActionOutcome::EnvironmentBlocked {
            reason: "rotation needs a second GUARDIAN to rotate to; set \
                     QUAL_GUARDIAN_MIGRATION_GRPC to one"
                .to_string(),
        };
    };

    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };

    let commitment = match migration_target_commitment(&target, session.scheme).await {
        Ok(commitment) => commitment,
        Err(error) => {
            return ActionOutcome::EnvironmentBlocked {
                reason: format!("the rotation target at {target} is unreachable: {error}"),
            };
        }
    };

    let client = &mut session.clients[0];
    let current = client
        .account()
        .and_then(|account| account.guardian_commitment().ok());
    if current == Some(commitment) {
        return ActionOutcome::EnvironmentBlocked {
            reason: format!(
                "the rotation target at {target} has the same identity as the current GUARDIAN"
            ),
        };
    }

    let proposal = match client
        .propose_transaction(miden_multisig_client::TransactionType::SwitchGuardian {
            new_endpoint: target.clone(),
            new_commitment: commitment,
        })
        .await
    {
        Ok(proposal) => proposal,
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "proposing a rotation to {target} through GUARDIAN failed: {error}"
            ));
        }
    };

    // What makes this the online path. `list_proposals` reads GUARDIAN's
    // pending set, so a proposal missing from it was coordinated somewhere
    // else, which is the offline flow wearing this scenario's name.
    match client.list_proposals().await {
        Ok(proposals) => {
            if !proposals.iter().any(|pending| pending.id == proposal.id) {
                return ActionOutcome::failed_product(format!(
                    "the rotation proposal {} is not in GUARDIAN's pending set, so it was not \
                     coordinated online",
                    proposal.id
                ));
            }
        }
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "listing proposals after proposing the rotation failed: {error}"
            ));
        }
    }

    session.proposal_id = Some(proposal.id);
    // Completion is judged differently for a rotation: the GUARDIAN the client
    // moves to has no history for an account it was just handed.
    session.migrating = true;
    ActionOutcome::Passed
}

/// Confirms the rotation actually moved the account, rather than only executing.
///
/// The offline scenario asserts the shape of the document it produced, which
/// says nothing about the account. This reads the account's own binding after
/// execution: a rotation that executed without changing the bound identity is
/// the failure worth catching, because every other signal looks like success.
pub async fn assert_guardian_switched(runner: &Runner) -> ActionOutcome {
    let Some(context) = runner.live.as_ref() else {
        return ActionOutcome::failed_setup("the live context is not configured");
    };
    let Some(target) = context.migration_endpoint.clone() else {
        return ActionOutcome::failed_setup("no rotation target is configured");
    };

    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };

    let expected = match migration_target_commitment(&target, session.scheme).await {
        Ok(commitment) => commitment,
        Err(error) => {
            return ActionOutcome::EnvironmentBlocked {
                reason: format!("the rotation target at {target} is unreachable: {error}"),
            };
        }
    };

    let client = &mut session.clients[0];
    if let Err(error) = client.sync().await {
        return ActionOutcome::failed_product(format!(
            "syncing after the rotation failed: {error}"
        ));
    }

    let Some(account) = client.account() else {
        return ActionOutcome::failed_setup("the client holds no account to read".to_string());
    };
    match account.guardian_commitment() {
        Ok(bound) if bound == expected => ActionOutcome::Passed,
        Ok(bound) => ActionOutcome::failed_product(format!(
            "the rotation executed but the account still binds {}, not {} at {target}",
            bound.to_hex(),
            expected.to_hex()
        )),
        Err(error) => ActionOutcome::failed_product(format!(
            "the account's GUARDIAN binding could not be read after the rotation: {error}"
        )),
    }
}

/// A paused account is refused on a live network, and works again once
/// unpaused.
///
/// The deterministic scenario proves GUARDIAN's own gate: a paused account is
/// refused a proposal. It cannot prove the part that matters to custody, which
/// is that the pause actually stops a transaction that would otherwise reach
/// the chain. Execution needs GUARDIAN's acknowledgement, so a paused account
/// cannot execute, and nothing lands.
///
/// The proposal is created before the pause deliberately: refusing to create
/// one shows the gate on the way in, while refusing to execute one that is
/// already signed and ready shows the gate standing between a client and the
/// chain. That second refusal is the custody property.
///
/// Unpauses whatever the attempt concluded, then leaves the proposal intact so
/// the scenario's next action can execute it. A pause that cannot be lifted, or
/// that leaves the account unable to transact afterwards, is as much a defect
/// as one that fails to stop anything.
pub async fn assert_paused_refuses_execution(runner: &Runner) -> ActionOutcome {
    let Some(fixtures) = runner.fixtures.as_ref() else {
        return ActionOutcome::failed_setup("the server fixtures were not loaded");
    };
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(proposal_id) = session.proposal_id.clone() else {
        return ActionOutcome::failed_setup("no proposal has been created in this scenario");
    };
    let account_id = session.account_id.to_string();

    let base = runner.endpoints.http.trim_end_matches('/').to_string();
    if let Err(outcome) = super::account::operator_login(runner, &base, fixtures).await {
        return outcome;
    }
    if let Err(outcome) = super::account::set_paused(runner, &base, &account_id, true).await {
        return outcome;
    }

    let attempt = match session.clients[0].execute_proposal(&proposal_id).await {
        Ok(_) => ActionOutcome::failed_product(
            "a paused account executed a proposal; the pause did not stand between the client \
             and the chain"
                .to_string(),
        ),
        Err(error) => {
            let rendered = error.to_string();
            // Wording, because the code is not reachable here. GUARDIAN answers
            // `GUARDIAN_ACCOUNT_PAUSED`, but by the time the refusal surfaces
            // as a `MultisigError` only the gRPC status and the human message
            // survive: `guardian_client::ClientError` exposes `guardian_code`
            // and `MultisigError` has no equivalent. Recorded as a finding in
            // docs/QUALIFICATION.md rather than worked around silently.
            //
            // A loose match is tolerable here only because the scenario proves
            // causation structurally rather than by reading the message: the
            // next action executes the same proposal with the same client and
            // must succeed once unpaused. Refused while paused and accepted
            // when not is the evidence; this check only rules out a refusal
            // that obviously has nothing to do with pausing.
            if rendered.contains("account is paused") {
                ActionOutcome::Passed
            } else {
                ActionOutcome::failed_product(format!(
                    "the paused account was refused, but not as paused: {rendered}"
                ))
            }
        }
    };

    // Always, whatever the attempt concluded. An account left paused fails the
    // rest of the scenario for a reason that has nothing to do with it.
    match super::account::set_paused(runner, &base, &account_id, false).await {
        Ok(()) => attempt,
        Err(unpause_failure) => match attempt {
            ActionOutcome::Passed => unpause_failure,
            other => other,
        },
    }
}

/// The label a producer chooses for a proposal type the SDK does not model.
const CUSTOM_PROPOSAL_TYPE: &str = "qualification_probe";

/// Proposes a transaction the SDK has no type for, the way a producer does.
///
/// Every other scenario proposes through the typed API, so all of them exercise
/// the seven built-in proposal types and none of them exercise the producer
/// path (issue #266): serialized Miden transaction bytes plus a label the SDK
/// has never heard of. That path is the unbounded one, and an integration built
/// on it would break without this suite noticing.
///
/// The transaction itself is an ordinary P2ID send, chosen because its
/// correctness is already covered elsewhere. What is under test is the label
/// surviving the round trip, not the payment.
pub async fn create_custom_proposal(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let (Some(faucet), Some(treasury)) = (session.faucet, session.treasury) else {
        return ActionOutcome::failed_setup(
            "the account was never funded, so it holds nothing to send",
        );
    };

    if let Err(error) = session.clients[0].sync().await {
        return ActionOutcome::failed_product(format!(
            "syncing before the proposal failed: {error}"
        ));
    }

    let Some(account) = session.clients[0].account() else {
        return ActionOutcome::failed_setup("the client holds no account to read".to_string());
    };
    let asset = match miden_protocol::asset::FungibleAsset::new(faucet, P2ID_AMOUNT) {
        Ok(asset) => Asset::Fungible(asset),
        Err(error) => {
            return ActionOutcome::failed_setup(format!(
                "{P2ID_AMOUNT} of {faucet} is not a valid asset: {error}"
            ));
        }
    };

    // Built and serialized here rather than through the typed API, because
    // producer-supplied bytes are the thing being qualified.
    let request = match miden_multisig_client::build_p2id_transaction_request(
        account.inner(),
        treasury,
        vec![asset],
        miden_protocol::note::NoteType::Public,
        miden_multisig_client::P2ideHeights::default(),
        miden_multisig_client::generate_salt(),
        [],
    ) {
        Ok(request) => request,
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "building a transaction request for a custom proposal failed: {error}"
            ));
        }
    };
    let bytes = miden_protocol::utils::serde::Serializable::to_bytes(&request);

    match session.clients[0]
        .propose_custom_transaction(&bytes, CUSTOM_PROPOSAL_TYPE)
        .await
    {
        Ok(proposal) => {
            if proposal.metadata.proposal_type.as_deref() != Some(CUSTOM_PROPOSAL_TYPE) {
                return ActionOutcome::failed_product(format!(
                    "the proposal came back labelled {:?}, not `{CUSTOM_PROPOSAL_TYPE}`",
                    proposal.metadata.proposal_type
                ));
            }
            session.custom_nonce = Some(proposal.nonce);
            session.proposal_id = Some(proposal.id);
            session.custom_request = Some(bytes);
            ActionOutcome::Passed
        }
        Err(error) => ActionOutcome::failed_product(format!(
            "proposing a `{CUSTOM_PROPOSAL_TYPE}` transaction failed: {error}"
        )),
    }
}

/// Confirms GUARDIAN stored and serves the producer's own label.
///
/// The label is the whole contract of the producer API: a GUARDIAN that
/// accepted the proposal but returned it as `custom`, or as one of its own
/// built-ins, would leave every producer unable to tell its proposals apart
/// while every other signal looked healthy. Read back from GUARDIAN rather
/// than from the client that made it, because the client's own copy would
/// agree with itself.
pub async fn assert_custom_proposal_type(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(proposal_id) = session.proposal_id.clone() else {
        return ActionOutcome::failed_setup("no proposal has been created in this scenario");
    };

    match session.clients[0].list_proposals().await {
        Ok(proposals) => match proposals.iter().find(|proposal| proposal.id == proposal_id) {
            Some(proposal) => match proposal.metadata.proposal_type.as_deref() {
                Some(CUSTOM_PROPOSAL_TYPE) => ActionOutcome::Passed,
                other => ActionOutcome::failed_product(format!(
                    "GUARDIAN serves the proposal as {other:?}, not as the producer's \
                         `{CUSTOM_PROPOSAL_TYPE}`"
                )),
            },
            None => ActionOutcome::failed_product(format!(
                "GUARDIAN does not list the custom proposal {proposal_id}"
            )),
        },
        Err(error) => ActionOutcome::failed_product(format!("listing proposals failed: {error}")),
    }
}

/// Far enough ahead that the note stays locked for the life of the run, without
/// needing the chain tip to compute it. The assets stay in the note; these
/// accounts are ephemeral and their residue is accepted rather than swept.
const P2IDE_TIMELOCK_HEIGHT: u32 = 4_000_000_000;

/// Sends a timelocked note to the account itself.
///
/// P2ID is covered; P2IDE is the same flow with a height attached, and nothing
/// exercised it. Self-addressed on purpose: the timelock is only observable
/// from the recipient's side, and sending to a counterparty this scenario does
/// not drive would leave nothing to assert against.
pub async fn send_p2ide(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(faucet) = session.faucet else {
        return ActionOutcome::failed_setup("the account was never funded, so it holds nothing");
    };
    let account_id = session.account_id;

    if let Err(error) = session.clients[0].sync().await {
        return ActionOutcome::failed_product(format!(
            "syncing before the transfer failed: {error}"
        ));
    }
    let Some(before) = held_balance(&session.clients[0], faucet) else {
        return ActionOutcome::failed_setup("the client holds no account to read");
    };
    if before <= P2ID_AMOUNT {
        return ActionOutcome::failed_setup(format!(
            "the account holds {before}, which is not enough to send {P2ID_AMOUNT} and pay the fee"
        ));
    }

    let Some(timelock) = std::num::NonZeroU32::new(P2IDE_TIMELOCK_HEIGHT) else {
        return ActionOutcome::failed_setup("the timelock height must be non-zero");
    };

    match propose_when_settled(
        &mut session.clients[0],
        miden_multisig_client::TransactionType::transfer_p2ide(
            account_id,
            faucet,
            P2ID_AMOUNT,
            miden_protocol::note::NoteType::Public,
            miden_multisig_client::P2ideHeights {
                reclaim: None,
                timelock: Some(timelock),
            },
        ),
    )
    .await
    {
        Ok(proposal) => {
            session.proposal_id = Some(proposal.id.clone());
            session.balance_before_send = Some(before);
            session.sent_amount = P2ID_AMOUNT;
            ActionOutcome::Passed
        }
        Err(error) => {
            ActionOutcome::failed_product(format!("proposing the timelocked send failed: {error}"))
        }
    }
}

/// Confirms the height on the note is doing something.
///
/// A P2IDE note whose timelock were dropped, or encoded as the on-chain "no
/// constraint" zero, would be indistinguishable from a plain P2ID at every
/// other point in this flow: the transaction executes, the balance moves, the
/// note lands. The difference shows only here, and only as the pair of answers
/// below. Committed alone would pass for a P2ID; not-consumable alone would
/// pass for a note that never arrived.
pub async fn assert_p2ide_timelocked(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };

    if let Err(error) = session.clients[0].sync().await {
        return ActionOutcome::failed_product(format!(
            "syncing after the transfer failed: {error}"
        ));
    }

    let committed = match session.clients[0].list_committed_notes().await {
        Ok(notes) => notes,
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "listing committed notes failed: {error}"
            ));
        }
    };
    if committed.is_empty() {
        return ActionOutcome::failed_product(
            "the timelocked note never reached the account, so the timelock cannot be read"
                .to_string(),
        );
    }

    let consumable = match session.clients[0].list_consumable_notes().await {
        Ok(notes) => notes,
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "listing consumable notes failed: {error}"
            ));
        }
    };

    let unlocked: Vec<String> = consumable
        .iter()
        .filter(|note| committed.iter().any(|held| held.id == note.id))
        .map(|note| note.id.to_hex())
        .collect();
    if unlocked.is_empty() {
        ActionOutcome::Passed
    } else {
        ActionOutcome::failed_product(format!(
            "a note timelocked to block {P2IDE_TIMELOCK_HEIGHT} is already consumable: {}",
            unlocked.join(", ")
        ))
    }
}

/// Assembles the execution advice a producer integration needs, which is where
/// the SDK's responsibility for a custom proposal ends.
///
/// A custom proposal is deliberately not executed by `execute_proposal`: the
/// SDK cannot rebuild an arbitrary producer transaction, so it hands back the
/// cosigner signatures and GUARDIAN's acknowledgement, and the integration
/// injects them into its own request and submits with its own Miden client.
/// This scenario stops at that boundary rather than reimplementing an
/// integration, and says so.
///
/// What the boundary is worth asserting for: preparing re-executes the
/// producer's own bytes at the proposal's anchored block and refuses unless
/// they reproduce the signed commitment. So a pass here means the label
/// survived, the threshold was met, and the bytes still match what was signed.
/// That last part is the anti-tamper property of the producer API.
pub async fn prepare_custom_execution(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(proposal_id) = session.proposal_id.clone() else {
        return ActionOutcome::failed_setup("no proposal has been created in this scenario");
    };
    let Some(request) = session.custom_request.clone() else {
        return ActionOutcome::failed_setup("no custom proposal was created in this scenario");
    };

    match session.clients[0]
        .prepare_custom_execution(&proposal_id, &request)
        .await
    {
        Ok(advice) if advice.is_empty() => ActionOutcome::failed_product(
            "the custom proposal prepared no execution advice, so an integration would have \
             nothing to inject"
                .to_string(),
        ),
        Ok(_) => ActionOutcome::Passed,
        Err(error) => {
            ActionOutcome::failed_product(format!("preparing the custom execution failed: {error}"))
        }
    }
}

/// How long an abandoned candidate may take to resolve. The quarantine is a
/// short wall-clock minimum plus a couple of at-base observations, so this is
/// generous rather than tight.
const ABANDON_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// The negative control for how every other scenario asserts completion.
///
/// Completion is chain confirmation plus a canonical delta, and not "the
/// proposal left the pending set", because canonicalization removes a discarded
/// delta exactly as it removes a successful one. Every other scenario exercises
/// the positive side of that rule. This is the negative: it produces a real
/// discard and checks nothing serves it as live, so the rule is falsified by
/// experiment rather than only correct by construction.
///
/// The candidate comes from the producer API, which is the one path that
/// separates acknowledgement from submission. `prepare_custom_execution` pushes
/// the delta to obtain GUARDIAN's acknowledgement, and `submit_transaction` is
/// a separate call the integration makes. Stopping in between leaves a
/// candidate that can never land, which is precisely the state the abandon API
/// exists for, and it reaches that state through supported calls rather than by
/// forcing GUARDIAN into it.
///
/// Nothing here reaches Miden, so the account stays at the candidate's base and
/// the abandon resolves through its designed at-base path rather than through
/// retry exhaustion.
pub async fn abandon_and_assert_hidden(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(nonce) = session.custom_nonce else {
        return ActionOutcome::failed_setup("no custom proposal was prepared in this scenario");
    };
    let Some(proposal_id) = session.proposal_id.clone() else {
        return ActionOutcome::failed_setup("no proposal has been created in this scenario");
    };

    let client = &mut session.clients[0];

    // Established before abandoning, or the check afterwards means nothing: a
    // proposal already gone from the pending set would satisfy it without the
    // discard having hidden anything.
    match client.list_proposals().await {
        Ok(proposals) => {
            if !proposals.iter().any(|pending| pending.id == proposal_id) {
                return ActionOutcome::failed_product(format!(
                    "the proposal {proposal_id} is not pending before the abandon, so its \
                     absence afterwards would prove nothing"
                ));
            }
        }
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "listing proposals before the abandon failed: {error}"
            ));
        }
    }

    if let Err(error) = client.abandon_candidate(nonce).await {
        return ActionOutcome::failed_product(format!(
            "abandoning the candidate at nonce {nonce} failed: {error}"
        ));
    }

    let deadline = std::time::Instant::now() + ABANDON_DEADLINE;
    loop {
        let state = match client.abandon_status(nonce).await {
            Ok(AbandonStatus::Abandoned) => break,
            Ok(AbandonStatus::Landed) => {
                return ActionOutcome::failed_product(
                    "the candidate canonicalized, so nothing was discarded to look for; this \
                     scenario never submits, so GUARDIAN saw a transaction it should not have"
                        .to_string(),
                );
            }
            Ok(other) => format!("{other:?}"),
            Err(error) => error.to_string(),
        };
        if std::time::Instant::now() >= deadline {
            return ActionOutcome::failed_product(format!(
                "the abandoned candidate at nonce {nonce} was still {state} after {}s",
                ABANDON_DEADLINE.as_secs()
            ));
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    // The discard is only safe while it is invisible to what a client reads by
    // default. A discarded delta still listed as a pending proposal is the
    // shape that makes "it left the pending set" look like completion.
    match client.list_proposals().await {
        Ok(proposals) => {
            if proposals.iter().any(|pending| pending.id == proposal_id) {
                return ActionOutcome::failed_product(format!(
                    "the delta at nonce {nonce} was discarded but its proposal {proposal_id} is \
                     still listed as pending"
                ));
            }
        }
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "listing proposals after the discard failed: {error}"
            ));
        }
    }

    // It must not have moved the account. Reading state back is what a client
    // does next, and it is the other way a discard could pass for a completion.
    if let Err(error) = client.verify_state_commitment().await {
        return ActionOutcome::failed_product(format!(
            "the account does not agree with chain after a discarded delta: {error}"
        ));
    }

    // The rule itself, not only its symptom. Everything above shows the discard
    // is invisible to what a client reads by default, which is worth having,
    // but the code that has to tell a discard from a success is
    // `wait_for_execution`: every other live scenario trusts its verdict. Ask
    // it about a delta that really was discarded, and it must say so rather
    // than read the empty pending set as completion.
    match wait_for_execution(client, &proposal_id, Binding::Nonce(nonce)).await {
        Completion::Discarded(_) => ActionOutcome::Passed,
        Completion::Confirmed => ActionOutcome::failed_product(
            "the completion check calls a discarded delta confirmed, so every scenario that \
             trusts it would read an abandoned candidate as a successful execution"
                .to_string(),
        ),
        Completion::Pending(reason) => ActionOutcome::failed_product(format!(
            "the completion check still calls the discarded delta pending after its deadline: \
             {reason}"
        )),
    }
}

/// Checks the offline proposal really is a GUARDIAN migration.
pub async fn assert_guardian_migration(runner: &Runner) -> ActionOutcome {
    let guard = runner.session.lock().await;
    let Some(session) = guard.as_ref() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(document) = session.exported_proposal.as_ref() else {
        return ActionOutcome::failed_setup(
            "no offline proposal has been created in this scenario",
        );
    };

    let exported = match miden_multisig_client::ExportedProposal::from_json(document) {
        Ok(exported) => exported,
        Err(error) => {
            return ActionOutcome::failed_product(format!(
                "the offline proposal is not readable: {error}"
            ));
        }
    };

    match exported.to_proposal() {
        Ok(proposal) => match proposal.transaction_type {
            miden_multisig_client::TransactionType::SwitchGuardian { .. } => ActionOutcome::Passed,
            other => ActionOutcome::failed_product(format!(
                "the offline proposal is a {} proposal, not a migration",
                other.type_name()
            )),
        },
        Err(error) => ActionOutcome::failed_product(format!(
            "the offline proposal does not decode to a proposal: {error}"
        )),
    }
}

/// Hands one cosigner's key to the TypeScript driver and has it sign the
/// proposal.
///
/// The signature has to come from the other SDK's own process for the scenario
/// to mean anything, so this shells out rather than signing here. The key
/// travels in a file, not an argument, so it stays out of the process list.
pub async fn handoff_to_typescript(runner: &Runner) -> ActionOutcome {
    let Some(context) = runner.live.as_ref() else {
        return ActionOutcome::failed_setup("the live context is not configured");
    };
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(proposal_id) = session.proposal_id.clone() else {
        return ActionOutcome::failed_setup("no proposal has been created in this scenario");
    };
    if session.clients.len() < 2 {
        return ActionOutcome::failed_setup("this shape has no second cosigner to hand off to");
    }

    let account_id = session.account_id;
    let scheme = session.scheme;
    let commitment = session.clients[1].user_commitment_hex();

    let Some(key) = session
        .signers
        .signers
        .get(1)
        .map(RunSigner::to_auth_secret_key)
    else {
        return ActionOutcome::failed_setup("the second cosigner has no retained key");
    };

    let dir = context.account_dir.join("handoff");
    if let Err(error) = std::fs::create_dir_all(&dir) {
        return ActionOutcome::failed_setup(format!(
            "cannot prepare the handoff directory: {error}"
        ));
    }
    let key_file = dir.join(format!("{account_id}.key"));
    if let Err(error) = crate::handoff::write_key(&key_file, &key) {
        return ActionOutcome::failed_setup(error.to_string());
    }

    let result = crate::handoff::cosign_with_typescript(crate::handoff::TypescriptCosign {
        network: context.network,
        // The TypeScript SDK reaches GUARDIAN over HTTP, unlike this driver.
        guardian_http_endpoint: runner.endpoints.http.clone(),
        account_id: account_id.to_hex(),
        proposal_id: proposal_id.clone(),
        scheme,
        key_file: key_file.clone(),
    })
    .await;
    let _ = std::fs::remove_file(&key_file);

    if let Err(error) = result {
        return ActionOutcome::failed_product(format!(
            "the TypeScript driver could not sign {proposal_id}: {error}"
        ));
    }

    let wanted = commitment.trim_start_matches("0x").to_ascii_lowercase();
    match session.clients[0].list_proposals().await {
        Ok(proposals) => {
            let signed = proposals
                .iter()
                .find(|entry| entry.id == proposal_id)
                .map(|entry| {
                    entry.signatures.iter().any(|signature| {
                        signature
                            .signer_commitment
                            .trim_start_matches("0x")
                            .eq_ignore_ascii_case(&wanted)
                    })
                })
                .unwrap_or(false);
            if signed {
                ActionOutcome::Passed
            } else {
                ActionOutcome::failed_product(
                    "the TypeScript driver reported success but its signature is not on the \
                     proposal"
                        .to_string(),
                )
            }
        }
        Err(error) => ActionOutcome::failed_product(format!(
            "reading back the handed-off signature failed: {error}"
        )),
    }
}

fn normalize_commitments(commitments: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = commitments
        .into_iter()
        .map(|entry| entry.trim_start_matches("0x").to_ascii_lowercase())
        .collect();
    out.sort();
    out
}

fn current_signer_set(client: &MultisigClient) -> Option<Vec<String>> {
    client
        .account()
        .map(|account| normalize_commitments(account.cosigner_commitments_hex()))
}

/// Proposes admitting a signer that does not yet hold the account.
///
/// The incoming signer gets its own client and is kept out of the cosigner list
/// until the change executes: a key that could sign its own admission would make
/// the threshold meaningless.
pub async fn add_signer(runner: &Runner, run_tag: &str) -> ActionOutcome {
    let Some(context) = runner.live.as_ref() else {
        return ActionOutcome::failed_setup("the live context is not configured");
    };
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };

    let Some(incoming_signers) = RunSigners::for_shape(1, 1, session.scheme) else {
        return ActionOutcome::failed_setup("cannot generate an incoming signer");
    };
    let incoming =
        match build_cosigners(context, &incoming_signers, &format!("{run_tag}-incoming")).await {
            Ok(clients) => clients,
            Err(error) => return ActionOutcome::failed_setup(error.to_string()),
        };
    let commitment = incoming[0].user_commitment();
    let commitment_hex = normalize_commitments(vec![incoming[0].user_commitment_hex()])
        .pop()
        .unwrap_or_default();

    let Some(before) = current_signer_set(&session.clients[0]) else {
        return ActionOutcome::failed_setup("the client holds no account to read");
    };
    if before.contains(&commitment_hex) {
        return ActionOutcome::failed_setup("the incoming signer already holds the account");
    }

    match propose_when_settled(
        &mut session.clients[0],
        miden_multisig_client::TransactionType::add_cosigner(commitment),
    )
    .await
    {
        Ok(proposal) => {
            session.proposal_id = Some(proposal.id.clone());
            let mut expected = before;
            expected.push(commitment_hex);
            expected.sort();
            session.expected_signers = Some(expected);
            session.expected_threshold = Some(session.threshold);
            session.incoming = Some(incoming);
            ActionOutcome::Passed
        }
        Err(error) => {
            ActionOutcome::failed_product(format!("proposing the added signer failed: {error}"))
        }
    }
}

/// Proposes removing the cosigner that has taken no part in the scenario.
pub async fn remove_signer(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };

    let last = session.clients.len() - 1;
    let departing = session.clients[last].user_commitment();
    let departing_hex = normalize_commitments(vec![session.clients[last].user_commitment_hex()])
        .pop()
        .unwrap_or_default();

    let Some(before) = current_signer_set(&session.clients[0]) else {
        return ActionOutcome::failed_setup("the client holds no account to read");
    };
    if !before.contains(&departing_hex) {
        return ActionOutcome::failed_setup(format!("{departing_hex} is not in the signer set"));
    }
    if (before.len() as u32) - 1 < session.threshold {
        return ActionOutcome::Skipped {
            reason: "removing a signer would drop the set below its own threshold".to_string(),
        };
    }

    match propose_when_settled(
        &mut session.clients[0],
        miden_multisig_client::TransactionType::remove_cosigner(departing),
    )
    .await
    {
        Ok(proposal) => {
            session.proposal_id = Some(proposal.id.clone());
            session.expected_signers =
                Some(before.into_iter().filter(|e| e != &departing_hex).collect());
            session.expected_threshold = Some(session.threshold);
            session.departed = Some(last);
            ActionOutcome::Passed
        }
        Err(error) => {
            ActionOutcome::failed_product(format!("proposing the removed signer failed: {error}"))
        }
    }
}

/// Proposes raising the threshold to require every current signer.
///
/// Expressed as an update to the whole signer set, because the on-chain
/// procedure takes the set and the threshold together. Both SDKs pass the
/// current set unchanged; changing membership here is refused.
pub async fn change_threshold(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };

    let Some(account) = session.clients[0].account() else {
        return ActionOutcome::failed_setup("the client holds no account to read");
    };
    let commitments = account.cosigner_commitments();
    let target = commitments.len() as u32;
    if target == session.threshold {
        return ActionOutcome::Skipped {
            reason: "the threshold already requires every signer".to_string(),
        };
    }
    let before = normalize_commitments(account.cosigner_commitments_hex());

    match propose_when_settled(
        &mut session.clients[0],
        miden_multisig_client::TransactionType::update_signers(target, commitments),
    )
    .await
    {
        Ok(proposal) => {
            session.proposal_id = Some(proposal.id.clone());
            session.expected_signers = Some(before);
            session.expected_threshold = Some(target);
            ActionOutcome::Passed
        }
        Err(error) => {
            ActionOutcome::failed_product(format!("proposing the threshold change failed: {error}"))
        }
    }
}

/// Asserts the executed membership change landed, on chain and in GUARDIAN's
/// view. Both are checked because a change visible in only one of them is the
/// divergence this suite exists to catch.
pub async fn assert_signer_set(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let (Some(expected_signers), Some(expected_threshold)) =
        (session.expected_signers.clone(), session.expected_threshold)
    else {
        return ActionOutcome::failed_setup("no membership change was proposed in this scenario");
    };
    let account_id = session.account_id;

    if let Err(error) = session.clients[0].sync().await {
        return ActionOutcome::failed_product(format!("syncing after the change failed: {error}"));
    }

    let Some(account) = session.clients[0].account() else {
        return ActionOutcome::failed_setup("the client holds no account to read");
    };
    let on_chain = normalize_commitments(account.cosigner_commitments_hex());
    if on_chain != expected_signers {
        return ActionOutcome::failed_product(format!(
            "the on-chain signer set is {} but {} was expected",
            on_chain.join(","),
            expected_signers.join(",")
        ));
    }
    match account.threshold() {
        Ok(threshold) if threshold == expected_threshold => {}
        Ok(threshold) => {
            return ActionOutcome::failed_product(format!(
                "the on-chain threshold is {threshold} but {expected_threshold} was expected"
            ));
        }
        Err(error) => {
            return ActionOutcome::failed_product(format!("reading the threshold failed: {error}"));
        }
    }

    // GUARDIAN's own view, reached through a client that did not execute the
    // change, so a stale local cache cannot answer for it.
    // A client that still holds the account and did not execute the change.
    // The obvious candidate, the last cosigner, is the one a removal just took
    // away, and a removed key cannot authenticate by design.
    let reader_index = (1..session.clients.len())
        .find(|index| {
            current_signer_set(&session.clients[*index]).is_some()
                && normalize_commitments(vec![session.clients[*index].user_commitment_hex()])
                    .first()
                    .is_some_and(|commitment| expected_signers.contains(commitment))
        })
        .unwrap_or(0);
    let reader = match session.incoming.as_mut() {
        Some(clients) => &mut clients[0],
        None => &mut session.clients[reader_index],
    };
    // Polled, not read once. A proposal leaving the pending set does not mean
    // GUARDIAN has finished applying it: its authorization list can still be
    // the pre-change one, and a newly admitted signer is refused until it
    // catches up.
    let started = std::time::Instant::now();
    let mut wait = std::time::Duration::from_secs(1);
    let mut last = String::from("never answered");
    while started.elapsed() < SETTLE_DEADLINE {
        match reader.pull_account(account_id).await {
            Ok(_) => match current_signer_set(reader) {
                Some(served) if served == expected_signers => return ActionOutcome::Passed,
                Some(served) => last = format!("serves {}", served.join(",")),
                None => last = "served no account state".to_string(),
            },
            Err(error) => last = error.to_string(),
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(std::time::Duration::from_secs(5));
    }

    ActionOutcome::failed_product(format!(
        "GUARDIAN {last} but {} was expected after {}s",
        expected_signers.join(","),
        SETTLE_DEADLINE.as_secs()
    ))
}

/// How long GUARDIAN may take to finish applying an executed change before the
/// suite treats the disagreement as real.
const SETTLE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(180);

/// Proposes, waiting out the window where GUARDIAN still reports the previous
/// change as pending. Creating a proposal is a GUARDIAN call, not a chain
/// submission, so retrying it risks nothing.
async fn propose_when_settled(
    client: &mut MultisigClient,
    transaction: miden_multisig_client::TransactionType,
) -> Result<miden_multisig_client::Proposal, String> {
    let started = std::time::Instant::now();
    let mut wait = std::time::Duration::from_secs(1);
    loop {
        match client.propose_transaction(transaction.clone()).await {
            Ok(proposal) => return Ok(proposal),
            Err(error) => {
                let message = error.to_string();
                if !message.contains("already a pending change")
                    || started.elapsed() >= SETTLE_DEADLINE
                {
                    return Err(message);
                }
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(std::time::Duration::from_secs(5));
            }
        }
    }
}

/// What the account sends back to the treasury, well under what it holds.
const P2ID_AMOUNT: u64 = 1_000;

/// The procedure whose threshold override the suite exercises.
const OVERRIDE_PROCEDURE: miden_multisig_client::ProcedureName =
    miden_multisig_client::ProcedureName::SendAsset;

fn held_balance(client: &MultisigClient, faucet: AccountId) -> Option<u64> {
    client
        .account()
        .map(|account| vault_amount(account.inner(), faucet))
}

/// Proposes sending assets out of the multisig, back to the treasury that
/// funded it. A real counterparty rather than the account itself, so the note
/// has to leave the vault to somewhere that can actually claim it.
pub async fn send_asset(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let (Some(faucet), Some(treasury)) = (session.faucet, session.treasury) else {
        return ActionOutcome::failed_setup(
            "the account was never funded, so it holds nothing to send",
        );
    };

    if let Err(error) = session.clients[0].sync().await {
        return ActionOutcome::failed_product(format!(
            "syncing before the transfer failed: {error}"
        ));
    }
    let Some(before) = held_balance(&session.clients[0], faucet) else {
        return ActionOutcome::failed_setup("the client holds no account to read");
    };
    if before <= P2ID_AMOUNT {
        return ActionOutcome::failed_setup(format!(
            "the account holds {before}, which is not enough to send {P2ID_AMOUNT} and pay the fee"
        ));
    }

    match propose_when_settled(
        &mut session.clients[0],
        miden_multisig_client::TransactionType::transfer_with_note_type(
            treasury,
            faucet,
            P2ID_AMOUNT,
            miden_protocol::note::NoteType::Public,
        ),
    )
    .await
    {
        Ok(proposal) => {
            session.proposal_id = Some(proposal.id.clone());
            session.balance_before_send = Some(before);
            session.sent_amount = P2ID_AMOUNT;
            ActionOutcome::Passed
        }
        Err(error) => {
            ActionOutcome::failed_product(format!("proposing the transfer failed: {error}"))
        }
    }
}

/// Asserts the sent assets left the vault.
///
/// Bounded rather than exact: the transaction pays its fee out of the same
/// asset, so the account gives up at least what it sent.
pub async fn assert_asset_sent(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let (Some(faucet), Some(before)) = (session.faucet, session.balance_before_send) else {
        return ActionOutcome::failed_setup("no transfer was proposed in this scenario");
    };
    let sent = session.sent_amount;

    if let Err(error) = session.clients[0].sync().await {
        return ActionOutcome::failed_product(format!(
            "syncing after the transfer failed: {error}"
        ));
    }
    let Some(after) = held_balance(&session.clients[0], faucet) else {
        return ActionOutcome::failed_setup("the client holds no account to read");
    };

    let floor = before.saturating_sub(sent);
    if after > floor {
        return ActionOutcome::failed_product(format!(
            "the account holds {after} after sending {sent}, but no more than {floor} was expected"
        ));
    }
    ActionOutcome::Passed
}

/// Proposes requiring every signer for one procedure, leaving the account's own
/// threshold alone. The override is what makes per-procedure policy testable:
/// the account stays 2-of-3 while that one procedure needs 3.
pub async fn set_procedure_threshold(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };

    let Some(account) = session.clients[0].account() else {
        return ActionOutcome::failed_setup("the client holds no account to read");
    };
    let target = account.cosigner_commitments().len() as u32;
    if target == session.threshold {
        return ActionOutcome::Skipped {
            reason: "an override equal to the account threshold proves nothing".to_string(),
        };
    }

    match propose_when_settled(
        &mut session.clients[0],
        miden_multisig_client::TransactionType::UpdateProcedureThreshold {
            procedure: OVERRIDE_PROCEDURE,
            new_threshold: target,
        },
    )
    .await
    {
        Ok(proposal) => {
            session.proposal_id = Some(proposal.id.clone());
            session.expected_procedure = Some(OVERRIDE_PROCEDURE);
            session.expected_procedure_threshold = Some(target);
            ActionOutcome::Passed
        }
        Err(error) => ActionOutcome::failed_product(format!(
            "proposing the procedure threshold override failed: {error}"
        )),
    }
}

/// Asserts the override landed and the account's own threshold did not move.
pub async fn assert_procedure_threshold(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let (Some(procedure), Some(expected)) = (
        session.expected_procedure,
        session.expected_procedure_threshold,
    ) else {
        return ActionOutcome::failed_setup("no override was proposed in this scenario");
    };
    // The account threshold this assertion expects, not the one the account
    // started with: a scenario may change it after setting the override, and
    // that the override outlives such a change is the point of doing both.
    let account_threshold = session.expected_threshold.unwrap_or(session.threshold);

    if let Err(error) = session.clients[0].sync().await {
        return ActionOutcome::failed_product(format!(
            "syncing after the override failed: {error}"
        ));
    }
    let Some(account) = session.clients[0].account() else {
        return ActionOutcome::failed_setup("the client holds no account to read");
    };

    match account.procedure_threshold(procedure) {
        Ok(Some(actual)) if actual == expected => {}
        Ok(actual) => {
            return ActionOutcome::failed_product(format!(
                "the override for {procedure:?} is {} but {expected} was expected",
                actual
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "absent".to_string())
            ));
        }
        Err(error) => {
            return ActionOutcome::failed_product(format!("reading the override failed: {error}"));
        }
    }

    match account.threshold() {
        Ok(threshold) if threshold == account_threshold => ActionOutcome::Passed,
        Ok(threshold) => ActionOutcome::failed_product(format!(
            "the account threshold moved to {threshold}; an override must not change it"
        )),
        Err(error) => {
            ActionOutcome::failed_product(format!("reading the account threshold failed: {error}"))
        }
    }
}

/// Asserts the removed signer is actually evicted, not merely delisted.
///
/// Serving a stale signer set and still honouring the removed key are different
/// failures: the first misleads a reader, the second means the removal did not
/// take effect and a compromised cosigner cannot be evicted. This separates them
/// by having the removed key attempt an authenticated call of its own.
pub async fn assert_removed_signer_refused(runner: &Runner) -> ActionOutcome {
    let mut guard = runner.session.lock().await;
    let Some(session) = guard.as_mut() else {
        return ActionOutcome::failed_setup("no account has been created in this scenario");
    };
    let Some(departed) = session.departed else {
        return ActionOutcome::failed_setup("no signer was removed in this scenario");
    };
    let account_id = session.account_id;
    let commitment = session.clients[departed].user_commitment_hex();

    match session.clients[departed].pull_account(account_id).await {
        Ok(_) => ActionOutcome::failed_product(format!(
            "GUARDIAN still serves {account_id} to the removed signer {commitment}"
        )),
        Err(error) => {
            let message = error.to_string().to_lowercase();
            // Refused is the point. Anything other than an authorization
            // refusal is not evidence of eviction, so it is reported rather
            // than counted.
            if message.contains("not an authorized signer")
                || message.contains("unauthenticated")
                || message.contains("authenticate")
                || message.contains("unauthorized")
            {
                ActionOutcome::Passed
            } else {
                ActionOutcome::failed_product(format!(
                    "the removed signer was refused, but not as unauthorized: {error}"
                ))
            }
        }
    }
}
