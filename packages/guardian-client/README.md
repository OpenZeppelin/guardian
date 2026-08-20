# @openzeppelin/guardian-client

TypeScript HTTP client for Guardian server.

## Installation

```bash
npm install @openzeppelin/guardian-client
```

## Setup

```typescript
import { GuardianHttpClient } from '@openzeppelin/guardian-client';

const client = new GuardianHttpClient('http://localhost:3000');
```

## Usage

### Get Server Public Key (Unauthenticated)

```typescript
const pubkey = await client.getPubkey();
console.log('GUARDIAN pubkey:', pubkey);
```

### Set Signer for Authenticated Requests

All endpoints except `getPubkey()` require authentication. You must provide a signer that implements the `Signer` interface:

```typescript
import type { Signer, RequestAuthPayload } from '@openzeppelin/guardian-client';

const signer: Signer = {
  commitment: '0x...', // 64 hex chars
  publicKey: '0x...',  // Full public key hex
  // Sign account ID + timestamp + request payload digest
  signRequest: (accountId: string, timestamp: number, requestPayload: RequestAuthPayload) => {
    // requestPayload is canonicalized by the client before this call
    // implement your signing logic here
    return '0x...';
  },
  signCommitment: (commitmentHex: string) => '0x...', // Returns signature hex
};

client.setSigner(signer);
```

### Configure an Account

```typescript
await client.configure({
  account_id: '0x...',
  auth: {
    MidenFalconRpo: {
      cosigner_commitments: ['0x...', '0x...'],
    },
  },
  initial_state: { data: '<base64-encoded-account>', account_id: '0x...' },
});
```

### Get Account State

```typescript
const state = await client.getState(accountId);
console.log('Commitment:', state.commitment);
console.log('State data:', state.state_json.data);
```

### Abandon a Stuck Candidate

If an approved transaction died client-side after guardian approval, its
candidate keeps the account locked (`409 conflict_pending_delta` on new
proposals). Record an abandon intent and poll for the resolution:

```typescript
const accepted = await client.abandonCandidate(accountId, nonce);
console.log(accepted.state); // 'pending'

// The guardian's worker confirms over a short quarantine that the tx did
// not land, then releases the account.
const status = await client.abandonStatus(accountId, nonce);
// 'waiting' | 'landed' | 'abandoned' | 'retained' | 'unexpected'
```

`'retained'` means the guardian stopped actively verifying the candidate
and released the account slot, but the on-chain outcome is still
**uncertain**: background reconciliation may promote the delta to
`canonical` until its retention TTL expires. It is "unlocked but
unresolved" — never read it as "the transaction did not land". Sync and
check the chain before replacing the slot, since a resubmission
supersedes the retained delta and forfeits automatic recovery.

| Status | Account locked? | Outcome known? | Client action |
|---|---|---|---|
| `candidate` | Yes | No | Wait, or request abandonment |
| `retained` | No | No | Sync/check chain before replacing |
| `discarded: client_abandoned` | No | Probably not landed; late reconciliation remains possible | Continue cautiously |
| `canonical` | No | Yes — landed | Sync account state |

### Look Up An Account By Key Commitment

When a wallet only holds a signing key, it cannot derive the account ID
directly. The Guardian server exposes `GET /state/lookup` so the wallet
can ask "which account(s) authorize this commitment?" and proceed with
the existing recovery flow.

The signer used here MUST implement `signLookupMessage`, which signs the
domain-separated `LookupAuthMessage::to_word(timestampMs, keyCommitment)`
digest. The canonical implementation lives in
`@openzeppelin/miden-multisig-client` (which has access to the Miden SDK's
RPO256); this package keeps the digest computation out of its zero-dependency
surface.

```typescript
const result = await client.lookupAccountByKeyCommitment(keyCommitmentHex);

if (result.accounts.length === 0) {
  console.log('No account authorizes this commitment with this operator.');
} else {
  for (const { accountId } of result.accounts) {
    console.log('Recovered account:', accountId);
    // Continue with the existing /state flow:
    const state = await client.getState(accountId);
    // ... register a new key via the existing delta/proposal flow.
  }
}
```

For a higher-level helper that composes lookup + state fetch, see
`recoverByKey` in `@openzeppelin/miden-multisig-client`.

#### Auth shape

The lookup endpoint accepts the same `x-pubkey` / `x-signature` /
`x-timestamp` headers as per-account requests for wire-format consistency,
but identity is derived from the signature itself: Falcon signatures embed
the public key, ECDSA signatures recover it via the recovery byte. The
server then requires the derived key to commit to the queried
`key_commitment`. This means the lookup endpoint works with wallet signers
that only expose a 32-byte commitment as `publicKey` (e.g., the Miden
browser wallet) — the signature is what proves possession.

### Work with Delta Proposals

```typescript
// Get all proposals for an account
const proposals = await client.getDeltaProposals(accountId);

// Get one proposal by commitment
const proposal = await client.getDeltaProposal(accountId, '0x...');

// Push a new proposal
const response = await client.pushDeltaProposal({
  account_id: accountId,
  nonce: 1,
  delta_payload: {
    tx_summary: { data: '<base64-tx-summary>' },
    signatures: [],
  },
});

// Sign a proposal
const delta = await client.signDeltaProposal({
  account_id: accountId,
  commitment: response.commitment,
  signature: { scheme: 'falcon', signature: '0x...' },
});

// Execute a proposal
const result = await client.pushDelta({
  account_id: accountId,
  nonce: 1,
  prev_commitment: '0x...',
  delta_payload: { data: '<base64-tx-summary>' },
  status: { status: 'pending', timestamp: '...', proposer_id: '0x...', cosigner_sigs: [] },
});
```

### Get Deltas

```typescript
// Get specific delta by nonce
const delta = await client.getDelta(accountId, 5);

// Get merged delta since a nonce
const merged = await client.getDeltaSince(accountId, 3);
```

## Error Handling

The client throws `GuardianHttpError` for non-2xx responses:

```typescript
import { GuardianHttpError } from '@openzeppelin/guardian-client';

try {
  await client.getState(accountId);
} catch (error) {
  if (error instanceof GuardianHttpError) {
    console.error(`HTTP ${error.status}: ${error.statusText}`);
    console.error('Body:', error.body);
  }
}
```

### Rate limits and retries

The server rate-limits both its HTTP and gRPC surfaces. The sustained
per-minute limit is keyed per IP alone, so HTTP and gRPC calls from one
client draw on the same allowance; the burst limit is keyed per IP and
endpoint. An over-budget request fails with HTTP 429, code
`rate_limit_exceeded`, and a backoff hint. `GuardianHttpError` classifies
it: `isRetryable()` reads the error envelope (falling back to the status
class), and `retryAfterSecs()` returns the server's hint, preferring the
`Retry-After` header over the envelope value.

Rate-limit rejections happen before the server touches any state, so
retrying them is always safe. The client does not retry rate limits
automatically (automatic backoff is tracked in
[#360](https://github.com/OpenZeppelin/guardian/issues/360)); a bounded
loop over the exposed hint is a few lines:

```typescript
async function getStateWithRetry(accountId: string, maxAttempts = 3) {
  for (let attempt = 0; ; attempt++) {
    try {
      return await client.getState(accountId);
    } catch (error) {
      if (
        !(error instanceof GuardianHttpError) ||
        !error.isRetryable() ||
        attempt >= maxAttempts
      ) {
        throw error;
      }
      await new Promise((r) => setTimeout(r, (error.retryAfterSecs() ?? 1) * 1000));
    }
  }
}
```

### Replay-protection retries

Signed requests carry a strictly increasing per-instance timestamp
(`max(Date.now(), previous + 1)`). When a correctly signed request still
loses the server's per-signer replay check (stable code
`authentication_replay`, typically two in-flight requests landing out of
order), the client retries automatically, up to 2 times with a 50ms
backoff, minting a fresh timestamp and signature over the identical
payload each attempt. Terminal authentication failures
(`authentication_failed`: clock outside the skew window, invalid or
unauthorized signature) are never retried; branch on `error.code`, never
on message text.

The client retries only `authentication_replay`; it never retries
`authentication_failed`. During a mixed server/client rollout, a replay CAS
reported under the older authentication code—or received by a client without
replay-specific retry handling—can therefore surface as a terminal 401 until
both sides use the same error contract.

The upgraded server also returns the standard `{ code, message, meta }` error
envelope for failed HTTP `/configure` requests instead of a
`ConfigureResponse` with `success: false`. Direct HTTP integrations that inspect
the old error body must migrate with the server rollout.

## Testing

```bash
npm test           # Run tests once
npm run test:watch # Run tests in watch mode
```
