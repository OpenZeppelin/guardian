------------------------- MODULE ProposalLifecycle -------------------------
(***************************************************************************)
(* One Guardian multisig account: proposals created against Guardian,     *)
(* executed by cosigner devices running the Rust or the TypeScript SDK,   *)
(* their executions pushed to Guardian for an acknowledgement and landed  *)
(* on chain, and the canonicalization worker promoting, parking and       *)
(* reconciling the resulting candidates.                                  *)
(*                                                                         *)
(* An account state is the path of transaction summaries that produced it *)
(* from genesis (<<>>), so a commitment is a sequence, its account nonce  *)
(* is its length, and applying summary s to state b gives Append(b, s).   *)
(* Summaries are the proposal ids (services/push_delta_proposal.rs:       *)
(* delta_proposal_id is the tx_summary commitment).                       *)
(*                                                                         *)
(* One step is one Guardian request (the storage gate under the account   *)
(* lock decides it), one SDK guard read, one chain event, or one fenced   *)
(* worker write. The candidate queue's own interleavings are verified by  *)
(* the verify-candidate-queue skill; here the worker is abstracted to its *)
(* verified outcomes. Paths are relative to the repo root.                *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Devices,       \* cosigner devices
    Sdk,           \* [Devices -> {"rust", "ts"}]
    MaxSummaries,  \* bound on proposals created (fresh summaries)
    MaxPending,    \* GUARDIAN_MAX_PENDING_CANDIDATES_PER_ACCOUNT
    Labels,        \* proposal labels clients send: subset of {"nonce", "timestamp"}
    Supersede      \* a submission at a parked row's nonce replaces it (storage submit_candidate)

VARIABLES
    head,      \* the account's on-chain state
    stored,    \* Guardian's canonical state (states table)
    queue,     \* candidate rows in nonce order: [label, prev, s]
    parked,    \* retained or client-abandoned rows: [label, prev, s]
    archived,  \* superseded_deltas: parked rows a resubmission replaced
    canon,     \* canonical delta rows: [label, prev, s]
    proposals, \* pending proposal rows: [s, pin, label, built]; built is the
               \* state the proposer computed the summary on (not stored)
    local,     \* each device's own record of the account
    inflight,  \* submitted transactions: [s, base, ackPrev, pin, built]
    landed,    \* transactions that landed: [s, base, ackPrev, pin, built]
    nextS      \* next fresh summary id

vars == <<head, stored, queue, parked, archived, canon, proposals, local, inflight, landed, nextS>>

TimestampLabel == 100

Range(s) == {s[i] : i \in DOMAIN s}
Last(s) == s[Len(s)]
IsPrefix(a, b) == Len(a) <= Len(b) /\ SubSeq(b, 1, Len(a)) = a
Post(r) == Append(r.prev, r.s)
Rows == Range(queue) \cup parked \cup archived \cup canon
QueueTail == IF queue = <<>> THEN stored ELSE Post(Last(queue))

Init ==
    /\ head = <<>>
    /\ stored = <<>>
    /\ queue = <<>>
    /\ parked = {}
    /\ archived = {}
    /\ canon = {}
    /\ proposals = {}
    /\ local = [d \in Devices |-> <<>>]
    /\ inflight = {}
    /\ landed = {}
    /\ nextS = 1

-----------------------------------------------------------------------------
(* Guardian push_delta: crates/server/src/services/push_delta.rs push_delta *)
(* and storage gate_candidate_submission / submit_candidate. Returns the   *)
(* acknowledgement (TRUE) and the next queue/parked rows, or FALSE.        *)

Admits(prev, label) ==
    /\ Len(queue) < MaxPending
    /\ prev = QueueTail
    /\ queue # <<>> => (label > Last(queue).label /\ label = Len(prev) + 1)
    /\ \A r \in Range(queue) \cup canon : r.label # label
    /\ Supersede \/ \A r \in parked : r.label # label

(* A superseded parked row moves to the archive (submit_candidate with      *)
(* CandidatePolicy.archive_superseded, retention on) instead of vanishing.  *)
Admit(prev, label, s) ==
    /\ queue' = Append(queue, [label |-> label, prev |-> prev, s |-> s])
    /\ parked' = {r \in parked : r.label # label}
    /\ archived' = archived \cup {r \in parked : r.label = label}

-----------------------------------------------------------------------------
(* Devices *)

(* Rust client/account.rs sync and TS syncState: a device adopts the chain *)
(* state or Guardian's canonical state.                                     *)
Sync(d) ==
    /\ \E st \in {head, stored} : local' = [local EXCEPT ![d] = st]
    /\ UNCHANGED <<head, stored, queue, parked, archived, canon, proposals, inflight, landed, nextS>>

(* Proposal creation: Rust client/proposals.rs propose_transaction_with_options *)
(* (label account.nonce() + 1), TS multisig.ts resolveProposalNonce (store       *)
(* nonce + 1, or Date.now() through 0.18.0); Guardian                              *)
(* services/push_delta_proposal.rs push_delta_proposal pins it to the queue tail  *)
(* and refuses it while the queue is full or when a queue exists and the label is *)
(* not the tail's plus one.                                                         *)
Propose(d) ==
    /\ nextS <= MaxSummaries
    /\ \E kind \in Labels :
         LET label == IF kind = "nonce" THEN Len(local[d]) + 1 ELSE TimestampLabel + nextS IN
         /\ Len(queue) < MaxPending
         /\ queue # <<>> => label = Last(queue).label + 1
         /\ proposals' = proposals \cup {[s |-> nextS, pin |-> QueueTail, label |-> label, built |-> local[d]]}
    /\ nextS' = nextS + 1
    /\ UNCHANGED <<head, stored, queue, parked, archived, canon, local, inflight, landed>>

(* Rust client/proposals.rs execute_proposal: sync, refuse when             *)
(* proposal.nonce <= account.nonce(), refuse when the synced account is not  *)
(* the state GUARDIAN pinned the proposal to (ensure_executes_on_pinned_base)*)
(* then helpers.rs get_guardian_ack_signature pushes the delta with          *)
(* prev_commitment = the local account's commitment, and finalize_transaction*)
(* executes on it.                                                            *)
ExecuteRust(d) ==
    /\ Sdk[d] = "rust"
    /\ \E p \in proposals, st \in {head, stored} :
         /\ p.label > Len(st)
         /\ st = p.pin
         /\ Admits(st, p.label)
         /\ Admit(st, p.label, p.s)
         /\ inflight' = inflight \cup {[s |-> p.s, base |-> st, ackPrev |-> st, pin |-> p.pin, built |-> p.built]}
         /\ local' = [local EXCEPT ![d] = Append(st, p.s)]
    /\ UNCHANGED <<head, stored, canon, proposals, landed, nextS>>

(* TS multisig.ts prepareProposalExecution: assertExecutesOnPinnedBase       *)
(* (local commitment must equal the proposal's pinned prevCommitment), then  *)
(* pushDelta with the served delta's prevCommitment and nonce; executeProposal*)
(* submits on the store account.                                              *)
ExecuteTs(d) ==
    /\ Sdk[d] = "ts"
    /\ \E p \in proposals :
         /\ local[d] = p.pin
         /\ Admits(p.pin, p.label)
         /\ Admit(p.pin, p.label, p.s)
         /\ inflight' = inflight \cup {[s |-> p.s, base |-> local[d], ackPrev |-> p.pin, pin |-> p.pin, built |-> p.built]}
         /\ local' = [local EXCEPT ![d] = Append(local[d], p.s)]
    /\ UNCHANGED <<head, stored, canon, proposals, landed, nextS>>

-----------------------------------------------------------------------------
(* The chain: a transaction lands from the state it was built on; the       *)
(* multisig replay map refuses a summary that already executed. A submitted  *)
(* transaction may also expire without landing.                              *)

Land ==
    /\ \E t \in inflight :
         /\ t.base = head
         /\ t.s \notin Range(head)
         /\ head' = Append(head, t.s)
         /\ landed' = landed \cup {t}
         /\ inflight' = inflight \ {t}
    /\ UNCHANGED <<stored, queue, parked, archived, canon, proposals, local, nextS>>

Expire ==
    /\ \E t \in inflight : inflight' = inflight \ {t}
    /\ UNCHANGED <<head, stored, queue, parked, archived, canon, proposals, local, landed, nextS>>

-----------------------------------------------------------------------------
(* Canonicalization worker, abstracted to the outcomes the                  *)
(* verify-candidate-queue model proves safe. Paths:                          *)
(* crates/server/src/jobs/canonicalization/processor.rs and reconciliation.rs.*)

(* canonicalize_verified_delta: the head candidate's post-state is on chain;*)
(* promote it and delete the proposal whose id is its summary.               *)
Promote ==
    /\ queue # <<>>
    /\ LET h == Head(queue) IN
       /\ stored = h.prev
       /\ IsPrefix(Post(h), head)
       /\ stored' = Post(h)
       /\ queue' = Tail(queue)
       /\ canon' = canon \cup {h}
       /\ proposals' = {p \in proposals : p.s # h.s}
    /\ UNCHANGED <<head, parked, archived, local, inflight, landed, nextS>>

(* retain_candidate / finalize_abandoned_candidate: the head candidate is   *)
(* not on chain (retry exhaustion, confirmed divergence, client abandon).    *)
(* handle_orphaned_candidate: the head no longer chains from the stored      *)
(* state (its predecessor left the queue), so it is parked as orphaned with  *)
(* no chain observation at all. Either way its proposal is deleted.          *)
Park ==
    /\ queue # <<>>
    /\ LET h == Head(queue) IN
       /\ h.prev # stored \/ ~IsPrefix(Post(h), head)
       /\ queue' = Tail(queue)
       /\ parked' = parked \cup {h}
       /\ proposals' = {p \in proposals : p.s # h.s}
    /\ UNCHANGED <<head, stored, archived, canon, local, inflight, landed, nextS>>

(* reconcile_account / reconcile_chain_advance: with no candidate queued,   *)
(* a parked or archived row on the stored base whose post-state the chain   *)
(* reached is promoted. An archived row (PromotableKind::Superseded) is      *)
(* written back as the canonical row at its nonce, never over a canonical   *)
(* row there; the parked row it replaces is archived in turn.               *)
Reconcile ==
    /\ queue = <<>>
    /\ \/ \E r \in parked :
            /\ r.prev = stored
            /\ IsPrefix(Post(r), head)
            /\ stored' = Post(r)
            /\ parked' = parked \ {r}
            /\ canon' = canon \cup {r}
            /\ proposals' = {p \in proposals : p.s # r.s}
            /\ UNCHANGED archived
       \/ \E r \in archived :
            /\ r.prev = stored
            /\ IsPrefix(Post(r), head)
            /\ \A c \in canon : c.label # r.label
            /\ stored' = Post(r)
            /\ archived' = (archived \ {r}) \cup {x \in parked : x.label = r.label}
            /\ parked' = {x \in parked : x.label # r.label}
            /\ canon' = canon \cup {r}
            /\ proposals' = {p \in proposals : p.s # r.s}
    /\ UNCHANGED <<head, queue, local, inflight, landed, nextS>>

-----------------------------------------------------------------------------

Next ==
    \/ \E d \in Devices : Sync(d) \/ Propose(d) \/ ExecuteRust(d) \/ ExecuteTs(d)
    \/ Land \/ Expire
    \/ Promote \/ Park \/ Reconcile

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Properties *)

(* A proposal Guardian pinned to the state its summary was built on lands *)
(* only from that state (spec/processes.md: "keep a proposal from          *)
(* executing anywhere but on the tail"; both SDKs are documented to refuse *)
(* a proposal pinned to a state the client does not hold).                 *)
ExecutesOnPinnedBase ==
    \A t \in landed : t.pin = t.built => t.base = t.pin

(* Every landed transaction carried a Guardian acknowledgement issued for  *)
(* the state it landed from, so Guardian queued exactly that transition.   *)
AckedForLandingBase ==
    \A t \in landed : t.ackPrev = t.base

(* Every on-chain step Guardian has not caught up with is still recorded in *)
(* a delta row the worker or reconciliation can promote; otherwise Guardian *)
(* can never reach the chain state again.                                   *)
LandedStepRecorded ==
    \A i \in (Len(stored) + 1)..Len(head) :
        \E r \in Rows : r.prev = SubSeq(head, 1, i - 1) /\ r.s = head[i]

(* Guardian catches up with the chain: with the worker's promote, park and *)
(* reconcile steps taken whenever they stay enabled, the stored state ends *)
(* up equal to the chain state and stays there. A landed step Guardian has *)
(* no row for, or a row reconciliation cannot promote, leaves it behind for *)
(* good.                                                                    *)
LiveSpec == Spec /\ WF_vars(Promote) /\ WF_vars(Park) /\ WF_vars(Reconcile)

GuardianCatchesUp == <>[](stored = head)

=============================================================================
