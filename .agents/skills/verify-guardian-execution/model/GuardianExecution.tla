-------------------------- MODULE GuardianExecution --------------------------
(***************************************************************************)
(* Guardian execution of threshold-met proposals (issue #254), Postgres    *)
(* backend. One account at base state "B", proposals that each move it to  *)
(* their own target, the per-account execution lease, the reservation and  *)
(* submission rows, the canonicalization worker, the execution reconciler  *)
(* on every replica, and the chain.                                        *)
(*                                                                         *)
(* One model step is one Postgres transaction (every execution write takes *)
(* lock_account_metadata, then checks the lease fence), one statement      *)
(* outside a transaction (lease acquire, renew, release), or one RPC read. *)
(* Paths below are relative to crates/server/src/.                         *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    Workers,        \* request handlers; each runs at most one attempt
    Reconcilers,    \* one reconciler holder per replica ("<replica>:reconcile")
    Proposals,      \* guardian-executable proposals pinned to "B"
    Expiry,         \* transaction expiration = reference block + Expiry
    MaxBlock,       \* chain height bound
    MaxLapses,      \* budget of reconciler-lease and reservation-deadline lapses
    MaxTok,         \* fence-token bound (state constraint)
    ParkedClient,   \* a retained client delta at the next nonce whose tx can still land
    ClientPushes,   \* client push_delta calls allowed
    ExternalMoves   \* chain moves that need no ack from this Guardian (guardian switch)

None == "none"
Base == "B"
ClientTarget == "C"
Moved == "X"
Target(p) == "E_" \o p

ClientTx == [kind |-> "client", w |-> None, p |-> None, target |-> ClientTarget, exp |-> MaxBlock]

NoRes == [used |-> FALSE, p |-> None, holder |-> None, tok |-> 0, live |-> FALSE,
          released |-> FALSE, ev |-> FALSE, ref |-> 0, exp |-> 0, outcome |-> None]

NoCand == [kind |-> None, target |-> None, w |-> None]

VARIABLES
    hist,       \* chain: hist[b + 1] is the account state as of block b
    mempool,    \* transactions submitted and not landed
    landed,     \* transactions that landed
    stored,     \* states.commitment
    cand,       \* the candidate delta at the next nonce (depth 1)
    canon,      \* target of the canonical delta at the next nonce, or None
    parked,     \* retained rows at the next nonce, or their superseded_deltas copies
    proposals,  \* delta_proposals rows
    res,        \* execution_reservations + execution_submissions + execution_outcomes, per attempt
    lease,      \* the worker_leases row "execution:<account>"
    wpc, wp, wtok, wref,
    rpc, rsnap, rtok, rverdict,
    ppc, psnap,
    lapses, pushes, moves

chainVars == <<hist, mempool, landed>>
storeVars == <<stored, cand, canon, parked, proposals, res, lease>>
workerVars == <<wpc, wp, wtok, wref>>
rcVars == <<rpc, rsnap, rtok, rverdict>>
procVars == <<ppc, psnap>>
budgetVars == <<lapses, pushes, moves>>
vars == <<chainVars, storeVars, workerVars, rcVars, procVars, budgetVars>>

Tip == Len(hist) - 1
At(b) == hist[b + 1]
Blocks == 0..Tip

Active == {w \in Workers : res[w].used /\ ~res[w].released}

-----------------------------------------------------------------------------
(* Lease statements: coordination/postgres/lease.rs                         *)

LeaseCurrent(h, t) == lease.holder = h /\ lease.tok = t /\ lease.live

\* try_acquire: INSERT ... ON CONFLICT DO UPDATE ... WHERE expires_at < now() OR
\* same holder; the token advances only on a change of holder.
CanAcquire(h) == ~lease.live \/ lease.holder = h
AcquiredTok(h) == IF lease.holder = h \/ lease.holder = None THEN lease.tok ELSE lease.tok + 1
Acquire(h) == lease' = [holder |-> h, tok |-> AcquiredTok(h), live |-> TRUE]

\* release: UPDATE ... SET expires_at = now() WHERE holder and token match.
ReleaseOf(h, t) == IF lease.holder = h /\ lease.tok = t
                   THEN [lease EXCEPT !.live = FALSE] ELSE lease

\* storage/postgres_execution.rs owns_live_reservation: the reservation's
\* fence is the caller's and lease_fence_is_current holds.
OwnsLive(a, h, t) == res[a].holder = h /\ res[a].tok = t /\ LeaseCurrent(h, t)

-----------------------------------------------------------------------------
(* resolve_execution_tx (resolve_execution with boundary = TRUE,            *)
(* fail_execution with boundary = FALSE): the attempt's own row, no outcome,*)
(* authorizes, owns_live, then the boundary side; post-boundary it deletes  *)
(* the candidate at the evidence nonce and the proposal.                    *)

ResolveOk(a, h, t, boundary) ==
    /\ res[a].used
    /\ res[a].outcome = None
    /\ res[a].holder = h
    /\ OwnsLive(a, h, t)
    /\ res[a].ev = boundary

ResolveEffect(a) ==
    /\ res' = [res EXCEPT ![a].outcome = "failed", ![a].released = TRUE]
    /\ cand' = IF res[a].ev THEN NoCand ELSE cand
    /\ proposals' = IF res[a].ev THEN proposals \ {res[a].p} ELSE proposals

\* resolve_execution for SubmissionNotProcessed: the node refused the send
\* without processing it, so the candidate goes and the proposal stays
\* executable (ExecutionFailureCode::ends_proposal is false for it).
ResolveKeepingProposal(a) ==
    /\ res' = [res EXCEPT ![a].outcome = "failed", ![a].released = TRUE]
    /\ cand' = NoCand
    /\ UNCHANGED proposals

\* commit_promoted_execution, run inside every promote_candidate transaction:
\* the active reservation whose evidence names the promoted nonce and whose
\* expected commitment is the promoted state is recorded committed and
\* released.
Settles(a, promoted) == a \in Active /\ res[a].ev /\ Target(res[a].p) = promoted
CommitPromotedRes(promoted) ==
    [a \in Workers |->
        IF Settles(a, promoted)
        THEN [res[a] EXCEPT !.outcome = "committed", !.released = TRUE]
        ELSE res[a]]

-----------------------------------------------------------------------------
Init ==
    /\ hist = <<Base>>
    /\ mempool = IF ParkedClient THEN {ClientTx} ELSE {}
    /\ landed = {}
    /\ stored = Base
    /\ cand = NoCand
    /\ canon = None
    /\ parked = IF ParkedClient THEN {ClientTarget} ELSE {}
    /\ proposals = Proposals
    /\ res = [w \in Workers |-> NoRes]
    /\ lease = [holder |-> None, tok |-> 0, live |-> FALSE]
    /\ wpc = [w \in Workers |-> "idle"]
    /\ wp = [w \in Workers |-> None]
    /\ wtok = [w \in Workers |-> 0]
    /\ wref = [w \in Workers |-> 0]
    /\ rpc = [r \in Reconcilers |-> "idle"]
    /\ rsnap = [r \in Reconcilers |-> [slot |-> None, holder |-> None, tok |-> 0, live |-> FALSE,
                                       ev |-> FALSE, ref |-> 0, exp |-> 0, p |-> None]]
    /\ rtok = [r \in Reconcilers |-> 0]
    /\ rverdict = [r \in Reconcilers |-> None]
    /\ ppc = "idle"
    /\ psnap = [verdict |-> None, cand |-> NoCand]
    /\ lapses = 0
    /\ pushes = 0
    /\ moves = 0

-----------------------------------------------------------------------------
(* The chain. A transaction lands only from the base it spends and no later *)
(* than its expiration block; reads elsewhere may lag but never lie.        *)

Land(tx) ==
    /\ tx \in mempool
    /\ Tip < MaxBlock
    /\ At(Tip) = Base
    /\ Tip + 1 <= tx.exp
    /\ hist' = Append(hist, tx.target)
    /\ mempool' = mempool \ {tx}
    /\ landed' = landed \cup {tx}
    /\ UNCHANGED <<storeVars, workerVars, rcVars, procVars, budgetVars>>

NewBlock ==
    /\ Tip < MaxBlock
    /\ hist' = Append(hist, At(Tip))
    /\ UNCHANGED <<mempool, landed, storeVars, workerVars, rcVars, procVars, budgetVars>>

ExternalMove ==
    /\ moves < ExternalMoves
    /\ Tip < MaxBlock
    /\ At(Tip) # Moved
    /\ hist' = Append(hist, Moved)
    /\ moves' = moves + 1
    /\ UNCHANGED <<mempool, landed, storeVars, workerVars, rcVars, procVars, lapses, pushes>>

-----------------------------------------------------------------------------
(* Time. Lapses are leases or reservation deadlines that run out while the  *)
(* holder takes no step. A worker that stops renewing always loses both     *)
(* eventually; the reconciler's own lapses are budgeted.                    *)

LapseWorkerLease ==
    /\ lease.live /\ lease.holder \in Workers
    /\ lease' = [lease EXCEPT !.live = FALSE]
    /\ UNCHANGED <<chainVars, stored, cand, canon, parked, proposals, res, workerVars, rcVars, procVars, budgetVars>>

LapseWorkerRes(a) ==
    /\ res[a].live /\ res[a].holder \in Workers
    /\ res' = [res EXCEPT ![a].live = FALSE]
    /\ UNCHANGED <<chainVars, stored, cand, canon, parked, proposals, lease, workerVars, rcVars, procVars, budgetVars>>

LapseOther ==
    /\ lapses < MaxLapses
    /\ \/ /\ lease.live /\ lease.holder \in Reconcilers
          /\ lease' = [lease EXCEPT !.live = FALSE]
          /\ UNCHANGED res
       \/ \E a \in Workers :
            /\ res[a].live /\ res[a].holder \in Reconcilers
            /\ res' = [res EXCEPT ![a].live = FALSE]
            /\ UNCHANGED lease
    /\ lapses' = lapses + 1
    /\ UNCHANGED <<chainVars, stored, cand, canon, parked, proposals, workerVars, rcVars, procVars, pushes, moves>>

-----------------------------------------------------------------------------
(* A client's own delta: storage/postgres.rs submit_candidate. Refused      *)
(* while an execution holds the account (ExecutionReserved) or a candidate  *)
(* is queued; a retained row at the nonce is superseded.                    *)

ClientPush ==
    /\ pushes < ClientPushes
    /\ Active = {}
    /\ cand.kind = None
    /\ stored = Base
    /\ canon = None
    /\ cand' = [kind |-> "client", target |-> ClientTarget, w |-> None]
    /\ parked' = {}
    /\ mempool' = mempool \cup {ClientTx}
    /\ pushes' = pushes + 1
    /\ UNCHANGED <<hist, landed, stored, canon, proposals, res, lease, workerVars, rcVars, procVars, lapses, moves>>

-----------------------------------------------------------------------------
(* services/execute_proposal/service.rs request_execution: the unlocked     *)
(* pre-checks only refuse early; the lease acquire and the locked           *)
(* create_execution_reservation_tx are the authority.                       *)

ReqAcquire(w) ==
    /\ wpc[w] = "idle"
    /\ \E p \in proposals :
        /\ wp' = [wp EXCEPT ![w] = p]
        /\ IF CanAcquire(w)
           THEN /\ Acquire(w)
                /\ wtok' = [wtok EXCEPT ![w] = AcquiredTok(w)]
                /\ wpc' = [wpc EXCEPT ![w] = "acquired"]
           ELSE /\ wpc' = [wpc EXCEPT ![w] = "done"]
                /\ UNCHANGED <<lease, wtok>>
    /\ UNCHANGED <<chainVars, stored, cand, canon, parked, proposals, res, wref, rcVars, procVars, budgetVars>>

\* create_execution_reservation_tx
ReqCreate(w) ==
    /\ wpc[w] = "acquired"
    /\ IF /\ LeaseCurrent(w, wtok[w])
          /\ Active = {}
          /\ cand.kind = None
          /\ wp[w] \in proposals
       THEN /\ res' = [res EXCEPT ![w] = [NoRes EXCEPT !.used = TRUE, !.p = wp[w],
                                          !.holder = w, !.tok = wtok[w], !.live = TRUE]]
            /\ wpc' = [wpc EXCEPT ![w] = "running"]
       ELSE /\ wpc' = [wpc EXCEPT ![w] = "release"]
            /\ UNCHANGED res
    /\ UNCHANGED <<chainVars, stored, cand, canon, parked, proposals, lease, wp, wtok, wref, rcVars, procVars, budgetVars>>

-----------------------------------------------------------------------------
(* services/execute_proposal/worker.rs run_execution.                       *)

\* approach_boundary: prepare/execute/prove fix the reference block (a tip
\* read, possibly lagging) and the expiration.
WPrepare(w) ==
    /\ wpc[w] = "running"
    /\ \E t \in Blocks :
        /\ t + Expiry <= MaxBlock
        /\ wref' = [wref EXCEPT ![w] = t]
    /\ wpc' = [wpc EXCEPT ![w] = "prepared"]
    /\ UNCHANGED <<chainVars, storeVars, wp, wtok, rcVars, procVars, budgetVars>>

\* Heartbeat / Heartbeat::advance: renew_execution_reservation_tx under the
\* worker's fence (a lease renew is a no-op in this abstraction).
WHeartbeat(w) ==
    /\ wpc[w] \in {"running", "prepared", "send"}
    /\ \E a \in Active :
        /\ OwnsLive(a, w, wtok[w])
        /\ ~res[a].live
        /\ res' = [res EXCEPT ![a].live = TRUE]
    /\ UNCHANGED <<chainVars, stored, cand, canon, parked, proposals, lease, workerVars, rcVars, procVars, budgetVars>>

\* Any failure short of the boundary (state mismatch, binding, expired,
\* inadmissible, node unavailable, shutdown): fail() -> fail_execution.
WFail(w) ==
    /\ wpc[w] \in {"running", "prepared", "failing"}
    /\ IF ResolveOk(w, w, wtok[w], FALSE)
       THEN ResolveEffect(w)
       ELSE UNCHANGED <<res, cand, proposals>>
    /\ wpc' = [wpc EXCEPT ![w] = "release"]
    /\ UNCHANGED <<chainVars, stored, canon, parked, lease, wp, wtok, wref, rcVars, procVars, budgetVars>>

\* cross_boundary -> admit_execution_candidate_tx: the candidate and the
\* evidence commit together. A parked row at the nonce moves to the
\* superseded_deltas archive (archive_parked_delta), where reconciliation
\* can still restore it, so `parked` keeps it.
WAdmit(w) ==
    /\ wpc[w] = "prepared"
    /\ IF ~(w \in Active /\ res[w].holder = w /\ ~res[w].ev)
       THEN /\ wpc' = [wpc EXCEPT ![w] = "release"]
            /\ UNCHANGED <<cand, parked, res>>
       ELSE IF ~OwnsLive(w, w, wtok[w])
       THEN /\ wpc' = [wpc EXCEPT ![w] = "release"]
            /\ UNCHANGED <<cand, parked, res>>
       ELSE IF stored # Base \/ cand.kind # None \/ canon # None
       THEN /\ wpc' = [wpc EXCEPT ![w] = "failing"]
            /\ UNCHANGED <<cand, parked, res>>
       ELSE /\ cand' = [kind |-> "exec", target |-> Target(wp[w]), w |-> w]
            /\ UNCHANGED parked
            /\ res' = [res EXCEPT ![w].ev = TRUE, ![w].ref = wref[w],
                                  ![w].exp = wref[w] + Expiry]
            /\ wpc' = [wpc EXCEPT ![w] = "admitted"]
    /\ UNCHANGED <<chainVars, stored, canon, proposals, lease, wp, wtok, wref, rcVars, procVars, budgetVars>>

\* claim_send, first half: Renewal::attempt renews the lease.
WClaimLease(w) ==
    /\ wpc[w] = "admitted"
    /\ wpc' = [wpc EXCEPT ![w] = IF LeaseCurrent(w, wtok[w]) THEN "claimedLease" ELSE "release"]
    /\ UNCHANGED <<chainVars, storeVars, wp, wtok, wref, rcVars, procVars, budgetVars>>

\* claim_send, second half: renew_execution_reservation_tx records Sent.
WClaimRes(w) ==
    /\ wpc[w] = "claimedLease"
    /\ IF \E a \in Active : OwnsLive(a, w, wtok[w])
       THEN /\ res' = [a \in Workers |-> IF a \in Active /\ OwnsLive(a, w, wtok[w])
                                         THEN [res[a] EXCEPT !.live = TRUE] ELSE res[a]]
            /\ wpc' = [wpc EXCEPT ![w] = "send"]
       ELSE /\ wpc' = [wpc EXCEPT ![w] = "release"]
            /\ UNCHANGED res
    /\ UNCHANGED <<chainVars, stored, cand, canon, parked, proposals, lease, wp, wtok, wref, rcVars, procVars, budgetVars>>

\* submit: Accepted and Unknown both leave the transaction in flight. A
\* rejection and a refusal the node made without processing it (rate limit,
\* authentication, an unimplemented call) leave nothing in flight.
WSubmit(w) ==
    /\ wpc[w] = "send"
    /\ \/ /\ mempool' = mempool \cup {[kind |-> "exec", w |-> w, p |-> wp[w],
                                       target |-> Target(wp[w]), exp |-> res[w].exp]}
          /\ wpc' = [wpc EXCEPT ![w] = "release"]
       \/ /\ wpc' = [wpc EXCEPT ![w] = "rejected"]
          /\ UNCHANGED mempool
       \/ /\ wpc' = [wpc EXCEPT ![w] = "notProcessed"]
          /\ UNCHANGED mempool
    /\ UNCHANGED <<hist, landed, storeVars, wp, wtok, wref, rcVars, procVars, budgetVars>>

\* SubmissionOutcome::Rejected -> resolve_execution under the worker's fence.
WResolveRejected(w) ==
    /\ wpc[w] = "rejected"
    /\ IF ResolveOk(w, w, wtok[w], TRUE)
       THEN ResolveEffect(w)
       ELSE UNCHANGED <<res, cand, proposals>>
    /\ wpc' = [wpc EXCEPT ![w] = "release"]
    /\ UNCHANGED <<chainVars, stored, canon, parked, lease, wp, wtok, wref, rcVars, procVars, budgetVars>>

\* SubmissionOutcome::NotProcessed -> resolve_execution under the worker's
\* fence, keeping the proposal.
WResolveNotProcessed(w) ==
    /\ wpc[w] = "notProcessed"
    /\ IF ResolveOk(w, w, wtok[w], TRUE)
       THEN ResolveKeepingProposal(w)
       ELSE UNCHANGED <<res, cand, proposals>>
    /\ wpc' = [wpc EXCEPT ![w] = "release"]
    /\ UNCHANGED <<chainVars, stored, canon, parked, lease, wp, wtok, wref, rcVars, procVars, budgetVars>>

\* release_quietly on every exit.
WRelease(w) ==
    /\ wpc[w] = "release"
    /\ lease' = ReleaseOf(w, wtok[w])
    /\ wpc' = [wpc EXCEPT ![w] = "done"]
    /\ UNCHANGED <<chainVars, stored, cand, canon, parked, proposals, res, wp, wtok, wref, rcVars, procVars, budgetVars>>

-----------------------------------------------------------------------------
(* jobs/execution_reconcile/mod.rs Reconciler::reconcile.                   *)

SetRpc(r, pc) == rpc' = [rpc EXCEPT ![r] = pc]

\* list_active_executions (unlocked snapshot), then the Owned check on the
\* snapshot's holder and lease_expires_at.
RcList(r) ==
    /\ rpc[r] = "idle"
    /\ \E a \in Active :
        /\ ~(res[a].holder # r /\ res[a].live)
        /\ rsnap' = [rsnap EXCEPT ![r] = [slot |-> a, holder |-> res[a].holder, tok |-> res[a].tok,
                                          live |-> res[a].live, ev |-> res[a].ev,
                                          ref |-> res[a].ref, exp |-> res[a].exp, p |-> res[a].p]]
    /\ SetRpc(r, "acquire")
    /\ UNCHANGED <<chainVars, storeVars, workerVars, rtok, rverdict, procVars, budgetVars>>

\* take_ownership: try_acquire, then (unless the fence is already ours) claim.
RcAcquire(r) ==
    /\ rpc[r] = "acquire"
    /\ IF CanAcquire(r)
       THEN /\ Acquire(r)
            /\ rtok' = [rtok EXCEPT ![r] = AcquiredTok(r)]
            /\ SetRpc(r, IF rsnap[r].holder = r /\ rsnap[r].tok = AcquiredTok(r) THEN "own" ELSE "claim")
       ELSE /\ SetRpc(r, "idle")
            /\ UNCHANGED <<lease, rtok>>
    /\ UNCHANGED <<chainVars, stored, cand, canon, parked, proposals, res, workerVars, rsnap, rverdict, procVars, budgetVars>>

\* claim_execution_reservation_tx: CAS on the expected fence, claimant's
\* token above it and current.
RcClaim(r) ==
    /\ rpc[r] = "claim"
    /\ IF \E a \in Active :
            /\ res[a].holder = rsnap[r].holder /\ res[a].tok = rsnap[r].tok
            /\ rtok[r] > rsnap[r].tok
            /\ LeaseCurrent(r, rtok[r])
       THEN /\ res' = [a \in Workers |->
                         IF a \in Active /\ res[a].holder = rsnap[r].holder /\ res[a].tok = rsnap[r].tok
                         THEN [res[a] EXCEPT !.holder = r, !.tok = rtok[r], !.live = TRUE]
                         ELSE res[a]]
            /\ SetRpc(r, "own")
            /\ UNCHANGED lease
       ELSE /\ lease' = ReleaseOf(r, rtok[r])
            /\ SetRpc(r, "idle")
            /\ UNCHANGED res
    /\ UNCHANGED <<chainVars, stored, cand, canon, parked, proposals, workerVars, rsnap, rtok, rverdict, procVars, budgetVars>>

\* No evidence in the snapshot: fail_execution (Abandoned / LeaseExpired),
\* then release_quietly.
RcFailPre(r) ==
    /\ rpc[r] = "own"
    /\ ~rsnap[r].ev
    /\ LET a == rsnap[r].slot IN
        IF ResolveOk(a, r, rtok[r], FALSE)
        THEN ResolveEffect(a)
        ELSE UNCHANGED <<res, cand, proposals>>
    /\ lease' = ReleaseOf(r, rtok[r])
    /\ SetRpc(r, "idle")
    /\ UNCHANGED <<chainVars, stored, canon, parked, workerVars, rsnap, rtok, rverdict, procVars, budgetVars>>

\* observe + moved_on: one account read at some block, and a history search
\* from the reference block up to a tip read separately.
Verdicts(s, b, t) ==
    LET st == At(b)
        found == \E k \in s.ref..t : At(k) = Target(s.p)
    IN  IF st = Target(s.p) THEN "expected"
        ELSE IF st = Base THEN (IF b >= s.exp THEN "expired" ELSE "pending")
        ELSE IF b < s.ref THEN "pending"
        ELSE IF found THEN "expected"
        ELSE IF t + 1 <= b THEN "pending"
        ELSE "discarded"

\* A read from the current tip.
RcObserveFresh(r) ==
    /\ rpc[r] = "own"
    /\ rsnap[r].ev
    /\ rverdict' = [rverdict EXCEPT ![r] = Verdicts(rsnap[r], Tip, Tip)]
    /\ SetRpc(r, "observed")
    /\ UNCHANGED <<chainVars, storeVars, workerVars, rsnap, rtok, procVars, budgetVars>>

\* A lagging node, or a read that fails or times out (held as pending).
RcObserveAny(r) ==
    /\ rpc[r] = "own"
    /\ rsnap[r].ev
    /\ \E b, t \in Blocks :
        rverdict' = [rverdict EXCEPT ![r] = Verdicts(rsnap[r], b, t)]
       \/ rverdict' = [rverdict EXCEPT ![r] = "pending"]
    /\ SetRpc(r, "observed")
    /\ UNCHANGED <<chainVars, storeVars, workerVars, rsnap, rtok, procVars, budgetVars>>

\* renew_after_observing, first half: the lease.
RcRenewLease(r) ==
    /\ rpc[r] = "observed"
    /\ IF LeaseCurrent(r, rtok[r])
       THEN SetRpc(r, "renewed") /\ UNCHANGED lease
       ELSE /\ lease' = ReleaseOf(r, rtok[r])
            /\ SetRpc(r, "idle")
    /\ UNCHANGED <<chainVars, stored, cand, canon, parked, proposals, res, workerVars, rsnap, rtok, rverdict, procVars, budgetVars>>

\* renew_after_observing, second half: the reservation (Reconciling).
RcRenewRes(r) ==
    /\ rpc[r] = "renewed"
    /\ IF \E a \in Active : OwnsLive(a, r, rtok[r])
       THEN /\ res' = [a \in Workers |-> IF a \in Active /\ OwnsLive(a, r, rtok[r])
                                         THEN [res[a] EXCEPT !.live = TRUE] ELSE res[a]]
            /\ SetRpc(r, "act")
            /\ UNCHANGED lease
       ELSE /\ lease' = ReleaseOf(r, rtok[r])
            /\ SetRpc(r, "idle")
            /\ UNCHANGED res
    /\ UNCHANGED <<chainVars, stored, cand, canon, parked, proposals, workerVars, rsnap, rtok, rverdict, procVars, budgetVars>>

\* hold: renew the reservation under our fence, then release the lease.
Hold(r) ==
    /\ res' = [a \in Workers |-> IF a \in Active /\ OwnsLive(a, r, rtok[r])
                                 THEN [res[a] EXCEPT !.live = TRUE] ELSE res[a]]
    /\ lease' = ReleaseOf(r, rtok[r])
    /\ UNCHANGED <<cand, proposals>>

\* settle_promoted_execution_tx: candidate canonical and stored state at the
\* expected commitment.
SettleOk(r) ==
    \E a \in Active :
        /\ OwnsLive(a, r, rtok[r])
        /\ res[a].ev
        /\ canon = Target(res[a].p)
        /\ stored = Target(res[a].p)

RcAct(r) ==
    /\ rpc[r] = "act"
    /\ CASE rverdict[r] = "expected" ->
              IF SettleOk(r)
              THEN /\ res' = [a \in Workers |-> IF a \in Active /\ OwnsLive(a, r, rtok[r])
                                                THEN [res[a] EXCEPT !.outcome = "committed", !.released = TRUE]
                                                ELSE res[a]]
                   /\ lease' = ReleaseOf(r, rtok[r])
                   /\ UNCHANGED <<cand, proposals>>
              ELSE Hold(r)
         [] rverdict[r] = "pending" -> Hold(r)
         [] OTHER ->
              LET a == rsnap[r].slot IN
              IF ResolveOk(a, r, rtok[r], TRUE)
              THEN ResolveEffect(a) /\ lease' = ReleaseOf(r, rtok[r])
              ELSE UNCHANGED <<res, cand, proposals, lease>>
    /\ SetRpc(r, "idle")
    /\ UNCHANGED <<chainVars, stored, canon, parked, workerVars, rsnap, rtok, rverdict, procVars, budgetVars>>

-----------------------------------------------------------------------------
(* jobs/canonicalization/processor.rs, reduced to the outcomes              *)
(* verify-candidate-queue proves for client candidates, plus the execution  *)
(* arms #254 added: executed_candidate_landed, ProtectedByExecution, and    *)
(* commit_promoted_execution inside promote_candidate.                      *)

ProcVerdict(b, t) ==
    LET st == At(b)
        owner == {a \in Active : res[a].ev}
        landedSince(a) == \E k \in res[a].ref..t : At(k) = cand.target
    IN  IF st = cand.target THEN "promote"
        ELSE IF st = Base THEN "wait"
        ELSE IF cand.kind = "exec" /\ \E a \in owner : landedSince(a) THEN "promote"
        \* Client candidates keep the outcome the client-side models prove:
        \* promoted once the chain reached them (external moves are outside
        \* those models, so a client step is never lost to one here).
        ELSE IF cand.kind = "client" /\ \E k \in Blocks : At(k) = cand.target THEN "promote"
        ELSE "discard"

PObserveWith(b, t) ==
    /\ ppc = "idle"
    /\ cand.kind # None
    /\ ProcVerdict(b, t) # "wait"
    /\ psnap' = [verdict |-> ProcVerdict(b, t), cand |-> cand]
    /\ ppc' = "write"
    /\ UNCHANGED <<chainVars, storeVars, workerVars, rcVars, budgetVars>>

PObserveFresh == PObserveWith(Tip, Tip)
PObserveAny == \E b, t \in Blocks : PObserveWith(b, t)

\* promote_candidate (base gate, kind gate) or discard_candidate /
\* update_candidate_status (refused with ProtectedByExecution). The base gate
\* also admits a stored state already at the candidate's target: a resumed
\* promotion, as after a /configure re-onboarding to the chain's state.
PWrite ==
    /\ ppc = "write"
    /\ ppc' = "idle"
    /\ IF cand # psnap.cand
       THEN UNCHANGED <<stored, cand, canon, res>>
       ELSE IF psnap.verdict = "promote"
       THEN IF stored \in {Base, cand.target}
            THEN /\ stored' = cand.target
                 /\ canon' = cand.target
                 /\ cand' = NoCand
                 /\ res' = CommitPromotedRes(cand.target)
            ELSE UNCHANGED <<stored, cand, canon, res>>
       ELSE IF \E a \in Active : res[a].ev
       THEN UNCHANGED <<stored, cand, canon, res>>
       ELSE /\ cand' = NoCand
            /\ UNCHANGED <<stored, canon, res>>
    /\ UNCHANGED <<chainVars, parked, proposals, lease, workerVars, rcVars, psnap, budgetVars>>

\* jobs/canonicalization/reconciliation.rs: a retained row whose post-state
\* the chain reached is promoted (PromotableKind::Retained, or
\* PromotableKind::Superseded for an archived copy), through the same
\* promote_candidate transaction. An archived row never replaces a candidate
\* still at its nonce (restore_superseded), so it waits for the execution
\* that archived it to resolve.
PReconcileRetained ==
    /\ ClientTarget \in parked
    /\ stored = Base
    /\ canon = None
    /\ cand.kind = None
    /\ \E b \in Blocks : At(b) = ClientTarget
    /\ stored' = ClientTarget
    /\ canon' = ClientTarget
    /\ parked' = {}
    /\ res' = CommitPromotedRes(ClientTarget)
    /\ UNCHANGED <<chainVars, cand, proposals, lease, workerVars, rcVars, procVars, budgetVars>>

-----------------------------------------------------------------------------
Next ==
    \/ \E tx \in mempool : Land(tx)
    \/ NewBlock \/ ExternalMove
    \/ LapseWorkerLease \/ (\E a \in Workers : LapseWorkerRes(a)) \/ LapseOther
    \/ ClientPush
    \/ \E w \in Workers :
        \/ ReqAcquire(w) \/ ReqCreate(w) \/ WPrepare(w) \/ WHeartbeat(w) \/ WFail(w)
        \/ WAdmit(w) \/ WClaimLease(w) \/ WClaimRes(w) \/ WSubmit(w)
        \/ WResolveRejected(w) \/ WResolveNotProcessed(w) \/ WRelease(w)
    \/ \E r \in Reconcilers :
        \/ RcList(r) \/ RcAcquire(r) \/ RcClaim(r) \/ RcFailPre(r)
        \/ RcObserveFresh(r) \/ RcObserveAny(r) \/ RcRenewLease(r) \/ RcRenewRes(r) \/ RcAct(r)
    \/ PObserveFresh \/ PObserveAny \/ PWrite \/ PReconcileRetained

Spec == Init /\ [][Next]_vars

\* Liveness: the reconcilers, the canonicalization worker and the chain keep
\* going, and a read that keeps being retried eventually comes from the tip
\* (strong fairness on the fresh reads: lagging or failed reads may happen
\* any finite number of times). Workers may stop anywhere (a crash), and
\* their lease and reservation deadline then run out.
Fairness ==
    /\ WF_vars(NewBlock)
    /\ WF_vars(LapseWorkerLease)
    /\ \A a \in Workers : WF_vars(LapseWorkerRes(a))
    /\ \A r \in Reconcilers :
        /\ WF_vars(RcList(r)) /\ WF_vars(RcAcquire(r)) /\ WF_vars(RcClaim(r))
        /\ WF_vars(RcFailPre(r)) /\ SF_vars(RcObserveFresh(r)) /\ WF_vars(RcRenewLease(r))
        /\ WF_vars(RcRenewRes(r)) /\ WF_vars(RcAct(r))
    /\ SF_vars(PObserveFresh) /\ WF_vars(PWrite) /\ WF_vars(PReconcileRetained)

LiveSpec == Spec /\ Fairness

StateBound == lease.tok <= MaxTok

\* TLC state view for the safety runs: locals a process overwrites before it
\* reads them again (an idle reconciler's snapshot, an idle canonicalization
\* step's snapshot, a finished worker's lease and reference block, the lease
\* and deadline of a released reservation) are dropped from the fingerprint.
NoSnap == [slot |-> None, holder |-> None, tok |-> 0, live |-> FALSE,
           ev |-> FALSE, ref |-> 0, exp |-> 0, p |-> None]
Fingerprint ==
    << hist, mempool, landed, stored, cand, canon, parked, proposals,
       [a \in Workers |-> IF res[a].released
                          THEN [res[a] EXCEPT !.holder = None, !.tok = 0, !.live = FALSE,
                                              !.ref = 0, !.exp = 0]
                          ELSE res[a]],
       lease, wpc,
       [w \in Workers |-> IF wpc[w] \in {"idle", "done"} THEN <<None, 0, 0>>
                         ELSE <<wp[w], wtok[w], wref[w]>>],
       rpc,
       [r \in Reconcilers |-> IF rpc[r] = "idle" THEN <<NoSnap, 0, None>>
                             ELSE <<rsnap[r], rtok[r], rverdict[r]>>],
       ppc, IF ppc = "idle" THEN None ELSE psnap,
       lapses, pushes, moves >>

-----------------------------------------------------------------------------
(* Properties *)

\* A Guardian transaction that landed is never recorded as a failed execution.
LandedNeverFailed ==
    \A tx \in landed : tx.kind = "exec" => res[tx.w].outcome # "failed"

\* An execution recorded committed is one whose transaction landed.
CommittedMeansLanded ==
    \A a \in Workers :
        res[a].outcome = "committed" => \E tx \in landed : tx.kind = "exec" /\ tx.w = a

\* Nothing is sent before the no-retry boundary commit recorded its evidence.
SentAfterBoundary ==
    \A tx \in mempool \cup landed : tx.kind = "exec" => res[tx.w].ev

\* At most one Guardian transaction that can still land is in flight.
OneLandableExecution ==
    Cardinality({tx \in mempool : tx.kind = "exec" /\ At(Tip) = Base /\ Tip < tx.exp}) <= 1

\* An execution and a client candidate never hold the account together.
ExecutionExcludesClientCandidate ==
    ~(Active # {} /\ cand.kind = "client")

\* The stored state is one the chain held.
StoredOnChain == \E b \in Blocks : At(b) = stored

\* The first step the chain took from the base, when it was a transaction
\* this Guardian acknowledged, stays recorded in a row Guardian can promote
\* until Guardian has caught up with it (verify-proposal-lifecycle's
\* LandedStepRecorded, for the execution path).
FirstStep == IF \E b \in Blocks : At(b) # Base
             THEN At(CHOOSE b \in Blocks : At(b) # Base /\ \A c \in 0..(b - 1) : At(c) = Base)
             ELSE None
LandedStepRecorded ==
    (stored = Base /\ FirstStep \notin {None, Moved}) =>
        \/ cand.target = FirstStep
        \/ FirstStep \in parked

\* Every reservation is eventually released.
ReservationsRelease == <>[](Active = {})

\* Guardian's stored state ends up at the chain's, short of a move this
\* Guardian did not acknowledge.
GuardianCatchesUp ==
    <>[](stored = At(Tip) \/ (At(Tip) = Moved /\ (stored = FirstStep \/ FirstStep = Moved)))

=============================================================================
