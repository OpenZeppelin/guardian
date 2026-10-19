---- MODULE ApprovalRelay ----
(***************************************************************************)
(* One cosigner (the executor) executes a multisig proposal: it relays    *)
(* the approvals it holds that Guardian's stored proposal lacks, then     *)
(* pushes the delta for Guardian's acknowledgement. Other cosigners may   *)
(* sign the same proposal through Guardian meanwhile. TLC explores every  *)
(* interleaving of their requests.                                        *)
(*                                                                        *)
(* Executor, new SDKs (this branch):                                      *)
(*   TS  multisig.ts prepareProposalExecution / executeProposal ->        *)
(*       relayCollectedApprovals -> guardian.pushDelta                    *)
(*   Rust client/offline.rs execute_imported_proposal ->                  *)
(*       relay_collected_approvals -> helpers.rs get_guardian_ack_signature*)
(* Executor, 0.18.2 SDKs: push without relaying.                          *)
(* Server, this branch: services/sign_delta_proposal.rs (credit the       *)
(*   approval to the key it verifies for), services/push_delta.rs ->      *)
(*   multisig_admission.rs MultisigAccount::authorize (count stored       *)
(*   approvals).                                                          *)
(* Server, 0.18.2: sign_delta_proposal refuses an approval that is not    *)
(*   the caller's own; push_delta acknowledges without counting.          *)
(*                                                                        *)
(* sign_delta_proposal is two steps: pull_delta_proposal, then a write of *)
(* the whole row. This branch writes with update_delta_proposal_if_status *)
(* (storage/postgres.rs one conditional UPDATE, storage/filesystem.rs     *)
(* under proposal_write_lock): it commits only while the stored status    *)
(* still equals the one read, and otherwise reads again and retries.      *)
(* 0.18.2 writes with update_delta_proposal, unconditionally.             *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Cosigners,      \* the account's approvers; the tail's and the metadata's agree
                    \* (candidate_chain::ensure_tail_keeps_auth)
    Executor,
    Required,       \* the gate's signature floor for the account (PushGateThreshold)
    Collected,      \* approvals the executor holds when it executes
    StoredAtStart,  \* approvals Guardian's proposal holds when the executor fetches it
    LateSigners,    \* cosigners who sign through Guardian while the executor runs
    Stored,         \* FALSE: Guardian never stored the proposal (a switch created offline)
    ServerGate,     \* TRUE: this branch's server; FALSE: a 0.18.2 server
    ClientRelays    \* TRUE: this branch's SDKs; FALSE: 0.18.2 SDKs

ASSUME /\ Executor \in Cosigners
       /\ Collected \subseteq Cosigners /\ Cardinality(Collected) >= Required
       /\ StoredAtStart \subseteq Collected
       /\ LateSigners \subseteq Cosigners \ Collected
       /\ Stored \/ (StoredAtStart = {} /\ LateSigners = {})

VARIABLES
    stored,     \* approvals in the stored proposal's cosigner_sigs
    acked,      \* approvals a sign request wrote and answered 200 for
    pc,         \* executor: "relay", "push", "done"
    toRelay,    \* approvals the executor still has to relay
    relaying,   \* the approval whose sign request is between read and write, or "none"
    relaySnap,  \* what that request read
    late,       \* late signer -> "idle" | "read" | "done"
    lateSnap,   \* late signer -> what its request read
    pushResult  \* "none", "acked", "refused" (insufficient_signatures), "aborted" (no push)

vars == <<stored, acked, pc, toRelay, relaying, relaySnap, late, lateSnap, pushResult>>

Init ==
    /\ stored = StoredAtStart
    /\ acked = {}
    /\ pc = "relay"
    \* relayCollectedApprovals skips what the served proposal already holds;
    \* the Rust relay sends everything, and the server answers the held ones
    \* with proposal_already_signed, which is skipped: the same writes.
    /\ toRelay = IF ClientRelays THEN Collected \ StoredAtStart ELSE {}
    /\ relaying = "none"
    /\ relaySnap = {}
    /\ late = [s \in LateSigners |-> "idle"]
    /\ lateSnap = [s \in LateSigners |-> {}]
    /\ pushResult = "none"

\* sign_delta_proposal's answer, from what it read, to `caller` sending
\* `approval`. Only "ok" writes.
\* This branch: proposal_already_signed when the approval's signer is held.
\* 0.18.2: proposal_already_signed when the caller is held, then
\* invalid_proposal_signature unless the approval is the caller's own.
Response(snap, caller, approval) ==
    IF ServerGate
    THEN IF approval \in snap THEN "already_signed" ELSE "ok"
    ELSE IF caller \in snap THEN "already_signed"
         ELSE IF approval # caller THEN "invalid"
         ELSE "ok"

\* Whether a write built from `snap` commits. This branch:
\* update_delta_proposal_if_status compares the stored status (which holds
\* every approval) with the one read. 0.18.2: update_delta_proposal always
\* writes.
Commits(snap) == ~ServerGate \/ stored = snap

\* The answers relayCollectedApprovals / relay_collected_approvals skip past;
\* any other refusal ends the execution before the push.
RelayContinues(r) == r \in {"ok", "already_signed", "invalid"}

\* pull_delta_proposal for a relayed approval. A proposal Guardian never
\* stored answers proposal_not_found: the TS switch path warns and pushes
\* anyway; that is the only executor that reaches here without a stored
\* proposal.
RelayRead ==
    /\ pc = "relay" /\ relaying = "none" /\ toRelay # {}
    /\ \E a \in toRelay :
         IF Stored
         THEN /\ relaying' = a /\ relaySnap' = stored
              /\ UNCHANGED toRelay
         ELSE /\ toRelay' = toRelay \ {a}
              /\ UNCHANGED <<relaying, relaySnap>>
    /\ UNCHANGED <<stored, acked, pc, late, lateSnap, pushResult>>

\* The write of the row read above plus the approval. A refused
\* conditional write reads the row again (the failed write and the re-read
\* are one step here: a write landing between them only fails the next
\* attempt the same way) and the request is answered later.
RelayRetry ==
    /\ relaying # "none"
    /\ Response(relaySnap, Executor, relaying) = "ok"
    /\ ~Commits(relaySnap)
    /\ relaySnap' = stored
    /\ UNCHANGED <<stored, acked, pc, toRelay, relaying, late, lateSnap, pushResult>>

RelayWrite ==
    /\ relaying # "none"
    /\ LET r == Response(relaySnap, Executor, relaying) IN
         /\ r = "ok" => Commits(relaySnap)
         /\ IF r = "ok"
            THEN /\ stored' = relaySnap \cup {relaying}
                 /\ acked' = acked \cup {relaying}
            ELSE UNCHANGED <<stored, acked>>
         /\ IF RelayContinues(r)
            THEN UNCHANGED <<pc, pushResult>>
            ELSE /\ pc' = "done"
                 /\ pushResult' = "aborted"
    /\ toRelay' = toRelay \ {relaying}
    /\ relaying' = "none"
    /\ relaySnap' = {}
    /\ UNCHANGED <<late, lateSnap>>

RelayDone ==
    /\ pc = "relay" /\ relaying = "none" /\ toRelay = {}
    /\ pc' = "push"
    /\ UNCHANGED <<stored, acked, toRelay, relaying, relaySnap, late, lateSnap, pushResult>>

\* push_delta: lookup_matching_proposal reads the stored proposal once and
\* MultisigAccount::authorize counts its verified approvers. A 0.18.2 server
\* acknowledges without counting.
Push ==
    /\ pc = "push"
    /\ pushResult' =
         IF ~ServerGate THEN "acked"
         ELSE IF Stored /\ Cardinality(stored \cap Cosigners) >= Required THEN "acked"
         ELSE "refused"
    /\ pc' = "done"
    /\ UNCHANGED <<stored, acked, toRelay, relaying, relaySnap, late, lateSnap>>

\* A cosigner signing through Guardian (its own approval), same two steps.
LateRead(s) ==
    /\ late[s] = "idle"
    /\ late' = [late EXCEPT ![s] = "read"]
    /\ lateSnap' = [lateSnap EXCEPT ![s] = stored]
    /\ UNCHANGED <<stored, acked, pc, toRelay, relaying, relaySnap, pushResult>>

LateRetry(s) ==
    /\ late[s] = "read"
    /\ Response(lateSnap[s], s, s) = "ok"
    /\ ~Commits(lateSnap[s])
    /\ lateSnap' = [lateSnap EXCEPT ![s] = stored]
    /\ UNCHANGED <<stored, acked, pc, toRelay, relaying, relaySnap, late, pushResult>>

LateWrite(s) ==
    /\ late[s] = "read"
    /\ Response(lateSnap[s], s, s) = "ok" => Commits(lateSnap[s])
    /\ IF Response(lateSnap[s], s, s) = "ok"
       THEN /\ stored' = lateSnap[s] \cup {s}
            /\ acked' = acked \cup {s}
       ELSE UNCHANGED <<stored, acked>>
    /\ late' = [late EXCEPT ![s] = "done"]
    /\ UNCHANGED <<pc, toRelay, relaying, relaySnap, lateSnap, pushResult>>

Done ==
    /\ pc = "done" /\ \A s \in LateSigners : late[s] = "done"
    /\ UNCHANGED vars

Next ==
    \/ RelayRead \/ RelayRetry \/ RelayWrite \/ RelayDone \/ Push
    \/ \E s \in LateSigners : LateRead(s) \/ LateRetry(s) \/ LateWrite(s)
    \/ Done

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* Properties                                                             *)
(***************************************************************************)

\* An executor holding a threshold of approvals gets its delta acknowledged.
\* If not, a proposal the chain would accept cannot be executed (or, for the
\* switch push-back, the old Guardian keeps serving the account).
ReadyExecutionAcked == pc = "done" => pushResult = "acked"

\* An approval Guardian answered 200 for stays on the stored proposal. If
\* not, a cosigner is told their approval is recorded while the gate no
\* longer counts it.
AckedApprovalKept == acked \subseteq stored

====
