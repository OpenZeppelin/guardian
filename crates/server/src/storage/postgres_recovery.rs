use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::{Array, BigInt, Bool, Jsonb, Nullable, Text, Timestamptz};
use diesel_async::scoped_futures::{ScopedBoxFuture, ScopedFutureExt};
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl, SimpleAsyncConnection};
use serde_json::Value;
use tokio::time::timeout;

use crate::coordination::{LeaderElector, Lease, postgres::PgLeaseElector};
use crate::delta_object::DeltaStatus;
use crate::metadata::{Auth, MidenNetworkType, NetworkConfig};
use crate::schema::{account_metadata, delta_proposals, states};
use crate::storage::LeaseFence;
use crate::storage::encryption::{cipher::StorageCipher, envelope::RecordAad};
use crate::utils::normalize_commitment_hex;

use super::{Pool, lease_fence_is_current, lock_account_metadata};

const UNSETTLED_ACCOUNT_WORK: &str = "
    CASE
        WHEN m.has_pending_candidate OR EXISTS (
            SELECT 1 FROM deltas pending WHERE pending.account_id = st.account_id
              AND pending.status_kind IN ('candidate', 'retained')
        ) THEN 'a delta awaits its on-chain outcome'
        WHEN EXISTS (
            SELECT 1 FROM execution_reservations r WHERE r.account_id = st.account_id AND r.released_at IS NULL
        ) THEN 'an execution is reserved'
    END";

const CANDIDATE_SOURCE: &str = "
    FROM account_metadata m
    JOIN states st USING (account_id)
    LEFT JOIN LATERAL (
        SELECT nonce, new_commitment FROM deltas
        WHERE account_id = st.account_id AND status_kind = 'canonical'
        ORDER BY nonce DESC LIMIT 1
    ) d ON TRUE
    LEFT JOIN LATERAL (
        SELECT nonce, prev_commitment FROM deltas
        WHERE account_id = st.account_id AND status_kind = 'canonical'
        ORDER BY nonce LIMIT 1
    ) first_held ON TRUE
    WHERE m.account_id > $1 AND m.network_config->>'kind' = 'miden'
      AND (m.network_config = $2 OR m.network_config->>'network_type' IN ('local', 'devnet'))
      AND m.released_at IS NULL
      AND (d.nonce IS NOT NULL OR st.updated_at > st.created_at)
      AND d.new_commitment IS DISTINCT FROM st.commitment";

const HELD_LEASE: &str = "
    FROM worker_leases
    WHERE lease_name = $1 AND holder_id = $2 AND fence_token = $3
      AND clock_timestamp() < expires_at";

#[derive(diesel::QueryableByName)]
pub(crate) struct RecoveryCandidate {
    #[diesel(sql_type = Text)]
    pub account_id: String,
    #[diesel(sql_type = Nullable<BigInt>)]
    pub stored_nonce: Option<i64>,
    #[diesel(sql_type = Text)]
    pub stored_commitment: String,
    #[diesel(sql_type = Nullable<BigInt>)]
    pub canonical_nonce: Option<i64>,
    #[diesel(sql_type = Nullable<Text>)]
    pub canonical_commitment: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub unsettled_work: Option<String>,
    #[diesel(sql_type = Nullable<BigInt>)]
    pub history_starts_at_nonce: Option<i64>,
    #[diesel(sql_type = Nullable<Text>)]
    pub history_base_commitment: Option<String>,
    #[diesel(sql_type = Timestamptz)]
    state_updated_at: DateTime<Utc>,
    #[diesel(sql_type = BigInt)]
    auth_version: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingOutcome {
    Deferred,
    Unresolved,
}

impl PendingOutcome {
    fn label(self) -> &'static str {
        match self {
            Self::Deferred => "deferred",
            Self::Unresolved => "unresolved",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProposalChange {
    Keep,
    Remove,
    DropApprovals {
        status: DeltaStatus,
        delta_payload: Value,
    },
}

impl ProposalChange {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Remove => "remove",
            Self::DropApprovals { .. } => "drop_approvals",
        }
    }
}

/// A pending proposal as the scan read it, and what the restore does with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewedProposal {
    pub commitment: String,
    pub status: DeltaStatus,
    pub change: ProposalChange,
}

/// The review of an account's pending proposals, or the fact that they could not be read.
/// A restore leaves unreadable proposals as they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProposalReview {
    Reviewed(Vec<ReviewedProposal>),
    Unreadable,
}

/// What a restore writes to one proposal row.
enum ProposalWrite {
    Remove,
    Replace { status: Value, delta_payload: Value },
}

#[derive(Debug, Default, Clone, PartialEq, Eq, diesel::QueryableByName)]
pub(crate) struct ScanProgress {
    #[diesel(sql_type = Text)]
    pub resume_after: String,
}

pub(crate) struct RecoveryStore {
    pool: Pool<AsyncPgConnection>,
    cipher: Option<Arc<dyn StorageCipher>>,
    pub leader: Arc<dyn LeaderElector>,
}

impl RecoveryStore {
    pub(crate) fn new(
        pool: Pool<AsyncPgConnection>,
        cipher: Option<Arc<dyn StorageCipher>>,
        holder_id: impl Into<String>,
    ) -> Self {
        let leader = Arc::new(PgLeaseElector::new(
            pool.clone(),
            "account_recovery",
            holder_id,
        ));
        Self {
            pool,
            cipher,
            leader,
        }
    }

    pub(crate) async fn startup_completed(
        &self,
        network_type: MidenNetworkType,
    ) -> Result<bool, String> {
        #[derive(diesel::QueryableByName)]
        struct Completion {
            #[diesel(sql_type = Bool)]
            completed: bool,
        }

        let network = miden_network_config(network_type)?;
        let mut conn = self.pool.get().await.map_err(|error| error.to_string())?;
        diesel::sql_query(
            "SELECT EXISTS (
                 SELECT 1 FROM account_recovery_startup
                 WHERE network_config = $1 AND unresolved_accounts IS NOT NULL
             ) AS completed",
        )
        .bind::<Jsonb, _>(network)
        .get_result::<Completion>(&mut conn)
        .await
        .map(|row| row.completed)
        .map_err(|error| format!("Could not read startup recovery completion: {error}"))
    }

    pub(crate) async fn progress(
        &self,
        network_type: MidenNetworkType,
    ) -> Result<ScanProgress, String> {
        let network = miden_network_config(network_type)?;
        let mut conn = self.pool.get().await.map_err(|error| error.to_string())?;
        diesel::sql_query(
            "SELECT resume_after FROM account_recovery_startup WHERE network_config = $1",
        )
        .bind::<Jsonb, _>(network)
        .get_result::<ScanProgress>(&mut conn)
        .await
        .optional()
        .map(Option::unwrap_or_default)
        .map_err(|error| format!("Could not read startup recovery progress: {error}"))
    }

    pub(crate) async fn save_progress(
        &self,
        network_type: MidenNetworkType,
        progress: &ScanProgress,
        lease: &Lease,
    ) -> Result<(), String> {
        let network = miden_network_config(network_type)?;
        let written = self
            .write("record startup recovery progress", |conn| {
                async move {
                    diesel::sql_query(format!(
                        "INSERT INTO account_recovery_startup (network_config, resume_after)
             SELECT $4, $5 {HELD_LEASE}
             ON CONFLICT (network_config) DO UPDATE
                 SET resume_after = EXCLUDED.resume_after
                 WHERE account_recovery_startup.unresolved_accounts IS NULL",
                    ))
                    .bind::<Text, _>(&lease.name)
                    .bind::<Text, _>(&lease.holder_id)
                    .bind::<BigInt, _>(lease.fence_token)
                    .bind::<Jsonb, _>(network)
                    .bind::<Text, _>(&progress.resume_after)
                    .execute(conn)
                    .await
                }
                .scope_boxed()
            })
            .await?;
        if written == 0 {
            return Err("Account recovery lost its lease before recording scan progress".into());
        }
        Ok(())
    }

    pub(crate) async fn complete_startup(
        &self,
        network_type: MidenNetworkType,
        unresolved_accounts: i64,
        lease: &Lease,
    ) -> Result<(), String> {
        let network = miden_network_config(network_type)?;
        let written = self.write("record startup recovery completion", |conn| async move {
            diesel::sql_query(format!(
            "INSERT INTO account_recovery_startup (network_config, unresolved_accounts, completed_at)
             SELECT $4, $5, now() {HELD_LEASE}
             ON CONFLICT (network_config) DO UPDATE
                 SET completed_at = EXCLUDED.completed_at,
                     unresolved_accounts = EXCLUDED.unresolved_accounts,
                     resume_after = ''
                 WHERE account_recovery_startup.unresolved_accounts IS NULL",
        ))
        .bind::<Text, _>(&lease.name)
        .bind::<Text, _>(&lease.holder_id)
        .bind::<BigInt, _>(lease.fence_token)
        .bind::<Jsonb, _>(network)
        .bind::<BigInt, _>(unresolved_accounts)
            .execute(conn)
            .await
        }.scope_boxed()).await?;
        if written == 0 {
            return Err(
                "Account recovery lost its lease before recording startup completion".into(),
            );
        }
        Ok(())
    }

    pub(crate) async fn candidates(
        &self,
        network_type: MidenNetworkType,
        after: &str,
        limit: i64,
    ) -> Result<Vec<RecoveryCandidate>, String> {
        let network = miden_network_config(network_type)?;
        let mut conn = self.pool.get().await.map_err(|error| error.to_string())?;
        diesel::sql_query(format!(
            "SELECT st.account_id, st.nonce AS stored_nonce, st.commitment AS stored_commitment,
                    d.nonce AS canonical_nonce, ({UNSETTLED_ACCOUNT_WORK}) AS unsettled_work,
                    d.new_commitment AS canonical_commitment, st.updated_at AS state_updated_at,
                    m.auth_version,
                    first_held.nonce AS history_starts_at_nonce,
                    first_held.prev_commitment AS history_base_commitment
             {CANDIDATE_SOURCE}
             ORDER BY m.account_id
             LIMIT $3",
        ))
        .bind::<Text, _>(after)
        .bind::<Jsonb, _>(network)
        .bind::<BigInt, _>(limit)
        .load(&mut conn)
        .await
        .map_err(|error| format!("Could not list recovery candidates: {error}"))
    }

    pub(crate) async fn remaining_candidates(
        &self,
        network_type: MidenNetworkType,
        after: &str,
    ) -> Result<i64, String> {
        #[derive(diesel::QueryableByName)]
        struct Remaining {
            #[diesel(sql_type = BigInt)]
            remaining: i64,
        }

        let network = miden_network_config(network_type)?;
        let mut conn = self.pool.get().await.map_err(|error| error.to_string())?;
        diesel::sql_query(format!("SELECT count(*) AS remaining {CANDIDATE_SOURCE}"))
            .bind::<Text, _>(after)
            .bind::<Jsonb, _>(network)
            .get_result::<Remaining>(&mut conn)
            .await
            .map(|row| row.remaining)
            .map_err(|error| format!("Could not count remaining recovery candidates: {error}"))
    }

    pub(crate) async fn record_pending(
        &self,
        candidate: &RecoveryCandidate,
        outcome: PendingOutcome,
        reason: &str,
        lease: &Lease,
    ) -> Result<(), String> {
        let written = self
            .write("record the pending account", |conn| {
                async move {
                    diesel::sql_query(format!(
                        "INSERT INTO account_recovery_pending
                 (account_id, outcome, reason, stored_commitment, canonical_nonce,
                  canonical_commitment, history_starts_at_nonce, history_base_commitment)
             SELECT $4, $5, $6, $7, $8, $9, $10, $11 {HELD_LEASE}
             ON CONFLICT (account_id) DO UPDATE
                 SET outcome = EXCLUDED.outcome,
                     reason = EXCLUDED.reason,
                     stored_commitment = EXCLUDED.stored_commitment,
                     canonical_nonce = EXCLUDED.canonical_nonce,
                     canonical_commitment = EXCLUDED.canonical_commitment,
                     history_starts_at_nonce = EXCLUDED.history_starts_at_nonce,
                     history_base_commitment = EXCLUDED.history_base_commitment,
                     recorded_at = EXCLUDED.recorded_at",
                    ))
                    .bind::<Text, _>(&lease.name)
                    .bind::<Text, _>(&lease.holder_id)
                    .bind::<BigInt, _>(lease.fence_token)
                    .bind::<Text, _>(&candidate.account_id)
                    .bind::<Text, _>(outcome.label())
                    .bind::<Text, _>(reason)
                    .bind::<Text, _>(&candidate.stored_commitment)
                    .bind::<Nullable<BigInt>, _>(candidate.canonical_nonce)
                    .bind::<Nullable<Text>, _>(candidate.canonical_commitment.as_deref())
                    .bind::<Nullable<BigInt>, _>(candidate.history_starts_at_nonce)
                    .bind::<Nullable<Text>, _>(candidate.history_base_commitment.as_deref())
                    .execute(conn)
                    .await
                }
                .scope_boxed()
            })
            .await?;
        if written == 0 {
            return Err(
                "Account recovery lost its lease before recording the pending account".into(),
            );
        }
        Ok(())
    }

    pub(crate) async fn clear_pending(
        &self,
        account_id: &str,
        lease: &Lease,
    ) -> Result<(), String> {
        self.write("clear the pending account", |conn| {
            async move {
                diesel::sql_query(format!(
                    "DELETE FROM account_recovery_pending WHERE account_id = $4
                 AND EXISTS (SELECT 1 {HELD_LEASE})",
                ))
                .bind::<Text, _>(&lease.name)
                .bind::<Text, _>(&lease.holder_id)
                .bind::<BigInt, _>(lease.fence_token)
                .bind::<Text, _>(account_id)
                .execute(conn)
                .await
                .map(drop)
            }
            .scope_boxed()
        })
        .await
    }

    pub(crate) async fn apply(
        &self,
        candidate: &RecoveryCandidate,
        state_json: &Value,
        auth: &Auth,
        proposals: &ProposalReview,
        now: DateTime<Utc>,
        lease: &Lease,
    ) -> Result<bool, String> {
        let account_id = &candidate.account_id;
        let reviewed = match proposals {
            ProposalReview::Reviewed(reviewed) => Some(reviewed),
            ProposalReview::Unreadable => None,
        };
        let expected = reviewed.map(|reviewed| {
            let mut expected: Vec<(String, DeltaStatus)> = reviewed
                .iter()
                .map(|proposal| (proposal.commitment.clone(), proposal.status.clone()))
                .collect();
            expected.sort_by(|left, right| left.0.cmp(&right.0));
            expected
        });
        let writes = reviewed
            .into_iter()
            .flatten()
            .filter_map(|proposal| {
                self.proposal_write(account_id, proposal)
                    .map(|write| write.map(|write| (proposal.commitment.clone(), write)))
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let changed: Vec<String> = writes
            .iter()
            .map(|(commitment, _)| commitment.clone())
            .collect();
        let canonical_nonce = candidate
            .canonical_nonce
            .ok_or("Recovery target has no canonical nonce")?;
        let canonical_commitment = candidate
            .canonical_commitment
            .as_deref()
            .ok_or("Recovery target has no canonical commitment")?;
        let stored_json = match &self.cipher {
            Some(cipher) => cipher
                .encrypt(&RecordAad::State { account_id }, state_json)
                .map_err(|error| error.to_string())?,
            None => state_json.clone(),
        };
        let auth_json = serde_json::to_value(auth).map_err(|error| error.to_string())?;
        self.write("restore the account", |conn| {
            async move {
                if lock_account_metadata(conn, account_id)
                    .await
                    .optional()?
                    .is_none()
                    || !candidate
                        .lock_if_eligible(conn, canonical_nonce, canonical_commitment)
                        .await?
                {
                    return Ok(false);
                }

                let fence = LeaseFence {
                    lease_name: lease.name.clone(),
                    holder_id: lease.holder_id.clone(),
                    fence_token: lease.fence_token,
                };
                if !lease_fence_is_current(conn, &fence).await? {
                    return Ok(false);
                }
                if let Some(expected) = &expected
                    && pending_proposals(conn, account_id).await? != *expected
                {
                    return Ok(false);
                }

                diesel::sql_query(
                    "INSERT INTO account_recovery_backups
                         (account_id, prior_state, prior_auth, prior_proposals,
                          recovered_commitment)
                     SELECT st.account_id, to_jsonb(st), m.auth,
                            COALESCE((
                                SELECT jsonb_agg(to_jsonb(p) ORDER BY p.commitment)
                                FROM delta_proposals p
                                WHERE p.account_id = st.account_id AND p.commitment = ANY($3)
                            ), '[]'::jsonb),
                            $2
                     FROM states st JOIN account_metadata m USING (account_id)
                     WHERE st.account_id = $1",
                )
                .bind::<Text, _>(account_id)
                .bind::<Text, _>(canonical_commitment)
                .bind::<Array<Text>, _>(&changed)
                .execute(conn)
                .await?;

                for (commitment, write) in &writes {
                    let proposal = delta_proposals::table
                        .filter(delta_proposals::account_id.eq(account_id))
                        .filter(delta_proposals::commitment.eq(commitment));
                    match write {
                        ProposalWrite::Replace {
                            status,
                            delta_payload,
                        } => {
                            diesel::update(proposal)
                                .set((
                                    delta_proposals::status.eq(status),
                                    delta_proposals::delta_payload.eq(delta_payload),
                                ))
                                .execute(conn)
                                .await?
                        }
                        ProposalWrite::Remove => diesel::delete(proposal).execute(conn).await?,
                    };
                }

                let written_nonce = diesel::update(states::table.find(account_id))
                    .set((
                        states::commitment.eq(canonical_commitment),
                        states::state_json.eq(stored_json),
                        states::nonce.eq(Some(canonical_nonce)),
                        states::updated_at.eq(now),
                    ))
                    .returning(states::nonce)
                    .get_result::<Option<i64>>(conn)
                    .await?;
                if written_nonce.is_none() {
                    diesel::update(states::table.find(account_id))
                        .set(states::nonce.eq(Some(canonical_nonce)))
                        .execute(conn)
                        .await?;
                }

                diesel::update(account_metadata::table.find(account_id))
                    .set((
                        account_metadata::auth.eq(auth_json),
                        account_metadata::updated_at.eq(now),
                    ))
                    .execute(conn)
                    .await?;
                Ok(true)
            }
            .scope_boxed()
        })
        .await
    }

    /// The row write for a reviewed proposal, with its payload encrypted as the storage layer
    /// stores it.
    fn proposal_write(
        &self,
        account_id: &str,
        proposal: &ReviewedProposal,
    ) -> Result<Option<ProposalWrite>, String> {
        let (status, delta_payload) = match &proposal.change {
            ProposalChange::Keep => return Ok(None),
            ProposalChange::Remove => return Ok(Some(ProposalWrite::Remove)),
            ProposalChange::DropApprovals {
                status,
                delta_payload,
            } => (status, delta_payload),
        };
        let commitment = normalize_commitment_hex(&proposal.commitment)
            .unwrap_or_else(|_| proposal.commitment.clone());
        let delta_payload = match &self.cipher {
            Some(cipher) => cipher
                .encrypt(
                    &RecordAad::Proposal {
                        account_id,
                        commitment: &commitment,
                    },
                    delta_payload,
                )
                .map_err(|error| error.to_string())?,
            None => delta_payload.clone(),
        };
        Ok(Some(ProposalWrite::Replace {
            status: serde_json::to_value(status).map_err(|error| error.to_string())?,
            delta_payload,
        }))
    }

    async fn write<'a, T, F>(&self, operation: &str, work: F) -> Result<T, String>
    where
        T: Send + 'a,
        F: for<'conn> FnOnce(
                &'conn mut AsyncPgConnection,
            ) -> ScopedBoxFuture<'a, 'conn, QueryResult<T>>
            + Send
            + 'a,
    {
        let mut conn = timeout(Duration::from_secs(3), self.pool.get())
            .await
            .map_err(|_| format!("Timed out waiting for a connection to {operation}"))?
            .map_err(|error| format!("Could not get a connection to {operation}: {error}"))?;
        AsyncConnection::transaction(&mut *conn, |conn| {
            async move {
                conn.batch_execute(
                    "SET LOCAL statement_timeout = '2s'; SET LOCAL lock_timeout = '2s';",
                )
                .await?;
                work(conn).await
            }
            .scope_boxed()
        })
        .await
        .map_err(|error| format!("Could not {operation}: {error}"))
    }
}

/// The account's pending proposals and their statuses, locked for the transaction and read
/// the way the storage layer reads them.
async fn pending_proposals(
    conn: &mut AsyncPgConnection,
    account_id: &str,
) -> QueryResult<Vec<(String, DeltaStatus)>> {
    #[derive(diesel::QueryableByName)]
    struct PendingProposal {
        #[diesel(sql_type = Text)]
        commitment: String,
        #[diesel(sql_type = Jsonb)]
        status: Value,
    }

    let mut pending: Vec<(String, DeltaStatus)> = diesel::sql_query(
        "SELECT commitment, status FROM delta_proposals
         WHERE account_id = $1 AND status_kind = 'pending'
         FOR UPDATE",
    )
    .bind::<Text, _>(account_id)
    .load::<PendingProposal>(conn)
    .await?
    .into_iter()
    .filter_map(|proposal| {
        serde_json::from_value::<DeltaStatus>(proposal.status)
            .ok()
            .filter(DeltaStatus::is_pending)
            .map(|status| (proposal.commitment, status))
    })
    .collect();
    pending.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(pending)
}

fn miden_network_config(network_type: MidenNetworkType) -> Result<Value, String> {
    serde_json::to_value(NetworkConfig::Miden { network_type }).map_err(|error| error.to_string())
}

#[derive(diesel::QueryableByName)]
struct LockedState {
    #[diesel(sql_type = Text, column_name = account_id)]
    _account_id: String,
}

impl RecoveryCandidate {
    async fn lock_if_eligible(
        &self,
        conn: &mut AsyncPgConnection,
        canonical_nonce: i64,
        canonical_commitment: &str,
    ) -> QueryResult<bool> {
        diesel::sql_query(format!(
            "SELECT st.account_id
             FROM states st JOIN account_metadata m USING (account_id)
             WHERE st.account_id = $1 AND st.nonce IS NOT DISTINCT FROM $2 AND st.commitment = $3
               AND st.updated_at = $4 AND m.auth_version = $5
               AND m.released_at IS NULL AND ({UNSETTLED_ACCOUNT_WORK}) IS NULL
               AND ($6, $7) = (
                   SELECT d.nonce, d.new_commitment FROM deltas d
                   WHERE d.account_id = st.account_id AND d.status_kind = 'canonical'
                   ORDER BY d.nonce DESC LIMIT 1
               )
             FOR UPDATE OF st",
        ))
        .bind::<Text, _>(&self.account_id)
        .bind::<Nullable<BigInt>, _>(self.stored_nonce)
        .bind::<Text, _>(&self.stored_commitment)
        .bind::<Timestamptz, _>(self.state_updated_at)
        .bind::<BigInt, _>(self.auth_version)
        .bind::<BigInt, _>(canonical_nonce)
        .bind::<Text, _>(canonical_commitment)
        .get_result::<LockedState>(conn)
        .await
        .optional()
        .map(|row| row.is_some())
    }
}
