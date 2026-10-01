use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use crate::coordination::{ExecutionLeases, InMemoryExecutionLeases};
use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::metadata::MetadataStore;
use crate::state_object::StateObject;
use crate::storage::{
    AdmissionWrite, CandidateAdmission, CandidatePromotion, CandidateSubmission, CanonicalWrite,
    ClaimWrite, DeltaStatusKind, ExecutionFailure, ExecutionFailureCode, ExecutionPhase,
    ExecutionResolution, ExecutionTerminal, LeaseFence, NewExecutionReservation, PromotableKind,
    PromoteWrite, ProposalAdmission, ProposalWrite, ReservationUpdate, ReservationWrite,
    ResolveWrite, StorageBackend, SubmissionEvidence,
};

const BASE: &str = "0xbase";
const NEXT: &str = "0xnext";

struct Harness {
    storage: Arc<dyn StorageBackend>,
    metadata: Arc<dyn MetadataStore>,
    leases: Arc<dyn ExecutionLeases>,
    canonicalization_fence: Option<LeaseFence>,
    account_id: String,
    _dir: Option<tempfile::TempDir>,
}

impl Harness {
    async fn filesystem() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = crate::storage::filesystem::FilesystemService::new(dir.path().join("store"))
            .await
            .expect("filesystem storage");
        let metadata =
            crate::metadata::filesystem::FilesystemMetadataStore::new(dir.path().join("metadata"))
                .await
                .expect("filesystem metadata");
        let harness = Self {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata),
            leases: Arc::new(InMemoryExecutionLeases::new()),
            canonicalization_fence: None,
            account_id: "0xexecution".to_string(),
            _dir: Some(dir),
        };
        harness.seed().await;
        harness
    }

    async fn seed(&self) {
        self.metadata
            .set(crate::metadata::AccountMetadata {
                account_id: self.account_id.clone(),
                auth: crate::metadata::auth::Auth::MidenFalconRpo {
                    cosigner_commitments: vec![],
                },
                network_config: crate::metadata::NetworkConfig::miden_default(),
                created_at: "2026-09-30T12:00:00Z".to_string(),
                updated_at: "2026-09-30T12:00:00Z".to_string(),
                has_pending_candidate: false,
                paused_at: None,
                paused_reason: None,
                released_at: None,
            })
            .await
            .expect("metadata seed");
        self.storage
            .submit_state(&StateObject {
                account_id: self.account_id.clone(),
                commitment: BASE.to_string(),
                state_json: serde_json::json!({ "state": "base" }),
                created_at: "2026-09-30T12:00:00Z".to_string(),
                updated_at: "2026-09-30T12:00:00Z".to_string(),
                auth_scheme: String::new(),
            })
            .await
            .expect("state seed");
    }

    async fn lease(&self, holder: &str, ttl: Duration) -> LeaseFence {
        let lease = self
            .leases
            .elector(&self.account_id, holder)
            .try_acquire(ttl)
            .await
            .expect("lease acquire")
            .expect("lease is free");
        LeaseFence {
            lease_name: lease.name,
            holder_id: lease.holder_id,
            fence_token: lease.fence_token,
        }
    }

    fn new_reservation(&self, proposal_id: &str, fence: &LeaseFence) -> NewExecutionReservation {
        NewExecutionReservation {
            account_id: self.account_id.clone(),
            proposal_id: proposal_id.to_string(),
            fence: fence.clone(),
            lease_expires_at: Utc::now() + chrono::Duration::minutes(5),
            ignored_signatures: 0,
            now: Utc::now(),
        }
    }

    async fn reserve(&self, proposal_id: &str, fence: &LeaseFence) -> u32 {
        match self
            .storage
            .create_execution_reservation(self.new_reservation(proposal_id, fence))
            .await
            .expect("create reservation")
        {
            ReservationWrite::Created { attempt } => attempt,
            other => panic!("expected a new reservation, got {other:?}"),
        }
    }

    fn candidate(&self, nonce: u64) -> DeltaObject {
        DeltaObject {
            account_id: self.account_id.clone(),
            nonce,
            prev_commitment: BASE.to_string(),
            new_commitment: Some(NEXT.to_string()),
            delta_payload: serde_json::json!({ "delta": nonce }),
            ack_sig: "0xack".to_string(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::candidate(Utc::now().to_rfc3339()),
            metadata: None,
        }
    }

    fn admission(
        &self,
        proposal_id: &str,
        attempt: u32,
        fence: &LeaseFence,
        nonce: u64,
    ) -> CandidateAdmission {
        let now = Utc::now();
        CandidateAdmission {
            fence: fence.clone(),
            delta: self.candidate(nonce),
            evidence: SubmissionEvidence {
                account_id: self.account_id.clone(),
                proposal_id: proposal_id.to_string(),
                attempt,
                candidate_nonce: nonce,
                transaction_id: "0xtx".to_string(),
                expected_commitment: NEXT.to_string(),
                reference_block: 1_000,
                expiration_block: 1_256,
                base_commitment: BASE.to_string(),
                committed_at: now,
            },
            now,
        }
    }

    fn resolution(
        &self,
        proposal_id: &str,
        attempt: u32,
        fence: &LeaseFence,
        code: ExecutionFailureCode,
    ) -> ExecutionResolution {
        ExecutionResolution {
            account_id: self.account_id.clone(),
            proposal_id: proposal_id.to_string(),
            attempt,
            fence: fence.clone(),
            failure: ExecutionFailure {
                code,
                message: "failed".to_string(),
            },
            now: Utc::now(),
        }
    }

    async fn store_proposal(&self, proposal_id: &str, nonce: u64) {
        let mut proposal = self.candidate(nonce);
        proposal.status = DeltaStatus::Pending {
            timestamp: Utc::now().to_rfc3339(),
            proposer_id: "0xproposer".to_string(),
            cosigner_sigs: vec![],
        };
        self.storage
            .submit_delta_proposal(proposal_id, &proposal)
            .await
            .expect("store proposal");
    }

    async fn pending_flag(&self) -> bool {
        self.metadata
            .get(&self.account_id)
            .await
            .expect("metadata read")
            .expect("metadata row")
            .has_pending_candidate
    }

    async fn delta_at(&self, nonce: u64) -> Option<DeltaObject> {
        self.storage.pull_delta(&self.account_id, nonce).await.ok()
    }
}

fn proposal_commitment(tag: u8) -> String {
    format!("0x{}", hex::encode([tag; 32]))
}

async fn reservation_is_created_only_on_a_clean_account(h: &Harness) {
    let proposal = proposal_commitment(1);
    let fence = h.lease("worker-a", Duration::from_secs(60)).await;
    assert_eq!(h.reserve(&proposal, &fence).await, 1);

    let active = h
        .storage
        .load_active_execution(&h.account_id)
        .await
        .unwrap()
        .expect("active reservation");
    assert_eq!(active.reservation.proposal_id, proposal);
    assert_eq!(active.reservation.phase, ExecutionPhase::Accepted);
    assert_eq!(active.reservation.fence, fence);
    assert!(active.evidence.is_none() && active.outcome.is_none());

    let other = h.new_reservation(&proposal_commitment(2), &fence);
    assert_eq!(
        h.storage.create_execution_reservation(other).await.unwrap(),
        ReservationWrite::AlreadyReserved {
            holder_id: fence.holder_id.clone(),
            proposal_id: proposal.clone(),
        }
    );
}

async fn reservation_is_refused_while_a_client_candidate_exists(h: &Harness) {
    let now = Utc::now().to_rfc3339();
    assert_eq!(
        h.storage
            .submit_candidate(h.metadata.as_ref(), &h.candidate(1), &now)
            .await
            .unwrap(),
        CandidateSubmission::Submitted
    );
    let fence = h.lease("worker-a", Duration::from_secs(60)).await;
    assert_eq!(
        h.storage
            .create_execution_reservation(h.new_reservation(&proposal_commitment(1), &fence))
            .await
            .unwrap(),
        ReservationWrite::CandidateExists
    );
    assert!(
        h.storage
            .load_active_execution(&h.account_id)
            .await
            .unwrap()
            .is_none()
    );
}

async fn only_the_owner_crosses_the_boundary(h: &Harness) {
    let proposal = proposal_commitment(1);
    let owner = h.lease("worker-a", Duration::from_secs(60)).await;
    let attempt = h.reserve(&proposal, &owner).await;
    let intruder = LeaseFence {
        holder_id: "intruder".to_string(),
        ..owner.clone()
    };

    assert_eq!(
        h.storage
            .admit_execution_candidate(
                h.metadata.as_ref(),
                h.admission(&proposal, attempt, &intruder, 1)
            )
            .await
            .unwrap(),
        AdmissionWrite::NotAuthorized
    );
    assert!(h.delta_at(1).await.is_none());
    let record = h
        .storage
        .load_active_execution(&h.account_id)
        .await
        .unwrap()
        .unwrap();
    assert!(record.evidence.is_none());

    assert_eq!(
        h.storage
            .admit_execution_candidate(
                h.metadata.as_ref(),
                h.admission(&proposal, attempt, &owner, 1)
            )
            .await
            .unwrap(),
        AdmissionWrite::Admitted
    );
    let record = h
        .storage
        .load_active_execution(&h.account_id)
        .await
        .unwrap()
        .unwrap();
    let evidence = record
        .evidence
        .expect("evidence committed with the candidate");
    assert_eq!(evidence.candidate_nonce, 1);
    assert_eq!(evidence.expiration_block, 1_256);
    assert_eq!(record.reservation.candidate_nonce, Some(1));
    assert_eq!(
        record.reservation.phase,
        ExecutionPhase::SubmissionCommitted
    );
    assert!(
        h.delta_at(1)
            .await
            .expect("candidate")
            .status
            .is_candidate()
    );
    assert!(h.pending_flag().await);

    assert_eq!(
        h.storage
            .admit_execution_candidate(
                h.metadata.as_ref(),
                h.admission(&proposal, attempt, &owner, 1)
            )
            .await
            .unwrap(),
        AdmissionWrite::NotAuthorized,
        "an attempt crosses the boundary once"
    );
}

async fn admission_refuses_a_moved_base(h: &Harness) {
    let proposal = proposal_commitment(1);
    let owner = h.lease("worker-a", Duration::from_secs(60)).await;
    let attempt = h.reserve(&proposal, &owner).await;
    let mut admission = h.admission(&proposal, attempt, &owner, 1);
    admission.delta.prev_commitment = "0xelsewhere".to_string();
    assert_eq!(
        h.storage
            .admit_execution_candidate(h.metadata.as_ref(), admission)
            .await
            .unwrap(),
        AdmissionWrite::StaleBase
    );
    assert!(h.delta_at(1).await.is_none());
}

async fn a_client_candidate_is_refused_while_reserved(h: &Harness) {
    let proposal = proposal_commitment(1);
    let owner = h.lease("worker-a", Duration::from_secs(60)).await;
    h.reserve(&proposal, &owner).await;
    let now = Utc::now().to_rfc3339();
    assert_eq!(
        h.storage
            .submit_candidate(h.metadata.as_ref(), &h.candidate(1), &now)
            .await
            .unwrap(),
        CandidateSubmission::ExecutionReserved {
            proposal_id: proposal
        }
    );
    assert!(h.delta_at(1).await.is_none());
}

async fn a_client_candidate_is_refused_while_guardian_holds_its_own(h: &Harness) {
    let proposal = proposal_commitment(1);
    let owner = h.lease("worker-a", Duration::from_secs(60)).await;
    let attempt = h.reserve(&proposal, &owner).await;
    h.storage
        .admit_execution_candidate(
            h.metadata.as_ref(),
            h.admission(&proposal, attempt, &owner, 1),
        )
        .await
        .unwrap();
    let now = Utc::now().to_rfc3339();
    assert_eq!(
        h.storage
            .submit_candidate(h.metadata.as_ref(), &h.candidate(2), &now)
            .await
            .unwrap(),
        CandidateSubmission::ExecutionReserved {
            proposal_id: proposal
        },
        "both backends name the execution, not a generic pending conflict"
    );
}

async fn a_pre_boundary_failure_releases_with_its_outcome(h: &Harness) {
    let proposal = proposal_commitment(1);
    let owner = h.lease("worker-a", Duration::from_secs(60)).await;
    let attempt = h.reserve(&proposal, &owner).await;
    let code = ExecutionFailureCode::ExpirationReached(
        crate::storage::execution::ExpirationBound::Approval,
    );

    assert_eq!(
        h.storage
            .resolve_execution(
                h.metadata.as_ref(),
                h.resolution(&proposal, attempt, &owner, code)
            )
            .await
            .unwrap(),
        ResolveWrite::WrongSideOfBoundary,
        "post-boundary resolution cannot fail a pre-boundary attempt"
    );
    assert_eq!(
        h.storage
            .fail_execution(h.resolution(&proposal, attempt, &owner, code))
            .await
            .unwrap(),
        ResolveWrite::Resolved
    );
    assert!(
        h.storage
            .load_active_execution(&h.account_id)
            .await
            .unwrap()
            .is_none()
    );
    let latest = h
        .storage
        .load_latest_execution(&h.account_id, &proposal)
        .await
        .unwrap()
        .expect("resolved attempt stays readable");
    assert!(latest.reservation.released_at.is_some());
    match latest.outcome.expect("outcome").terminal {
        ExecutionTerminal::Failed { failure } => assert_eq!(failure.code, code),
        ExecutionTerminal::Committed => panic!("expected a failed outcome"),
    }
    assert_eq!(
        h.storage
            .fail_execution(h.resolution(&proposal, attempt, &owner, code))
            .await
            .unwrap(),
        ResolveWrite::AlreadyResolved
    );

    let retry = h.lease("worker-a", Duration::from_secs(60)).await;
    assert_eq!(
        h.reserve(&proposal, &retry).await,
        2,
        "a retry is a new attempt"
    );
}

async fn a_post_boundary_failure_discards_the_candidate_and_the_proposal(h: &Harness) {
    let proposal = proposal_commitment(1);
    h.store_proposal(&proposal, 1).await;
    let owner = h.lease("worker-a", Duration::from_secs(60)).await;
    let attempt = h.reserve(&proposal, &owner).await;
    assert_eq!(
        h.storage
            .admit_execution_candidate(
                h.metadata.as_ref(),
                h.admission(&proposal, attempt, &owner, 1)
            )
            .await
            .unwrap(),
        AdmissionWrite::Admitted
    );
    assert_eq!(
        h.storage
            .fail_execution(h.resolution(
                &proposal,
                attempt,
                &owner,
                ExecutionFailureCode::SealingFailed
            ))
            .await
            .unwrap(),
        ResolveWrite::WrongSideOfBoundary,
        "a pre-boundary failure cannot be written after the boundary"
    );
    assert_eq!(
        h.storage
            .resolve_execution(
                h.metadata.as_ref(),
                h.resolution(
                    &proposal,
                    attempt,
                    &owner,
                    ExecutionFailureCode::SubmissionRejected
                )
            )
            .await
            .unwrap(),
        ResolveWrite::Resolved
    );
    assert!(h.delta_at(1).await.is_none(), "candidate discarded");
    assert!(
        h.storage
            .pull_delta_proposal(&h.account_id, &proposal)
            .await
            .is_err(),
        "proposal deleted in the same commit"
    );
    assert!(!h.pending_flag().await);
    let latest = h
        .storage
        .load_latest_execution(&h.account_id, &proposal)
        .await
        .unwrap()
        .unwrap();
    assert!(latest.reservation.released_at.is_some());
    assert!(latest.evidence.is_some());
}

async fn canonicalization_cannot_discard_an_executing_candidate(h: &Harness) {
    let proposal = proposal_commitment(1);
    let owner = h.lease("worker-a", Duration::from_secs(60)).await;
    let attempt = h.reserve(&proposal, &owner).await;
    h.storage
        .admit_execution_candidate(
            h.metadata.as_ref(),
            h.admission(&proposal, attempt, &owner, 1),
        )
        .await
        .unwrap();
    let now = Utc::now().to_rfc3339();
    assert_eq!(
        h.storage
            .discard_candidate(
                h.metadata.as_ref(),
                &h.account_id,
                1,
                DeltaStatusKind::Candidate,
                &now,
                h.canonicalization_fence.as_ref(),
            )
            .await
            .unwrap(),
        CanonicalWrite::ProtectedByExecution
    );
    for status in [
        DeltaStatus::retained(
            now.clone(),
            crate::delta_object::RetainReason::RetryExhausted,
        ),
        DeltaStatus::discarded_client_abandoned(now.clone()),
    ] {
        assert_eq!(
            h.storage
                .update_candidate_status(
                    &h.account_id,
                    1,
                    status,
                    h.canonicalization_fence.as_ref()
                )
                .await
                .unwrap(),
            CanonicalWrite::ProtectedByExecution,
            "canonicalization cannot retire an executing candidate"
        );
    }
    assert!(h.delta_at(1).await.unwrap().status.is_candidate());
    assert_eq!(
        h.storage
            .update_candidate_status(
                &h.account_id,
                1,
                DeltaStatus::candidate_with_retry(now, 1),
                h.canonicalization_fence.as_ref()
            )
            .await
            .unwrap(),
        CanonicalWrite::Applied,
        "retry bookkeeping on a candidate is not a state change"
    );
}

async fn promotion_commits_and_releases_the_execution(h: &Harness) {
    let proposal = proposal_commitment(1);
    let owner = h.lease("worker-a", Duration::from_secs(60)).await;
    let attempt = h.reserve(&proposal, &owner).await;
    h.storage
        .admit_execution_candidate(
            h.metadata.as_ref(),
            h.admission(&proposal, attempt, &owner, 1),
        )
        .await
        .unwrap();
    let now = Utc::now();
    let mut delta = h.candidate(1);
    delta.status = DeltaStatus::canonical(now.to_rfc3339());
    let promotion = CandidatePromotion {
        state: StateObject {
            account_id: h.account_id.clone(),
            commitment: NEXT.to_string(),
            state_json: serde_json::json!({ "state": "next" }),
            created_at: "2026-09-30T12:00:00Z".to_string(),
            updated_at: now.to_rfc3339(),
            auth_scheme: String::new(),
        },
        delta,
        new_auth: None,
        now: now.to_rfc3339(),
        fence: h.canonicalization_fence.clone(),
        source: PromotableKind::Candidate,
    };
    assert_eq!(
        h.storage
            .promote_candidate(h.metadata.as_ref(), promotion)
            .await
            .unwrap(),
        PromoteWrite::Applied
    );
    assert!(
        h.storage
            .load_active_execution(&h.account_id)
            .await
            .unwrap()
            .is_none()
    );
    let latest = h
        .storage
        .load_latest_execution(&h.account_id, &proposal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        latest.outcome.expect("outcome").terminal,
        ExecutionTerminal::Committed
    );
    assert_eq!(
        h.storage
            .resolve_execution(
                h.metadata.as_ref(),
                h.resolution(&proposal, attempt, &owner, ExecutionFailureCode::Expired)
            )
            .await
            .unwrap(),
        ResolveWrite::AlreadyResolved,
        "the loser of promotion versus resolution writes nothing"
    );
}

async fn ownership_transfers_by_compare_and_set(h: &Harness, short_ttl: Duration, wait: Duration) {
    let proposal = proposal_commitment(1);
    let original = h.lease("worker-a", short_ttl).await;
    let attempt = h.reserve(&proposal, &original).await;
    h.storage
        .admit_execution_candidate(
            h.metadata.as_ref(),
            h.admission(&proposal, attempt, &original, 1),
        )
        .await
        .unwrap();
    tokio::time::sleep(wait).await;
    let reconciler = h.lease("reconciler", Duration::from_secs(60)).await;
    assert!(reconciler.fence_token > original.fence_token);

    let stale_expectation = LeaseFence {
        holder_id: "someone-else".to_string(),
        ..original.clone()
    };
    assert_eq!(
        h.storage
            .claim_execution_reservation(
                &h.account_id,
                &stale_expectation,
                &reconciler,
                Utc::now() + chrono::Duration::minutes(5)
            )
            .await
            .unwrap(),
        ClaimWrite::ClaimSuperseded
    );
    assert_eq!(
        h.storage
            .claim_execution_reservation(
                &h.account_id,
                &original,
                &reconciler,
                Utc::now() + chrono::Duration::minutes(5)
            )
            .await
            .unwrap(),
        ClaimWrite::Claimed
    );
    let active = h
        .storage
        .load_active_execution(&h.account_id)
        .await
        .unwrap()
        .expect("never released during the handover");
    assert_eq!(active.reservation.fence, reconciler);
    assert!(active.evidence.is_some());

    let far_future = Utc::now() + chrono::Duration::minutes(5);
    assert_eq!(
        h.storage
            .renew_execution_reservation(&h.account_id, &original, far_future, ExecutionPhase::Sent)
            .await
            .unwrap(),
        ReservationUpdate::StaleLease
    );
    assert_ne!(
        h.storage
            .resolve_execution(
                h.metadata.as_ref(),
                h.resolution(&proposal, attempt, &original, ExecutionFailureCode::Expired)
            )
            .await
            .unwrap(),
        ResolveWrite::Resolved,
        "the superseded owner writes nothing"
    );
    assert_eq!(
        h.storage
            .claim_execution_reservation(&h.account_id, &original, &reconciler, far_future)
            .await
            .unwrap(),
        ClaimWrite::ClaimSuperseded,
        "a repeated claim with the old expectation cannot steal back"
    );
    assert_eq!(
        h.storage
            .resolve_execution(
                h.metadata.as_ref(),
                h.resolution(
                    &proposal,
                    attempt,
                    &reconciler,
                    ExecutionFailureCode::Expired
                )
            )
            .await
            .unwrap(),
        ResolveWrite::Resolved
    );
}

async fn active_executions_list_every_unreleased_attempt(h: &Harness) {
    let proposal = proposal_commitment(1);
    let owner = h.lease("worker-a", Duration::from_secs(60)).await;
    let attempt = h.reserve(&proposal, &owner).await;
    let listed = |records: Vec<crate::storage::ExecutionRecord>| -> Vec<bool> {
        records
            .into_iter()
            .filter(|record| record.reservation.account_id == h.account_id)
            .map(|record| record.boundary_crossed())
            .collect()
    };
    assert_eq!(
        listed(h.storage.list_active_executions().await.unwrap()),
        vec![false]
    );
    h.storage
        .admit_execution_candidate(
            h.metadata.as_ref(),
            h.admission(&proposal, attempt, &owner, 1),
        )
        .await
        .unwrap();
    assert_eq!(
        listed(h.storage.list_active_executions().await.unwrap()),
        vec![true]
    );
}

impl Harness {
    fn proposal_admission(&self, tag: u8, base: &str, request_bytes: u64) -> ProposalAdmission {
        let mut proposal = self.candidate(u64::from(tag));
        proposal.prev_commitment = base.to_string();
        proposal.status = DeltaStatus::Pending {
            timestamp: Utc::now().to_rfc3339(),
            proposer_id: "0xproposer".to_string(),
            cosigner_sigs: vec![],
        };
        ProposalAdmission {
            commitment: proposal_commitment(tag),
            proposal,
            request_bytes,
            max_viable_proposals: 3,
            max_account_request_bytes: 100,
        }
    }
}

async fn proposal_admission_counts_only_viable_request_bytes(h: &Harness) {
    let admit = |admission| h.storage.admit_delta_proposal(admission);
    assert_eq!(
        admit(h.proposal_admission(1, "0xsuperseded", 90))
            .await
            .unwrap(),
        ProposalWrite::Stored
    );
    assert_eq!(
        admit(h.proposal_admission(2, BASE, 60)).await.unwrap(),
        ProposalWrite::Stored,
        "a proposal on a superseded base holds no capacity"
    );
    assert_eq!(
        admit(h.proposal_admission(3, BASE, 50)).await.unwrap(),
        ProposalWrite::AccountRequestBytesLimit {
            limit: 100,
            used: 60
        }
    );
    assert_eq!(
        admit(h.proposal_admission(2, BASE, 60)).await.unwrap(),
        ProposalWrite::AlreadyStored
    );
    assert!(
        h.storage
            .pull_delta_proposal(&h.account_id, &proposal_commitment(3))
            .await
            .is_err(),
        "a refused proposal is not stored"
    );
}

async fn proposal_admission_enforces_the_viable_count(h: &Harness) {
    for tag in 1..=3 {
        assert_eq!(
            h.storage
                .admit_delta_proposal(h.proposal_admission(tag, BASE, 0))
                .await
                .unwrap(),
            ProposalWrite::Stored
        );
    }
    assert_eq!(
        h.storage
            .admit_delta_proposal(h.proposal_admission(4, BASE, 0))
            .await
            .unwrap(),
        ProposalWrite::PendingLimit { limit: 3 }
    );
}

async fn concurrent_admissions_for_the_last_slot_accept_exactly_one(h: &Harness) {
    for tag in 1..=2 {
        h.storage
            .admit_delta_proposal(h.proposal_admission(tag, BASE, 0))
            .await
            .unwrap();
    }
    let (first, second) = tokio::join!(
        h.storage
            .admit_delta_proposal(h.proposal_admission(3, BASE, 0)),
        h.storage
            .admit_delta_proposal(h.proposal_admission(4, BASE, 0)),
    );
    let outcomes = [first.unwrap(), second.unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == ProposalWrite::Stored)
            .count(),
        1,
        "{outcomes:?}"
    );
    assert!(outcomes.contains(&ProposalWrite::PendingLimit { limit: 3 }));
}

impl Harness {
    fn promotion_to_next(&self, nonce: u64) -> CandidatePromotion {
        let now = Utc::now();
        let mut delta = self.candidate(nonce);
        delta.status = DeltaStatus::canonical(now.to_rfc3339());
        CandidatePromotion {
            state: StateObject {
                account_id: self.account_id.clone(),
                commitment: NEXT.to_string(),
                state_json: serde_json::json!({ "state": "next" }),
                created_at: "2026-09-30T12:00:00Z".to_string(),
                updated_at: now.to_rfc3339(),
                auth_scheme: String::new(),
            },
            delta,
            new_auth: None,
            now: now.to_rfc3339(),
            fence: self.canonicalization_fence.clone(),
            source: PromotableKind::Candidate,
        }
    }
}

async fn promotion_and_resolution_racing_leave_one_terminal_outcome(h: &Harness) {
    let mut winners = Vec::new();
    for round in 0..8 {
        let account = h.sibling(&format!("race{round}")).await;
        winners.push(race_promotion_against_resolution(&account, round % 2 == 0).await);
    }
    eprintln!(
        "promotion won {} of {} rounds",
        winners.iter().filter(|won| **won).count(),
        winners.len()
    );
}

impl Harness {
    /// Another account on the same stores and lease registry.
    async fn sibling(&self, suffix: &str) -> Harness {
        let sibling = Harness {
            storage: self.storage.clone(),
            metadata: self.metadata.clone(),
            leases: self.leases.clone(),
            canonicalization_fence: self.canonicalization_fence.clone(),
            account_id: format!("{}{suffix}", self.account_id),
            _dir: None,
        };
        sibling.seed().await;
        sibling
    }
}

/// Returns whether promotion won. `promotion_first` picks which side is polled first, so both
/// orders are exercised.
async fn race_promotion_against_resolution(h: &Harness, promotion_first: bool) -> bool {
    let proposal = proposal_commitment(1);
    h.store_proposal(&proposal, 1).await;
    let owner = h.lease("worker-a", Duration::from_secs(60)).await;
    let attempt = h.reserve(&proposal, &owner).await;
    assert_eq!(
        h.storage
            .admit_execution_candidate(
                h.metadata.as_ref(),
                h.admission(&proposal, attempt, &owner, 1)
            )
            .await
            .unwrap(),
        AdmissionWrite::Admitted
    );

    let promote = h
        .storage
        .promote_candidate(h.metadata.as_ref(), h.promotion_to_next(1));
    let resolve = h.storage.resolve_execution(
        h.metadata.as_ref(),
        h.resolution(
            &proposal,
            attempt,
            &owner,
            ExecutionFailureCode::SubmissionRejected,
        ),
    );
    let (promoted, resolved) = if promotion_first {
        tokio::join!(promote, resolve)
    } else {
        let (resolved, promoted) = tokio::join!(resolve, promote);
        (promoted, resolved)
    };
    let (promoted, resolved) = (promoted.unwrap(), resolved.unwrap());
    let promotion_won = promoted == PromoteWrite::Applied;
    let resolution_won = resolved == ResolveWrite::Resolved;
    assert!(
        promotion_won != resolution_won,
        "exactly one side wins: promoted {promoted:?}, resolved {resolved:?}"
    );

    assert!(
        h.storage
            .load_active_execution(&h.account_id)
            .await
            .unwrap()
            .is_none(),
        "the reservation is released once"
    );
    let outcome = h
        .storage
        .load_latest_execution(&h.account_id, &proposal)
        .await
        .unwrap()
        .unwrap()
        .outcome
        .expect("one terminal outcome is written");
    let state = h.storage.pull_state(&h.account_id).await.unwrap();
    if promotion_won {
        assert_eq!(outcome.terminal, ExecutionTerminal::Committed);
        assert_eq!(state.commitment, NEXT);
        assert!(h.delta_at(1).await.unwrap().status.is_canonical());
    } else {
        assert!(matches!(outcome.terminal, ExecutionTerminal::Failed { .. }));
        assert_eq!(state.commitment, BASE, "the losing promotion wrote nothing");
        assert!(h.delta_at(1).await.is_none(), "the candidate was discarded");
    }
    promotion_won
}

async fn executions_on_two_accounts_proceed_independently(h: &Harness) {
    let other_harness = h.sibling("b").await;

    let proposal = proposal_commitment(1);
    let first = h.lease("worker-a", Duration::from_secs(60)).await;
    let second = other_harness
        .lease("worker-b", Duration::from_secs(60))
        .await;
    assert_ne!(
        first.lease_name, second.lease_name,
        "each account has its own execution lease"
    );
    h.reserve(&proposal, &first).await;
    other_harness.reserve(&proposal, &second).await;
    for harness in [h, &other_harness] {
        assert!(
            harness
                .storage
                .load_active_execution(&harness.account_id)
                .await
                .unwrap()
                .is_some(),
            "{} executes while the other account does",
            harness.account_id
        );
    }

    let contender = h
        .leases
        .elector(&h.account_id, "worker-c")
        .try_acquire(Duration::from_secs(60))
        .await
        .unwrap();
    assert!(
        contender.is_none(),
        "a second execution on the same account waits for the lease"
    );
}

#[derive(Debug, Clone, Copy)]
enum Terminal {
    PreBoundaryFailure,
    PostBoundaryResolution,
    Promotion,
}

fn promotion(h: &Harness) -> CandidatePromotion {
    let now = Utc::now();
    let mut delta = h.candidate(1);
    delta.status = DeltaStatus::canonical(now.to_rfc3339());
    CandidatePromotion {
        state: StateObject {
            account_id: h.account_id.clone(),
            commitment: NEXT.to_string(),
            state_json: serde_json::json!({ "state": "next" }),
            created_at: "2026-09-30T12:00:00Z".to_string(),
            updated_at: now.to_rfc3339(),
            auth_scheme: String::new(),
        },
        delta,
        new_auth: None,
        now: now.to_rfc3339(),
        fence: h.canonicalization_fence.clone(),
        source: PromotableKind::Candidate,
    }
}

/// Runs `terminal` once and reports whether it wrote its outcome.
async fn write_terminal(
    h: &Harness,
    terminal: Terminal,
    proposal: &str,
    attempt: u32,
    owner: &LeaseFence,
) -> Result<bool, String> {
    match terminal {
        Terminal::PreBoundaryFailure => h
            .storage
            .fail_execution(h.resolution(
                proposal,
                attempt,
                owner,
                ExecutionFailureCode::ProvingFailed,
            ))
            .await
            .map(|written| written == ResolveWrite::Resolved),
        Terminal::PostBoundaryResolution => h
            .storage
            .resolve_execution(
                h.metadata.as_ref(),
                h.resolution(
                    proposal,
                    attempt,
                    owner,
                    ExecutionFailureCode::SubmissionRejected,
                ),
            )
            .await
            .map(|written| written == ResolveWrite::Resolved),
        Terminal::Promotion => h
            .storage
            .promote_candidate(h.metadata.as_ref(), promotion(h))
            .await
            .map(|written| written == PromoteWrite::Applied),
    }
}

macro_rules! filesystem_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                super::$name(&Harness::filesystem().await).await;
            }
        )*
    };
}

mod filesystem {
    use super::*;

    filesystem_tests!(
        reservation_is_created_only_on_a_clean_account,
        reservation_is_refused_while_a_client_candidate_exists,
        only_the_owner_crosses_the_boundary,
        admission_refuses_a_moved_base,
        a_client_candidate_is_refused_while_reserved,
        a_client_candidate_is_refused_while_guardian_holds_its_own,
        a_pre_boundary_failure_releases_with_its_outcome,
        a_post_boundary_failure_discards_the_candidate_and_the_proposal,
        canonicalization_cannot_discard_an_executing_candidate,
        promotion_commits_and_releases_the_execution,
        active_executions_list_every_unreleased_attempt,
        proposal_admission_counts_only_viable_request_bytes,
        proposal_admission_enforces_the_viable_count,
        concurrent_admissions_for_the_last_slot_accept_exactly_one,
        promotion_and_resolution_racing_leave_one_terminal_outcome,
        executions_on_two_accounts_proceed_independently,
    );

    #[tokio::test]
    async fn ownership_transfers_by_compare_and_set() {
        super::ownership_transfers_by_compare_and_set(
            &Harness::filesystem().await,
            Duration::ZERO,
            Duration::ZERO,
        )
        .await;
    }

    /// The file a crash is injected into. Every write goes through a fixed temporary path, so a
    /// directory squatting on it fails exactly that write, whoever runs the test.
    #[derive(Debug, Clone, Copy)]
    enum FileFault {
        ExecutionRecord,
        State,
        Delta,
    }

    fn fault_path(h: &Harness, fault: FileFault) -> std::path::PathBuf {
        let account = h
            ._dir
            .as_ref()
            .expect("filesystem harness")
            .path()
            .join("store")
            .join(&h.account_id);
        match fault {
            FileFault::ExecutionRecord => account.join("executions.tmp"),
            FileFault::State => account.join("state.tmp"),
            FileFault::Delta => account.join("deltas").join("1.tmp"),
        }
    }

    async fn a_crash_between_terminal_writes_is_recovered_by_running_it_again(
        terminal: Terminal,
        fault: FileFault,
    ) {
        let h = Harness::filesystem().await;
        let proposal = proposal_commitment(1);
        h.store_proposal(&proposal, 1).await;
        let owner = h.lease("worker-a", Duration::from_secs(60)).await;
        let attempt = h.reserve(&proposal, &owner).await;
        if !matches!(terminal, Terminal::PreBoundaryFailure) {
            h.storage
                .admit_execution_candidate(
                    h.metadata.as_ref(),
                    h.admission(&proposal, attempt, &owner, 1),
                )
                .await
                .unwrap();
        }

        let path = fault_path(&h, fault);
        std::fs::create_dir_all(path.join("squat")).unwrap();
        let crashed = write_terminal(&h, terminal, &proposal, attempt, &owner).await;
        std::fs::remove_dir_all(&path).unwrap();

        let context = format!("{terminal:?} crashed at {fault:?}");
        assert!(crashed.is_err(), "{context}: the injected fault surfaces");
        let interrupted = h
            .storage
            .load_latest_execution(&h.account_id, &proposal)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            interrupted.reservation.released_at.is_some(),
            interrupted.outcome.is_some(),
            "{context}: an outcome and its release are written together or not at all"
        );

        let rerun = write_terminal(&h, terminal, &proposal, attempt, &owner)
            .await
            .unwrap();
        assert!(
            rerun || interrupted.outcome.is_some(),
            "{context}: running the terminal write again completes it"
        );
        let latest = h
            .storage
            .load_latest_execution(&h.account_id, &proposal)
            .await
            .unwrap()
            .unwrap();
        assert!(latest.reservation.released_at.is_some(), "{context}");
        let terminal_written = latest.outcome.expect("one outcome").terminal;
        match terminal {
            Terminal::Promotion => {
                assert_eq!(terminal_written, ExecutionTerminal::Committed, "{context}");
                assert_eq!(
                    h.storage
                        .pull_state(&h.account_id)
                        .await
                        .unwrap()
                        .commitment,
                    NEXT,
                    "{context}"
                );
                assert!(
                    h.delta_at(1).await.unwrap().status.is_canonical(),
                    "{context}: the candidate is canonical"
                );
                assert!(
                    !h.pending_flag().await,
                    "{context}: the account is unlocked"
                );
            }
            Terminal::PostBoundaryResolution => {
                assert!(h.delta_at(1).await.is_none(), "{context}");
                assert!(
                    h.storage
                        .pull_delta_proposal(&h.account_id, &proposal)
                        .await
                        .is_err(),
                    "{context}"
                );
                assert!(!h.pending_flag().await, "{context}");
            }
            Terminal::PreBoundaryFailure => {
                assert!(
                    h.storage
                        .pull_delta_proposal(&h.account_id, &proposal)
                        .await
                        .is_ok(),
                    "{context}: a pre-boundary failure keeps the proposal"
                );
            }
        }
    }

    #[tokio::test]
    async fn an_admission_interrupted_before_its_evidence_is_an_unsent_attempt_that_fails_clean() {
        let h = Harness::filesystem().await;
        let proposal = proposal_commitment(1);
        h.store_proposal(&proposal, 1).await;
        let owner = h.lease("worker-a", Duration::from_secs(60)).await;
        let attempt = h.reserve(&proposal, &owner).await;

        let path = fault_path(&h, FileFault::ExecutionRecord);
        std::fs::create_dir_all(path.join("squat")).unwrap();
        let crashed = h
            .storage
            .admit_execution_candidate(
                h.metadata.as_ref(),
                h.admission(&proposal, attempt, &owner, 1),
            )
            .await;
        std::fs::remove_dir_all(&path).unwrap();

        assert!(crashed.is_err(), "the injected fault surfaces");
        let interrupted = h
            .storage
            .load_active_execution(&h.account_id)
            .await
            .unwrap()
            .expect("the reservation is held");
        assert!(
            interrupted.evidence.is_none(),
            "without its evidence the attempt never crossed the boundary"
        );
        assert!(h.delta_at(1).await.is_some(), "the candidate was written");

        assert_eq!(
            h.storage
                .fail_execution(h.resolution(
                    &proposal,
                    attempt,
                    &owner,
                    ExecutionFailureCode::Abandoned
                ))
                .await
                .unwrap(),
            ResolveWrite::Resolved
        );
        assert!(
            h.delta_at(1).await.is_none(),
            "the orphaned candidate is removed"
        );
        assert!(
            h.storage
                .pull_delta_proposal(&h.account_id, &proposal)
                .await
                .is_ok(),
            "the unsent proposal survives for a retry"
        );
        assert!(
            h.storage
                .load_active_execution(&h.account_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    macro_rules! file_crash_tests {
        ($($name:ident: $terminal:ident at $fault:ident),* $(,)?) => {
            $(
                #[tokio::test]
                async fn $name() {
                    a_crash_between_terminal_writes_is_recovered_by_running_it_again(
                        Terminal::$terminal,
                        FileFault::$fault,
                    )
                    .await;
                }
            )*
        };
    }

    file_crash_tests!(
        a_pre_boundary_failure_crashing_at_its_record_is_retried:
            PreBoundaryFailure at ExecutionRecord,
        a_resolution_crashing_after_the_discard_is_retried:
            PostBoundaryResolution at ExecutionRecord,
        a_promotion_crashing_at_its_record_is_retried: Promotion at ExecutionRecord,
        a_promotion_crashing_after_its_record_is_finished_by_running_it_again:
            Promotion at State,
        a_promotion_crashing_after_its_state_is_finished_by_running_it_again:
            Promotion at Delta,
    );

    #[tokio::test]
    async fn an_expired_owner_cannot_write_before_anyone_claims() {
        let h = Harness::filesystem().await;
        let proposal = proposal_commitment(1);
        let owner = h.lease("worker-a", Duration::from_secs(60)).await;
        let mut reservation = h.new_reservation(&proposal, &owner);
        reservation.lease_expires_at = Utc::now() - chrono::Duration::seconds(1);
        let ReservationWrite::Created { attempt } = h
            .storage
            .create_execution_reservation(reservation)
            .await
            .unwrap()
        else {
            panic!("reservation");
        };
        assert_eq!(
            h.storage
                .admit_execution_candidate(
                    h.metadata.as_ref(),
                    h.admission(&proposal, attempt, &owner, 1)
                )
                .await
                .unwrap(),
            AdmissionWrite::StaleLease
        );
        assert!(h.delta_at(1).await.is_none());
    }
}

#[cfg(feature = "postgres")]
mod postgres {
    use super::*;
    use crate::coordination::LeaderElector;
    use crate::coordination::postgres::PgLeaseElector;
    use diesel::sql_types::Text;
    use diesel_async::RunQueryDsl;

    struct PgHarness {
        harness: Harness,
        url: String,
    }

    async fn pg_harness() -> PgHarness {
        let url = crate::testing::pg::test_database_url().await;
        let storage = crate::storage::postgres::PostgresService::new(&url, 4)
            .await
            .expect("storage");
        let metadata = crate::metadata::postgres::PostgresMetadataStore::new(&url, 2)
            .await
            .expect("metadata");
        let stamp = Utc::now().timestamp_micros();
        let pool = crate::storage::postgres::build_postgres_pool_lazy(&url, 4).expect("pool");
        let canonicalization = PgLeaseElector::new(
            pool.clone(),
            format!("canon-exec-{stamp}"),
            "canonicalization-worker",
        )
        .try_acquire(Duration::from_secs(300))
        .await
        .expect("canonicalization lease")
        .expect("free canonicalization lease");
        let harness = Harness {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata),
            leases: Arc::new(crate::coordination::PgExecutionLeases::new(pool)),
            canonicalization_fence: Some(LeaseFence {
                lease_name: canonicalization.name,
                holder_id: canonicalization.holder_id,
                fence_token: canonicalization.fence_token,
            }),
            account_id: format!("0xexec{stamp}"),
            _dir: None,
        };
        harness.seed().await;
        PgHarness { harness, url }
    }

    macro_rules! postgres_tests {
        ($($name:ident),* $(,)?) => {
            $(
                #[tokio::test]
                #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
                async fn $name() {
                    super::$name(&pg_harness().await.harness).await;
                }
            )*
        };
    }

    postgres_tests!(
        reservation_is_created_only_on_a_clean_account,
        reservation_is_refused_while_a_client_candidate_exists,
        only_the_owner_crosses_the_boundary,
        admission_refuses_a_moved_base,
        a_client_candidate_is_refused_while_reserved,
        a_client_candidate_is_refused_while_guardian_holds_its_own,
        a_pre_boundary_failure_releases_with_its_outcome,
        a_post_boundary_failure_discards_the_candidate_and_the_proposal,
        canonicalization_cannot_discard_an_executing_candidate,
        promotion_commits_and_releases_the_execution,
        active_executions_list_every_unreleased_attempt,
        proposal_admission_counts_only_viable_request_bytes,
        proposal_admission_enforces_the_viable_count,
        concurrent_admissions_for_the_last_slot_accept_exactly_one,
        promotion_and_resolution_racing_leave_one_terminal_outcome,
        executions_on_two_accounts_proceed_independently,
    );

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn ownership_transfers_by_compare_and_set() {
        super::ownership_transfers_by_compare_and_set(
            &pg_harness().await.harness,
            Duration::from_secs(1),
            Duration::from_millis(1_200),
        )
        .await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn concurrent_reservations_admit_exactly_one() {
        let pg = pg_harness().await;
        let h = Arc::new(pg.harness);
        let fence = h.lease("worker-a", Duration::from_secs(60)).await;
        let attempts = (0..8u8).map(|tag| {
            let h = h.clone();
            let fence = fence.clone();
            tokio::spawn(async move {
                h.storage
                    .create_execution_reservation(
                        h.new_reservation(&proposal_commitment(tag + 1), &fence),
                    )
                    .await
                    .unwrap()
            })
        });
        let outcomes: Vec<ReservationWrite> = futures::future::join_all(attempts)
            .await
            .into_iter()
            .map(|joined| joined.unwrap())
            .collect();
        let created = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ReservationWrite::Created { .. }))
            .count();
        assert_eq!(created, 1, "{outcomes:?}");
        assert!(outcomes.iter().all(|outcome| matches!(
            outcome,
            ReservationWrite::Created { .. } | ReservationWrite::AlreadyReserved { .. }
        )));
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn a_fence_that_is_not_the_current_lease_writes_nothing() {
        let h = pg_harness().await.harness;
        let fence = h.lease("worker-a", Duration::from_secs(60)).await;
        let forged = LeaseFence {
            fence_token: fence.fence_token + 7,
            ..fence.clone()
        };
        assert_eq!(
            h.storage
                .create_execution_reservation(h.new_reservation(&proposal_commitment(1), &forged))
                .await
                .unwrap(),
            ReservationWrite::StaleLease
        );
        assert!(
            h.storage
                .load_active_execution(&h.account_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn the_schema_rejects_a_second_active_reservation() {
        let pg = pg_harness().await;
        let h = &pg.harness;
        let fence = h.lease("worker-a", Duration::from_secs(60)).await;
        h.reserve(&proposal_commitment(1), &fence).await;
        let pool = crate::storage::postgres::build_postgres_pool_lazy(&pg.url, 1).unwrap();
        let mut conn = pool.get().await.unwrap();
        let bypass = diesel::sql_query(
            "INSERT INTO execution_reservations \
             (account_id, proposal_id, attempt, holder_id, lease_name, fence_token, \
              lease_expires_at, phase) \
             VALUES ($1, $2, 1, 'bypass', 'bypass', 0, now(), 'accepted')",
        )
        .bind::<Text, _>(&h.account_id)
        .bind::<Text, _>(proposal_commitment(9))
        .execute(&mut conn)
        .await;
        assert!(bypass.is_err(), "partial unique index must reject it");
    }

    /// The write a crash is injected into. The outcome insert follows every other terminal
    /// write except the release, so the two points split each terminal transaction into its
    /// earlier writes, its outcome and its release.
    #[derive(Debug, Clone, Copy)]
    enum FaultPoint {
        OutcomeInsert,
        ReservationRelease,
    }

    struct InjectedFault {
        pool: diesel_async::pooled_connection::deadpool::Pool<diesel_async::AsyncPgConnection>,
        trigger: String,
        table: &'static str,
    }

    impl InjectedFault {
        async fn install(url: &str, account_id: &str, point: FaultPoint) -> Self {
            let pool = crate::storage::postgres::build_postgres_pool_lazy(url, 1).unwrap();
            let mut conn = pool.get().await.unwrap();
            let trigger = format!("reject_{point:?}_{}", &account_id[2..]).to_lowercase();
            diesel::sql_query(format!(
                "CREATE FUNCTION {trigger}() RETURNS trigger AS $$ \
                 BEGIN RAISE EXCEPTION 'injected'; END $$ LANGUAGE plpgsql"
            ))
            .execute(&mut conn)
            .await
            .unwrap();
            let (table, event) = match point {
                FaultPoint::OutcomeInsert => ("execution_outcomes", "BEFORE INSERT"),
                FaultPoint::ReservationRelease => {
                    ("execution_reservations", "BEFORE UPDATE OF released_at")
                }
            };
            diesel::sql_query(format!(
                "CREATE TRIGGER {trigger} {event} ON {table} FOR EACH ROW \
                 WHEN (NEW.account_id = '{account_id}') EXECUTE FUNCTION {trigger}()"
            ))
            .execute(&mut conn)
            .await
            .unwrap();
            drop(conn);
            Self {
                pool,
                trigger,
                table,
            }
        }

        async fn remove(self) {
            let mut conn = self.pool.get().await.unwrap();
            diesel::sql_query(format!("DROP TRIGGER {} ON {}", self.trigger, self.table))
                .execute(&mut conn)
                .await
                .unwrap();
            diesel::sql_query(format!("DROP FUNCTION {}()", self.trigger))
                .execute(&mut conn)
                .await
                .unwrap();
        }
    }

    async fn a_crash_inside_a_terminal_write_leaves_the_attempt_as_it_was(
        terminal: Terminal,
        point: FaultPoint,
    ) {
        let pg = pg_harness().await;
        let h = &pg.harness;
        let proposal = proposal_commitment(1);
        h.store_proposal(&proposal, 1).await;
        let owner = h.lease("worker-a", Duration::from_secs(60)).await;
        let attempt = h.reserve(&proposal, &owner).await;
        let boundary_crossed = !matches!(terminal, Terminal::PreBoundaryFailure);
        if boundary_crossed {
            h.storage
                .admit_execution_candidate(
                    h.metadata.as_ref(),
                    h.admission(&proposal, attempt, &owner, 1),
                )
                .await
                .unwrap();
        }

        let fault = InjectedFault::install(&pg.url, &h.account_id, point).await;
        let crashed = write_terminal(h, terminal, &proposal, attempt, &owner).await;
        fault.remove().await;

        let context = format!("{terminal:?} crashed at {point:?}");
        assert!(crashed.is_err(), "{context}: the injected fault surfaces");
        let active = h
            .storage
            .load_active_execution(&h.account_id)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{context}: the reservation is still held"));
        assert!(
            active.outcome.is_none(),
            "{context}: no outcome was written"
        );
        assert_eq!(
            h.storage
                .pull_state(&h.account_id)
                .await
                .unwrap()
                .commitment,
            BASE,
            "{context}: the state did not advance"
        );
        assert!(
            h.storage
                .pull_delta_proposal(&h.account_id, &proposal)
                .await
                .is_ok(),
            "{context}: the proposal survives"
        );
        if boundary_crossed {
            let candidate = h
                .delta_at(1)
                .await
                .unwrap_or_else(|| panic!("{context}: the candidate survives"));
            assert!(
                matches!(candidate.status, DeltaStatus::Candidate { .. }),
                "{context}: the candidate is still a candidate"
            );
            assert!(
                h.pending_flag().await,
                "{context}: the account stays locked"
            );
        }

        assert!(
            write_terminal(h, terminal, &proposal, attempt, &owner)
                .await
                .unwrap(),
            "{context}: the write succeeds once the fault is gone"
        );
        let latest = h
            .storage
            .load_latest_execution(&h.account_id, &proposal)
            .await
            .unwrap()
            .unwrap();
        assert!(latest.reservation.released_at.is_some(), "{context}");
        assert!(latest.outcome.is_some(), "{context}: exactly one outcome");
    }

    macro_rules! crash_injection_tests {
        ($($name:ident: $terminal:ident at $point:ident),* $(,)?) => {
            $(
                #[tokio::test]
                #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
                async fn $name() {
                    a_crash_inside_a_terminal_write_leaves_the_attempt_as_it_was(
                        Terminal::$terminal,
                        FaultPoint::$point,
                    )
                    .await;
                }
            )*
        };
    }

    crash_injection_tests!(
        a_pre_boundary_failure_crashing_at_its_outcome_leaves_the_reservation_held:
            PreBoundaryFailure at OutcomeInsert,
        a_pre_boundary_failure_crashing_at_its_release_leaves_the_reservation_held:
            PreBoundaryFailure at ReservationRelease,
        a_resolution_crashing_at_its_outcome_keeps_the_candidate_and_the_proposal:
            PostBoundaryResolution at OutcomeInsert,
        a_resolution_crashing_at_its_release_keeps_the_candidate_and_the_proposal:
            PostBoundaryResolution at ReservationRelease,
        a_promotion_crashing_at_its_outcome_keeps_the_base_state:
            Promotion at OutcomeInsert,
        a_promotion_crashing_at_its_release_keeps_the_base_state:
            Promotion at ReservationRelease,
    );

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn promotion_and_resolution_race_to_exactly_one_outcome() {
        for _ in 0..5 {
            let pg = pg_harness().await;
            let h = Arc::new(pg.harness);
            let proposal = proposal_commitment(1);
            let owner = h.lease("worker-a", Duration::from_secs(60)).await;
            let attempt = h.reserve(&proposal, &owner).await;
            h.storage
                .admit_execution_candidate(
                    h.metadata.as_ref(),
                    h.admission(&proposal, attempt, &owner, 1),
                )
                .await
                .unwrap();
            let now = Utc::now();
            let mut delta = h.candidate(1);
            delta.status = DeltaStatus::canonical(now.to_rfc3339());
            let promotion = CandidatePromotion {
                state: StateObject {
                    account_id: h.account_id.clone(),
                    commitment: NEXT.to_string(),
                    state_json: serde_json::json!({ "state": "next" }),
                    created_at: "2026-09-30T12:00:00Z".to_string(),
                    updated_at: now.to_rfc3339(),
                    auth_scheme: String::new(),
                },
                delta,
                new_auth: None,
                now: now.to_rfc3339(),
                fence: h.canonicalization_fence.clone(),
                source: PromotableKind::Candidate,
            };
            let promote = {
                let h = h.clone();
                tokio::spawn(async move {
                    h.storage
                        .promote_candidate(h.metadata.as_ref(), promotion)
                        .await
                        .unwrap()
                })
            };
            let resolve = {
                let h = h.clone();
                let resolution =
                    h.resolution(&proposal, attempt, &owner, ExecutionFailureCode::Expired);
                tokio::spawn(async move {
                    h.storage
                        .resolve_execution(h.metadata.as_ref(), resolution)
                        .await
                        .unwrap()
                })
            };
            let (promoted, resolved) = (promote.await.unwrap(), resolve.await.unwrap());
            let promotion_won = promoted == PromoteWrite::Applied;
            let resolution_won = resolved == ResolveWrite::Resolved;
            assert!(
                promotion_won != resolution_won,
                "exactly one wins: {promoted:?} / {resolved:?}"
            );
            let latest = h
                .storage
                .load_latest_execution(&h.account_id, &proposal)
                .await
                .unwrap()
                .unwrap();
            assert!(latest.reservation.released_at.is_some());
            let terminal = latest.outcome.expect("one outcome").terminal;
            assert_eq!(terminal == ExecutionTerminal::Committed, promotion_won);
        }
    }

    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn execution_leases_are_per_account() {
        let pg = pg_harness().await;
        let leases = &pg.harness.leases;
        let stamp = Utc::now().timestamp_micros();
        let (one, two) = (format!("0xa{stamp}"), format!("0xb{stamp}"));
        let ttl = Duration::from_secs(60);
        let lease_one = leases
            .elector(&one, "task-a")
            .try_acquire(ttl)
            .await
            .unwrap();
        let lease_two = leases
            .elector(&two, "task-b")
            .try_acquire(ttl)
            .await
            .unwrap();
        assert!(
            lease_one.is_some() && lease_two.is_some(),
            "accounts do not contend"
        );
        assert!(
            leases
                .elector(&one, "task-c")
                .try_acquire(ttl)
                .await
                .unwrap()
                .is_none(),
            "one holder per account"
        );
        assert_ne!(
            lease_one.unwrap().name,
            crate::coordination::CANONICALIZATION_LEASE
        );
    }
}
