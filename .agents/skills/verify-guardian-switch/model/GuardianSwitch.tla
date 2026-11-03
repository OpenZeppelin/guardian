---------------------------- MODULE GuardianSwitch ----------------------------
(***************************************************************************)
(* The account status lifecycle around a guardian switch (issues #305,     *)
(* #434) and an operator pause (#181), for one account on Postgres in      *)
(* candidate mode, checked against every interleaving of client pushes,    *)
(* the canonicalization worker and its release hook, the release sweep,    *)
(* /configure re-onboarding, operator pause and unpause, and the chain.    *)
(*                                                                         *)
(* A state is an index into `st`; its commitment is the index. The chain   *)
(* is the account's on-chain history of states. One step is one storage    *)
(* transaction or one statement. Paths are relative to crates/server/src/. *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Requests,    \* concurrent push_delta requests
    Configures,  \* /configure re-onboardings
    MaxStates,   \* bound on distinct account states ever built
    MaxNonce,    \* bound on any state's nonce
    MaxPauses,   \* bound on operator pauses
    Public,      \* the account publishes its storage (the sweep can read the guardian key)
    Mutation     \* "none", or a variant (SKILL.md, "Guards" and "Findings")

Own == "own"
Other == "other"

VARIABLES
    st,        \* every state built: [n |-> nonce, k |-> guardian key, p |-> parent state]
    chain,     \* on-chain history of states, oldest first
    stored,    \* the states row (storage::pull_state)
    promoted,  \* the newest canonical delta produced `stored` and carries this server's ack
    released,  \* account_metadata.released_at IS NOT NULL
    paused,    \* account_metadata.paused_at IS NOT NULL
    cand,      \* the queued candidate (queue depth 1), or 0
    acks,      \* states whose delta this server acked and returned to the client
    known,     \* switch proposals pending here, by the post-state they executed to
    retained,  \* candidates that left the queue unpromoted (Retained, StaleBase, abandoned)
    req,       \* per push_delta request: [pc, s]
    hook,      \* a promoted state whose release hook has not run yet, or 0
    sweep,     \* the release sweep holder: [pc, e] (e: the stored state it proved against)
    cfg,       \* per /configure: [pc, s, saw, base, v]
    pauses,    \* operator pauses so far
    badAcks,   \* acks returned while the account was paused or released: [s, why]
    relBase    \* ghost: the stored state a release was proved against, or a re-onboarded one

vars == <<st, chain, stored, promoted, released, paused, cand, acks, known,
          req, hook, sweep, cfg, pauses, badAcks, relBase, retained>>

ChainHead == chain[Len(chain)]
InChain(s) == \E i \in 1..Len(chain) : chain[i] = s
Key(s) == st[s].k
Nonce(s) == st[s].n
CanGrow(parent) == Len(st) < MaxStates /\ Nonce(parent) < MaxNonce
NewState(parent, key) == [n |-> Nonce(parent) + 1, k |-> key, p |-> parent]

Init ==
    /\ st = <<[n |-> 0, k |-> Own, p |-> 0]>>
    /\ chain = <<1>>
    /\ stored = 1
    /\ promoted = FALSE
    /\ released = FALSE
    /\ paused = FALSE
    /\ cand = 0
    /\ acks = {}
    /\ known = {}
    /\ retained = {}
    /\ req = [q \in Requests |-> [pc |-> "idle", s |-> 0]]
    /\ hook = 0
    /\ sweep = [pc |-> "idle", e |-> 0]
    /\ cfg = [c \in Configures |-> [pc |-> "idle", s |-> 0, saw |-> FALSE, base |-> 0, v |-> TRUE]]
    /\ pauses = 0
    /\ badAcks = {}
    /\ relBase = 1

(***************************************************************************)
(* metadata/postgres.rs set_released_if_state: one transaction, FOR UPDATE *)
(* on the metadata row, then the states row read, then the write. Shared   *)
(* by the push-path hook and the sweep through                             *)
(* services/release_on_switch.rs release_switched_account. m1 drops the    *)
(* state condition. m8 stops releasing once a /configure re-onboarded a    *)
(* released account (a release that only ever happens once).               *)
(***************************************************************************)
ReonboardedAfterRelease == \E c \in Configures : cfg[c].pc = "done" /\ cfg[c].saw

ReleaseIfState(e) ==
    /\ released' = \/ released
                    \/ (stored = e /\ ~(Mutation = "m8" /\ ReonboardedAfterRelease))
                    \/ Mutation = "m1"
    /\ relBase' = IF ~released /\ released' THEN stored ELSE relBase

(***************************************************************************)
(* The chain. A guarded multisig executes a transaction from a state bound *)
(* to this server only with its ack; the one exception is the guardian key *)
(* rotation. Under another guardian, that guardian signs.                  *)
(***************************************************************************)
ChainLand ==
    /\ \E s \in acks :
         /\ st[s].p = ChainHead
         /\ chain' = Append(chain, s)
    /\ UNCHANGED <<relBase, retained, st, stored, promoted, released, paused, cand, acks, known,
                   req, hook, sweep, cfg, pauses, badAcks>>

(* An offline switch away from this server. `recorded`: the cosigners      *)
(* created the switch as a proposal here first (push_delta_proposal, which *)
(* the pause/release chokepoint gates, and which a queued candidate       *)
(* refuses at depth 1), then executed it without pushing.                  *)
ChainSwitch ==
    /\ Key(ChainHead) = Own
    /\ CanGrow(ChainHead)
    /\ \E recorded \in BOOLEAN :
         /\ recorded => ~(paused \/ released) /\ cand = 0
         /\ st' = Append(st, NewState(ChainHead, Other))
         /\ chain' = Append(chain, Len(st'))
         /\ known' = IF recorded THEN known \cup {Len(st')} ELSE known
    /\ UNCHANGED <<relBase, retained, stored, promoted, released, paused, cand, acks,
                   req, hook, sweep, cfg, pauses, badAcks>>

(* Under the other guardian: an ordinary transaction, or a rotation back.  *)
ChainForeign ==
    /\ Key(ChainHead) = Other
    /\ CanGrow(ChainHead)
    /\ \E k \in {Own, Other} :
         /\ st' = Append(st, NewState(ChainHead, k))
         /\ chain' = Append(chain, Len(st'))
    /\ UNCHANGED <<relBase, retained, stored, promoted, released, paused, cand, acks, known,
                   req, hook, sweep, cfg, pauses, badAcks>>

(***************************************************************************)
(* services/push_delta.rs push_delta. Step 1: resolve_account and          *)
(* ensure_account_active_metadata on the metadata loaded at the start of   *)
(* the request, the queue reads, the reconstruction and apply_delta. The   *)
(* delta keeps the guardian key or moves it away (a switch pushed to the   *)
(* pre-switch guardian, #287).                                             *)
(***************************************************************************)
PushRead(q) ==
    /\ req[q].pc = "idle"
    /\ IF released \/ paused \/ cand # 0 \/ ~CanGrow(stored)
       THEN /\ req' = [req EXCEPT ![q] = [pc |-> "done", s |-> 0]]
            /\ UNCHANGED st
       ELSE \E k \in {Own, Other} :
              /\ st' = Append(st, NewState(stored, k))
              /\ req' = [req EXCEPT ![q] = [pc |-> "applied", s |-> Len(st')]]
    /\ UNCHANGED <<relBase, retained, chain, stored, promoted, released, paused, cand, acks, known,
                   hook, sweep, cfg, pauses, badAcks>>

(* Step 2: ack_delta, then storage submit_candidate, which reads           *)
(* paused_at / released_at in the FOR UPDATE that locks the account row    *)
(* (the row set_pause and set_released_if_state write), then runs          *)
(* gate_candidate_submission; the response carries the ack. m10 skips the  *)
(* status read (the pre-fix lock_account_metadata).                        *)
PushCommit(q) ==
    /\ req[q].pc = "applied"
    /\ LET s == req[q].s
           admitted == /\ cand = 0
                       /\ st[s].p = stored
                       /\ ~(Mutation # "m10" /\ (paused \/ released))
       IN /\ cand' = IF admitted THEN s ELSE cand
          /\ acks' = IF admitted THEN acks \cup {s} ELSE acks
          /\ badAcks' =
               IF admitted
               THEN badAcks
                    \cup (IF paused THEN {[s |-> s, why |-> "paused"]} ELSE {})
                    \cup (IF released THEN {[s |-> s, why |-> "released"]} ELSE {})
               ELSE badAcks
    /\ req' = [req EXCEPT ![q] = [pc |-> "done", s |-> 0]]
    /\ UNCHANGED <<relBase, retained, st, chain, stored, promoted, released, paused, known,
                   hook, sweep, cfg, pauses>>

(***************************************************************************)
(* The canonicalization worker (jobs/canonicalization/processor.rs). It    *)
(* ignores paused_at. Promote: storage/postgres.rs promote_candidate, the  *)
(* states row advanced only from the candidate's base (CAS), once the      *)
(* chain head is the candidate's post-state. A stored state already at the *)
(* candidate (a /configure re-onboarding wrote it first) is a resumed      *)
(* promotion. The release hook runs after the promotion commits.           *)
(***************************************************************************)
CanPromote == cand # 0 /\ ChainHead = cand /\ (st[cand].p = stored \/ stored = cand)

Promote ==
    /\ CanPromote
    /\ hook = 0
    /\ stored' = cand
    /\ promoted' = TRUE
    /\ cand' = 0
    /\ hook' = cand
    /\ UNCHANGED <<relBase, retained, st, chain, released, paused, acks, known, req, sweep, cfg,
                   pauses, badAcks>>

(* A candidate that is not promoted now leaves the queue: the chain moved  *)
(* past its base or past the candidate itself (handle_diverged_candidate,  *)
(* Retained), /configure replaced its base with another state (StaleBase),  *)
(* or it never landed                                                       *)
(* and used up max_retries after submission_grace_period_seconds. It stays *)
(* recoverable (within retained_ttl_seconds) for the reconcile pass and    *)
(* the sweep, also once a resubmission at its nonce archives it to         *)
(* superseded_deltas.                                                      *)
Retire ==
    /\ cand # 0
    /\ ~CanPromote
    /\ cand' = 0
    /\ retained' = retained \cup {cand}
    /\ UNCHANGED <<relBase, known, st, chain, stored, promoted, released, paused, acks, req,
                   hook, sweep, cfg, pauses, badAcks>>

(* jobs/canonicalization/reconciliation.rs (issue #345): a retained delta  *)
(* whose recomputed post-state is the chain head is promoted from the      *)
(* stored base, and the release hook runs as for any promotion.            *)
ReconcilePromote ==
    /\ hook = 0
    /\ \E r \in retained :
         /\ ChainHead = r
         /\ st[r].p = stored
         /\ stored' = r
         /\ promoted' = TRUE
         /\ hook' = r
         /\ retained' = retained \ {r}
    /\ UNCHANGED <<relBase, st, chain, released, paused, cand, acks, known, req,
                   sweep, cfg, pauses, badAcks>>

(* services/release_on_switch.rs release_if_guardian_switched, called from *)
(* processor.rs after the promotion. Best-effort: the write may fail.      *)
HookRelease ==
    /\ hook # 0
    /\ Key(hook) = Other
    /\ ReleaseIfState(hook)
    /\ hook' = 0
    /\ UNCHANGED <<retained, st, chain, stored, promoted, paused, cand, acks, known, req,
                   sweep, cfg, pauses, badAcks>>

HookSkip ==
    /\ hook # 0
    /\ hook' = 0
    /\ UNCHANGED <<relBase, retained, st, chain, stored, promoted, released, paused, cand, acks,
                   known, req, sweep, cfg, pauses, badAcks>>

(***************************************************************************)
(* The release sweep (jobs/release_sweep/sweep.rs visit). One visit reads  *)
(* the metadata row (skips released rows and rows with a candidate in      *)
(* flight), the stored state and the chain, and decides on evidence; the   *)
(* release write is a later step. Evidence, against the stored state s:    *)
(*  - check_stored_state: s itself carries a foreign key and the newest    *)
(*    canonical delta produced it with this server's ack (Delta);          *)
(*  - find_executed_switch: a known switch post-state built on s is on     *)
(*    chain (ProposalMatch, RecoverableDelta);                             *)
(*  - fetch_on_chain_guardian_binding: a public account's head carries a   *)
(*    key that is neither this server's nor s's, at a nonce not below s's  *)
(*    (ChainSweep; confirmations are not modeled, the chain is exact).     *)
(***************************************************************************)
DeltaEvidence(s) == Key(s) = Other /\ promoted

(* switch_candidates / candidate_bases: the stored state and the end state *)
(* of every retained delta chaining from it, transitively. m7 keeps only   *)
(* the stored state (the one-hop match this replaced).                     *)
RECURSIVE Reach(_, _)
Reach(B, n) ==
    IF n = 0 THEN B ELSE Reach(B \cup {r \in retained : st[r].p \in B}, n - 1)

Bases(s) == IF Mutation = "m7" THEN {s} ELSE Reach({s}, Cardinality(retained))

MatchEvidence(s) ==
    /\ ChainHead # s
    /\ \E p \in known \cup retained : Key(p) = Other /\ st[p].p \in Bases(s) /\ InChain(p)

ChainEvidence(s) ==
    /\ Public
    /\ ChainHead # s
    /\ Key(ChainHead) = Other
    /\ Key(s) = Own
    /\ Nonce(ChainHead) >= Nonce(s)

SweepRead ==
    /\ sweep.pc = "idle"
    /\ ~released
    /\ cand = 0
    /\ DeltaEvidence(stored) \/ MatchEvidence(stored) \/ ChainEvidence(stored)
    /\ sweep' = [pc |-> "write", e |-> stored]
    /\ UNCHANGED <<relBase, retained, st, chain, stored, promoted, released, paused, cand, acks,
                   known, req, hook, cfg, pauses, badAcks>>

SweepWrite ==
    /\ sweep.pc = "write"
    /\ ReleaseIfState(sweep.e)
    /\ sweep' = [pc |-> "idle", e |-> 0]
    /\ UNCHANGED <<retained, st, chain, stored, promoted, paused, cand, acks, known, req,
                   hook, cfg, pauses, badAcks>>

SweepWriteFails ==
    /\ sweep.pc = "write"
    /\ sweep' = [pc |-> "idle", e |-> 0]
    /\ UNCHANGED <<relBase, retained, st, chain, stored, promoted, released, paused, cand, acks,
                   known, req, hook, cfg, pauses, badAcks>>

(***************************************************************************)
(* services/configure_account.rs configure_account on an existing account. *)
(* The wallet re-onboards with any state the chain has held that binds the *)
(* account to this server (validate_guardian_commitment): the request      *)
(* carries the state, and a stale wallet's may be old. Steps: the       *)
(* metadata and stored commitment reads, replace_state (written only over *)
(* the stored state the request read, else 400 commitment_mismatch), then *)
(* clear_released_if_state (same lock and state condition as the release).*)
(* metadata.set never touches lifecycle columns and is not a step. m2      *)
(* clears only when the first read saw a release; m3 clears unconditionally*)
(* before the state write; m6 writes the state unconditionally (the        *)
(* submit_state upsert this replaced).                                     *)
(*                                                                         *)
(* v: the chain vouched for the state (ensure_on_chain_state found it at   *)
(* the head). Every configuration needs it except a stored cosigner        *)
(* re-registering the stored state of an account not released at the      *)
(* first read, which skips the chain read; a release written since is      *)
(* undone only once the state proves to be the head (CfgRecheck). First    *)
(* configurations are out of scope: the model starts with a stored state.  *)
(***************************************************************************)
CfgRead(c) ==
    /\ cfg[c].pc = "idle"
    /\ \E i \in 1..Len(chain), v \in BOOLEAN :
         /\ Key(chain[i]) = Own
         /\ v => chain[i] = ChainHead
         /\ ~v => ~released /\ (chain[i] = stored \/ Mutation = "m9")
                 /\ Mutation \notin {"m2", "m3"}
         /\ cfg' = [cfg EXCEPT ![c] = [pc |-> "read", s |-> chain[i], saw |-> released,
                                        base |-> stored, v |-> v]]
    /\ UNCHANGED <<relBase, retained, st, chain, stored, promoted, released, paused, cand, acks,
                   known, req, hook, sweep, pauses, badAcks>>

(* configure_account's stored_state_landed check: the submitted state is   *)
(* older than a stored state the chain reached (at the head or in the      *)
(* transaction history). m9 drops it.                                      *)
StaleConfigure(c) ==
    /\ Mutation \notin {"m6", "m9"}
    /\ Nonce(cfg[c].s) < Nonce(stored)
    /\ InChain(stored)

CfgSubmit(c) ==
    /\ \/ cfg[c].pc = "read" /\ Mutation # "m3"
       \/ cfg[c].pc = "cleared" /\ Mutation = "m3"
    /\ stored = cfg[c].base \/ Mutation = "m6"
    /\ ~StaleConfigure(c)
    /\ stored' = cfg[c].s
    /\ relBase' = cfg[c].s
    /\ promoted' = FALSE
    /\ cfg' = [cfg EXCEPT ![c].pc = IF Mutation = "m3" THEN "done" ELSE "stored"]
    /\ UNCHANGED <<retained, st, chain, released, paused, cand, acks, known, req, hook,
                   sweep, pauses, badAcks>>

(* replace_state found the stored state moved since the request read it,  *)
(* or StaleConfigure: nothing is written and the request fails.            *)

CfgRefused(c) ==
    /\ \/ cfg[c].pc = "read" /\ Mutation # "m3"
       \/ cfg[c].pc = "cleared" /\ Mutation = "m3"
    /\ \/ (stored # cfg[c].base /\ Mutation # "m6")
       \/ StaleConfigure(c)
    /\ cfg' = [cfg EXCEPT ![c].pc = "done"]
    /\ UNCHANGED <<relBase, retained, st, chain, stored, promoted, released, paused, cand,
                   acks, known, req, hook, sweep, pauses, badAcks>>

CfgClear(c) ==
    /\ cfg[c].v
    /\ \/ cfg[c].pc = "stored" /\ Mutation # "m3"
       \/ cfg[c].pc = "read" /\ Mutation = "m3"
    /\ released' =
         CASE Mutation = "m3" -> FALSE
           [] Mutation = "m2" -> released /\ ~(cfg[c].saw /\ stored = cfg[c].s)
           [] OTHER -> released /\ stored # cfg[c].s
    /\ cfg' = [cfg EXCEPT ![c].pc = IF Mutation = "m3" THEN "cleared" ELSE "done"]
    /\ UNCHANGED <<relBase, retained, st, chain, stored, promoted, paused, cand, acks, known, req,
                   hook, sweep, pauses, badAcks>>

(* A configure that skipped the chain read re-reads released_at after the  *)
(* write. Not released: nothing to clear. Released since the first read:   *)
(* ensure_on_chain_state, then clear_released_if_state when the state is   *)
(* the head; otherwise the release stands and the request fails           *)
(* (state_not_on_chain or commitment_mismatch).                            *)
CfgRecheck(c) ==
    /\ cfg[c].pc = "stored"
    /\ ~cfg[c].v
    /\ IF released /\ cfg[c].s # ChainHead
       THEN /\ cfg' = [cfg EXCEPT ![c].pc = "refused"]
            /\ UNCHANGED released
       ELSE /\ cfg' = [cfg EXCEPT ![c].pc = "done"]
            /\ released' = (released /\ stored # cfg[c].s)
    /\ UNCHANGED <<relBase, retained, st, chain, stored, promoted, paused, cand, acks, known, req,
                   hook, sweep, pauses, badAcks>>

(***************************************************************************)
(* Operator pause and unpause (services/pause_account.rs,                  *)
(* unpause_account.rs; metadata set_pause / clear_pause). Neither touches  *)
(* released_at; m5 has unpause clear it.                                   *)
(***************************************************************************)
Pause ==
    /\ ~paused
    /\ pauses < MaxPauses
    /\ paused' = TRUE
    /\ pauses' = pauses + 1
    /\ UNCHANGED <<relBase, retained, st, chain, stored, promoted, released, cand, acks, known, req,
                   hook, sweep, cfg, badAcks>>

Unpause ==
    /\ paused
    /\ paused' = FALSE
    /\ released' = (released /\ Mutation # "m5")
    /\ UNCHANGED <<relBase, retained, st, chain, stored, promoted, cand, acks, known, req, hook,
                   sweep, cfg, pauses, badAcks>>

Next ==
    \/ ChainLand \/ ChainSwitch \/ ChainForeign
    \/ \E q \in Requests : PushRead(q) \/ PushCommit(q)
    \/ Promote \/ Retire \/ ReconcilePromote \/ HookRelease \/ HookSkip
    \/ SweepRead \/ SweepWrite \/ SweepWriteFails
    \/ \E c \in Configures : CfgRead(c) \/ CfgSubmit(c) \/ CfgRefused(c) \/ CfgClear(c) \/ CfgRecheck(c)
    \/ Pause \/ Unpause

Spec == Init /\ [][Next]_vars

(* The server side keeps working: the canonicalization worker promotes or  *)
(* retires a candidate and runs the hook (which may fail), and the sweep   *)
(* visits again until a release write succeeds. Clients, operators and    *)
(* the chain owe nothing.                                                  *)
FairSpec ==
    /\ Spec
    /\ WF_vars(Promote) /\ WF_vars(Retire) /\ WF_vars(ReconcilePromote)
    /\ WF_vars(HookRelease \/ HookSkip)
    /\ WF_vars(SweepRead) /\ SF_vars(SweepWrite)

(***************************************************************************)
(* Properties                                                              *)
(***************************************************************************)

(* The account had left this server at or after state s: s itself is bound *)
(* to another guardian, or a transaction after s moved the key away.       *)
SwitchedSince(s) ==
    \/ Key(s) = Other
    \/ \E j \in 2..Len(chain) :
         /\ Key(chain[j - 1]) = Own
         /\ Key(chain[j]) = Other
         /\ Nonce(chain[j]) > Nonce(s)

(* Every release is true of the state the server holds: a re-onboarded     *)
(* account never ends up released by evidence about an older state. A      *)
(* /configure between submit_state and its clear is exempt: the clear       *)
(* follows, and requests meanwhile are refused, not wrongly served. So is   *)
(* one refused after its write because the account was released while it  *)
(* ran and its state is not the chain head: the client is told, and the    *)
(* account stays released until a re-onboarding the chain vouches for.     *)
Reonboarding(s) == \E c \in Configures : cfg[c].pc \in {"stored", "refused"} /\ cfg[c].s = s

ReleasedIsTrue == (released /\ ~Reonboarding(stored)) => SwitchedSince(relBase)

(* A transaction that needed this server's ack (from a state bound to it,  *)
(* keeping it) executed on an ack returned while the account was paused.   *)
NeedsAck(s) == Key(st[s].p) = Own /\ Key(s) = Own

PausedAckNeverExecutes ==
    \A b \in badAcks : (b.why = "paused" /\ NeedsAck(b.s)) => ~InChain(b.s)

ReleasedAckNeverExecutes ==
    \A b \in badAcks : (b.why = "released" /\ NeedsAck(b.s)) => ~InChain(b.s)

(* No ack is returned while the account is paused or released.             *)
NoAckWhileInactive == badAcks = {}

(* Only a /configure re-onboarding reactivates a released account.         *)
OnlyReonboardingReactivates ==
    [][released /\ ~released' =>
         \E c \in Configures : cfg[c].pc # cfg'[c].pc]_vars


(* An account that left this server for good is eventually released for   *)
(* good, every time it leaves (not just once in a run), when               *)
(* the server can see the switch: the account publishes its storage, or    *)
(* every switch transaction is one this server holds (a delta it acked, a *)
(* switch proposal, a recoverable switch delta). A private account's       *)
(* offline switch with no proposal here is a documented gap               *)
(* (docs/CONCEPTS.md).                                                     *)
SwitchedAway == Key(ChainHead) = Other

SwitchesAway == {j \in 2..Len(chain) : Key(chain[j - 1]) = Own /\ Key(chain[j]) = Other}

SwitchVisible ==
    \/ Public
    \/ \A j \in SwitchesAway : chain[j] \in known \cup acks

SwitchIsReleased == <>[](SwitchedAway /\ SwitchVisible) => <>[]released

=============================================================================
