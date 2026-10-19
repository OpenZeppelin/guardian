----------------------------- MODULE WorkerLease -----------------------------
(***************************************************************************)
(* The Postgres worker lease (issue #242) and the fenced writes it guards, *)
(* checked against every interleaving of replicas acquiring, renewing,     *)
(* stalling and writing while the database clock advances.                 *)
(*                                                                         *)
(* One step is one SQL statement or one transaction step. A fenced write   *)
(* is the shape every fenced storage write has (storage/postgres.rs        *)
(* promote_candidate, discard_candidate, update_candidate_status; the      *)
(* stats store's mark_refresh_started): take the account lock, check the   *)
(* fence, write, commit. Paths are relative to crates/server/src/.         *)
(***************************************************************************)
EXTENDS Naturals, Sequences

CONSTANTS
    Replicas,   \* server processes; a restarted process is a new replica (fresh holder_id)
    TTL,        \* lease_ttl in clock ticks (3 x renew interval in the code)
    MaxTime,    \* bound on the database clock
    MaxWrites   \* bound on committed fenced writes

VARIABLES
    now,        \* the database clock (now() / clock_timestamp())
    row,        \* the worker_leases row: [holder, token, expires], or NoRow
    rpc,        \* per replica: "idle", "pass", "locked", "checked"
    token,      \* per replica: fence_token of the lease its current pass holds
    believed,   \* per replica: expiry it last obtained (acquire or renew)
    cancelled,  \* per replica: the pass's cancellation token was tripped
    errs,       \* per replica: consecutive renew store errors
    lock,       \* the account lock (lock_account_metadata): "none" or a replica
    log         \* committed fenced writes, in commit order: [by, token]

vars == <<now, row, rpc, token, believed, cancelled, errs, lock, log>>

NoRow == [holder |-> "none", token |-> 0, expires |-> 0]

Init ==
    /\ now = 0
    /\ row = NoRow
    /\ rpc = [r \in Replicas |-> "idle"]
    /\ token = [r \in Replicas |-> 0]
    /\ believed = [r \in Replicas |-> 0]
    /\ cancelled = [r \in Replicas |-> FALSE]
    /\ errs = [r \in Replicas |-> 0]
    /\ lock = "none"
    /\ log = <<>>

(* The clock advances whether or not anyone acts: a replica that takes no  *)
(* step while time passes is a stalled or partitioned replica.             *)
Tick ==
    /\ now < MaxTime
    /\ now' = now + 1
    /\ UNCHANGED <<row, rpc, token, believed, cancelled, errs, lock, log>>

(* coordination/postgres/lease.rs try_acquire: one INSERT ... ON CONFLICT  *)
(* DO UPDATE ... WHERE expires_at < now() OR holder_id = EXCLUDED.holder_id.*)
(* The token advances only on a change of holder. Called by run_worker     *)
(* (jobs/canonicalization/worker.rs) at the start of every pass.           *)
StealAllowed(r) == row.expires < now \/ row.holder = r

Acquire(r) ==
    /\ rpc[r] = "idle"
    /\ \/ /\ row = NoRow
          /\ row' = [holder |-> r, token |-> 0, expires |-> now + TTL]
       \/ /\ row # NoRow
          /\ StealAllowed(r)
          /\ row' = [holder |-> r,
                     token |-> IF row.holder = r THEN row.token ELSE row.token + 1,
                     expires |-> now + TTL]
    /\ rpc' = [rpc EXCEPT ![r] = "pass"]
    /\ token' = [token EXCEPT ![r] = row'.token]
    /\ believed' = [believed EXCEPT ![r] = now + TTL]
    /\ cancelled' = [cancelled EXCEPT ![r] = FALSE]
    /\ errs' = [errs EXCEPT ![r] = 0]
    /\ UNCHANGED <<now, lock, log>>

(* lease.rs renew from worker.rs spawn_renewal, concurrent with the pass:  *)
(* a definitive loss cancels; a store error is tolerated once, the second  *)
(* consecutive one cancels.                                                *)
RenewHeld(r) ==
    /\ rpc[r] # "idle"
    /\ ~cancelled[r]
    /\ row.holder = r /\ row.token = token[r] /\ now < row.expires
    /\ row' = [row EXCEPT !.expires = now + TTL]
    /\ believed' = [believed EXCEPT ![r] = now + TTL]
    /\ errs' = [errs EXCEPT ![r] = 0]
    /\ UNCHANGED <<now, rpc, token, cancelled, lock, log>>

RenewLost(r) ==
    /\ rpc[r] # "idle"
    /\ ~cancelled[r]
    /\ ~(row.holder = r /\ row.token = token[r] /\ now < row.expires)
    /\ cancelled' = [cancelled EXCEPT ![r] = TRUE]
    /\ UNCHANGED <<now, row, rpc, token, believed, errs, lock, log>>

RenewError(r) ==
    /\ rpc[r] # "idle"
    /\ ~cancelled[r]
    /\ errs' = [errs EXCEPT ![r] = errs[r] + 1]
    /\ cancelled' = [cancelled EXCEPT ![r] = (errs[r] + 1 >= 2)]
    /\ UNCHANGED <<now, row, rpc, token, believed, lock, log>>

(* A fenced write, step 1: BEGIN and lock_account_metadata (FOR UPDATE).   *)
(* Cancellation is only polled between candidates, so a write may still    *)
(* start after the token tripped.                                          *)
Lock(r) ==
    /\ rpc[r] = "pass"
    /\ Len(log) < MaxWrites
    /\ lock = "none"
    /\ lock' = r
    /\ rpc' = [rpc EXCEPT ![r] = "locked"]
    /\ UNCHANGED <<now, row, token, believed, cancelled, errs, log>>

(* Step 2: storage/postgres.rs lease_fence_is_current, a plain SELECT on   *)
(* worker_leases with clock_timestamp() < expires_at. A stale lease rolls  *)
(* the write back (StaleLease) and releases the lock.                      *)
FenceOk(r) == row.holder = r /\ row.token = token[r] /\ now < row.expires

FenceCheck(r) ==
    /\ rpc[r] = "locked"
    /\ IF FenceOk(r)
       THEN /\ rpc' = [rpc EXCEPT ![r] = "checked"]
            /\ UNCHANGED lock
       ELSE /\ rpc' = [rpc EXCEPT ![r] = "pass"]
            /\ lock' = "none"
    /\ UNCHANGED <<now, row, token, believed, cancelled, errs, log>>

(* Step 3: the conditional write and COMMIT, which releases the lock. The  *)
(* lease may have expired, and even been stolen, since step 2.             *)
Commit(r) ==
    /\ rpc[r] = "checked"
    /\ log' = Append(log, [by |-> r, token |-> token[r]])
    /\ lock' = "none"
    /\ rpc' = [rpc EXCEPT ![r] = "pass"]
    /\ UNCHANGED <<now, row, token, believed, cancelled, errs>>

(* The pass ends (finished, or cancelled at a checkpoint); run_worker then *)
(* cancels and awaits the renewal task. The lease is not released: nothing *)
(* calls release in production.                                            *)
EndPass(r) ==
    /\ rpc[r] = "pass"
    /\ rpc' = [rpc EXCEPT ![r] = "idle"]
    /\ UNCHANGED <<now, row, token, believed, cancelled, errs, lock, log>>

Next ==
    \/ Tick
    \/ \E r \in Replicas :
         \/ Acquire(r) \/ RenewHeld(r) \/ RenewLost(r) \/ RenewError(r)
         \/ Lock(r) \/ FenceCheck(r) \/ Commit(r) \/ EndPass(r)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Properties *)

(* At most one replica runs a pass on a lease it believes is unexpired.    *)
(* "At most one replica holds a named lease at a time" (worker_leases      *)
(* migration, horizontal-scaling runbook).                                 *)
SingleBelievedHolder ==
    \A r1, r2 \in Replicas :
        (r1 # r2 /\ rpc[r1] # "idle" /\ rpc[r2] # "idle")
            => ~(believed[r1] > now /\ believed[r2] > now)

(* Fenced writes commit in fence-token order: once a newer holder has      *)
(* committed a write, no write from a superseded holder commits after it.  *)
(* This is what lets every fenced write assume it is not racing an older   *)
(* leader's decision.                                                      *)
WritesInTokenOrder ==
    \A i, j \in DOMAIN log : i < j => log[i].token <= log[j].token

(* Writes under one token come from one replica. *)
OneHolderPerToken ==
    \A i, j \in DOMAIN log : log[i].token = log[j].token => log[i].by = log[j].by

(* The token never goes back, and every change of holder advances it. *)
TokenAdvancesOnSteal ==
    [][row # NoRow =>
         /\ row'.token >= row.token
         /\ (row'.holder # row.holder => row'.token > row.token)]_vars

=============================================================================
