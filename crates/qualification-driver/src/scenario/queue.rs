//! The candidate queue (issue #17), qualified on the stack's queue server: a
//! GUARDIAN with the same acknowledgement identity as the main one and
//! `GUARDIAN_MAX_PENDING_CANDIDATES_PER_ACCOUNT=2`, holding the fixture account
//! in a database of its own.
//!
//! The deterministic chain is a stub, so nothing queued here ever
//! canonicalizes: candidates stay queued for the whole run, which is what lets
//! the admission rules be observed one at a time. It is also why these actions
//! read the queue before they write to it. Every Rust scenario runs again after
//! the main server restarts, and an upgrade run starts this server on a fresh
//! database, so an action finds either nothing queued yet or exactly what an
//! earlier pass left, and both have to come out the same.

use std::sync::Arc;

use guardian_client::delta_status::Status;
use guardian_client::{AuthConfig, GuardianClient, MidenFalconRpoAuth, auth_config::AuthType};
use miden_protocol::account::AccountId;

use super::{ActionOutcome, Runner};
use crate::fixtures::{ChainedDelta, Fixtures};

/// The depth the stack runs the queue server at in this profile. Three chained
/// fixture deltas exist, so two is the deepest queue this profile can fill and
/// still have a correctly chained delta left over to refuse.
const DEPTH: usize = 2;

const CONFLICT: &str = "conflict_pending_delta";

struct QueueServer {
    client: GuardianClient,
    id: AccountId,
}

/// Connects to the queue server with the fixture signer and makes sure the
/// fixture account is registered there.
async fn queue_server(fixtures: &Fixtures) -> Result<QueueServer, ActionOutcome> {
    let Ok(endpoint) = std::env::var("QUAL_GUARDIAN_QUEUE_GRPC") else {
        return Err(ActionOutcome::EnvironmentBlocked {
            reason: "QUAL_GUARDIAN_QUEUE_GRPC is unset, so no queue-enabled GUARDIAN is running \
                     to exercise the candidate queue against"
                .to_string(),
        });
    };
    let depth = std::env::var("QUAL_QUEUE_DEPTH")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(DEPTH);
    if depth != DEPTH {
        return Err(ActionOutcome::failed_setup(format!(
            "the queue server runs at depth {depth}, but this profile's three fixture deltas \
             can only fill and overflow a queue of {DEPTH}"
        )));
    }

    let id = AccountId::from_hex(&fixtures.account_id).map_err(|error| {
        ActionOutcome::failed_setup(format!("the fixture account id is malformed: {error}"))
    })?;
    let signer = fixtures.signer().map_err(|error| {
        ActionOutcome::failed_setup(format!("cannot build the fixture signer: {error}"))
    })?;
    let mut client = GuardianClient::connect(endpoint.clone())
        .await
        .map_err(|error| {
            ActionOutcome::failed_setup(format!(
                "cannot reach the queue server at {endpoint}: {error}"
            ))
        })?
        .with_signer(Arc::new(signer));

    let auth = AuthConfig {
        auth_type: Some(AuthType::MidenFalconRpo(MidenFalconRpoAuth {
            cosigner_commitments: fixtures.cosigner_commitments.clone(),
        })),
    };
    match client.configure(&id, auth, &fixtures.account).await {
        Ok(_) => {}
        Err(error) if error.guardian_code().as_deref() == Some("account_already_configured") => {}
        Err(error) => {
            return Err(ActionOutcome::failed_product(format!(
                "registering the fixture account on the queue server failed: {error}"
            )));
        }
    }
    Ok(QueueServer { client, id })
}

/// What GUARDIAN holds at `nonce`: `None` when nothing is stored there.
async fn status_at(server: &mut QueueServer, nonce: u64) -> Result<Option<Status>, ActionOutcome> {
    match server.client.get_delta(&server.id, nonce).await {
        Ok(response) => Ok(response
            .delta
            .and_then(|delta| delta.status)
            .and_then(|status| status.status)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(ActionOutcome::failed_product(format!(
            "reading the delta at nonce {nonce} failed: {error}"
        ))),
    }
}

/// Makes sure `delta` is queued, pushing it when nothing is stored at its nonce
/// yet. Anything other than a queued candidate there means the server lost or
/// resolved a candidate the stub chain can never confirm.
async fn ensure_queued(
    server: &mut QueueServer,
    delta: &ChainedDelta,
) -> Result<(), ActionOutcome> {
    match status_at(server, delta.nonce).await? {
        Some(Status::CandidateAt(_)) => return Ok(()),
        None => {}
        Some(other) => {
            return Err(ActionOutcome::failed_product(format!(
                "nonce {} holds {other:?} rather than a queued candidate; the stub chain confirms \
                 nothing, so it can only have left the queue through a defect",
                delta.nonce
            )));
        }
    }
    match server
        .client
        .push_delta(
            &server.id,
            delta.nonce,
            delta.prev_commitment.clone(),
            &delta.payload,
        )
        .await
    {
        Ok(response) => {
            let queued = response.delta.as_ref().is_some_and(|stored| {
                matches!(
                    stored
                        .status
                        .as_ref()
                        .and_then(|status| status.status.as_ref()),
                    Some(Status::CandidateAt(_))
                ) && same_commitment(&stored.new_commitment, &delta.post_commitment)
            });
            if queued {
                Ok(())
            } else {
                Err(ActionOutcome::failed_product(format!(
                    "the delta at nonce {} was accepted but did not come back as a candidate \
                     carrying {}: {:?}",
                    delta.nonce, delta.post_commitment, response.delta
                )))
            }
        }
        Err(error) => Err(ActionOutcome::failed_product(format!(
            "pushing the delta at nonce {} on the queue tail was refused: {error}",
            delta.nonce
        ))),
    }
}

fn same_commitment(left: &str, right: &str) -> bool {
    let canonical = |value: &str| value.trim().trim_start_matches("0x").to_ascii_lowercase();
    !canonical(left).is_empty() && canonical(left) == canonical(right)
}

/// Requires a refusal with `conflict_pending_delta`, naming what was refused.
fn refused_as_conflict(
    what: &str,
    outcome: Result<(), guardian_client::ClientError>,
) -> Result<(), ActionOutcome> {
    match outcome {
        Ok(()) => Err(ActionOutcome::failed_product(format!(
            "{what} was accepted"
        ))),
        Err(error) => match error.guardian_code() {
            Some(code) if code == CONFLICT => Ok(()),
            Some(code) => Err(ActionOutcome::failed_product(format!(
                "{what} was refused with `{code}` rather than `{CONFLICT}`: {error}"
            ))),
            None => Err(ActionOutcome::failed_product(format!(
                "{what} failed without a GUARDIAN error code: {error}"
            ))),
        },
    }
}

/// A cosigner that synced the canonical state proposes on it while another
/// device's candidate is queued, and is refused at once.
///
/// `/state` serves only the canonical state, so a second device builds on it
/// and its SDK labels the proposal with the canonical nonce plus one: the slot
/// the queued candidate already holds. Its delta could never be admitted, so
/// the proposal is refused before anyone signs it, as it was before the queue
/// existed, rather than stored for cosigners to sign something doomed.
///
/// The queue has room when this runs (one candidate at depth two), so the
/// refusal is the nonce rule and not a full queue, and a proposal at the next
/// nonce is accepted beside it, pinned to the queue tail: the refusal is about
/// the nonce, not about proposing at all.
pub async fn assert_cosigner_proposal_refused(runner: &Runner) -> ActionOutcome {
    let Some(fixtures) = runner.fixtures.as_ref() else {
        return ActionOutcome::failed_setup("the server fixtures were not loaded");
    };
    let mut server = match queue_server(fixtures).await {
        Ok(server) => server,
        Err(outcome) => return outcome,
    };
    let [first, second, _] = &fixtures.chained[..] else {
        return ActionOutcome::failed_setup("the fixtures carry no three-delta chain");
    };

    if let Err(outcome) = ensure_queued(&mut server, first).await {
        return outcome;
    }
    // A later pass finds the queue already full. The nonce rule is still
    // checked, but cannot be told apart from the full queue there, so the
    // accepted proposal beside it is only expected while there is room.
    let room = match status_at(&mut server, second.nonce).await {
        Ok(status) => status.is_none(),
        Err(outcome) => return outcome,
    };

    // The cosigner's own transaction, built on the canonical state: it carries
    // the summary of the first delta and the nonce that state implies.
    let (on_canonical, extending) = match (
        Fixtures::proposal_payload_for(&first.payload),
        Fixtures::proposal_payload_for(&second.payload),
    ) {
        (Ok(on_canonical), Ok(extending)) => (on_canonical, extending),
        (Err(error), _) | (_, Err(error)) => {
            return ActionOutcome::failed_setup(format!(
                "building the fixture proposal payloads: {error}"
            ));
        }
    };
    let refused = server
        .client
        .push_delta_proposal(&server.id, first.nonce, &on_canonical)
        .await
        .map(|_| ());
    if let Err(outcome) = refused_as_conflict(
        &format!(
            "a proposal at the queued candidate's nonce {} (the canonical nonce plus one)",
            first.nonce
        ),
        refused,
    ) {
        return outcome;
    }
    if !room {
        return ActionOutcome::Passed;
    }

    // Idempotent on a later pass: GUARDIAN keeps the first copy of a proposal.
    if let Err(error) = server
        .client
        .push_delta_proposal(&server.id, second.nonce, &extending)
        .await
    {
        return ActionOutcome::failed_product(format!(
            "a proposal at nonce {}, which extends the queue, was refused: {error}",
            second.nonce
        ));
    }
    match server.client.get_delta_proposals(&server.id).await {
        Ok(response) => {
            let pinned = response.proposals.iter().any(|proposal| {
                proposal.nonce == second.nonce
                    && same_commitment(&proposal.prev_commitment, &first.post_commitment)
            });
            if pinned {
                ActionOutcome::Passed
            } else {
                ActionOutcome::failed_product(format!(
                    "the proposal at nonce {} is not pinned to the queue tail {}: {:?}",
                    second.nonce,
                    first.post_commitment,
                    response
                        .proposals
                        .iter()
                        .map(|proposal| (proposal.nonce, proposal.prev_commitment.clone()))
                        .collect::<Vec<_>>()
                ))
            }
        }
        Err(error) => ActionOutcome::failed_product(format!("listing proposals failed: {error}")),
    }
}

/// The queue fills to its depth and then refuses every submission.
///
/// The second delta is admitted chained on the first, before either
/// canonicalized. With the queue full, the correctly chained third delta is
/// refused, and so is a delta on a state the server does not know: a full
/// queue answers every base with `conflict_pending_delta`, which is exactly the
/// one-in-flight gate a depth of one has always been, so a deployment that did
/// not opt in sees no change. Nothing moved the canonical state.
pub async fn assert_depth_limit(runner: &Runner) -> ActionOutcome {
    let Some(fixtures) = runner.fixtures.as_ref() else {
        return ActionOutcome::failed_setup("the server fixtures were not loaded");
    };
    let mut server = match queue_server(fixtures).await {
        Ok(server) => server,
        Err(outcome) => return outcome,
    };
    let [first, second, third] = &fixtures.chained[..] else {
        return ActionOutcome::failed_setup("the fixtures carry no three-delta chain");
    };

    for delta in [first, second] {
        if let Err(outcome) = ensure_queued(&mut server, delta).await {
            return outcome;
        }
    }

    let unknown_base = format!("0x{}", "ee".repeat(32));
    for (prev, what) in [
        (
            second.post_commitment.clone(),
            format!("a third delta chained on the tail of a queue of depth {DEPTH}, already full"),
        ),
        (
            unknown_base,
            "a delta on a base the server does not know, while the queue is full".to_string(),
        ),
    ] {
        let pushed = server
            .client
            .push_delta(&server.id, third.nonce, prev, &third.payload)
            .await
            .map(|_| ());
        if let Err(outcome) = refused_as_conflict(&what, pushed) {
            return outcome;
        }
    }
    if let Ok(Some(status)) = status_at(&mut server, third.nonce).await {
        return ActionOutcome::failed_product(format!(
            "nonce {} holds {status:?} after every push there was refused",
            third.nonce
        ));
    }

    match server.client.get_state(&server.id).await {
        Ok(response) => match response.state {
            Some(state) if same_commitment(&state.commitment, &fixtures.initial_commitment) => {
                ActionOutcome::Passed
            }
            Some(state) => ActionOutcome::failed_product(format!(
                "the canonical state moved to {} although the stub chain confirmed nothing",
                state.commitment
            )),
            None => ActionOutcome::failed_product(
                "GUARDIAN returned no state for the fixture account".to_string(),
            ),
        },
        Err(error) => {
            ActionOutcome::failed_product(format!("reading the canonical state failed: {error}"))
        }
    }
}
