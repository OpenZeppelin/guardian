use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};

use super::{
    NewDelta, PostgresService, clear_pending_flag_if_none, client_abandoned_reason,
    derive_status_columns, lease_fence_is_current, lock_account_metadata,
};
use crate::schema::{
    account_metadata, delta_proposals, deltas, execution_outcomes, execution_reservations,
    execution_submissions, states,
};
use crate::storage::{
    AdmissionWrite, CandidateAdmission, ClaimWrite, ExecutionFailure, ExecutionOutcome,
    ExecutionPhase, ExecutionRecord, ExecutionReservation, ExecutionResolution, ExecutionTerminal,
    LeaseFence, NewExecutionReservation, ReservationUpdate, ReservationWrite, ResolveWrite,
    SettleWrite, SubmissionEvidence,
};

#[derive(Queryable, Selectable)]
#[diesel(table_name = execution_reservations)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct ReservationRow {
    id: i64,
    account_id: String,
    proposal_id: String,
    attempt: i32,
    holder_id: String,
    lease_name: String,
    fence_token: i64,
    lease_expires_at: DateTime<Utc>,
    phase: String,
    candidate_nonce: Option<i64>,
    ignored_signatures: i32,
    released_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ReservationRow {
    fn fence(&self) -> LeaseFence {
        LeaseFence {
            lease_name: self.lease_name.clone(),
            holder_id: self.holder_id.clone(),
            fence_token: self.fence_token,
        }
    }

    fn authorizes(&self, fence: &LeaseFence, proposal_id: &str, attempt: u32) -> bool {
        self.holder_id == fence.holder_id
            && self.proposal_id == proposal_id
            && i64::from(self.attempt) == i64::from(attempt)
    }

    fn into_reservation(self) -> Result<ExecutionReservation, String> {
        Ok(ExecutionReservation {
            fence: self.fence(),
            account_id: self.account_id,
            proposal_id: self.proposal_id,
            attempt: to_u32(i64::from(self.attempt), "attempt")?,
            lease_expires_at: self.lease_expires_at,
            phase: ExecutionPhase::parse(&self.phase)?,
            candidate_nonce: self
                .candidate_nonce
                .map(|nonce| to_u64(nonce, "candidate_nonce"))
                .transpose()?,
            ignored_signatures: to_u32(i64::from(self.ignored_signatures), "ignored_signatures")?,
            released_at: self.released_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = execution_submissions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct SubmissionRow {
    account_id: String,
    proposal_id: String,
    attempt: i32,
    candidate_nonce: i64,
    transaction_id: String,
    expected_commitment: String,
    reference_block: i64,
    expiration_block: i64,
    base_commitment: String,
    committed_at: DateTime<Utc>,
}

impl SubmissionRow {
    fn into_evidence(self) -> Result<SubmissionEvidence, String> {
        Ok(SubmissionEvidence {
            account_id: self.account_id,
            proposal_id: self.proposal_id,
            attempt: to_u32(i64::from(self.attempt), "attempt")?,
            candidate_nonce: to_u64(self.candidate_nonce, "candidate_nonce")?,
            transaction_id: self.transaction_id,
            expected_commitment: self.expected_commitment,
            reference_block: to_u32(self.reference_block, "reference_block")?,
            expiration_block: to_u32(self.expiration_block, "expiration_block")?,
            base_commitment: self.base_commitment,
            committed_at: self.committed_at,
        })
    }
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = execution_outcomes)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct OutcomeRow {
    account_id: String,
    proposal_id: String,
    attempt: i32,
    state: String,
    error_code: Option<String>,
    error_message: Option<String>,
    error_meta: Option<serde_json::Value>,
    resolved_at: DateTime<Utc>,
}

impl OutcomeRow {
    fn into_outcome(self) -> Result<ExecutionOutcome, String> {
        let terminal = match self.state.as_str() {
            "committed" => ExecutionTerminal::Committed,
            "failed" => {
                let code = self
                    .error_code
                    .ok_or_else(|| "failed execution outcome has no error code".to_string())?;
                ExecutionTerminal::Failed {
                    failure: ExecutionFailure {
                        code: crate::storage::ExecutionFailureCode::from_parts(
                            &code,
                            self.error_meta.as_ref(),
                        )?,
                        message: self.error_message.unwrap_or_default(),
                    },
                }
            }
            other => return Err(format!("unknown execution outcome state '{other}'")),
        };
        Ok(ExecutionOutcome {
            account_id: self.account_id,
            proposal_id: self.proposal_id,
            attempt: to_u32(i64::from(self.attempt), "attempt")?,
            terminal,
            resolved_at: self.resolved_at,
        })
    }
}

#[derive(Insertable)]
#[diesel(table_name = execution_outcomes)]
struct NewOutcomeRow<'a> {
    account_id: &'a str,
    proposal_id: &'a str,
    attempt: i32,
    state: &'a str,
    error_code: Option<&'a str>,
    error_message: Option<&'a str>,
    error_meta: Option<serde_json::Value>,
    resolved_at: DateTime<Utc>,
}

fn to_u32(value: i64, field: &str) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| format!("stored execution {field} {value} is out of range"))
}

fn to_u64(value: i64, field: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("stored execution {field} {value} is out of range"))
}

fn to_i32(value: u32, field: &str) -> Result<i32, String> {
    i32::try_from(value).map_err(|_| format!("execution {field} {value} does not fit the store"))
}

type TxResult<T> = Result<T, diesel::result::Error>;

async fn active_reservation(
    conn: &mut AsyncPgConnection,
    account_id: &str,
) -> TxResult<Option<ReservationRow>> {
    execution_reservations::table
        .filter(execution_reservations::account_id.eq(account_id))
        .filter(execution_reservations::released_at.is_null())
        .select(ReservationRow::as_select())
        .first(conn)
        .await
        .optional()
}

async fn attempt_reservation(
    conn: &mut AsyncPgConnection,
    account_id: &str,
    proposal_id: &str,
    attempt: i32,
) -> TxResult<Option<ReservationRow>> {
    execution_reservations::table
        .filter(execution_reservations::account_id.eq(account_id))
        .filter(execution_reservations::proposal_id.eq(proposal_id))
        .filter(execution_reservations::attempt.eq(attempt))
        .select(ReservationRow::as_select())
        .first(conn)
        .await
        .optional()
}

async fn attempt_evidence(
    conn: &mut AsyncPgConnection,
    account_id: &str,
    proposal_id: &str,
    attempt: i32,
) -> TxResult<Option<SubmissionRow>> {
    execution_submissions::table
        .filter(execution_submissions::account_id.eq(account_id))
        .filter(execution_submissions::proposal_id.eq(proposal_id))
        .filter(execution_submissions::attempt.eq(attempt))
        .select(SubmissionRow::as_select())
        .first(conn)
        .await
        .optional()
}

async fn attempt_has_outcome(
    conn: &mut AsyncPgConnection,
    account_id: &str,
    proposal_id: &str,
    attempt: i32,
) -> TxResult<bool> {
    diesel::select(diesel::dsl::exists(
        execution_outcomes::table
            .filter(execution_outcomes::account_id.eq(account_id))
            .filter(execution_outcomes::proposal_id.eq(proposal_id))
            .filter(execution_outcomes::attempt.eq(attempt)),
    ))
    .get_result(conn)
    .await
}

/// Whether `fence` is both the reservation's owner and a current lease.
async fn owns_live_reservation(
    conn: &mut AsyncPgConnection,
    reservation: &ReservationRow,
    fence: &LeaseFence,
) -> TxResult<bool> {
    Ok(reservation.fence() == *fence && lease_fence_is_current(conn, fence).await?)
}

async fn release_reservation(
    conn: &mut AsyncPgConnection,
    reservation_id: i64,
    now: DateTime<Utc>,
) -> TxResult<()> {
    diesel::update(execution_reservations::table)
        .filter(execution_reservations::id.eq(reservation_id))
        .set((
            execution_reservations::released_at.eq(Some(now)),
            execution_reservations::updated_at.eq(now),
        ))
        .execute(conn)
        .await
        .map(|_| ())
}

async fn insert_outcome(conn: &mut AsyncPgConnection, outcome: NewOutcomeRow<'_>) -> TxResult<()> {
    diesel::insert_into(execution_outcomes::table)
        .values(&outcome)
        .execute(conn)
        .await
        .map(|_| ())
}

async fn attach_records(
    conn: &mut AsyncPgConnection,
    rows: Vec<ReservationRow>,
) -> TxResult<Vec<Result<ExecutionRecord, String>>> {
    let mut records = Vec::with_capacity(rows.len());
    for row in rows {
        let evidence =
            attempt_evidence(conn, &row.account_id, &row.proposal_id, row.attempt).await?;
        let outcome = execution_outcomes::table
            .filter(execution_outcomes::account_id.eq(&row.account_id))
            .filter(execution_outcomes::proposal_id.eq(&row.proposal_id))
            .filter(execution_outcomes::attempt.eq(row.attempt))
            .select(OutcomeRow::as_select())
            .first(conn)
            .await
            .optional()?;
        records.push((|| {
            Ok(ExecutionRecord {
                reservation: row.into_reservation()?,
                evidence: evidence.map(SubmissionRow::into_evidence).transpose()?,
                outcome: outcome.map(OutcomeRow::into_outcome).transpose()?,
            })
        })());
    }
    Ok(records)
}

fn collect_records(
    records: Vec<Result<ExecutionRecord, String>>,
) -> Result<Vec<ExecutionRecord>, String> {
    records.into_iter().collect()
}

/// Whether the candidate at `nonce` belongs to the account's unresolved,
/// boundary-crossed execution. Callers must hold the account lock.
pub(super) async fn execution_owns_candidate(
    conn: &mut AsyncPgConnection,
    account_id: &str,
    nonce: u64,
) -> TxResult<bool> {
    Ok(owning_reservation(conn, account_id, nonce).await?.is_some())
}

async fn owning_reservation(
    conn: &mut AsyncPgConnection,
    account_id: &str,
    nonce: u64,
) -> TxResult<Option<ReservationRow>> {
    let Some(reservation) = active_reservation(conn, account_id).await? else {
        return Ok(None);
    };
    let owns = diesel::select(diesel::dsl::exists(
        execution_submissions::table
            .filter(execution_submissions::account_id.eq(account_id))
            .filter(execution_submissions::proposal_id.eq(&reservation.proposal_id))
            .filter(execution_submissions::attempt.eq(reservation.attempt))
            .filter(execution_submissions::candidate_nonce.eq(nonce as i64)),
    ))
    .get_result::<bool>(conn)
    .await?;
    Ok(owns.then_some(reservation))
}

/// Persist `committed` and release the reservation whose candidate the
/// caller's transaction just promoted. Callers must hold the account lock.
pub(super) async fn commit_promoted_execution(
    conn: &mut AsyncPgConnection,
    account_id: &str,
    nonce: u64,
    now: DateTime<Utc>,
) -> TxResult<()> {
    let Some(reservation) = owning_reservation(conn, account_id, nonce).await? else {
        return Ok(());
    };
    insert_outcome(
        conn,
        NewOutcomeRow {
            account_id,
            proposal_id: &reservation.proposal_id,
            attempt: reservation.attempt,
            state: "committed",
            error_code: None,
            error_message: None,
            error_meta: None,
            resolved_at: now,
        },
    )
    .await?;
    release_reservation(conn, reservation.id, now).await
}

/// The proposal whose execution holds the account, if any. Callers must hold
/// the account lock.
pub(super) async fn reserving_proposal(
    conn: &mut AsyncPgConnection,
    account_id: &str,
) -> TxResult<Option<String>> {
    Ok(active_reservation(conn, account_id)
        .await?
        .map(|reservation| reservation.proposal_id))
}

impl PostgresService {
    async fn connection(
        &self,
    ) -> Result<diesel_async::pooled_connection::deadpool::Object<AsyncPgConnection>, String> {
        self.pool
            .get()
            .await
            .map_err(|e| format!("Failed to get connection: {e}"))
    }

    pub(super) async fn create_execution_reservation_tx(
        &self,
        reservation: NewExecutionReservation,
    ) -> Result<ReservationWrite, String> {
        crate::storage::execution::ensure_execution_lease(
            &reservation.account_id,
            &reservation.fence,
        )?;
        let ignored_signatures = to_i32(reservation.ignored_signatures, "ignored_signatures")?;
        let mut conn = self.connection().await?;
        conn.transaction::<ReservationWrite, diesel::result::Error, _>(|conn| {
            async move {
                lock_account_metadata(conn, &reservation.account_id).await?;
                if !lease_fence_is_current(conn, &reservation.fence).await? {
                    return Ok(ReservationWrite::StaleLease);
                }
                if let Some(active) = active_reservation(conn, &reservation.account_id).await? {
                    return Ok(ReservationWrite::AlreadyReserved {
                        holder_id: active.holder_id,
                        proposal_id: active.proposal_id,
                    });
                }
                let candidate_exists: bool = diesel::select(diesel::dsl::exists(
                    deltas::table
                        .filter(deltas::account_id.eq(&reservation.account_id))
                        .filter(deltas::status_kind.eq("candidate")),
                ))
                .get_result(conn)
                .await?;
                if candidate_exists {
                    return Ok(ReservationWrite::CandidateExists);
                }
                let proposal_exists: bool = diesel::select(diesel::dsl::exists(
                    delta_proposals::table
                        .filter(delta_proposals::account_id.eq(&reservation.account_id))
                        .filter(delta_proposals::commitment.eq(&reservation.proposal_id)),
                ))
                .get_result(conn)
                .await?;
                if !proposal_exists {
                    return Ok(ReservationWrite::ProposalGone);
                }
                let previous: Option<i32> = execution_reservations::table
                    .filter(execution_reservations::account_id.eq(&reservation.account_id))
                    .filter(execution_reservations::proposal_id.eq(&reservation.proposal_id))
                    .select(diesel::dsl::max(execution_reservations::attempt))
                    .first(conn)
                    .await?;
                let attempt = previous.unwrap_or(0) + 1;
                diesel::insert_into(execution_reservations::table)
                    .values((
                        execution_reservations::account_id.eq(&reservation.account_id),
                        execution_reservations::proposal_id.eq(&reservation.proposal_id),
                        execution_reservations::attempt.eq(attempt),
                        execution_reservations::holder_id.eq(&reservation.fence.holder_id),
                        execution_reservations::lease_name.eq(&reservation.fence.lease_name),
                        execution_reservations::fence_token.eq(reservation.fence.fence_token),
                        execution_reservations::lease_expires_at.eq(reservation.lease_expires_at),
                        execution_reservations::phase.eq(ExecutionPhase::Accepted.as_str()),
                        execution_reservations::ignored_signatures.eq(ignored_signatures),
                        execution_reservations::created_at.eq(reservation.now),
                        execution_reservations::updated_at.eq(reservation.now),
                    ))
                    .execute(conn)
                    .await?;
                Ok(ReservationWrite::Created {
                    attempt: attempt as u32,
                })
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| format!("Failed to create execution reservation: {e}"))
    }

    pub(super) async fn renew_execution_reservation_tx(
        &self,
        account_id: &str,
        fence: &LeaseFence,
        lease_expires_at: DateTime<Utc>,
        phase: ExecutionPhase,
    ) -> Result<ReservationUpdate, String> {
        let account_id = account_id.to_string();
        let fence = fence.clone();
        let mut conn = self.connection().await?;
        conn.transaction::<ReservationUpdate, diesel::result::Error, _>(|conn| {
            async move {
                lock_account_metadata(conn, &account_id).await?;
                let Some(active) = active_reservation(conn, &account_id).await? else {
                    return Ok(ReservationUpdate::NotActive);
                };
                if !owns_live_reservation(conn, &active, &fence).await? {
                    return Ok(ReservationUpdate::StaleLease);
                }
                diesel::update(execution_reservations::table)
                    .filter(execution_reservations::id.eq(active.id))
                    .set((
                        execution_reservations::lease_expires_at.eq(lease_expires_at),
                        execution_reservations::phase.eq(phase.as_str()),
                        execution_reservations::updated_at.eq(diesel::dsl::now),
                    ))
                    .execute(conn)
                    .await?;
                Ok(ReservationUpdate::Applied)
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| format!("Failed to renew execution reservation: {e}"))
    }

    pub(super) async fn claim_execution_reservation_tx(
        &self,
        account_id: &str,
        expected: &LeaseFence,
        claimant: &LeaseFence,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<ClaimWrite, String> {
        crate::storage::execution::ensure_execution_lease(account_id, claimant)?;
        let account_id = account_id.to_string();
        let expected = expected.clone();
        let claimant = claimant.clone();
        let mut conn = self.connection().await?;
        conn.transaction::<ClaimWrite, diesel::result::Error, _>(|conn| {
            async move {
                lock_account_metadata(conn, &account_id).await?;
                let Some(active) = active_reservation(conn, &account_id).await? else {
                    return Ok(ClaimWrite::NotActive);
                };
                if active.fence() != expected {
                    return Ok(ClaimWrite::ClaimSuperseded);
                }
                if claimant.fence_token <= expected.fence_token
                    || !lease_fence_is_current(conn, &claimant).await?
                {
                    return Ok(ClaimWrite::StaleLease);
                }
                diesel::update(execution_reservations::table)
                    .filter(execution_reservations::id.eq(active.id))
                    .set((
                        execution_reservations::holder_id.eq(&claimant.holder_id),
                        execution_reservations::fence_token.eq(claimant.fence_token),
                        execution_reservations::lease_expires_at.eq(lease_expires_at),
                        execution_reservations::updated_at.eq(diesel::dsl::now),
                    ))
                    .execute(conn)
                    .await?;
                Ok(ClaimWrite::Claimed)
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| format!("Failed to claim execution reservation: {e}"))
    }

    pub(super) async fn load_active_execution_tx(
        &self,
        account_id: &str,
    ) -> Result<Option<ExecutionRecord>, String> {
        let mut conn = self.connection().await?;
        let row = active_reservation(&mut conn, account_id)
            .await
            .map_err(|e| format!("Failed to load active execution: {e}"))?;
        let records = attach_records(&mut conn, row.into_iter().collect())
            .await
            .map_err(|e| format!("Failed to load active execution: {e}"))?;
        Ok(collect_records(records)?.into_iter().next())
    }

    pub(super) async fn load_latest_execution_tx(
        &self,
        account_id: &str,
        proposal_id: &str,
    ) -> Result<Option<ExecutionRecord>, String> {
        let mut conn = self.connection().await?;
        let row = execution_reservations::table
            .filter(execution_reservations::account_id.eq(account_id))
            .filter(execution_reservations::proposal_id.eq(proposal_id))
            .order(execution_reservations::attempt.desc())
            .select(ReservationRow::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(|e| format!("Failed to load execution: {e}"))?;
        let records = attach_records(&mut conn, row.into_iter().collect())
            .await
            .map_err(|e| format!("Failed to load execution: {e}"))?;
        Ok(collect_records(records)?.into_iter().next())
    }

    pub(super) async fn admit_execution_candidate_tx(
        &self,
        admission: CandidateAdmission,
    ) -> Result<AdmissionWrite, String> {
        admission.ensure_evidence_describes_candidate()?;
        let CandidateAdmission {
            fence,
            delta,
            evidence,
            now,
        } = admission;
        let attempt = to_i32(evidence.attempt, "attempt")?;
        let status_json = serde_json::to_value(&delta.status)
            .map_err(|e| format!("Failed to serialize status: {e}"))?;
        let (status_kind, status_timestamp) = derive_status_columns(&delta.status)?;
        let metadata_json = delta
            .metadata
            .as_ref()
            .map(crate::delta_summary::metadata_to_value);
        let mut conn = self.connection().await?;
        conn.transaction::<AdmissionWrite, diesel::result::Error, _>(|conn| {
            async move {
                lock_account_metadata(conn, &delta.account_id).await?;
                let Some(active) = active_reservation(conn, &delta.account_id).await? else {
                    return Ok(AdmissionWrite::NotAuthorized);
                };
                if !active.authorizes(&fence, &evidence.proposal_id, evidence.attempt)
                    || attempt_evidence(conn, &delta.account_id, &evidence.proposal_id, attempt)
                        .await?
                        .is_some()
                {
                    return Ok(AdmissionWrite::NotAuthorized);
                }
                if !owns_live_reservation(conn, &active, &fence).await? {
                    return Ok(AdmissionWrite::StaleLease);
                }
                let current_commitment = states::table
                    .filter(states::account_id.eq(&delta.account_id))
                    .select(states::commitment)
                    .first::<String>(conn)
                    .await?;
                if current_commitment != delta.prev_commitment {
                    return Ok(AdmissionWrite::StaleBase);
                }
                let (paused_at, released_at): (Option<DateTime<Utc>>, Option<DateTime<Utc>>) =
                    account_metadata::table
                        .filter(account_metadata::account_id.eq(&delta.account_id))
                        .select((account_metadata::paused_at, account_metadata::released_at))
                        .first(conn)
                        .await?;
                if paused_at.is_some() || released_at.is_some() {
                    return Ok(AdmissionWrite::AccountInactive);
                }
                let candidate_exists: bool = diesel::select(diesel::dsl::exists(
                    deltas::table
                        .filter(deltas::account_id.eq(&delta.account_id))
                        .filter(deltas::status_kind.eq("candidate")),
                ))
                .get_result(conn)
                .await?;
                if candidate_exists {
                    return Ok(AdmissionWrite::CandidateExists);
                }
                diesel::delete(deltas::table)
                    .filter(deltas::account_id.eq(&delta.account_id))
                    .filter(deltas::nonce.eq(delta.nonce as i64))
                    .filter(
                        deltas::status_kind.eq("retained").or(deltas::status_kind
                            .eq("discarded")
                            .and(client_abandoned_reason())),
                    )
                    .execute(conn)
                    .await?;
                let inserted = diesel::insert_into(deltas::table)
                    .values(&NewDelta {
                        account_id: &delta.account_id,
                        nonce: delta.nonce as i64,
                        prev_commitment: &delta.prev_commitment,
                        new_commitment: delta.new_commitment.as_deref(),
                        delta_payload: &delta.delta_payload,
                        ack_sig: Some(delta.ack_sig.as_str()),
                        status: status_json.clone(),
                        status_kind,
                        status_timestamp,
                        metadata: metadata_json.as_ref(),
                    })
                    .on_conflict((deltas::account_id, deltas::nonce))
                    .do_nothing()
                    .execute(conn)
                    .await?;
                if inserted == 0 {
                    return Ok(AdmissionWrite::NonceOccupied);
                }
                diesel::insert_into(execution_submissions::table)
                    .values((
                        execution_submissions::account_id.eq(&evidence.account_id),
                        execution_submissions::proposal_id.eq(&evidence.proposal_id),
                        execution_submissions::attempt.eq(attempt),
                        execution_submissions::candidate_nonce.eq(delta.nonce as i64),
                        execution_submissions::transaction_id.eq(&evidence.transaction_id),
                        execution_submissions::expected_commitment
                            .eq(&evidence.expected_commitment),
                        execution_submissions::reference_block
                            .eq(i64::from(evidence.reference_block)),
                        execution_submissions::expiration_block
                            .eq(i64::from(evidence.expiration_block)),
                        execution_submissions::base_commitment.eq(&evidence.base_commitment),
                        execution_submissions::committed_at.eq(evidence.committed_at),
                    ))
                    .execute(conn)
                    .await?;
                diesel::update(execution_reservations::table)
                    .filter(execution_reservations::id.eq(active.id))
                    .set((
                        execution_reservations::candidate_nonce.eq(Some(delta.nonce as i64)),
                        execution_reservations::phase
                            .eq(ExecutionPhase::SubmissionCommitted.as_str()),
                        execution_reservations::updated_at.eq(now),
                    ))
                    .execute(conn)
                    .await?;
                diesel::update(crate::schema::account_metadata::table)
                    .filter(crate::schema::account_metadata::account_id.eq(&delta.account_id))
                    .set((
                        crate::schema::account_metadata::has_pending_candidate.eq(true),
                        crate::schema::account_metadata::updated_at.eq(now),
                    ))
                    .execute(conn)
                    .await?;
                Ok(AdmissionWrite::Admitted)
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| format!("Failed to admit execution candidate: {e}"))
    }

    pub(super) async fn settle_promoted_execution_tx(
        &self,
        account_id: &str,
        fence: &LeaseFence,
        now: DateTime<Utc>,
    ) -> Result<SettleWrite, String> {
        let account_id = account_id.to_string();
        let fence = fence.clone();
        let mut conn = self.connection().await?;
        conn.transaction::<SettleWrite, diesel::result::Error, _>(|conn| {
            async move {
                lock_account_metadata(conn, &account_id).await?;
                let Some(reservation) = active_reservation(conn, &account_id).await? else {
                    return Ok(SettleWrite::NotActive);
                };
                if !owns_live_reservation(conn, &reservation, &fence).await? {
                    return Ok(SettleWrite::StaleLease);
                }
                let Some(evidence) = attempt_evidence(
                    conn,
                    &account_id,
                    &reservation.proposal_id,
                    reservation.attempt,
                )
                .await?
                else {
                    return Ok(SettleWrite::NotPromoted);
                };
                let current_commitment = states::table
                    .filter(states::account_id.eq(&account_id))
                    .select(states::commitment)
                    .first::<String>(conn)
                    .await?;
                let canonical = diesel::select(diesel::dsl::exists(
                    deltas::table
                        .filter(deltas::account_id.eq(&account_id))
                        .filter(deltas::nonce.eq(evidence.candidate_nonce))
                        .filter(deltas::status_kind.eq("canonical")),
                ))
                .get_result::<bool>(conn)
                .await?;
                if !canonical
                    || !current_commitment.eq_ignore_ascii_case(&evidence.expected_commitment)
                {
                    return Ok(SettleWrite::NotPromoted);
                }
                insert_outcome(
                    conn,
                    NewOutcomeRow {
                        account_id: &account_id,
                        proposal_id: &reservation.proposal_id,
                        attempt: reservation.attempt,
                        state: "committed",
                        error_code: None,
                        error_message: None,
                        error_meta: None,
                        resolved_at: now,
                    },
                )
                .await?;
                release_reservation(conn, reservation.id, now).await?;
                Ok(SettleWrite::Settled)
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| format!("Failed to settle a promoted execution: {e}"))
    }

    pub(super) async fn resolve_execution_tx(
        &self,
        resolution: ExecutionResolution,
        boundary_crossed: bool,
    ) -> Result<ResolveWrite, String> {
        let attempt = to_i32(resolution.attempt, "attempt")?;
        let error_meta = resolution.failure.code.meta();
        let mut conn = self.connection().await?;
        conn.transaction::<ResolveWrite, diesel::result::Error, _>(|conn| {
            async move {
                let ExecutionResolution {
                    account_id,
                    proposal_id,
                    fence,
                    failure,
                    now,
                    ..
                } = &resolution;
                lock_account_metadata(conn, account_id).await?;
                let Some(reservation) =
                    attempt_reservation(conn, account_id, proposal_id, attempt).await?
                else {
                    return Ok(ResolveWrite::NotAuthorized);
                };
                if attempt_has_outcome(conn, account_id, proposal_id, attempt).await? {
                    return Ok(ResolveWrite::AlreadyResolved);
                }
                if !reservation.authorizes(fence, proposal_id, resolution.attempt) {
                    return Ok(ResolveWrite::NotAuthorized);
                }
                if !owns_live_reservation(conn, &reservation, fence).await? {
                    return Ok(ResolveWrite::StaleLease);
                }
                let evidence = attempt_evidence(conn, account_id, proposal_id, attempt).await?;
                if evidence.is_some() != boundary_crossed {
                    return Ok(ResolveWrite::WrongSideOfBoundary);
                }
                if let Some(evidence) = evidence {
                    diesel::delete(deltas::table)
                        .filter(deltas::account_id.eq(account_id))
                        .filter(deltas::nonce.eq(evidence.candidate_nonce))
                        .filter(deltas::status_kind.eq("candidate"))
                        .execute(conn)
                        .await?;
                    diesel::delete(delta_proposals::table)
                        .filter(delta_proposals::account_id.eq(account_id))
                        .filter(delta_proposals::commitment.eq(proposal_id))
                        .execute(conn)
                        .await?;
                    clear_pending_flag_if_none(conn, account_id, *now).await?;
                }
                insert_outcome(
                    conn,
                    NewOutcomeRow {
                        account_id,
                        proposal_id,
                        attempt,
                        state: "failed",
                        error_code: Some(failure.code.as_str()),
                        error_message: Some(&failure.message),
                        error_meta,
                        resolved_at: *now,
                    },
                )
                .await?;
                release_reservation(conn, reservation.id, *now).await?;
                Ok(ResolveWrite::Resolved)
            }
            .scope_boxed()
        })
        .await
        .map_err(|e| format!("Failed to resolve execution: {e}"))
    }

    pub(super) async fn list_active_executions_tx(&self) -> Result<Vec<ExecutionRecord>, String> {
        let mut conn = self.connection().await?;
        let rows = execution_reservations::table
            .filter(execution_reservations::released_at.is_null())
            .order(execution_reservations::account_id.asc())
            .select(ReservationRow::as_select())
            .load(&mut conn)
            .await
            .map_err(|e| format!("Failed to list active executions: {e}"))?;
        let records = attach_records(&mut conn, rows)
            .await
            .map_err(|e| format!("Failed to list active executions: {e}"))?;
        collect_records(records)
    }
}
