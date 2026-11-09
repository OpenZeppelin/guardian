--------------------------- MODULE ModuleCounting ---------------------------
(***************************************************************************)
(* RFC 0002 (docs/rfcs/0002-guardian-modules.md) sections 3.3 and 3.4:    *)
(* how a stateful module (a spending cap) counts what the Guardian signed *)
(* for one account. The module keeps counters and a nonce watermark       *)
(* updated from Notify; every Evaluate carries `pending`, the candidates  *)
(* and the committed-but-unacknowledged events read in one snapshot.      *)
(*                                                                         *)
(* One step is one storage transaction, one module write, one message     *)
(* delivery, or one on-chain execution. Transactions form a tree: each    *)
(* signed delta names its parent state, and the chain executes a delta    *)
(* only on top of its parent, so two deltas on one base exclude each      *)
(* other on chain. Every amount is spent only once it executes on chain.  *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Cap,             \* the module's spending cap for the account
    Amounts,         \* amounts a transaction may carry
    MaxTx,           \* deltas the Guardian may sign in one behaviour
    Depth,           \* GUARDIAN_MAX_PENDING_CANDIDATES
    Requests,        \* concurrent push_delta requests
    Leaders,         \* replicas that can hold the notifier lease
    Resync,          \* retention drop and operator resync are possible
    Mutation         \* "none", or a broken variant (SKILL.md, "Mutations")

ASSUME Mutation \in {"none", "m1", "m2", "m3", "m4", "m5", "m6", "m7"}

None == "none"

VARIABLES
    txs,       \* signed deltas: txs[t] = [parent, nonce, amt]; parent 0 is the registered state
    canon,     \* the stored (canonical) state: a delta id, 0 before any
    queue,     \* candidate queue, oldest first; queue[1].parent = canon
    retained,  \* recoverable rows: Retained and client-abandoned deltas (issue #345)
    events,    \* module_events outbox, in commit order
    unwritten, \* m5 only: a promoted delta whose event row is not written yet (0 = none)
    chain,     \* deltas executed on chain, in order
    req,       \* push_delta requests in flight
    cnt,       \* module counter (sum of applied amounts)
    wm,        \* module delivery watermark (highest applied nonce)
    applied,   \* ghost: deltas whose amount is in cnt
    evalLog,   \* ghost: double counts of the last evaluation, misses of the last admitted one
    cursor,    \* acknowledged prefix of `events` for this (module, account)
    holder,    \* replica holding the notifier lease
    believes,  \* replicas that believe they hold it (a stale holder still does)
    msgs,      \* Notify / NotifyGap calls in flight to the module
    acks,      \* NotifyAck replies in flight back to replicas
    resyncReq, \* resync_required for the (module, account) pair
    gapOwed,   \* the Gap obligation stored with the cursor (gap_resume_from_nonce set)
    gapOwner,  \* the replica that ran the resync (only it knows of the Gap under m7)
    resume     \* resume_from_nonce of the latest resync

vars == <<txs, canon, queue, retained, events, unwritten, chain, req, cnt, wm, applied, evalLog,
          cursor, holder, believes, msgs, acks, resyncReq, gapOwed, gapOwner, resume>>

guardianVars == <<txs, canon, queue, retained, events, unwritten, chain>>
moduleVars == <<cnt, wm, applied>>
notifyVars == <<cursor, holder, believes, msgs, acks, resyncReq, gapOwed, gapOwner, resume>>

Nonce(t) == IF t = 0 THEN 0 ELSE txs[t].nonce
Amt(t) == txs[t].amt
Ids == 1..Len(txs)
Elems(s) == {s[i] : i \in 1..Len(s)}
QueueTail == IF queue = <<>> THEN canon ELSE queue[Len(queue)]
ChainTip == IF chain = <<>> THEN 0 ELSE chain[Len(chain)]

\* The deltas a state was built from: the committed history and the queue.
RECURSIVE Path(_)
Path(t) == IF t = 0 THEN {} ELSE {t} \cup Path(txs[t].parent)

RECURSIVE Sum(_)
Sum(S) == IF S = {} THEN 0 ELSE LET t == CHOOSE x \in S : TRUE IN Amt(t) + Sum(S \ {t})

Unacked == {events[i] : i \in (cursor + 1)..Len(events)}
\* RFC 3.3 (revision 23): refused under resync_required and until the
\* module acknowledges the resync's NotifyGap.
EvalBlocked == resyncReq \/ (gapOwed /\ Mutation # "m6")
NoMiss == [live |-> {}, recov |-> {}]
Idle == [phase |-> "idle", base |-> 0, amt |-> 0, pend |-> {}, live |-> {}, recov |-> {},
         blocked |-> FALSE, missed |-> NoMiss]

\* RFC 3.3 (revision 23): every replica reads the cursor and the Gap
\* obligation in one statement and sends no event while it is open.
GapVisible(l) == gapOwed /\ (Mutation # "m7" \/ l = gapOwner)

\* A replica awaits each call: it sends again only once the reply arrived
\* or the call or its reply was lost (a timeout). Late duplicates still
\* reach the module from the other replica.
Awaiting(l) == (\E m \in msgs : m.from = l) \/ (\E a \in acks : a.to = l)

Init ==
    /\ txs = <<>>
    /\ canon = 0
    /\ queue = <<>>
    /\ retained = {}
    /\ events = <<>>
    /\ unwritten = 0
    /\ chain = <<>>
    /\ req = [r \in Requests |-> Idle]
    /\ cnt = 0
    /\ wm = 0
    /\ applied = {}
    /\ evalLog = [double |-> {}, missedLive |-> {}, missedRecoverable |-> {}]
    /\ cursor = 0
    /\ holder \in Leaders
    /\ believes = {holder}
    /\ msgs = {}
    /\ acks = {}
    /\ resyncReq = FALSE
    /\ gapOwed = FALSE
    /\ gapOwner = None
    /\ resume = 0

(***************************************************************************)
(* push_delta: snapshot, evaluate, store.                                  *)
(***************************************************************************)

\* push_delta reads the candidate queue (services/push_delta.rs) and, per
\* RFC 3.4, builds `pending` from the queue and the unacknowledged events in
\* one repeatable-read transaction, together with resync_required. The
\* client built its delta on the tail it knows.
Snapshot(r) ==
    /\ req[r].phase = "idle"
    /\ Len(txs) < MaxTx
    /\ \E a \in Amounts :
         req' = [req EXCEPT ![r] =
                   [phase |-> IF Mutation = "m3" THEN "half" ELSE "snap",
                    base |-> QueueTail, amt |-> a,
                    pend |-> IF Mutation = "m3" THEN Unacked ELSE Unacked \cup Elems(queue),
                    live |-> Path(QueueTail),
                    recov |-> retained,
                    blocked |-> EvalBlocked,
                    missed |-> NoMiss]]
    /\ UNCHANGED <<guardianVars, moduleVars, evalLog, notifyVars>>

\* m3 only: the queue is read in a second transaction after the events.
SnapshotQueue(r) ==
    /\ req[r].phase = "half"
    /\ req' = [req EXCEPT ![r].phase = "snap", ![r].pend = @ \cup Elems(queue)]
    /\ UNCHANGED <<guardianVars, moduleVars, evalLog, notifyVars>>

\* The module's Evaluate: counters and watermark from one consistent read of
\* its state, plus every pending entry above the watermark (RFC 3.4). Under
\* resync_required the call is not made and on_error = deny refuses.
Evaluate(r) ==
    /\ req[r].phase = "snap"
    /\ LET p == req[r].pend
           counted == {t \in p : Mutation = "m1" \/ Nonce(t) > wm}
           total == cnt + Sum(counted) + req[r].amt
           seen == req[r].live \cup req[r].recov
       IN /\ evalLog' = [evalLog EXCEPT !.double = counted \cap applied]
          /\ req' = [req EXCEPT
                       ![r].phase = IF ~req[r].blocked /\ total <= Cap THEN "allowed" ELSE "idle",
                       ![r].missed = [live |-> (seen \cap Path(QueueTail))
                                                 \ (applied \cup counted),
                                      recov |-> (seen \cap retained) \ (applied \cup counted)]]
    /\ UNCHANGED <<guardianVars, moduleVars, notifyVars>>

\* The candidate gate (storage/mod.rs gate_candidate_submission) under the
\* account lock: the delta must extend the current tail and the queue must
\* have room. Admission deletes a recoverable row at the same nonce
\* (storage/postgres.rs submit_candidate, "reconcile_superseded"). Only an
\* admitted delta's ack signature is returned.
Store(r) ==
    /\ req[r].phase = "allowed"
    /\ LET t == Len(txs) + 1
           n == Nonce(req[r].base) + 1
       IN IF req[r].base = QueueTail /\ Len(queue) < Depth /\ Len(txs) < MaxTx
          THEN /\ txs' = Append(txs, [parent |-> req[r].base, nonce |-> n, amt |-> req[r].amt])
               /\ queue' = Append(queue, t)
               /\ retained' = {x \in retained : txs[x].nonce # n}
               /\ evalLog' = [evalLog EXCEPT !.missedLive = req[r].missed.live,
                                             !.missedRecoverable = req[r].missed.recov]
          ELSE UNCHANGED <<txs, queue, retained, evalLog>>
    /\ req' = [req EXCEPT ![r] = Idle]
    /\ UNCHANGED <<canon, events, unwritten, chain, moduleVars, notifyVars>>

(***************************************************************************)
(* Chain and canonicalization.                                             *)
(***************************************************************************)

\* A cosigner submits any signed delta; it executes only on its parent.
Execute(t) ==
    /\ t \in Ids
    /\ txs[t].parent = ChainTip
    /\ t \notin Elems(chain)
    /\ chain' = Append(chain, t)
    /\ UNCHANGED <<txs, canon, queue, retained, events, unwritten, req, moduleVars, evalLog, notifyVars>>

\* promote_candidate (storage/postgres.rs) once the head is on chain. RFC
\* 2.3/3.3: the module_events row is written in the same transaction.
Promote ==
    /\ queue # <<>>
    /\ unwritten = 0
    /\ Head(queue) \in Elems(chain)
    /\ canon' = Head(queue)
    /\ queue' = Tail(queue)
    /\ IF Mutation = "m5"
       THEN /\ unwritten' = Head(queue)
            /\ UNCHANGED events
       ELSE /\ events' = Append(events, Head(queue))
            /\ UNCHANGED unwritten
    /\ UNCHANGED <<txs, retained, chain, req, moduleVars, evalLog, notifyVars>>

\* m5 only: the outbox row lands in a later transaction than the promotion.
WriteEvent ==
    /\ unwritten # 0
    /\ events' = Append(events, unwritten)
    /\ unwritten' = 0
    /\ UNCHANGED <<txs, canon, queue, retained, chain, req, moduleVars, evalLog, notifyVars>>

\* retain_candidate (jobs/canonicalization/processor.rs): a head the worker
\* could not confirm is parked, and its successors are retained as orphans.
\* The verdict may be spurious: the delta can still land.
Retain ==
    /\ queue # <<>>
    /\ retained' = retained \cup Elems(queue)
    /\ queue' = <<>>
    /\ UNCHANGED <<txs, canon, events, unwritten, chain, req, moduleVars, evalLog, notifyVars>>

\* reconcile_chain_advance / reconcile_recoverable_path (reconciliation.rs):
\* a recoverable row that chains from the stored base and landed is promoted
\* through promote_candidate (PromotableKind::Retained), writing its event.
\* A queue that no longer chains from the new base is orphaned.
Reconcile(t) ==
    /\ unwritten = 0
    /\ t \in retained
    /\ txs[t].parent = canon
    /\ t \in Elems(chain)
    /\ canon' = t
    /\ retained' = (retained \ {t}) \cup Elems(queue)
    /\ queue' = <<>>
    /\ events' = Append(events, t)
    /\ UNCHANGED <<txs, unwritten, chain, req, moduleVars, evalLog, notifyVars>>

\* Retained-row TTL (reconciliation.rs step 1).
Expire(t) ==
    /\ t \in retained
    /\ retained' = retained \ {t}
    /\ UNCHANGED <<txs, canon, queue, events, unwritten, chain, req, moduleVars, evalLog, notifyVars>>

(***************************************************************************)
(* Notify delivery (RFC 3.3) on the notifier lease.                        *)
(***************************************************************************)

\* try_acquire (coordination/postgres/lease.rs): a replica takes the lease;
\* the previous holder keeps believing it holds it until it notices.
Acquire(l) ==
    /\ holder # l
    /\ holder' = l
    /\ believes' = believes \cup {l}
    /\ UNCHANGED <<guardianVars, req, moduleVars, evalLog, cursor, msgs, acks,
                   resyncReq, gapOwed, gapOwner, resume>>

Notice(l) ==
    /\ l \in believes
    /\ l # holder
    /\ believes' = believes \ {l}
    /\ UNCHANGED <<guardianVars, req, moduleVars, evalLog, cursor, holder, msgs, acks,
                   resyncReq, gapOwed, gapOwner, resume>>

\* Read the cursor, send the next event.
SendNotify(l) ==
    /\ l \in believes
    /\ ~Awaiting(l)
    /\ ~resyncReq
    /\ ~GapVisible(l)
    /\ cursor < Len(events)
    /\ msgs' = msgs \cup {[kind |-> "notify", tx |-> events[cursor + 1], from |-> l]}
    /\ UNCHANGED <<guardianVars, req, moduleVars, evalLog, cursor, holder, believes, acks,
                   resyncReq, gapOwed, gapOwner, resume>>

SendGap(l) ==
    /\ l \in believes
    /\ ~Awaiting(l)
    /\ GapVisible(l)
    /\ msgs' = msgs \cup {[kind |-> "gap", resume |-> resume, from |-> l]}
    /\ UNCHANGED <<guardianVars, req, moduleVars, evalLog, cursor, holder, believes, acks,
                   resyncReq, gapOwed, gapOwner, resume>>

\* The module (the SDK's StatefulModule helper): apply an event only above
\* the watermark, counters and watermark in one write; on NotifyGap re-seed
\* from its own sources (here: the chain) and set the watermark to
\* resume_from_nonce - 1, only if that raises it.
Receive(m) ==
    /\ m \in msgs
    /\ msgs' = msgs \ {m}
    /\ IF m.kind = "notify"
       THEN /\ IF Nonce(m.tx) > wm \/ Mutation = "m4"
               THEN /\ cnt' = cnt + Amt(m.tx)
                    /\ wm' = IF Nonce(m.tx) > wm THEN Nonce(m.tx) ELSE wm
                    /\ applied' = applied \cup {m.tx}
               ELSE UNCHANGED moduleVars
            /\ acks' = acks \cup {[kind |-> "notify", tx |-> m.tx, to |-> m.from]}
       ELSE /\ IF m.resume - 1 > wm \/ Mutation = "m2"
               THEN LET seed == {t \in Elems(chain) : Nonce(t) < m.resume}
                    IN /\ cnt' = Sum(seed)
                       /\ wm' = m.resume - 1
                       /\ applied' = seed
               ELSE UNCHANGED moduleVars
            /\ acks' = acks \cup {[kind |-> "gap", resume |-> m.resume, to |-> m.from]}
    /\ UNCHANGED <<guardianVars, req, evalLog, cursor, holder, believes,
                   resyncReq, gapOwed, gapOwner, resume>>

\* Fenced cursor write: only the current holder advances the cursor, and
\* only past the event it delivered.
ReceiveAck(a) ==
    /\ a \in acks
    /\ acks' = acks \ {a}
    /\ IF a.kind = "notify"
       THEN /\ cursor' = IF a.to = holder /\ cursor < Len(events) /\ events[cursor + 1] = a.tx
                         THEN cursor + 1 ELSE cursor
            /\ UNCHANGED gapOwed
       ELSE /\ gapOwed' = IF a.to = holder /\ a.resume = resume THEN FALSE ELSE gapOwed
            /\ UNCHANGED cursor
    /\ UNCHANGED <<guardianVars, req, moduleVars, evalLog, holder, believes, msgs,
                   resyncReq, gapOwner, resume>>

\* Calls and replies can be lost (timeouts, transport errors); the replica
\* then sends again. A failed NotifyGap is retried until acknowledged
\* (RFC 3.3), so Gap calls are delayed and
\* duplicated here but not lost.
LoseNotify(m) ==
    /\ m \in msgs
    /\ m.kind = "notify"
    /\ msgs' = msgs \ {m}
    /\ UNCHANGED <<guardianVars, req, moduleVars, evalLog, cursor, holder, believes, acks,
                   resyncReq, gapOwed, gapOwner, resume>>

LoseAck(a) ==
    /\ a \in acks
    /\ acks' = acks \ {a}
    /\ UNCHANGED <<guardianVars, req, moduleVars, evalLog, cursor, holder, believes, msgs,
                   resyncReq, gapOwed, gapOwner, resume>>

(***************************************************************************)
(* Retention and resync (RFC 3.3).                                         *)
(***************************************************************************)

\* The subscriber fell GUARDIAN_MODULE_EVENT_RETENTION behind: its backlog
\* is dropped and the pair enters resync_required. Retention is days and a
\* request lives for seconds, so no request is in flight across it.
DropBacklog ==
    /\ Resync
    /\ ~resyncReq
    /\ cursor < Len(events)
    /\ \A r \in Requests : req[r].phase = "idle"
    /\ resyncReq' = TRUE
    /\ UNCHANGED <<guardianVars, req, moduleVars, evalLog, cursor, holder, believes, msgs, acks,
                   gapOwed, gapOwner, resume>>

\* POST /dashboard/modules/{id}/resync, one write fenced by the lease
\* holder: the cursor moves past every committed event and the Gap
\* obligation is stored with it, resume_from_nonce one above the last
\* skipped event (RFC 3.3, revision 23).
OperatorResync ==
    /\ resyncReq
    /\ resyncReq' = FALSE
    /\ cursor' = Len(events)
    /\ resume' = Nonce(canon) + 1
    /\ gapOwed' = TRUE
    /\ gapOwner' = holder
    /\ UNCHANGED <<guardianVars, req, moduleVars, evalLog, holder, believes, msgs, acks>>

\* One named disjunct per action, so TLC's trace names every step.
ExecuteOnChain == \E t \in Ids : Execute(t)
ReconcileRow == \E t \in Ids : Reconcile(t)
ExpireRow == \E t \in Ids : Expire(t)
ModuleReceives == \E m \in msgs : Receive(m)
CallLost == \E m \in msgs : LoseNotify(m)
ReplyArrives == \E a \in acks : ReceiveAck(a)
ReplyLost == \E a \in acks : LoseAck(a)

Next ==
    \/ \E r \in Requests : Snapshot(r) \/ SnapshotQueue(r) \/ Evaluate(r) \/ Store(r)
    \/ ExecuteOnChain \/ ReconcileRow \/ ExpireRow
    \/ Promote \/ WriteEvent \/ Retain
    \/ \E l \in Leaders : Acquire(l) \/ Notice(l) \/ SendNotify(l) \/ SendGap(l)
    \/ ModuleReceives \/ CallLost \/ ReplyArrives \/ ReplyLost
    \/ DropBacklog \/ OperatorResync

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* Properties.                                                             *)
(***************************************************************************)

\* The harm: the account spends more on chain than the cap allows.
NoOvershoot == Sum(Elems(chain)) <= Cap

\* RFC 3.4: no evaluation counts a delta twice (harm: a payment that fits
\* is refused).
NoDoubleCount == evalLog.double = {}

\* RFC 3.4, for every evaluation whose delta is then admitted: each delta
\* that was committed or queued at the snapshot, and still is when the
\* module evaluates, is counted ("never both and never neither"). A delta
\* superseded or expired in between is gone and need not count; a refused
\* delta is never signed, so what its evaluation missed cannot overspend.
CountsQueueAndCommitted == evalLog.missedLive = {}

\* The same for deltas that are recoverable (Retained or client-abandoned)
\* when the module evaluates: signed, able to land, and outside `pending`
\* as RFC 3.4 defines it.
CountsRecoverable == evalLog.missedRecoverable = {}

\* The module's counter holds each applied delta once (harm of a double
\* apply: payments that fit are refused).
NoDoubleApply == cnt = Sum(applied)
=============================================================================
