----------------------------- MODULE ClientSync -----------------------------
(***************************************************************************)
(* How a cosigner device's local view of one multisig account follows the  *)
(* chain and Guardian, for the Rust SDK (crates/miden-multisig-client) and *)
(* the TypeScript SDK (packages/miden-multisig-client). Two devices, one   *)
(* per SDK, propose and execute against Guardian (candidate mode, queue    *)
(* depth 1) and a private account on chain, whose node accepts a submitted *)
(* transaction into its mempool and later either includes it or drops it  *)
(* (expired, or evicted with a dropped dependency).                        *)
(*                                                                         *)
(* A state is an index into `st`; its commitment is the index. One step is *)
(* one SDK call's decision or one server or node step.                     *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    MaxStates,   \* bound on distinct account states built
    MaxNonce,    \* bound on any state's nonce
    MaxDrops,    \* bound on transactions the node drops
    Mutation     \* "none", or a variant (SKILL.md)

Devices == {"rust", "ts"}

VARIABLES
    st,        \* every state built: [n |-> nonce, p |-> parent, c |-> signer config, k |-> kind]
    chain,     \* states included on chain, oldest first
    mempool,   \* states whose transaction the node accepted but has not included
    drops,     \* transactions dropped so far
    stored,    \* Guardian's canonical state
    cand,      \* Guardian's queued candidate, or 0
    retained,  \* candidates Guardian retired but may still promote (Retained)
    props,     \* proposals Guardian holds: [post, base] (base: the state Guardian pinned it to)
    local,     \* per device: the state in its local store
    tracked,   \* per device: miden-client holds a transaction record for `local`
    cfg,       \* per device: the signer config the SDK caches (TS) or derives (Rust)
    staleUse   \* a TS decision read a cached signer config the local state no longer has

vars == <<st, chain, mempool, drops, stored, cand, props, local, tracked, cfg, staleUse,
          retained>>

ChainHead == chain[Len(chain)]
InChain(s) == \E i \in 1..Len(chain) : chain[i] = s
Nonce(s) == st[s].n
Cfg(s) == st[s].c
CanGrow(parent) == Len(st) < MaxStates /\ Nonce(parent) < MaxNonce

Init ==
    /\ st = <<[n |-> 0, p |-> 0, c |-> 0, k |-> "plain"]>>
    /\ chain = <<1>>
    /\ mempool = {}
    /\ drops = 0
    /\ stored = 1
    /\ cand = 0
    /\ props = {}
    /\ local = [d \in Devices |-> 1]
    /\ tracked = [d \in Devices |-> FALSE]
    /\ cfg = [d \in Devices |-> 0]
    /\ staleUse = FALSE
    /\ retained = {}

(***************************************************************************)
(* Sync from Guardian. Rust client/account.rs sync_from_guardian_internal; *)
(* TS multisig.ts syncState with state/adopt.ts isSafeToAdoptGuardianState. *)
(* Both adopt Guardian's state when it is ahead of local and is the        *)
(* chain's (the account is deployed, so a missing chain commitment never  *)
(* arises here). Rust also rolls back a local state whose transaction will *)
(* not land (local_state_abandoned): the node no longer has it pending     *)
(* (UntrackedSubmission::dropped_at, miden-client's own discard rule:      *)
(* expired, or pending for tx_discard_delta blocks), Guardian not ahead,   *)
(* the chain at Guardian's state, Guardian not holding local as a          *)
(* candidate or retained delta, and no pending miden-client record (a      *)
(* recorded transaction is miden-client's to undo). TS never needs it: its *)
(* transactions are recorded. m1 drops the rollback; m3 takes Guardian's   *)
(* word alone that local will not land.                                    *)
(***************************************************************************)
GuardianAdopts(d) ==
    \/ /\ Nonce(stored) > Nonce(local[d])
       /\ ChainHead = stored
    \/ /\ d = "rust"
       /\ Mutation # "m1"
       /\ Nonce(stored) <= Nonce(local[d])
       /\ stored # local[d]
       /\ ChainHead = stored
       /\ cand # local[d]
       /\ local[d] \notin retained
       /\ (Mutation # "m3" => local[d] \notin mempool)
       /\ (tracked[d] => InChain(local[d]))

SyncGuardian(d) ==
    /\ GuardianAdopts(d)
    /\ local' = [local EXCEPT ![d] = stored]
    /\ tracked' = [tracked EXCEPT ![d] = FALSE]
    /\ cfg' = [cfg EXCEPT ![d] = Cfg(stored)]
    /\ UNCHANGED <<retained, st, chain, mempool, drops, stored, cand, props, staleUse>>

(* syncState's other outcomes still refresh the TS config cache from the   *)
(* local state (refreshConfigFromAccount); Rust derives it on every read.  *)
RefreshConfig(d) ==
    /\ cfg[d] # Cfg(local[d])
    /\ cfg' = [cfg EXCEPT ![d] = Cfg(local[d])]
    /\ UNCHANGED <<retained, st, chain, mempool, drops, stored, cand, props, local, tracked, staleUse>>

(***************************************************************************)
(* Chain sync (miden-client sync_state, both SDKs). miden-client undoes    *)
(* the local state of a transaction it holds a record of once the node    *)
(* discards it (sqlite store undo_account_state). It holds a record only   *)
(* for transactions applied through it: TS submission.apply(), Rust        *)
(* submit_transaction_at_tip / apply_transaction_update. Rust membership,  *)
(* threshold and switch transactions are written with add_or_update_account*)
(* instead (helpers.rs finalize_transaction, rebuilds_local_state_from_delta)*)
(* and leave no record. The account is private: chain sync never installs *)
(* a state.                                                                *)
(***************************************************************************)
SyncChainUndo(d) ==
    /\ tracked[d]
    /\ local[d] \notin mempool
    /\ ~InChain(local[d])
    /\ local' = [local EXCEPT ![d] = st[local[d]].p]
    /\ tracked' = [tracked EXCEPT ![d] = FALSE]
    /\ UNCHANGED <<retained, st, chain, mempool, drops, stored, cand, props, cfg, staleUse>>

(***************************************************************************)
(* Proposal creation: summary built on the device's local state, nonce     *)
(* label local + 1, recorded by Guardian against its tail (the stored      *)
(* state at depth 1; refused while a candidate is queued). "config" kind   *)
(* changes the signer config (add/remove signer, threshold).               *)
(***************************************************************************)
Propose(d) ==
    /\ cand = 0
    /\ CanGrow(local[d])
    /\ \E kind \in {"plain", "config"} :
         /\ st' = Append(st, [n |-> Nonce(local[d]) + 1, p |-> local[d],
                              c |-> IF kind = "config" THEN 1 - Cfg(local[d]) ELSE Cfg(local[d]),
                              k |-> kind])
         /\ props' = props \cup {[post |-> Len(st'), base |-> stored]}
    /\ UNCHANGED <<retained, chain, mempool, drops, stored, cand, local, tracked, cfg, staleUse>>

(***************************************************************************)
(* Execution. The summary binding re-executes against the local state, so *)
(* a proposal executes only from the state it was built on.               *)
(*  Rust (client/proposals.rs execute_proposal): sync first (the guard     *)
(*  below is evaluated on the synced view by the separate sync steps),    *)
(*  refuse a nonce not above local, push_delta with prev = local          *)
(*  (helpers.rs get_guardian_ack_signature), then finalize_transaction.  *)
(*  TS (multisig.ts prepareProposalExecution / executeProposal): refuse   *)
(*  unless local is the pinned base (assertExecutesOnPinnedBase), readiness*)
(*  from the cached config, push the server's delta, submit and apply.    *)
(*  The TS readiness check reads the signer config from the store record  *)
(*  it reads for the pinned-base check (refreshConfigFromAccount), as do  *)
(*  membership proposals and registration (refreshConfigFromStore). m2    *)
(*  reads the cached config instead.                                       *)
(* Guardian admits the delta as a candidate only on its stored state with *)
(* the queue empty; the node takes the transaction into its mempool.     *)
(***************************************************************************)
Execute(d) ==
    /\ \E p \in props :
         /\ st[p.post].p = local[d]
         /\ Nonce(p.post) > Nonce(local[d])
         /\ d = "ts" => p.base = local[d]
         /\ local[d] = stored
         /\ cand = 0
         /\ cand' = p.post
         /\ mempool' = mempool \cup {p.post}
         /\ local' = [local EXCEPT ![d] = p.post]
         /\ tracked' = [tracked EXCEPT ![d] = ~(d = "rust" /\ st[p.post].k = "config")]
         /\ LET used == IF d = "ts" /\ Mutation = "m2" THEN cfg[d] ELSE Cfg(local[d])
            IN /\ cfg' = [cfg EXCEPT ![d] = IF d = "rust" THEN Cfg(p.post) ELSE used]
               /\ staleUse' = (staleUse \/ (d = "ts" /\ used # Cfg(local[d])))
    /\ UNCHANGED <<retained, st, chain, drops, stored, props>>

(***************************************************************************)
(* The node: include a pending transaction built on the chain head, or     *)
(* drop one. Guardian: promote the candidate once the chain holds it,      *)
(* retire it once it can never land.                                       *)
(***************************************************************************)
Land ==
    /\ \E s \in mempool :
         /\ st[s].p = ChainHead
         /\ chain' = Append(chain, s)
         /\ mempool' = mempool \ {s}
    /\ UNCHANGED <<retained, st, drops, stored, cand, props, local, tracked, cfg, staleUse>>

Drop ==
    /\ drops < MaxDrops
    /\ \E s \in mempool :
         /\ mempool' = mempool \ {s}
         /\ drops' = drops + 1
    /\ UNCHANGED <<retained, st, chain, stored, cand, props, local, tracked, cfg, staleUse>>

Promote ==
    /\ cand # 0
    /\ ChainHead = cand
    /\ st[cand].p = stored
    /\ stored' = cand
    /\ cand' = 0
    /\ UNCHANGED <<retained, st, chain, mempool, drops, props, local, tracked, cfg, staleUse>>

Retire ==
    /\ cand # 0
    /\ ~InChain(cand)
    /\ cand' = 0
    /\ retained' = retained \cup {cand}
    /\ UNCHANGED <<st, chain, mempool, drops, stored, props, local, tracked, cfg, staleUse>>

(* jobs/canonicalization/reconciliation.rs: a retained delta whose state   *)
(* is the chain head is promoted from the stored base.                     *)
ReconcilePromote ==
    /\ \E r \in retained :
         /\ ChainHead = r
         /\ st[r].p = stored
         /\ stored' = r
         /\ retained' = retained \ {r}
    /\ UNCHANGED <<st, chain, mempool, drops, cand, props, local, tracked, cfg, staleUse>>

(* retained_ttl_seconds: a retained delta whose transaction is gone ages   *)
(* out. FairSpec assumes this of the server.                               *)
Expire ==
    /\ \E r \in retained :
         /\ r \notin mempool
         /\ ~InChain(r)
         /\ retained' = retained \ {r}
    /\ UNCHANGED <<st, chain, mempool, drops, stored, cand, props, local, tracked, cfg, staleUse>>

(* What the server can also do: drop a delta whose transaction the node    *)
(* still has pending. Reconciliation expires a retained row past           *)
(* retained_ttl_seconds without looking at the node (reconciliation.rs     *)
(* reconcile_expired), and with retention disabled a give-up candidate is  *)
(* deleted outright (Retire then Forget). Guardian then answers not-found  *)
(* for a transaction that may still land. Spec includes it, so the safety  *)
(* cells hold without trusting Guardian's absence; FairSpec leaves it out, *)
(* because a forgotten transaction that lands leaves Guardian behind the   *)
(* chain, a server-side gap no client sync can close.                      *)
Forget ==
    /\ \E r \in retained : retained' = retained \ {r}
    /\ UNCHANGED <<st, chain, mempool, drops, stored, cand, props, local, tracked, cfg, staleUse>>

Core ==
    \/ \E d \in Devices :
         \/ SyncGuardian(d) \/ RefreshConfig(d) \/ SyncChainUndo(d)
         \/ Propose(d) \/ Execute(d)
    \/ Land \/ Drop \/ Promote \/ Retire \/ ReconcilePromote \/ Expire

Next == Core \/ Forget

Spec == Init /\ [][Next]_vars

(* The server, the node and each device's syncs keep running; nobody owes  *)
(* a proposal or an execution. Guardian forgets only what the node dropped. *)
FairSpec ==
    /\ Init /\ [][Core]_vars
    /\ WF_vars(Land) /\ WF_vars(Promote) /\ WF_vars(Retire)
    /\ WF_vars(ReconcilePromote) /\ WF_vars(Expire)
    /\ \A d \in Devices :
         WF_vars(SyncGuardian(d)) /\ WF_vars(RefreshConfig(d)) /\ WF_vars(SyncChainUndo(d))

(***************************************************************************)
(* Properties                                                              *)
(***************************************************************************)

(* Once nothing is pending at the node, every device's local state ends up *)
(* the chain's: a device never stays on a state that did not land.         *)
DeviceConverges ==
    \A d \in Devices : <>[](mempool = {}) => <>[](local[d] = ChainHead)

(* A device's local state is always one the chain holds, or one whose      *)
(* transaction the node still has pending.                                 *)
LocalIsReal == \A d \in Devices : InChain(local[d]) \/ local[d] \in mempool

(* No TS decision reads a cached signer config the local state lost.       *)
ConfigFresh == ~staleUse

(* A sync never rolls a device back from a transaction the node still has *)
(* pending: if it then landed, the device would already have built on the  *)
(* older base and forked against it.                                       *)
NeverRollsBackPending ==
    [][\A d \in Devices :
         (/\ local'[d] # local[d]
          /\ local'[d] = stored
          /\ st[local'[d]].n <= st[local[d]].n) => local[d] \notin mempool]_vars

=============================================================================
