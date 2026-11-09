----------------------------- MODULE AuthReplay -----------------------------
(***************************************************************************)
(* Replay protection for one Guardian account: signed requests carry a     *)
(* client timestamp, and each signer (or session key) has a replay floor   *)
(* that a request must strictly exceed to be accepted. Checked against     *)
(* every interleaving of honest requests, replays of any request ever      *)
(* sent, concurrent request handlers, session grant and revoke, the        *)
(* session-floor purge, and the passage of time.                           *)
(*                                                                         *)
(* One model step is one storage statement (the Postgres CAS and purge are *)
(* single statements; the filesystem twins run under the auth_state write  *)
(* lock), or one in-memory check between two awaits. Paths are relative   *)
(* to crates/server/src/.                                                  *)
(***************************************************************************)
EXTENDS Naturals, Integers, FiniteSets

CONSTANTS
    Handlers,     \* concurrent request handlers (any replica)
    MaxRequests,  \* bound on distinct requests clients sign
    MaxTime,      \* bound on the server clock
    Skew,         \* MAX_TIMESTAMP_SKEW, in clock units
    Ttl,          \* session max TTL, in clock units
    SignKeys,     \* keys clients sign with: subset of {"s1", "s2", "sess"}
    Kinds,        \* request kinds clients send: subset of
                  \* {"configure", "push", "read", "nonce"}
    MaxStall,     \* longest a handler may sit between its skew check and its
                  \* last step, in clock units (MaxTime or more = unbounded)
    Scenario,     \* "fresh": account not configured yet;
                  \* "existing": account configured, no legacy floor;
                  \* "migrated": account exists with a legacy account floor
    FloorChecks,  \* the session floor's freshness and fence checks (the code:
                  \* TRUE; FALSE reproduces F2)
    NonceFuture,  \* how far ahead of the server clock a session nonce poll may
                  \* be stamped (the code: 30 s against the 60 s past window)
    NonceRecheck, \* the nonce serving step re-checks the window against the
                  \* database clock (the code: TRUE)
    NonceLiveCheck \* the nonce serving step looks the session up in the same
                  \* call (the code: TRUE)

VARIABLES
    now,       \* server clock (all replicas read the same clock)
    exists,    \* account_metadata row present
    floors,    \* account_auth_state: floor key -> last accepted timestamp
    version,   \* the stored account state: 0 = a configure's initial state,
               \* each accepted push adds 1
    session,   \* the s1 session: [st |-> "none"|"live"|"revoked", at |-> grant time]
    sent,      \* every request a client ever signed (a replay can resend any)
    acc,       \* acc[id]: how many times request id won the CAS
    wrote,     \* wrote[id]: how many handler runs of request id wrote
    floorFence, \* replay_fence 'session_floor': floors at or below it are refused
    readFence,  \* replay_fence 'session_read'
    seen,       \* session_read_seen: the session reads admitted
    nbad,       \* a session nonce poll was served outside a live session or its window
    hpc, hreq, hexist, hwrote, hstart

vars == <<now, exists, floors, version, session, sent, acc, wrote, floorFence, readFence, seen,
          nbad, hpc, hreq, hexist, hwrote, hstart>>

FenceVars == <<floorFence, readFence, seen, nbad>>

Wallets == {"s1", "s2"}
FloorKeys == Wallets \cup {"sess"}
Ids == 0..MaxRequests
None == -100
LegacyFloor == IF Scenario = "migrated" THEN 1 ELSE None
NoReq == [id |-> 0, kind |-> "none", key |-> "s1", ts |-> 0]

PreMigration == [id |-> 0, kind |-> "push", key |-> "s2", ts |-> 1]

Max(a, b) == IF a > b THEN a ELSE b
Abs(x) == IF x < 0 THEN -x ELSE x

(* Proposed window-only session nonce polls (decision-nonce-read-window.md *)
(* in speckit/features/317-secure-scalability): a session-signed nonce     *)
(* poll is served every time it arrives inside its window, which reaches   *)
(* Skew into the past and only NonceFuture ahead. Every other request keeps *)
(* the symmetric window. A wallet-signed nonce stays a floor request.      *)
NonceReq(r) == r.kind = "nonce" /\ r.key = "sess"

InWindow(r) ==
    IF NonceReq(r)
    THEN now - r.ts <= Skew /\ r.ts - now <= NonceFuture
    ELSE Abs(now - r.ts) <= Skew

(* session.rs stale_floor_age_ms *)
PurgeCutoff == now - (Ttl + 2 * Skew)

Init ==
    /\ now = 1
    /\ exists = (Scenario # "fresh")
    /\ floors = [k \in FloorKeys |-> None]
    /\ version = 0
    /\ session = [st |-> "none", at |-> 0]
    /\ sent = IF Scenario = "migrated" THEN {PreMigration} ELSE {}
    /\ acc = [i \in Ids |-> IF Scenario = "migrated" /\ i = 0 THEN 1 ELSE 0]
    /\ wrote = [i \in Ids |-> IF Scenario = "migrated" /\ i = 0 THEN 1 ELSE 0]
    /\ hpc = [h \in Handlers |-> "idle"]
    /\ hreq = [h \in Handlers |-> NoReq]
    /\ hexist = [h \in Handlers |-> FALSE]
    /\ hwrote = [h \in Handlers |-> FALSE]
    /\ hstart = [h \in Handlers |-> 0]
    /\ floorFence = None
    /\ readFence = None
    /\ seen = {}
    /\ nbad = FALSE

-----------------------------------------------------------------------------
(* Environment *)

PastSkew == {"p_find", "p_cas", "p_record", "p_write", "c_existing", "c_claim", "c_cas_first", "c_state", "c_meta"}

(* Time only passes while no handler would exceed MaxStall since its skew *)
(* check. Nothing in the server bounds this today: there is no request    *)
(* timeout, and the pool waits for a connection without a deadline.       *)
Tick ==
    /\ now < MaxTime
    /\ \A h \in Handlers : hpc[h] \in PastSkew => now + 1 - hstart[h] <= MaxStall
    /\ now' = now + 1
    /\ UNCHANGED <<exists, floors, version, session, sent, acc, wrote, hpc, hreq, hexist, hwrote, hstart>>
    /\ UNCHANGED FenceVars

(* A client signs a new request; its clock may sit anywhere in the skew    *)
(* window around the server's. Ids start at 1 (0 is the pre-migration      *)
(* request in the "migrated" scenario).                                    *)
Sign ==
    /\ Cardinality({r \in sent : r.id > 0}) < MaxRequests
    /\ \E kind \in Kinds, key \in SignKeys, ts \in (now - Skew)..(now + Skew) :
         /\ ts >= 0
         /\ ~(kind = "configure" /\ key = "sess")
         /\ sent' = sent \cup {[id |-> Cardinality({r \in sent : r.id > 0}) + 1,
                                kind |-> kind, key |-> key, ts |-> ts]}
    /\ UNCHANGED <<now, exists, floors, version, session, acc, wrote, hpc, hreq, hexist, hwrote, hstart>>
    /\ UNCHANGED FenceVars

(* session.rs grant (POST /session) and revoke; one session, for s1. *)
Grant ==
    /\ "sess" \in SignKeys
    /\ session.st = "none"
    /\ session' = [st |-> "live", at |-> now]
    /\ UNCHANGED <<now, exists, floors, version, sent, acc, wrote, hpc, hreq, hexist, hwrote, hstart>>
    /\ UNCHANGED FenceVars

Revoke ==
    /\ session.st = "live"
    /\ session' = [session EXCEPT !.st = "revoked"]
    /\ UNCHANGED <<now, exists, floors, version, sent, acc, wrote, hpc, hreq, hexist, hwrote, hstart>>
    /\ UNCHANGED FenceVars

SessionLive == session.st = "live" /\ now < session.at + Ttl

(* builder/handle.rs start_session_sweep_worker -> purge_session_floors   *)
(* (metadata/postgres.rs guardian_raise_session_floor_fence, then          *)
(* guardian_delete_fenced_session_floors; metadata/filesystem.rs under the *)
(* auth_state write lock): raise the floor fence to just below the cutoff, *)
(* then delete.                                                            *)
FloorDeletable == floors["sess"] # None /\ floors["sess"] < PurgeCutoff
FenceBehind == FloorChecks /\ PurgeCutoff - 1 > floorFence

Purge ==
    /\ FloorDeletable \/ FenceBehind
    /\ floors' = IF FloorDeletable THEN [floors EXCEPT !["sess"] = None] ELSE floors
    /\ floorFence' = IF FenceBehind THEN PurgeCutoff - 1 ELSE floorFence
    /\ UNCHANGED <<now, exists, version, session, sent, acc, wrote, hpc, hreq, hexist, hwrote, hstart>>
    /\ UNCHANGED <<readFence, seen, nbad>>

(* coordination/read_records.rs maintain (guardian_advance_session_read_fence *)
(* under the exclusive fence lock): raise the read fence to the retention  *)
(* cutoff, then drop the reads at or below it. Retention is the skew window *)
(* plus a margin (session.rs READ_RECORD_RETAIN_MS), here one unit.        *)
ReadCutoff == now - (Skew + 1)

PurgeReads ==
    /\ "read" \in Kinds
    /\ ReadCutoff > readFence
    /\ readFence' = ReadCutoff
    /\ seen' = {s \in seen : s.ts > ReadCutoff}
    /\ UNCHANGED <<now, exists, floors, version, session, sent, acc, wrote, hpc, hreq, hexist, hwrote, hstart>>
    /\ UNCHANGED <<floorFence, nbad>>

-----------------------------------------------------------------------------
(* Request handlers. A handler serves any request ever sent: the first     *)
(* run of a request is the honest one, any later run is a replay.          *)

HUnchanged == UNCHANGED <<now, session, sent, floorFence, readFence, seen, nbad>>

Start(h) ==
    /\ hpc[h] = "idle"
    /\ \E r \in sent :
         /\ hreq' = [hreq EXCEPT ![h] = r]
         /\ hpc' = [hpc EXCEPT ![h] = IF r.kind = "configure" THEN "c_skew" ELSE "p_meta"]
    /\ hwrote' = [hwrote EXCEPT ![h] = FALSE]
    /\ HUnchanged
    /\ UNCHANGED <<exists, floors, version, acc, wrote, hexist, hstart>>

Done(h) == hpc' = [hpc EXCEPT ![h] = "idle"]

(* services/mod.rs consume_auth_timestamp -> update_last_auth_timestamp_cas *)
(* (metadata/postgres.rs: INSERT ... WHERE NOT EXISTS (legacy >= ts) ON    *)
(* CONFLICT DO UPDATE ... WHERE last < ts; metadata/filesystem.rs: check   *)
(* max(signer, legacy) and swap under the auth_state write lock). A        *)
(* session floor goes through advance_session_floor instead, which first   *)
(* re-checks the timestamp window and the floor fence in the same step.    *)
FloorAdmits(r) ==
    \/ r.key # "sess"
    \/ ~FloorChecks
    \/ (Abs(now - r.ts) <= Skew /\ r.ts > floorFence)

CasWins(r) == r.ts > Max(floors[r.key], LegacyFloor) /\ FloorAdmits(r)

CasStep(h, onWin) ==
    LET r == hreq[h] IN
    IF CasWins(r)
    THEN /\ floors' = [floors EXCEPT ![r.key] = r.ts]
         /\ acc' = [acc EXCEPT ![r.id] = @ + 1]
         /\ hpc' = [hpc EXCEPT ![h] = onWin]
    ELSE /\ UNCHANGED <<floors, acc>>
         /\ Done(h)

(* A handler's write; a run counts once in wrote[id] however many writes. *)
Write(h) ==
    /\ hwrote' = [hwrote EXCEPT ![h] = TRUE]
    /\ wrote' = IF hwrote[h] THEN wrote ELSE [wrote EXCEPT ![hreq[h].id] = @ + 1]

(* --- push_delta and every other account route: services/mod.rs          *)
(* resolve_account_with_head -> consume_auth_timestamp, then the route's write. *)

PMeta(h) ==
    /\ hpc[h] = "p_meta"
    /\ IF exists THEN hpc' = [hpc EXCEPT ![h] = "p_skew"] ELSE Done(h)
    /\ HUnchanged
    /\ UNCHANGED <<exists, floors, version, acc, wrote, hreq, hexist, hwrote, hstart>>

(* services/mod.rs validate_request_timestamp *)
PSkew(h) ==
    /\ hpc[h] = "p_skew"
    /\ IF ~InWindow(hreq[h]) THEN Done(h)
       ELSE hpc' = [hpc EXCEPT ![h] = IF hreq[h].key = "sess" THEN "p_find" ELSE "p_cas"]
    /\ hstart' = [hstart EXCEPT ![h] = now]
    /\ HUnchanged
    /\ UNCHANGED <<exists, floors, version, acc, wrote, hreq, hexist, hwrote>>

(* services/mod.rs verify_session_request -> session.rs find, and the     *)
(* replay check resolve_account_with_head or resolve_unique_session_read picks: *)
(* a session read is admitted once, every other request advances the floor. *)
PFind(h) ==
    /\ hpc[h] = "p_find"
    /\ ~NonceReq(hreq[h])
    /\ IF SessionLive
       THEN hpc' = [hpc EXCEPT ![h] =
                IF hreq[h].kind = "read" THEN "p_record" ELSE "p_cas"]
       ELSE Done(h)
    /\ HUnchanged
    /\ UNCHANGED <<exists, floors, version, acc, wrote, hreq, hexist, hwrote, hstart>>

PCas(h) ==
    /\ hpc[h] = "p_cas"
    /\ CasStep(h, IF hreq[h].kind \in {"read", "nonce"} THEN "idle" ELSE "p_write")
    /\ HUnchanged
    /\ UNCHANGED <<exists, version, wrote, hreq, hexist, hwrote, hstart>>

(* services/mod.rs resolve_unique_session_read -> admit_read_record       *)
(* (guardian_record_session_read: shared fence lock, window re-check, fence *)
(* check, insert, in one call): a session read is admitted               *)
(* once instead of advancing a floor. The record may wait, so the session   *)
(* is looked up again after it; a read whose session ended meanwhile is    *)
(* refused and its record removed in the same transaction (Postgres). The  *)
(* local stores keep that record, which only refuses more.                 *)
PRecord(h) ==
    LET r == hreq[h] IN
    /\ hpc[h] = "p_record"
    /\ IF Abs(now - r.ts) <= Skew /\ r.ts > readFence /\ r.id \notin {s.id : s \in seen}
            /\ SessionLive
       THEN /\ seen' = seen \cup {[id |-> r.id, ts |-> r.ts]}
            /\ acc' = [acc EXCEPT ![r.id] = @ + 1]
       ELSE UNCHANGED <<seen, acc>>
    /\ Done(h)
    /\ UNCHANGED <<now, session, sent, floorFence, readFence, nbad>>
    /\ UNCHANGED <<exists, floors, version, wrote, hreq, hexist, hwrote, hstart>>

(* A session nonce poll is served by one read-only call                  *)
(* (guardian_serve_session_read) that looks up the session and re-checks *)
(* the window against one reading of the database clock, then reads the  *)
(* state head. Nothing is recorded and no floor moves. NonceServes is what *)
(* the call returns data for; NonceLiveCheck and NonceRecheck switch off  *)
(* its two checks, so the property is checked against what is served, not *)
(* assumed by it.                                                          *)
NonceServes(r) ==
    /\ (~NonceLiveCheck \/ SessionLive)
    /\ (~NonceRecheck \/ InWindow(r))

PNonce(h) ==
    LET r == hreq[h] IN
    /\ hpc[h] = "p_find"
    /\ NonceReq(r)
    /\ nbad' = (nbad \/ (NonceServes(r) /\ ~(SessionLive /\ InWindow(r))))
    /\ Done(h)
    /\ UNCHANGED <<now, session, sent, floorFence, readFence, seen>>
    /\ UNCHANGED <<exists, floors, version, acc, wrote, hreq, hexist, hwrote, hstart>>

(* services/push_delta.rs: the delta advances the stored state. *)
PWrite(h) ==
    /\ hpc[h] = "p_write"
    /\ version' = version + 1
    /\ Write(h)
    /\ Done(h)
    /\ HUnchanged
    /\ UNCHANGED <<exists, floors, acc, hreq, hexist, hstart>>

(* --- services/configure_account.rs configure_account --- *)

CSkew(h) ==
    /\ hpc[h] = "c_skew"
    /\ IF Abs(now - hreq[h].ts) > Skew THEN Done(h) ELSE hpc' = [hpc EXCEPT ![h] = "c_existing"]
    /\ hstart' = [hstart EXCEPT ![h] = now]
    /\ HUnchanged
    /\ UNCHANGED <<exists, floors, version, acc, wrote, hreq, hexist, hwrote>>

(* state.metadata.get: an existing account goes straight to the replay    *)
(* check; a new one first claims the account.                              *)
CExisting(h) ==
    /\ hpc[h] = "c_existing"
    /\ hexist' = [hexist EXCEPT ![h] = exists]
    /\ hpc' = [hpc EXCEPT ![h] = IF exists THEN "c_cas_first" ELSE "c_claim"]
    /\ HUnchanged
    /\ UNCHANGED <<exists, floors, version, acc, wrote, hreq, hwrote, hstart>>

(* metadata.create (INSERT ... ON CONFLICT DO NOTHING; the filesystem twin *)
(* under the cache lock): the winner creates the account's metadata row;   *)
(* a loser re-reads it and continues as a reconfiguration of that account. *)
CClaim(h) ==
    /\ hpc[h] = "c_claim"
    /\ IF exists
       THEN /\ hexist' = [hexist EXCEPT ![h] = TRUE]
            /\ UNCHANGED <<exists, wrote, hwrote>>
       ELSE /\ exists' = TRUE
            /\ UNCHANGED <<hexist, wrote, hwrote>>
    /\ hpc' = [hpc EXCEPT ![h] = "c_cas_first"]
    /\ HUnchanged
    /\ UNCHANGED <<floors, version, acc, hreq, hstart>>

(* consume_auth_timestamp, before any state write, for every configuration *)
(* (the claim above guarantees the metadata row its FK needs).             *)
CCasFirst(h) ==
    /\ hpc[h] = "c_cas_first"
    /\ CasStep(h, "c_state")
    /\ HUnchanged
    /\ UNCHANGED <<exists, version, wrote, hreq, hexist, hwrote, hstart>>

(* storage submit_state: the request's initial_state replaces the state. *)
CState(h) ==
    /\ hpc[h] = "c_state"
    /\ version' = 0
    /\ Write(h)
    /\ hpc' = [hpc EXCEPT ![h] = "c_meta"]
    /\ HUnchanged
    /\ UNCHANGED <<exists, floors, acc, hreq, hexist, hstart>>

(* metadata.set, for an account that existed (or that this request lost   *)
(* the claim to); a created account's row was written by the claim.       *)
CMeta(h) ==
    /\ hpc[h] = "c_meta"
    /\ IF hexist[h]
       THEN Write(h)
       ELSE UNCHANGED <<wrote, hwrote>>
    /\ hpc' = [hpc EXCEPT ![h] = "idle"]
    /\ HUnchanged
    /\ UNCHANGED <<exists, floors, version, acc, hreq, hexist, hstart>>

-----------------------------------------------------------------------------

Next ==
    \/ Tick \/ Sign \/ Grant \/ Revoke \/ Purge \/ PurgeReads
    \/ \E h \in Handlers :
         \/ Start(h)
         \/ PMeta(h) \/ PSkew(h) \/ PFind(h) \/ PNonce(h) \/ PCas(h) \/ PRecord(h) \/ PWrite(h)
         \/ CSkew(h) \/ CExisting(h) \/ CClaim(h) \/ CCasFirst(h) \/ CState(h) \/ CMeta(h)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Properties *)

(* A signed request is accepted at most once, however often it is resent  *)
(* and whichever handler or replica serves it.                             *)
AcceptedAtMostOnce == \A i \in Ids : acc[i] <= 1

(* A signed request overwrites stored data at most once: a replay never   *)
(* overwrites, even one the replay check goes on to reject. Creating the  *)
(* account's absent metadata row in the claim (CClaim) is not counted: it *)
(* succeeds only where no row exists, so it can never land over data the  *)
(* account has moved to.                                                   *)
WritesAtMostOnce == \A i \in Ids : wrote[i] <= 1

(* Proposed nonce polls: served only while the session is live and the    *)
(* request is inside its window, however long a handler stalled; and they  *)
(* never take part in replay state, so they cannot consume a floor or a    *)
(* read record another request relies on.                                  *)
NonceServedOnlyLiveInWindow == ~nbad

NonceLeavesReplayState ==
    \A r \in sent : NonceReq(r) => (acc[r.id] = 0 /\ r.id \notin {s.id : s \in seen})

(* A replay floor never moves back while it exists (the purge may delete  *)
(* a stale session floor outright).                                        *)
FloorsOnlyRise ==
    [][\A k \in FloorKeys : (floors[k] # None /\ floors'[k] # None) => floors'[k] >= floors[k]]_vars

=============================================================================
