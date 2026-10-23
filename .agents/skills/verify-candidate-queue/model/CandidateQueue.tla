--------------------------- MODULE CandidateQueue ---------------------------
(***************************************************************************)
(* One Guardian account's canonicalization candidate queue (issue #17),    *)
(* checked against every interleaving of client pushes, client abandon     *)
(* requests, the chain landing acknowledged transactions, and the          *)
(* canonicalization worker's full, fast-promotion and reconcile passes.    *)
(*                                                                         *)
(* Atomicity follows the code: a step is one Postgres transaction (or one  *)
(* statement outside a transaction), or one `delta_write_lock` critical    *)
(* section on the filesystem backend. Every await between two storage      *)
(* calls in the worker is a separate step. Each action names the Rust it   *)
(* models; paths are relative to crates/server/src/.                       *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Clients,      \* concurrent client sessions pushing deltas
    MaxDeltas,    \* bound on deltas built (fresh post-state commitments)
    MaxPending,   \* GUARDIAN_MAX_PENDING_CANDIDATES_PER_ACCOUNT
    Backend,      \* "postgres" or "filesystem"
    PassModes,    \* worker pass kinds explored: subset of {"full","fast","reconcile"}
    Labels,       \* delta labels clients send: subset of {"nonce","timestamp"}
    LeaseLoss     \* explore a promotion outliving its pass's lease (Postgres)

VARIABLES
    chain,    \* on-chain commitment history, oldest first
    stored,   \* commitment of the stored canonical state (states table)
    rows,     \* committed delta rows: [nonce, prev, new, pid, kind, abandon];
              \* pid is the post-state the row's payload actually produces
    flag,     \* account_metadata.has_pending_candidate
    acked,    \* transactions Guardian acknowledged (may land on chain)
    cnonce,   \* account nonce of every commitment built so far
    created,  \* number of deltas built so far
    lock,     \* holder of the account lock: "none" or a client
    archive,  \* superseded_deltas: parked rows a resubmission replaced
    cpc, cd, csup,
    zombie,   \* a promotion still in flight from a pass whose lease was lost
    wpc, wnext, wsnap, widx, wbroken, wd, wsource, wlive, wpath, wstored

cvars == <<cpc, cd, csup>>
wvars == <<wpc, wnext, wsnap, widx, wbroken, wd, wsource, wlive, wpath, wstored>>
svars == <<chain, stored, rows, flag, acked, cnonce, created, lock, archive, zombie>>
vars == <<svars, cvars, wvars>>

NoDelta == [nonce |-> 0, prev |-> 0, new |-> 0, pid |-> 0, kind |-> "none", abandon |-> FALSE]

TimestampLabel == 100

Range(s) == {s[i] : i \in DOMAIN s}
ChainIndex(s, c) == CHOOSE i \in DOMAIN s : s[i] = c
Last(s) == s[Len(s)]
RowsAt(n) == {r \in rows : r.nonce = n}
Candidates == {r \in rows : r.kind = "candidate"}
MinNonce(S) == CHOOSE r \in S : \A x \in S : r.nonce <= x.nonce

Sorted(S) ==
    LET f[T \in SUBSET S] ==
        IF T = {} THEN <<>> ELSE <<MinNonce(T)>> \o f[T \ {MinNonce(T)}]
    IN f[S]

Queue == Sorted(Candidates)
Successors(snap, i) == {snap[j].new : j \in (i + 1)..Len(snap)}

(* storage/mod.rs first_unchained_candidate *)
RECURSIVE Unchained(_, _)
Unchained(running, q) ==
    IF q = <<>> THEN FALSE
    ELSE IF Head(q).prev = running THEN Unchained(Head(q).new, Tail(q))
    ELSE TRUE

(* storage/mod.rs classify_chain_position *)
Position(st, q, prev) ==
    IF q = <<>> THEN (IF prev = st THEN "tail" ELSE "unrelated")
    ELSE IF Last(q).new = prev THEN "tail"
    ELSE IF prev = st \/ \E i \in DOMAIN q : q[i].new = prev THEN "competing"
    ELSE "unrelated"

(* storage/mod.rs gate_candidate_submission *)
Gate(st, q, d) ==
    IF Len(q) >= MaxPending THEN "conflict"
    ELSE IF Unchained(st, q) THEN "conflict"
    ELSE IF Position(st, q, d.prev) = "competing" THEN "conflict"
    ELSE IF Position(st, q, d.prev) = "unrelated" THEN "mismatch"
    ELSE IF q # <<>> /\ d.nonce <= Last(q).nonce THEN "conflict"
    ELSE "ok"

(* jobs/canonicalization/reconciliation.rs select_recoverable_path: try    *)
(* the rows chaining from the running commitment (the hint on the chain     *)
(* first, then by nonce), backing out of a hop that leads nowhere. No two   *)
(* rows on a path share a nonce.                                             *)
HopOrder(S, h) ==
    LET key(r) == (IF r.new = h THEN 0 ELSE 1000) + r.nonce
        f[T \in SUBSET S] ==
            IF T = {} THEN <<>>
            ELSE LET m == CHOOSE r \in T : \A x \in T : key(r) <= key(x)
                 IN <<m>> \o f[T \ {m}]
    IN f[S]

RECURSIVE Walk(_, _, _, _, _), Try(_, _, _, _, _)
Walk(running, h, live, taken, k) ==
    IF k = 0 THEN <<>>
    ELSE Try(HopOrder({r \in live : r.prev = running /\ r.nonce \notin taken}, h),
             h, live, taken, k)
Try(cands, h, live, taken, k) ==
    IF cands = <<>> THEN <<>>
    ELSE LET r == Head(cands)
             rest == IF r.new = h THEN <<>>
                     ELSE Walk(r.new, h, live, taken \cup {r.nonce}, k - 1)
         IN IF r.new = h THEN <<r>>
            ELSE IF rest # <<>> THEN <<r>> \o rest
            ELSE Try(Tail(cands), h, live, taken, k)

SelectPath(st, h, live) == Walk(st, h, live, {}, Cardinality(live))

(* A step of the current lease holder that takes the account lock: from   *)
(* then on, a write still in flight from a superseded holder is fenced out.*)
ZombieSeesNewHolder ==
    zombie' = IF zombie.active THEN [zombie EXCEPT !.blocked = TRUE] ELSE zombie

ClearEntry == IF Backend = "postgres" THEN "clr_pg" ELSE "clr_fs"

Init ==
    /\ chain = <<0>>
    /\ stored = 0
    /\ rows = {}
    /\ flag = FALSE
    /\ acked = {}
    /\ cnonce = [x \in {0} |-> 0]
    /\ created = 0
    /\ lock = "none"
    /\ archive = {}
    /\ zombie = [active |-> FALSE, blocked |-> FALSE, d |-> NoDelta, source |-> "none"]
    /\ cpc = [c \in Clients |-> "idle"]
    /\ cd = [c \in Clients |-> NoDelta]
    /\ csup = [c \in Clients |-> {}]
    /\ wpc = "idle"
    /\ wnext = "idle"
    /\ wsnap = <<>>
    /\ widx = 1
    /\ wbroken = FALSE
    /\ wd = NoDelta
    /\ wsource = "none"
    /\ wlive = {}
    /\ wpath = <<>>
    /\ wstored = 0

-----------------------------------------------------------------------------
(* Clients: services/push_delta.rs. The client may build on any state it   *)
(* has ever seen, so the request-path checks (load_for_admission, the auth *)
(* binding and label rules) only narrow what reaches the storage gate and  *)
(* are left out; the in-lock gate below is the authority.                  *)

(* The label is the delta's `nonce` column: the account nonce it leaves   *)
(* the account at, or a timestamp (TypeScript SDK 0.18.0 and earlier),     *)
(* modeled as a value above every real nonce.                              *)
ClientBuild(c) ==
    /\ cpc[c] = "idle"
    /\ created < MaxDeltas
    /\ \E base \in 0..created, timestamp \in {l = "timestamp" : l \in Labels} :
         LET n == created + 1 IN
         /\ cd' = [cd EXCEPT ![c] = [nonce |-> IF timestamp THEN TimestampLabel + n
                                                ELSE cnonce[base] + 1,
                                     prev |-> base, new |-> n, pid |-> n,
                                     kind |-> "candidate", abandon |-> FALSE]]
         /\ cnonce' = [x \in 0..n |-> IF x = n THEN cnonce[base] + 1 ELSE cnonce[x]]
         /\ created' = n
    /\ cpc' = [cpc EXCEPT ![c] = "label"]
    /\ UNCHANGED <<chain, stored, rows, flag, acked, lock, archive, zombie, csup, wvars>>

(* services/push_delta.rs: behind a queued candidate the label must be the *)
(* nonce the delta leaves the account at; judged on the request path's own *)
(* read of the queue, before the storage gate.                             *)
ClientLabel(c) ==
    /\ cpc[c] = "label"
    /\ cpc' = [cpc EXCEPT ![c] =
                 IF Queue # <<>> /\ cd[c].nonce # cnonce[cd[c].new] THEN "idle" ELSE "submit"]
    /\ UNCHANGED <<chain, stored, rows, flag, acked, cnonce, created, lock, archive, zombie, cd, csup, wvars>>

(* storage/postgres.rs + storage/filesystem.rs submit_candidate: take the  *)
(* account lock, run the gate, supersede a retained or client-abandoned    *)
(* row at the nonce (archived to superseded_deltas, retention on), refuse  *)
(* any other row there.                                                    *)
ClientSubmit(c) ==
    /\ cpc[c] = "submit"
    /\ lock = "none"
    /\ LET d == cd[c]
           sup == {r \in RowsAt(d.nonce) : r.kind \in {"retained", "abandoned"}}
           refused == Gate(stored, Queue, d) # "ok" \/ RowsAt(d.nonce) \ sup # {}
       IN IF refused
          THEN /\ cpc' = [cpc EXCEPT ![c] = "idle"]
               /\ UNCHANGED <<rows, lock, archive, csup>>
          ELSE IF Backend = "postgres"
          THEN /\ lock' = c
               /\ csup' = [csup EXCEPT ![c] = sup]
               /\ cpc' = [cpc EXCEPT ![c] = "commit"]
               /\ UNCHANGED <<rows, archive>>
          ELSE /\ lock' = c
               /\ rows' = (rows \ sup) \cup {d}
               /\ archive' = archive \cup sup
               /\ cpc' = [cpc EXCEPT ![c] = "fsflag"]
               /\ UNCHANGED csup
    /\ UNCHANGED <<chain, stored, flag, acked, cnonce, created, zombie, cd, wvars>>

(* Postgres: the insert and has_pending_candidate = true become visible at *)
(* COMMIT; the acknowledgement reaches the client only on success.         *)
ClientCommit(c) ==
    /\ cpc[c] = "commit"
    /\ rows' = (rows \ csup[c]) \cup {cd[c]}
    /\ archive' = archive \cup csup[c]
    /\ flag' = TRUE
    /\ acked' = acked \cup {[prev |-> cd[c].prev, new |-> cd[c].new]}
    /\ lock' = "none"
    /\ cpc' = [cpc EXCEPT ![c] = "idle"]
    /\ UNCHANGED <<chain, stored, cnonce, created, zombie, cd, csup, wvars>>

(* Filesystem: write_delta_holding_lock, then set_has_pending_candidate,  *)
(* both inside the delta_write_lock section.                               *)
ClientFsFlag(c) ==
    /\ cpc[c] = "fsflag"
    /\ flag' = TRUE
    /\ acked' = acked \cup {[prev |-> cd[c].prev, new |-> cd[c].new]}
    /\ lock' = "none"
    /\ cpc' = [cpc EXCEPT ![c] = "idle"]
    /\ UNCHANGED <<chain, stored, rows, archive, cnonce, created, zombie, cd, csup, wvars>>

(* request_candidate_abandon (POST /delta/candidate/abandon), row-locked. *)
Abandon ==
    /\ lock = "none"
    /\ \E r \in Candidates :
         /\ ~r.abandon
         /\ rows' = (rows \ {r}) \cup {[r EXCEPT !.abandon = TRUE]}
    /\ UNCHANGED <<chain, stored, flag, acked, cnonce, created, lock, archive, zombie, cvars, wvars>>

(* The chain: a Guardian-protected account only moves by a transaction     *)
(* this Guardian acknowledged, and only from the state it was built on.    *)
Land ==
    /\ \E t \in acked :
         /\ t.prev = Last(chain)
         /\ t.new \notin Range(chain)
         /\ chain' = Append(chain, t.new)
    /\ UNCHANGED <<stored, rows, flag, acked, cnonce, created, lock, archive, zombie, cvars, wvars>>

-----------------------------------------------------------------------------
(* Worker: jobs/canonicalization/processor.rs and reconciliation.rs. One   *)
(* lease holder runs one pass at a time (worker.rs run_worker).            *)

WUnchangedS == UNCHANGED <<chain, stored, rows, flag, acked, cnonce, created, lock, archive, zombie, cvars>>

WIdle ==
    /\ wpc = "idle"
    /\ \E m \in PassModes :
         wpc' = CASE m = "full" -> "full_list"
                  [] m = "fast" -> "fast_pull"
                  [] m = "reconcile" -> "rc_pull"
    /\ wbroken' = FALSE
    /\ widx' = 1
    /\ WUnchangedS
    /\ UNCHANGED <<wnext, wsnap, wd, wsource, wlive, wpath, wstored>>

(* process_full_pass: list_with_pending_candidates, then process_account   *)
(* reads the metadata row and the candidate rows.                          *)
FullList ==
    /\ wpc = "full_list"
    /\ wpc' = IF flag THEN "full_pull" ELSE "idle"
    /\ WUnchangedS
    /\ UNCHANGED <<wnext, wsnap, widx, wbroken, wd, wsource, wlive, wpath, wstored>>

(* process_account: an empty queue under a set flag is the stale-flag heal *)
(* (metadata.clear_pending_candidate_if_none).                             *)
FullPull ==
    /\ wpc = "full_pull"
    /\ wsnap' = Queue
    /\ widx' = 1
    /\ wbroken' = FALSE
    /\ IF Queue = <<>>
       THEN /\ wpc' = ClearEntry
            /\ wnext' = "idle"
       ELSE /\ wpc' = "full_iter"
            /\ UNCHANGED wnext
    /\ WUnchangedS
    /\ UNCHANGED <<wd, wsource, wlive, wpath, wstored>>

(* process_candidate_list loop head. *)
FullIter ==
    /\ wpc = "full_iter"
    /\ IF widx > Len(wsnap)
       THEN /\ wpc' = "idle"
            /\ UNCHANGED wd
       ELSE /\ wd' = wsnap[widx]
            /\ wpc' = IF widx = 1 THEN "pfc_read" ELSE "full_stored"
    /\ WUnchangedS
    /\ UNCHANGED <<wnext, wsnap, widx, wbroken, wsource, wlive, wpath, wstored>>

ToOrphan ==
    /\ wnext' = "full_next"
    /\ wsource' = IF wd.abandon THEN "abandoned" ELSE "retained"
    /\ wpc' = "park"

(* process_candidate_list: pull_state before a non-head candidate, then    *)
(* chains_from_store / chains_from_queue.                                  *)
FullStored ==
    /\ wpc = "full_stored"
    /\ IF stored = wd.prev
       THEN /\ wbroken' = FALSE
            /\ wpc' = "pfc_read"
            /\ UNCHANGED <<wnext, wsource>>
       ELSE IF ~wbroken /\ wsnap[widx - 1].new = wd.prev
       THEN /\ wpc' = "full_predq"
            /\ UNCHANGED <<wbroken, wnext, wsource>>
       ELSE /\ wbroken' = TRUE
            /\ ToOrphan
    /\ WUnchangedS
    /\ UNCHANGED <<wsnap, widx, wd, wlive, wpath, wstored>>

(* predecessor_still_queued: pull_delta of the predecessor's nonce. *)
FullPredQ ==
    /\ wpc = "full_predq"
    /\ IF \E r \in RowsAt(wsnap[widx - 1].nonce) : r.kind = "candidate"
       THEN /\ wpc' = "idle"
            /\ UNCHANGED <<wbroken, wnext, wsource>>
       ELSE /\ wbroken' = TRUE
            /\ ToOrphan
    /\ WUnchangedS
    /\ UNCHANGED <<wsnap, widx, wd, wlive, wpath, wstored>>

FullNext ==
    /\ wpc = "full_next"
    /\ widx' = widx + 1
    /\ wpc' = "full_iter"
    /\ WUnchangedS
    /\ UNCHANGED <<wnext, wsnap, wbroken, wd, wsource, wlive, wpath, wstored>>

(* process_full_candidate: pull_state; a head off the stored base is an    *)
(* orphan (handle_orphaned_candidate).                                     *)
PfcRead ==
    /\ wpc = "pfc_read"
    /\ IF stored # wd.prev
       THEN /\ wbroken' = TRUE
            /\ ToOrphan
       ELSE /\ wpc' = "pfc_observe"
            /\ UNCHANGED <<wbroken, wnext, wsource>>
    /\ WUnchangedS
    /\ UNCHANGED <<wsnap, widx, wd, wlive, wpath, wstored>>

(* process_full_candidate: verify_commitment (one chain read), then the    *)
(* match / landed_through_successor / at-base / diverged classification.   *)
(* Retry, grace and divergence-confirmation counters are abstracted: any   *)
(* observation that could ever lead to parking may park.                   *)
PfcObserve ==
    /\ wpc = "pfc_observe"
    /\ LET h == Last(chain) IN
       IF h = wd.new \/ h \in Successors(wsnap, widx)
       THEN /\ wsource' = "candidate"
            /\ wnext' = "full_next"
            /\ wpc' = "promote"
       ELSE IF h = wd.prev
       THEN \/ /\ wpc' = "full_next"
               /\ UNCHANGED <<wnext, wsource>>
            \/ /\ wsource' = IF wd.abandon THEN "abandoned" ELSE "retained"
               /\ wnext' = "full_next"
               /\ wpc' = "park"
       ELSE \/ /\ wpc' = "full_next"
               /\ UNCHANGED <<wnext, wsource>>
            \/ /\ wsource' = IF wd.abandon THEN "abandoned" ELSE "retained"
               /\ wnext' = "full_next"
               /\ wpc' = "park"
    /\ WUnchangedS
    /\ UNCHANGED <<wsnap, widx, wbroken, wd, wlive, wpath, wstored>>

(* retain_candidate / finalize_abandoned_candidate: update_candidate_status*)
(* (locked; refuses a retained flip over a stored abandon intent), then    *)
(* release_account_if_queue_empty.                                         *)
ParkFlip ==
    /\ wpc = "park"
    /\ lock = "none"
    /\ \E r \in RowsAt(wd.nonce) :
         /\ r.kind = "candidate"
         /\ ~(wsource = "retained" /\ r.abandon)
         /\ rows' = (rows \ {r}) \cup {[r EXCEPT !.kind = wsource]}
    /\ wpc' = "rel_read"
    /\ ZombieSeesNewHolder
    /\ UNCHANGED <<chain, stored, flag, acked, cnonce, created, lock, archive, cvars>>
    /\ UNCHANGED <<wnext, wsnap, widx, wbroken, wd, wsource, wlive, wpath, wstored>>

ParkRefused ==
    /\ wpc = "park"
    /\ lock = "none"
    /\ ~\E r \in RowsAt(wd.nonce) :
           r.kind = "candidate" /\ ~(wsource = "retained" /\ r.abandon)
    /\ wpc' = wnext
    /\ ZombieSeesNewHolder
    /\ UNCHANGED <<chain, stored, rows, flag, acked, cnonce, created, lock, archive, cvars>>
    /\ UNCHANGED <<wnext, wsnap, widx, wbroken, wd, wsource, wlive, wpath, wstored>>

(* release_account_if_queue_empty: storage.has_pending_candidate (no lock).*)
RelRead ==
    /\ wpc = "rel_read"
    /\ wpc' = IF Candidates # {} THEN wnext ELSE ClearEntry
    /\ WUnchangedS
    /\ UNCHANGED <<wnext, wsnap, widx, wbroken, wd, wsource, wlive, wpath, wstored>>

(* metadata/postgres.rs clear_pending_candidate_if_none: one transaction  *)
(* that locks the metadata row first (waiting out any submission holding   *)
(* it), then runs the conditional UPDATE, whose snapshot is taken after    *)
(* the lock and so sees every committed candidate.                         *)
ClrPg ==
    /\ wpc = "clr_pg"
    /\ lock = "none"
    /\ flag' = (flag /\ Candidates # {})
    /\ wpc' = wnext
    /\ ZombieSeesNewHolder
    /\ UNCHANGED <<chain, stored, rows, acked, cnonce, created, lock, archive, cvars>>
    /\ UNCHANGED <<wnext, wsnap, widx, wbroken, wd, wsource, wlive, wpath, wstored>>

(* metadata/mod.rs default clear_pending_candidate_if_none (the filesystem *)
(* metadata store): set_has_pending_candidate(false), no lock, no re-check.*)
ClrFs ==
    /\ wpc = "clr_fs"
    /\ flag' = FALSE
    /\ wpc' = wnext
    /\ UNCHANGED <<chain, stored, rows, acked, cnonce, created, lock, archive, zombie, cvars>>
    /\ UNCHANGED <<wnext, wsnap, widx, wbroken, wd, wsource, wlive, wpath, wstored>>

(* promote_candidate (postgres.rs / filesystem.rs): under the account lock,*)
(* flip the row only if it is still the exact kind the pass read, advance  *)
(* the state only if it still sits at prev_commitment, clear the flag when *)
(* no candidate remains. The filesystem twin skips the kind gate when the  *)
(* row is missing.                                                          *)
(* Postgres flips status and new_commitment on whatever row sits at the    *)
(* nonce; the filesystem twin rewrites the row from the promoted delta.    *)
(* source "superseded" (PromotableKind::Superseded; postgres.rs            *)
(* restore_superseded, filesystem.rs restore_superseded_holding_lock):     *)
(* write an archived row back as the canonical row at its nonce, only      *)
(* over a parked row (archived in turn), never a candidate or canonical.   *)
PromoteWrite(d, source) ==
    LET at == RowsAt(d.nonce)
        kindOk == IF Backend = "filesystem" /\ at = {}
                  THEN TRUE
                  ELSE \E r \in at : r.kind = source
        ok == kindOk /\ stored = d.prev
        after == IF Backend = "postgres"
                 THEN (rows \ at) \cup {[r EXCEPT !.kind = "canonical", !.new = d.new] : r \in at}
                 ELSE (rows \ at) \cup {[d EXCEPT !.kind = "canonical"]}
        archived == {r \in archive : r.nonce = d.nonce /\ r.prev = d.prev /\ r.new = d.new}
        replaceable == \A r \in at : r.kind \in {"retained", "abandoned"}
        restored == (rows \ at) \cup {[d EXCEPT !.kind = "canonical"]}
    IN IF source = "superseded"
       THEN IF replaceable /\ archived # {} /\ stored = d.prev
            THEN /\ rows' = restored
                 /\ archive' = (archive \ archived) \cup at
                 /\ stored' = d.new
                 /\ flag' = (flag /\ \E r \in restored : r.kind = "candidate")
            ELSE UNCHANGED <<rows, stored, flag, archive>>
       ELSE IF ok
       THEN /\ rows' = after
            /\ stored' = d.new
            /\ flag' = (flag /\ \E r \in after : r.kind = "candidate")
            /\ UNCHANGED archive
       ELSE UNCHANGED <<rows, stored, flag, archive>>

Promote ==
    /\ wpc = "promote"
    /\ lock = "none"
    /\ PromoteWrite(wd, wsource)
    /\ wpc' = wnext
    /\ ZombieSeesNewHolder
    /\ UNCHANGED <<chain, acked, cnonce, created, lock, cvars>>
    /\ UNCHANGED <<wnext, wsnap, widx, wbroken, wd, wsource, wlive, wpath, wstored>>

(* worker.rs: the lease is lost (renewal fails, holder stalls) while a     *)
(* promotion is about to be written; another replica takes the lease and   *)
(* starts its own pass while the old write is still in flight.             *)
LoseLease ==
    /\ wpc = "promote"
    /\ ~zombie.active
    /\ zombie' = [active |-> TRUE, blocked |-> FALSE, d |-> wd, source |-> wsource]
    /\ wpc' = "idle"
    /\ UNCHANGED <<chain, stored, rows, flag, acked, cnonce, created, lock, archive, cvars>>
    /\ UNCHANGED <<wnext, wsnap, widx, wbroken, wd, wsource, wlive, wpath, wstored>>

(* postgres.rs lease_fence_is_current runs after lock_account_metadata, in *)
(* the write's own transaction. A stale write whose fence check ran before  *)
(* the lease expired can still commit, but only before the new holder's   *)
(* first step that takes the account lock; once one has, the stale check   *)
(* runs after the steal and is refused (StaleLease). This ordering is the  *)
(* WritesInTokenOrder guarantee checked by .agents/skills/                 *)
(* verify-worker-lease. The filesystem backend is single-process and has   *)
(* no lease to lose.                                                        *)
ZombieWrite ==
    /\ zombie.active
    /\ lock = "none"
    /\ IF zombie.blocked
       THEN UNCHANGED <<rows, stored, flag, archive>>
       ELSE PromoteWrite(zombie.d, zombie.source)
    /\ zombie' = [active |-> FALSE, blocked |-> FALSE, d |-> NoDelta, source |-> "none"]
    /\ UNCHANGED <<chain, acked, cnonce, created, lock, cvars, wvars>>

-----------------------------------------------------------------------------
(* Fast promotion pass: process_recent_pass reads a time window of         *)
(* candidates (any subset here), sorted by nonce per account.              *)

FastPull ==
    /\ wpc = "fast_pull"
    /\ \E S \in SUBSET Candidates :
         /\ wsnap' = Sorted(S)
         /\ wpc' = IF S = {} THEN "idle" ELSE "fast_iter"
    /\ widx' = 1
    /\ wbroken' = FALSE
    /\ WUnchangedS
    /\ UNCHANGED <<wnext, wd, wsource, wlive, wpath, wstored>>

FastIter ==
    /\ wpc = "fast_iter"
    /\ IF widx > Len(wsnap)
       THEN /\ wpc' = "idle"
            /\ UNCHANGED wd
       ELSE /\ wd' = wsnap[widx]
            /\ wpc' = IF widx = 1 THEN "fr_observe" ELSE "fast_stored"
    /\ WUnchangedS
    /\ UNCHANGED <<wnext, wsnap, widx, wbroken, wsource, wlive, wpath, wstored>>

(* Fast mode: a candidate off the stored base stops the account's batch.   *)
FastStored ==
    /\ wpc = "fast_stored"
    /\ wpc' = IF stored = wd.prev THEN "fr_observe" ELSE "idle"
    /\ WUnchangedS
    /\ UNCHANGED <<wnext, wsnap, widx, wbroken, wd, wsource, wlive, wpath, wstored>>

(* process_recent_candidate: verify_commitment on the claim first. *)
FrObserve ==
    /\ wpc = "fr_observe"
    /\ LET h == Last(chain) IN
       wpc' = IF h = wd.new \/ h \in Successors(wsnap, widx) THEN "fr_read" ELSE "fast_next"
    /\ WUnchangedS
    /\ UNCHANGED <<wnext, wsnap, widx, wbroken, wd, wsource, wlive, wpath, wstored>>

FrRead ==
    /\ wpc = "fr_read"
    /\ IF stored # wd.prev
       THEN /\ wpc' = "fast_next"
            /\ UNCHANGED <<wnext, wsource>>
       ELSE /\ wsource' = "candidate"
            /\ wnext' = "fast_next"
            /\ wpc' = "promote"
    /\ WUnchangedS
    /\ UNCHANGED <<wsnap, widx, wbroken, wd, wlive, wpath, wstored>>

FastNext ==
    /\ wpc = "fast_next"
    /\ widx' = widx + 1
    /\ wpc' = "fast_iter"
    /\ WUnchangedS
    /\ UNCHANGED <<wnext, wsnap, wbroken, wd, wsource, wlive, wpath, wstored>>

-----------------------------------------------------------------------------
(* Reconcile pass: reconciliation.rs reconcile_account. *)

RcPull ==
    /\ wpc = "rc_pull"
    /\ LET live == {r \in rows : r.kind \in {"retained", "abandoned"}}
                   \cup {[r EXCEPT !.kind = "superseded"] : r \in archive}
       IN /\ wlive' = live
          /\ wpc' = IF Candidates # {} \/ live = {} THEN "idle" ELSE "rc_expire"
    /\ WUnchangedS
    /\ UNCHANGED <<wnext, wsnap, widx, wbroken, wd, wsource, wpath, wstored>>

(* TTL expiry: remove_delta(kind Retained) -> discard_candidate, locked,   *)
(* with the in-transaction flag clear.                                     *)
RcExpire ==
    /\ wpc = "rc_expire"
    /\ lock = "none"
    /\ \E r \in wlive :
         /\ r.kind = "retained"
         /\ wlive' = wlive \ {r}
         /\ LET gone == {x \in RowsAt(r.nonce) : x.kind = "retained"}
                after == rows \ gone
            IN /\ rows' = after
               /\ flag' = (flag /\ \E x \in after : x.kind = "candidate")
    /\ ZombieSeesNewHolder
    /\ UNCHANGED <<chain, stored, acked, cnonce, created, lock, archive, cvars>>
    /\ UNCHANGED <<wpc, wnext, wsnap, widx, wbroken, wd, wsource, wpath, wstored>>

(* reconcile_account -> prune_superseded_deltas: archived rows past the     *)
(* retention TTL are deleted (no account lock). Any archived row may age   *)
(* out at any time here.                                                    *)
RcPrune ==
    /\ \E r \in archive : archive' = archive \ {r}
    /\ UNCHANGED <<chain, stored, rows, flag, acked, cnonce, created, lock, zombie, cvars, wvars>>

RcRead ==
    /\ wpc = "rc_expire"
    /\ wstored' = stored
    /\ wpc' = IF wlive = {} THEN "idle" ELSE "rc_observe"
    /\ WUnchangedS
    /\ UNCHANGED <<wnext, wsnap, widx, wbroken, wd, wsource, wlive, wpath>>

(* verify_commitment against the stored commitment, then                  *)
(* reconcile_chain_advance: a multi-hop path, or one matching row.         *)
RcObserve ==
    /\ wpc = "rc_observe"
    /\ LET h == Last(chain)
           path == SelectPath(wstored, h, wlive)
           single == {r \in wlive : r.prev = wstored /\ r.new = h}
       IN IF h = wstored
          THEN /\ wpc' = "idle"
               /\ UNCHANGED <<wpath, widx, wd, wsource, wnext>>
          ELSE IF Len(path) > 1
          THEN /\ wpath' = path
               /\ widx' = 1
               /\ wpc' = "rc_path"
               /\ UNCHANGED <<wd, wsource, wnext>>
          ELSE IF single # {}
          THEN \E r \in single :
                 /\ wd' = r
                 /\ wsource' = r.kind
                 /\ wnext' = "idle"
                 /\ wpc' = "promote"
                 /\ UNCHANGED <<wpath, widx>>
          ELSE /\ wpc' = "idle"
               /\ UNCHANGED <<wpath, widx, wd, wsource, wnext>>
    /\ WUnchangedS
    /\ UNCHANGED <<wsnap, wbroken, wlive, wstored>>

(* reconcile_recoverable_path: promote every hop in order. *)
RcPath ==
    /\ wpc = "rc_path"
    /\ IF widx > Len(wpath)
       THEN /\ wpc' = "idle"
            /\ UNCHANGED <<wd, wsource, wnext>>
       ELSE /\ wd' = wpath[widx]
            /\ wsource' = wpath[widx].kind
            /\ wnext' = "rc_path_next"
            /\ wpc' = "promote"
    /\ WUnchangedS
    /\ UNCHANGED <<wsnap, widx, wbroken, wlive, wpath, wstored>>

RcPathNext ==
    /\ wpc = "rc_path_next"
    /\ widx' = widx + 1
    /\ wpc' = "rc_path"
    /\ WUnchangedS
    /\ UNCHANGED <<wnext, wsnap, wbroken, wd, wsource, wlive, wpath, wstored>>

-----------------------------------------------------------------------------

Next ==
    \/ \E c \in Clients :
         ClientBuild(c) \/ ClientLabel(c) \/ ClientSubmit(c) \/ ClientCommit(c) \/ ClientFsFlag(c)
    \/ Abandon
    \/ Land
    \/ WIdle \/ FullList \/ FullPull \/ FullIter \/ FullStored \/ FullPredQ \/ FullNext
    \/ PfcRead \/ PfcObserve \/ ParkFlip \/ ParkRefused \/ RelRead
    \/ ClrPg \/ ClrFs \/ Promote
    \/ (LeaseLoss /\ Backend = "postgres" /\ LoseLease) \/ ZombieWrite
    \/ FastPull \/ FastIter \/ FastStored \/ FrObserve \/ FrRead \/ FastNext
    \/ RcPull \/ RcExpire \/ RcPrune \/ RcRead \/ RcObserve \/ RcPath \/ RcPathNext

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Properties *)

(* The state Guardian serves as canonical is one the chain actually held,  *)
(* and every delta it recorded as canonical is a step the chain took, with *)
(* the payload that produces it.                                           *)
CanonicalIsOnChain ==
    /\ stored \in Range(chain)
    /\ \A r \in rows : r.kind = "canonical" =>
         /\ r.new = r.pid
         /\ r.prev \in Range(chain)
         /\ r.new \in Range(chain)
         /\ ChainIndex(chain, r.new) = ChainIndex(chain, r.prev) + 1

(* The stored canonical state only moves forward along the chain: never    *)
(* back to an older state, never sideways onto a branch the chain left.    *)
CanonicalMovesForward ==
    [][stored' # stored =>
         /\ stored' \in Range(chain')
         /\ stored \in Range(chain')
         /\ ChainIndex(chain', stored') > ChainIndex(chain', stored)]_vars

(* Never two queued candidates on the same base: Guardian is never holding *)
(* two acknowledged, competing transactions in its queue at once.          *)
NoCompetingCandidates ==
    \A r1, r2 \in Candidates : r1 # r2 => r1.prev # r2.prev

(* An account with a queued candidate is always listed for the full pass   *)
(* (list_with_pending_candidates reads has_pending_candidate), outside the *)
(* moment a submission is still inside its own locked write.               *)
QueuedAccountIsListed ==
    (\A c \in Clients : cpc[c] \notin {"commit", "fsflag"})
        => (Candidates # {} => flag)

=============================================================================
