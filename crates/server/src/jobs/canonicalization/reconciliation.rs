//! Background reconciliation of recoverable deltas (issue #345).
//!
//! Recoverable rows — `retained` deltas and recent client-abandoned
//! discards (the late-landing net for issue #319's abandon quarantine) —
//! are swept by a dedicated [`ProcessingMode::ReconcileRecoverable`]
//! pass, on its own cadence, never inside the full candidate pass. The
//! sweep is account-oriented and cheap by construction:
//!
//! 1. Expired retained rows are dropped first, without network work.
//! 2. One chain probe per due account decides everything else: if the
//!    chain still sits at the stored base, nothing can have landed and
//!    no reconstruction runs at all.
//! 3. Only when the chain moved past the stored base is the matching
//!    row — located via its submission-computed `new_commitment` —
//!    reconstructed and validated before the fenced promotion.
//!
//! Accounts are additionally backed off as their recoverable rows age
//! (see [`reconcile_backoff_seconds`]), and one pass visits at most
//! `reconcile_page_size` accounts under a rotation cursor, so a
//! correlated backlog (e.g. after a node outage) drains across passes
//! instead of thundering back all at once.

use super::*;

/// For this long after a row becomes recoverable it is probed on every
/// reconcile tick — a late-landing transaction usually surfaces within
/// minutes.
pub(super) const RECONCILE_WARM_PERIOD_SECONDS: u64 = 900;

/// Probe spacing never grows beyond this, so even day-old rows are
/// still reconsidered a few times an hour until the TTL bounds them out.
pub(super) const RECONCILE_MAX_BACKOFF_SECONDS: u64 = 600;

/// First spacing step after the warm period; doubles from here.
pub(super) const RECONCILE_BACKOFF_BASE_SECONDS: u64 = 60;

/// Probe spacing for a recoverable row of this age: every tick during
/// the warm period (returns 0), then doubling per elapsed warm period,
/// capped at [`RECONCILE_MAX_BACKOFF_SECONDS`].
pub(super) fn reconcile_backoff_seconds(age_seconds: u64) -> u64 {
    if age_seconds < RECONCILE_WARM_PERIOD_SECONDS {
        return 0;
    }
    let doublings = u32::try_from(age_seconds / RECONCILE_WARM_PERIOD_SECONDS)
        .unwrap_or(u32::MAX)
        .min(32);
    RECONCILE_BACKOFF_BASE_SECONDS
        .saturating_mul(2u64.saturating_pow(doublings))
        .min(RECONCILE_MAX_BACKOFF_SECONDS)
}

/// Whether an account whose *youngest* recoverable row has this age is
/// due on the current tick. Stateless by design: the schedule derives
/// entirely from the row's age, so it survives restarts and lease
/// failover with no persisted cursor, every replica computes the same
/// answer, and a missed tick self-corrects at the next grid point.
pub(super) fn reconcile_due(age_seconds: u64, interval_seconds: u64) -> bool {
    let backoff = reconcile_backoff_seconds(age_seconds);
    if backoff == 0 {
        return true;
    }
    age_seconds % backoff < interval_seconds.max(1)
}

impl DeltasProcessorBase {
    fn recoverable_age_seconds(&self, delta: &DeltaObject, now: DateTime<Utc>) -> Option<u64> {
        let timestamp = match &delta.status {
            DeltaStatus::Retained { timestamp, .. } => timestamp,
            DeltaStatus::Discarded { timestamp, .. } if delta.status.is_client_abandoned() => {
                timestamp
            }
            _ => return None,
        };

        let recoverable_at = DateTime::parse_from_rfc3339(timestamp).ok()?;
        let age = now.signed_duration_since(recoverable_at.with_timezone(&Utc));
        Some(age.num_seconds().max(0) as u64)
    }

    /// The oldest client-abandoned discard timestamp still in scope for
    /// reconciliation: abandoned rows are kept as history forever, so
    /// the TTL bounds the scan, not their lifetime.
    fn abandoned_cutoff(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        // An unrepresentable TTL (e.g. the test processor's u64::MAX)
        // means "no cutoff": everything since the epoch is in scope.
        i64::try_from(self.retained_ttl_seconds)
            .ok()
            .and_then(chrono::Duration::try_seconds)
            .and_then(|ttl| now.checked_sub_signed(ttl))
            .unwrap_or(DateTime::<Utc>::MIN_UTC)
    }

    /// One reconcile pass: list accounts with recoverable rows, visit at
    /// most one page of them (rotation cursor for fairness), one account
    /// probe each. Bounded by the pass deadline the worker derives from
    /// the next full-pass tick, so reconciliation can never delay
    /// ordinary candidate processing.
    pub(super) async fn process_reconcile_pass(&self) -> Result<PassSummary> {
        let started = Instant::now();
        let cutoff = self.abandoned_cutoff(self.state.clock.now());
        let mut account_ids = self
            .state
            .storage
            .list_accounts_with_recoverable_deltas(cutoff)
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!(
                    "Failed to list accounts with recoverable deltas: {e}"
                ))
            })?;
        if account_ids.is_empty() {
            self.reconcile_state.set_cursor(None);
            return Ok(self.pass_summary(0, 0));
        }
        account_ids.sort();
        account_ids.dedup();
        let total = account_ids.len();

        // Fair rotation under the page bound: start after the account
        // the previous pass ended on, wrapping around, so a backlog
        // larger than one page drains breadth-first across passes.
        if let Some(cursor) = self.reconcile_state.cursor() {
            let start = account_ids
                .iter()
                .position(|account_id| *account_id > cursor)
                .unwrap_or(0);
            account_ids.rotate_left(start);
        }
        let page: Vec<String> = account_ids
            .into_iter()
            .take(self.reconcile_page_size.max(1) as usize)
            .collect();
        let truncated = total > page.len();
        self.reconcile_state
            .set_cursor(truncated.then(|| page.last().cloned()).flatten());
        if truncated {
            tracing::debug!(
                total,
                page = page.len(),
                "Recoverable backlog exceeds one reconcile page; \
                 remaining accounts continue on the next pass"
            );
        }

        let accounts = page.len();
        let failed_accounts = futures::stream::iter(page)
            .map(|account_id| async move { self.reconcile_account_absorbing(&account_id).await })
            .buffer_unordered(self.max_concurrent_accounts.max(1))
            .fold(0, |failed, account_failed| async move {
                failed + usize::from(account_failed)
            })
            .await;

        tracing::debug!(
            total,
            accounts,
            failed_accounts,
            deadline_reached = self.pass_deadline_reached(),
            duration_seconds = started.elapsed().as_secs_f64(),
            "Reconcile pass completed"
        );

        Ok(self.pass_summary(accounts, failed_accounts))
    }

    async fn reconcile_account_absorbing(&self, account_id: &str) -> bool {
        if self.admission_closed() {
            return false;
        }
        match self.reconcile_account(account_id).await {
            Ok(()) => false,
            Err(e) => {
                tracing::error!(
                    account_id = %account_id,
                    error = %e,
                    "Failed to reconcile recoverable deltas for account"
                );
                true
            }
        }
    }

    /// Reconcile one account's recoverable deltas. Only acts while the
    /// account has no candidate in flight: a reconcile promotion moves
    /// the stored base, and doing that under an in-flight candidate
    /// would strand a signed proposal — the exact race issue #345
    /// rejects for a syncState-driven write. The check is read-then-act,
    /// so a candidate submitted after it can still see the base move
    /// under it — but such a candidate was never viable: a reconcile
    /// only promotes when the chain already sits past the stored base,
    /// so any candidate built on that base was doomed on-chain before it
    /// was submitted, and the promotion moves the base to where the
    /// client must rebuild anyway.
    ///
    /// Deliberately NOT gated on `paused_at` / `released_at`, matching
    /// the candidate pass: pause and release gate *client* mutations at
    /// the API chokepoint, while the worker records chain truth — a
    /// promotion only happens when the chain already holds the delta's
    /// commitment. Gating on `released_at` would also strand a retained
    /// switch-guardian delta, whose reconciliation is exactly what
    /// releases the account correctly.
    async fn reconcile_account(&self, account_id: &str) -> Result<()> {
        let candidates = self
            .state
            .storage
            .pull_candidate_deltas(account_id)
            .await
            .map_err(|e| GuardianError::StorageError(format!("Failed to pull deltas: {e}")))?;
        if !candidates.is_empty() {
            return Ok(());
        }

        let now = self.state.clock.now();
        let cutoff = self.abandoned_cutoff(now);
        let recoverable = self
            .state
            .storage
            .pull_recoverable_deltas(account_id, cutoff)
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to pull recoverable deltas: {e}"))
            })?;
        if recoverable.is_empty() {
            return Ok(());
        }

        // Expiry first, before any network work: an unreadable retention
        // timestamp counts as expired — the TTL exists so no row can
        // outlive the feature's bound, and a row whose age cannot be
        // established must not become immortal. Past-TTL abandoned rows
        // normally never reach here (the read is cutoff-filtered); if
        // one does, it is skipped, not deleted — abandoned discards are
        // preserved history (issue #319).
        let mut live: Vec<(u64, DeltaObject)> = Vec::new();
        let mut first_error: Option<GuardianError> = None;
        for delta in recoverable {
            let age = self.recoverable_age_seconds(&delta, now);
            if age.is_none_or(|age| age >= self.retained_ttl_seconds) {
                if delta.status.is_client_abandoned() {
                    continue;
                }
                tracing::warn!(
                    account_id = %delta.account_id,
                    nonce = delta.nonce,
                    retained_age_seconds = age,
                    retained_ttl_seconds = self.retained_ttl_seconds,
                    "Retained delta expired without ever verifying; dropping it"
                );
                match self
                    .remove_delta(&delta, DeltaStatusKind::Retained, &now.to_rfc3339())
                    .await
                {
                    Ok(CanonicalWrite::Applied) => {
                        record_candidate_outcome(
                            crate::metrics::labels::CandidateOutcome::ReconcileExpired,
                        );
                    }
                    Ok(CanonicalWrite::StaleLease) => return Err(Self::stale_lease_error(&delta)),
                    Ok(CanonicalWrite::NotCandidate) => {
                        Self::log_not_candidate(&delta, "reconcile_expiry");
                    }
                    Err(e) => {
                        tracing::error!(
                            account_id = %delta.account_id,
                            nonce = delta.nonce,
                            error = %e,
                            "Failed to drop expired retained delta"
                        );
                        first_error.get_or_insert(e);
                    }
                }
                continue;
            }
            live.push((age.unwrap_or(0), delta));
        }
        let Some(min_age) = live.iter().map(|(age, _)| *age).min() else {
            return first_error.map_or(Ok(()), Err);
        };

        // Age-derived backoff: the youngest recoverable row sets the
        // account's probe cadence. Disabled for the test processor so
        // process-now endpoints reconcile deterministically.
        if self.reconcile_backoff && !reconcile_due(min_age, self.reconcile_interval_seconds) {
            return first_error.map_or(Ok(()), Err);
        }

        // Retry proposal cleanup for retained rows: `retain_candidate`
        // deletes the matching proposal after its committed status flip,
        // and a failed delete there would otherwise strand the proposal
        // as `pending` forever once a resubmission supersedes the row.
        for (_, delta) in &live {
            if delta.status.is_retained() {
                let _ = self.delete_matching_proposal(delta).await;
            }
        }

        // One chain probe decides the rest: while the chain still sits
        // at the stored base (or has no state for the account at all),
        // no recoverable delta can have landed, so the expensive
        // reconstruction path is skipped entirely.
        let current_state = self
            .state
            .storage
            .pull_state(account_id)
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to get current state: {e}"))
            })?;
        let verification = self
            .state
            .network_client
            .verify_commitment(account_id, &current_state.commitment)
            .await;
        let result = match verification {
            Ok(StateVerification::Match) | Ok(StateVerification::Absent) => {
                tracing::debug!(
                    account_id = %account_id,
                    recoverable = live.len(),
                    "Chain still at the stored base; nothing to reconcile"
                );
                record_candidate_outcome(
                    crate::metrics::labels::CandidateOutcome::ReconcileDeferred,
                );
                Ok(())
            }
            Err(e) => {
                tracing::info!(
                    account_id = %account_id,
                    error = %e,
                    "Chain probe unavailable; deferring reconciliation"
                );
                record_candidate_outcome(
                    crate::metrics::labels::CandidateOutcome::ReconcileDeferred,
                );
                Ok(())
            }
            Ok(StateVerification::Mismatch { on_chain }) => {
                self.reconcile_chain_advance(account_id, current_state, &on_chain, live)
                    .await
            }
        };

        match (result, first_error) {
            (Err(e), _) => Err(e),
            (Ok(()), Some(e)) => Err(e),
            (Ok(()), None) => Ok(()),
        }
    }

    /// The chain moved past the stored base: locate the recoverable row
    /// whose end state is the observed on-chain commitment — the
    /// submission-computed `new_commitment` is the lookup hint — and
    /// reconstruct exactly that path before the fenced promotion. The
    /// hint alone never promotes: the reconstruction must reproduce the
    /// observed commitment from the stored base. Rows without a stored
    /// hint fall back to reconstruct-and-compare.
    async fn reconcile_chain_advance(
        &self,
        account_id: &str,
        current_state: StateObject,
        on_chain: &str,
        live: Vec<(u64, DeltaObject)>,
    ) -> Result<()> {
        for (_, delta) in live {
            // Only a row chaining from the stored base can apply to it.
            if delta.prev_commitment != current_state.commitment {
                continue;
            }
            // A stored end-state that differs from the chain head means
            // this row is not what landed; a genuinely diverged row can
            // never start matching, so it ages out through the TTL.
            if let Some(hint) = &delta.new_commitment
                && hint != on_chain
            {
                record_candidate_outcome(
                    crate::metrics::labels::CandidateOutcome::ReconcileDeferred,
                );
                continue;
            }

            // A base the delta no longer applies to (e.g. the client
            // re-supplied state via configure) is a deferral, not an
            // account failure: the row ages out through the TTL.
            let applied = {
                let client = self.state.network_client.clone();
                let prev_state_json = current_state.state_json.clone();
                let delta_payload = Arc::new(delta.delta_payload.clone());
                crate::network::reconstructor()
                    .run_background(move || client.apply_delta(&prev_state_json, &delta_payload))
                    .await
            };
            let (new_state_json, recomputed_commitment) = match applied {
                Ok(applied) => applied,
                Err(e) => {
                    tracing::info!(
                        account_id = %delta.account_id,
                        nonce = delta.nonce,
                        error = %GuardianError::from(e),
                        "Recoverable delta no longer applies to the stored base; \
                         deferring until it expires"
                    );
                    record_candidate_outcome(
                        crate::metrics::labels::CandidateOutcome::ReconcileDeferred,
                    );
                    continue;
                }
            };
            if recomputed_commitment != on_chain {
                tracing::info!(
                    account_id = %delta.account_id,
                    nonce = delta.nonce,
                    "Recoverable delta reconstructs to a commitment the chain \
                     does not show; deferring"
                );
                record_candidate_outcome(
                    crate::metrics::labels::CandidateOutcome::ReconcileDeferred,
                );
                continue;
            }

            tracing::info!(
                account_id = %delta.account_id,
                nonce = delta.nonce,
                "Recoverable delta now verifies against the on-chain \
                 commitment; promoting the recovered state"
            );
            return self
                .canonicalize_verified_delta(
                    delta,
                    new_state_json,
                    recomputed_commitment,
                    crate::metrics::labels::CandidateOutcome::Reconciled,
                )
                .await;
        }

        // The chain advanced somewhere none of the recoverable rows
        // reach (e.g. an externally-driven state move); the rows age
        // out through the TTL.
        tracing::info!(
            account_id = %account_id,
            on_chain = %on_chain,
            "Chain advanced past the stored base but no recoverable delta \
             matches it; deferring"
        );
        record_candidate_outcome(crate::metrics::labels::CandidateOutcome::ReconcileDeferred);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_probes_every_tick_during_warm_period() {
        assert_eq!(reconcile_backoff_seconds(0), 0);
        assert_eq!(
            reconcile_backoff_seconds(RECONCILE_WARM_PERIOD_SECONDS - 1),
            0
        );
        assert!(reconcile_due(0, 60));
        assert!(reconcile_due(899, 60));
    }

    #[test]
    fn backoff_doubles_with_age_and_caps() {
        // 15–30 min: 120s spacing; 30–45 min: 240s; 45–60 min: 480s;
        // beyond an hour the cap holds at 600s.
        assert_eq!(reconcile_backoff_seconds(900), 120);
        assert_eq!(reconcile_backoff_seconds(1800), 240);
        assert_eq!(reconcile_backoff_seconds(2700), 480);
        assert_eq!(reconcile_backoff_seconds(3600), 600);
        assert_eq!(reconcile_backoff_seconds(86_400), 600);
        assert_eq!(reconcile_backoff_seconds(u64::MAX), 600);
    }

    #[test]
    fn due_matches_spacing_grid() {
        // 120s spacing at 60s ticks: due on every other tick.
        assert!(reconcile_due(960, 60));
        assert!(!reconcile_due(1020, 60));
        assert!(reconcile_due(1080, 60));
        // Capped spacing: due roughly once per 10 minutes.
        let due_ticks = (0..60)
            .map(|i| 7200 + i * 60)
            .filter(|age| reconcile_due(*age, 60))
            .count();
        assert_eq!(due_ticks, 6, "one probe per 10 minutes over an hour");
    }
}
