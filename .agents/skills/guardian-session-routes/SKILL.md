---
name: guardian-session-routes
description: Add or change a per-account Guardian route that a session (delegated signer) may call, keeping session-v2 operation binding and replay rules consistent across the shared registry, server, Rust and TypeScript clients, test vectors, docs and models. Use when a change adds an HTTP route or gRPC method that takes an account id, lets an existing wallet-only route accept sessions, changes whether a route reads or writes, or changes how a session read is admitted.
---

# Guardian session routes

Session-v2 (issue #317) signs every session request over an envelope that
binds the transport and the route's operation, and judges it by that
operation: reads are admitted once each, writes advance a per-key timestamp
floor, and the canonical nonce poll is the one window-only read. A route that
misses a registration point either refuses sessions, accepts a signature made
for another route, or judges a write as a read. The compiler catches some of
this (exhaustive matches), not all.

Wire contract: `spec/api.md`, Session Authentication. Rationale for the
nonce exception: `speckit/features/317-secure-scalability/decision-nonce-read-window.md`.

## Is the route session-eligible?

New routes are wallet-only until they opt in. Opt in only when a session
should be able to call it without a wallet prompt (routine reads, proposal
create and sign, pushes, execution requests). Keep wallet-only anything that
changes who controls the account (configure, signer changes outside a
proposal, guardian switch, revoke-all) or that a stolen session must not do.

## Checklist for a session-eligible route

1. **Operation registry (shared, permanent ids).**
   - `crates/shared/src/session_request.rs`: add a variant to
     `SessionOperation` with the next unused id (never renumber or reuse),
     add it to `ALL`, to `name()` (the gRPC method name), and to
     `replay_policy()`: `Read` if it never changes stored state, `Write`
     otherwise. Use `Write` when in doubt.
   - `packages/guardian-client/src/session-request.ts`: the same id in
     `SessionOperation`, and in `WRITE_OPERATIONS` for a write.
2. **Test vectors.** Regenerate the cross-language fixture:
   `GUARDIAN_REGEN_SESSION_FIXTURES=1 cargo test -p guardian-shared --test session_request_vectors`,
   then run the TypeScript vector tests
   (`packages/guardian-client/src/session-request.test.ts`,
   `packages/miden-multisig-client/tests/session-request-vectors.test.ts`)
   so both languages agree byte for byte.
3. **Server service.** In the service (not the transport adapter), resolve
   the account with the operation:
   - writes: `resolve_account_allowing_session(state, id, creds, SessionOperation::X)`;
   - reads: `resolve_account_for_read(..., SessionOperation::X)` (metadata
     from the validated cache), or `resolve_account_for_head_read` when the
     read also needs the state head in the same database call;
   - wallet-only routes keep `resolve_account`.
   Both transports must reach the same service so HTTP and gRPC bind the same
   operation.
4. **How a session read is admitted.** `SessionReadKind::of` in
   `crates/server/src/services/mod.rs` is an exhaustive match: a new read is
   `Recorded` (admitted once, through the read record). `WindowOnly` (served
   every time inside its window, no record) is a security decision: it needs a
   decision note like the nonce poll's, a `verify-auth-replay` cell, and a
   reviewed exception in `spec/api.md`.
5. **Clients.**
   - Rust (`crates/client/src/client.rs`): call
     `send_allowing_session(account_id, SessionOperation::X, message, ...)`;
     wallet-only methods keep the plain wallet send.
   - TypeScript (`packages/guardian-client/src/http.ts`): pass
     `SessionOperation.X` as the last argument of `fetchAuthenticated`
     (it defaults to `'wallet'`).
   - Multisig SDKs only change if they call the new client method.
6. **Tests.**
   - Add the route to the route-to-operation map in
     `crates/server/src/testing/integration/session_http.rs`, so the session
     HTTP tests sign it correctly.
   - Cover: accepted with a session, refused on another operation's
     signature (`authentication_failed`), and the replay rule (a resent read
     or write is `authentication_replay`).
   - Client tests in both SDKs for the session path.
7. **Docs.** `spec/api.md` (the session-eligible routes and their read or
   write rule), `docs/openapi*.json` regenerated for HTTP changes, and
   `docs/MIDEN_COMPATIBILITY.md` if SDK users see a change.
8. **Models.** If the change touches a function listed in a `verify-*`
   skill's `mirrored.txt` (`verify-auth-replay` mirrors the auth and replay
   path), run that skill's `check.sh`. Update the model when behaviour
   changed; run `--accept-drift` only after checking the drift is mechanical.

## Changing an existing route

- Never change an operation's id or reuse one; add a new operation instead.
- Moving a route from wallet-only to session-eligible is a security change:
  say so in the PR and in `docs/MIDEN_COMPATIBILITY.md`.
- Changing `replay_policy()` changes how old signatures are judged; treat it
  as a contract change (`guardian-contract-change`).

## Validation

```bash
cargo test -p guardian-shared --test session_request_vectors
cargo test -p guardian-server --lib --features integration -- \
  testing::integration::session_http testing::integration::canonical_nonce_http
./scripts/test-postgres.sh        # when admission SQL changed
(cd packages && npm test -w @openzeppelin/guardian-client && npm test -w @openzeppelin/miden-multisig-client)
.agents/skills/verify-auth-replay/check.sh
```
