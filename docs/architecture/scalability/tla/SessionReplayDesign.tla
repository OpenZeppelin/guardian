------------------------- MODULE SessionReplayDesign -------------------------
(***************************************************************************)
(* Design model, not a code mirror: replay protection for session-signed  *)
(* requests on one account, with the design choices as constants so TLC  *)
(* can compare them.                                                       *)
(*                                                                         *)
(*   ReadMode       "strict": reads consume the session replay floor, as  *)
(*                  today; "window": reads check only the timestamp       *)
(*                  window and the live session.                          *)
(*   Bound          the signed message includes the route.                 *)
(*   RespEnc        read responses are encrypted to the session key.       *)
(*   CollidingWrite a write route signs the same payload as the reads     *)
(*                  ({account_id}). No such route exists today.            *)
(*                                                                         *)
(* Writes always keep the strict floor. A delivery is one atomic step     *)
(* (skew check, session lookup, replay check, effect); step-level races   *)
(* inside a handler are the code model's job                              *)
(* (.agents/skills/verify-auth-replay).                                    *)
(*                                                                         *)
(* The client delivers each request it signs exactly once, in any order   *)
(* and concurrently with others. An attacker holds a copy of every signed *)
(* request (leaked headers) and may deliver it any number of times, at    *)
(* any moment, on any route the signature verifies for.                   *)
(***************************************************************************)
EXTENDS Naturals, Integers, FiniteSets

CONSTANTS
    MaxRequests,
    MaxTime,
    Skew,
    ReadMode,
    Bound,
    RespEnc,
    CollidingWrite,
    Attacker     \* FALSE: only the client's own traffic (for the zero-failures check)

VARIABLES
    now,
    floor,       \* the session key's replay floor (None = never consumed)
    live,        \* the session is live (FALSE once revoked)
    sent,        \* every request the client signed: [id, route, ts]
    honest,      \* honest[id]: outcome of the client's own delivery
    writes,      \* writes[id]: accepted write executions (saturates at 2)
    offRoute,    \* some delivery was accepted on a route it was not signed for
    offRouteWrite, \* ... and that route is a write
    leakAfter,   \* a replayer read plaintext from a request already served to the client
    leakAny      \* a replayer read plaintext at all

vars == <<now, floor, live, sent, honest, writes, offRoute, offRouteWrite, leakAfter, leakAny>>

None == -100
Ids == 1..MaxRequests

Reads == {"get_state", "get_nonce"}
Writes == IF CollidingWrite THEN {"push", "w2"} ELSE {"push"}
Routes == Reads \cup Writes

(* api/http.rs: StateQuery, ProposalQuery and an uncursored HistoryQuery all *)
(* serialize to {account_id}; push signs its delta.                         *)
Payload(r) == IF r = "push" THEN "delta" ELSE "account_id"

SigValid(q, r) == IF Bound THEN r = q.route ELSE Payload(r) = Payload(q.route)

Abs(x) == IF x < 0 THEN -x ELSE x
Min(a, b) == IF a < b THEN a ELSE b

Init ==
    /\ now = 1
    /\ floor = None
    /\ live = TRUE
    /\ sent = {}
    /\ honest = [i \in Ids |-> "unsent"]
    /\ writes = [i \in Ids |-> 0]
    /\ offRoute = FALSE
    /\ offRouteWrite = FALSE
    /\ leakAfter = FALSE
    /\ leakAny = FALSE

Tick ==
    /\ now < MaxTime
    /\ now' = now + 1
    /\ UNCHANGED <<floor, live, sent, honest, writes, offRoute, offRouteWrite, leakAfter, leakAny>>

Revoke ==
    /\ live
    /\ live' = FALSE
    /\ UNCHANGED <<now, floor, sent, honest, writes, offRoute, offRouteWrite, leakAfter, leakAny>>

Sign ==
    /\ Cardinality(sent) < MaxRequests
    /\ \E r \in Routes, ts \in (now - Skew)..(now + Skew) :
         /\ ts >= 0
         /\ LET id == Cardinality(sent) + 1 IN
              /\ sent' = sent \cup {[id |-> id, route |-> r, ts |-> ts]}
              /\ honest' = [honest EXCEPT ![id] = "pending"]
    /\ UNCHANGED <<now, floor, live, writes, offRoute, offRouteWrite, leakAfter, leakAny>>

(* One delivery of request q on route r, by the client or a replayer. *)
Deliver(q, r, byAttacker) ==
    LET isRead == r \in Reads
        strict == ~isRead \/ ReadMode = "strict"
        okSkew == Abs(now - q.ts) <= Skew
        okReplay == ~strict \/ q.ts > floor
        accepted == okSkew /\ live /\ okReplay
        leaks == accepted /\ byAttacker /\ isRead /\ ~RespEnc
    IN
    /\ floor' = IF accepted /\ strict THEN q.ts ELSE floor
    /\ honest' = IF byAttacker THEN honest
                 ELSE [honest EXCEPT ![q.id] =
                        IF accepted THEN "accepted"
                        ELSE IF okSkew /\ live THEN "rejected_replay"
                        ELSE "rejected_other"]
    /\ writes' = IF accepted /\ r \in Writes
                 THEN [writes EXCEPT ![q.id] = Min(@ + 1, 2)]
                 ELSE writes
    /\ offRoute' = (offRoute \/ (accepted /\ r # q.route))
    /\ offRouteWrite' = (offRouteWrite \/ (accepted /\ r # q.route /\ r \in Writes))
    /\ leakAfter' = (leakAfter \/ (leaks /\ honest[q.id] = "accepted"))
    /\ leakAny' = (leakAny \/ leaks)
    /\ UNCHANGED <<now, live, sent>>

ClientDelivers ==
    \E q \in sent : honest[q.id] = "pending" /\ Deliver(q, q.route, FALSE)

AttackerDelivers ==
    /\ Attacker
    /\ \E q \in sent, r \in Routes : SigValid(q, r) /\ Deliver(q, r, TRUE)

Next == Tick \/ Revoke \/ Sign \/ ClientDelivers \/ AttackerDelivers

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Properties *)

(* The zero auth-window failures criterion, for reads: the client's own   *)
(* read is never refused by the replay check while its timestamp is in    *)
(* the window and the session is live, whatever else it sends in parallel *)
(* or in whatever order its requests arrive. Checked with Attacker =      *)
(* FALSE: an attacker who front-runs a captured request can always burn  *)
(* its floor, which is a denial of service, not an auth-window failure.  *)
NoFalseReadRejection ==
    \A q \in sent : honest[q.id] = "rejected_replay" => q.route \notin Reads

(* A signed write takes effect at most once, however often it is resent.  *)
WritesAtMostOnce == \A i \in Ids : writes[i] <= 1

(* No write is executed from a signature made for another route.          *)
WritesOnlyOnSignedRoute == ~offRouteWrite

(* No request is accepted on any route other than the one it was signed for. *)
AcceptedOnlyOnSignedRoute == ~offRoute

(* A leaked copy of a read the client already received yields nothing:     *)
(* what today's strict floor guarantees once the honest copy is served.    *)
NoReplayedReadAfterServed == ~leakAfter

(* A replayer never reads plaintext state, whatever it holds or races.    *)
NoPlaintextToReplayer == ~leakAny

=============================================================================
