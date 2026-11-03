# Session replay design model

`SessionReplayDesign.tla` is a **design model**, not a mirror of the code.
It compares candidate designs for replay protection on session-signed
requests before any code is written. The model that mirrors today's code,
with drift checks against the Rust, is
[`.agents/skills/verify-auth-replay`](../../../../.agents/skills/verify-auth-replay/SKILL.md).

See [`../gap-analysis.md`](../gap-analysis.md) ("Gap 1") for why this
matters for the scalability target.

## Run it

```bash
docs/architecture/scalability/tla/run.sh                       # verdict matrix, about 7 min
docs/architecture/scalability/tla/run.sh window-bound NoReplayedReadAfterServed   # one cell, full TLC trace
docs/architecture/scalability/tla/run.sh today NoFalseReadRejection | python3 -I docs/architecture/scalability/tla/trace.py
```

It needs `java` and `python3`, and uses the TLC jar that
`verify-auth-replay/check.sh` downloads to `~/.cache/guardian-tla/`.

## What is modeled

One account, one session key. The client signs up to 3 requests on any
route with a timestamp anywhere in the skew window, and delivers each one
exactly once, in any order. An attacker holds a copy of every signed
request (for example from leaked headers) and can deliver it any number of
times, at any moment, on any route the signature verifies for. The session
can be revoked at any point. Each delivery is one atomic step: skew check,
session lookup, replay check, effect.

Design switches:

| Constant | Meaning |
|---|---|
| `ReadMode` | `"strict"`: reads consume the session replay floor, as today. `"window"`: reads check only the timestamp window and the live session. Writes always keep the strict floor. |
| `Bound` | The signed message includes the route. Today it does not: `get_state`, `get_canonical_nonce`, `get_delta_proposals` and an uncursored `get_delta_history` all sign `{account_id}`. |
| `RespEnc` | Read responses are encrypted to a key only the session holder has. Idealized; see Limits. |
| `CollidingWrite` | A write route signs the same `{account_id}` payload as the reads. No such route exists today; this checks the latent hazard. |
| `Attacker` | `FALSE` checks only the client's own traffic. Used for `NoFalseReadRejection`, since an attacker who front-runs a captured request can always burn its floor (denial of service, not an auth-window failure). |

## Properties

| Property | In plain words |
|---|---|
| `NoFalseReadRejection` | The client's own read is never refused by the replay check while it is inside the window with a live session, whatever it sends in parallel or out of order. This is the "zero auth-window failures" criterion. |
| `WritesAtMostOnce` | A signed write takes effect at most once. |
| `WritesOnlyOnSignedRoute` | No write is executed from a signature made for another route. |
| `AcceptedOnlyOnSignedRoute` | No request is accepted on a route other than the one it was signed for. |
| `NoReplayedReadAfterServed` | A leaked copy of a read the client already received yields nothing to a replayer. This is what today's strict floor gives. |
| `NoPlaintextToReplayer` | A replayer never reads plaintext state, whatever it holds or races. |

## Results

Run on 2026-10-29 (TLC 1.7.4, about 7 minutes for the whole matrix).

| Design | NoFalseReadRejection | WritesAtMostOnce | WritesOnlyOnSignedRoute | AcceptedOnlyOnSignedRoute | NoReplayedReadAfterServed | NoPlaintextToReplayer |
|---|---|---|---|---|---|---|
| `today` (strict, unbound) | **FAIL** | PASS | PASS | FAIL | PASS | FAIL |
| `window-unbound` | PASS | PASS | PASS | FAIL | FAIL | FAIL |
| `window-bound` | PASS | PASS | PASS | PASS | FAIL | FAIL |
| `window-bound-enc` | PASS | PASS | PASS | PASS | PASS | PASS |
| `today+colliding` | FAIL | PASS | **FAIL** | FAIL | PASS | FAIL |
| `window-unbound+colliding` | PASS | PASS | **FAIL** | FAIL | FAIL | FAIL |
| `window-bound+colliding` | PASS | PASS | PASS | PASS | FAIL | FAIL |

`NoPlaintextToReplayer` fails for `today` too: an attacker who delivers a
captured copy before the client does is accepted once. The strict floor
only protects copies used after the client's request was served.

## Counterexamples worth knowing

- **`today` / `NoFalseReadRejection`**: the client signs two `get_state`
  requests with the same timestamp and sends both. The first is accepted
  and sets the floor; the second is refused as a replay. At 20k readers
  this is the auth-window failure the target forbids.
- **`today+colliding` / `WritesOnlyOnSignedRoute`**: the client signs a
  `get_canonical_nonce`; an attacker delivers the copy to a write route
  that signs the same `{account_id}` payload, and the write executes. Not
  exploitable today because no write route signs that payload, but nothing
  but payload shapes prevents it, under any `ReadMode`.
- **`window-bound` / `NoReplayedReadAfterServed`**: the client's
  `get_state` is served; an attacker then delivers the copy inside the
  window and receives plaintext state. This is the risk the window-only
  design accepts, bounded by the window and by session revocation.

## Conclusions

1. Today's design fails the zero-failures criterion
   (`NoFalseReadRejection`) without any attacker.
2. Relaxing reads to a window check fixes that and keeps every write
   property, but only binding the route keeps `AcceptedOnlyOnSignedRoute`
   and closes the colliding-write hazard. **Route binding is required for
   any relaxation**, and is worth having regardless.
3. Window-only reads give up `NoReplayedReadAfterServed`: a leaked read
   is usable until the window closes or the session is revoked.
4. Encrypting read responses to a key only the session holder has restores
   both leak properties, so with `window-bound-enc` every property passes.
   That is a property of the idea, not of a deliverable design: see Limits.

## Limits

- Bounds: 3 requests, clock 1 to 4, skew 1. A pass covers these bounds.
- Deliveries are atomic. Races between the steps of one handler, session
  expiry, the floor purge and stalled handlers (finding F2) are covered by
  the code model, not here.
- Encryption is a black box: `RespEnc` means a replayer without the
  holder's private key learns nothing from a response. It is not evidence
  that today's session key can do this. The browser session key is a
  non-extractable ECDSA signing key, and WebCrypto refuses ECDH derivation
  with it, so a real design needs a separate encryption key authenticated
  by the grant, plus response binding, rotation and downgrade protection.
  See the [session-auth RFC](../session-auth-rfc.md), "Alternatives".
- `ReadMode = "window"` uses the same skew as writes. The window length,
  asymmetry and client clock correction are not modeled.
- There is no account-type switch: public accounts also return off-chain
  proposals and deltas, so they are not exempt.
- Signatures are unforgeable; wallet-signed requests are out of scope.
