# Contract: SDK Surface Changes

Both SDKs change in lockstep. Any behavior here MUST have a matching test in
each language.

## TypeScript `@openzeppelin/guardian-client`

```ts
export enum SessionOperation {
  GetState = 1, GetCanonicalNonce = 2, GetDelta = 3, GetDeltaSince = 4,
  GetDeltaHistory = 5, GetDeltaProposals = 6, GetDeltaProposal = 7,
  PushDelta = 8, PushDeltaProposal = 9, SignDeltaProposal = 10,
  ExecuteDeltaProposal = 11, GetDeltaProposalExecution = 12,
  GetCurrentExecution = 13,
}
export enum SessionTransport { Http = 1, Grpc = 2 }

export interface SessionRequestSigner {
  readonly publicKey: string;
  readonly signerCommitment: string;
  readonly expiresAt: number;
  readonly writeSequencer: WriteSequencer;
  nextTimestamp(accountId: string): Promise<number>;  // per (session, account), drift-guarded
  signRequest(
    operation: SessionOperation,
    accountId: string,
    timestamp: number,
    requestPayload: RequestAuthPayload,
  ): Promise<string> | string;
  signLogout(timestampMs: number): Promise<string> | string;
  onEnded?(reason: SessionEndReason): void;
}

export class WriteSequencer {
  run<T>(accountId: string, write: () => Promise<T>): Promise<T>;
}

export interface CreateSessionResponse {
  signerCommitment: string;
  expiresAt: string;
  requestAuthVersion: 2;
}
```

Breaking: `signRequest` gains a leading `operation` argument. Custom
signers must update. `session-v2` is the only session format sent.

## TypeScript `@openzeppelin/miden-multisig-client`

`GuardianSession` implements the new `SessionRequestSigner`: signs the v2
envelope bytes with the non-extractable WebCrypto key
(`subtle.sign({name: 'ECDSA', hash: 'SHA-256'}, key, envelope)`), owns one
`WriteSequencer` and one timestamp source. No grant or wallet-prompt change.

## Rust `guardian-client`

- `ActiveSession` (inside `SessionSlot`) owns `WriteSequencer` and the
  session timestamp source, so per-operation `GuardianClient`s built with
  `with_session_slot` share them.
- `add_auth_metadata` takes `SessionOperation`; gRPC transport is `Grpc`.
- `ClientError::UnsupportedAuthVersion` (terminal), parsed from the wire
  code.
- `start_session` validates the advertised policy fields.

## Rust `miden-multisig-client`

No public API change beyond re-exported error variants; per-operation
clients share the session slot as today.

## Behavior shared by both SDKs

| Behavior | Rule |
|---|---|
| Writes | Serialized per (session, account) via `WriteSequencer`, held across bounded replay retries |
| Reads | Not serialized; timestamps strictly increasing per (session, account), never more than 1 s ahead of the local clock |
| `authentication_replay` | Up to 2 retries, each with a fresh timestamp and signature (unchanged) |
| Ambiguous write timeout | No automatic re-sign and resend |
| `unsupported_auth_version` | Terminal, grant kept, no wallet fallback |
| Session expired / revoked | Unchanged: clear session, `onEnded` |
