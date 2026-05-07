import { Felt, FeltArray, Rpo256, Word } from '@miden-sdk/miden-sdk';

/**
 * Lookup-bound message format used to sign requests against the Guardian
 * `/state/lookup` endpoint. MUST produce byte-identical digests to
 * `crates/shared/src/lookup_auth_message.rs`; parity is verified against
 * `crates/shared/tests/fixtures/lookup_auth_vectors.json`.
 */

const DOMAIN_TAG_BYTES = new TextEncoder().encode('guardian.lookup.v1');

let cachedDomainTag: Word | null = null;

/**
 * Convert a byte array to a sequence of Goldilocks-field felts using the same
 * 8-byte little-endian chunking convention as
 * `guardian_shared::auth_request_payload::AuthRequestPayload::from_bytes`. The
 * final chunk is zero-padded if `bytes.length` is not a multiple of 8.
 */
function bytesToFelts(bytes: Uint8Array): Felt[] {
  const felts: Felt[] = [];
  for (let offset = 0; offset < bytes.length; offset += 8) {
    let value = 0n;
    for (let i = 0; i < 8; i += 1) {
      const byte = offset + i < bytes.length ? bytes[offset + i] : 0;
      value |= BigInt(byte) << BigInt(8 * i);
    }
    felts.push(new Felt(value));
  }
  return felts;
}

/**
 * The 4-felt RPO domain tag prepended to every lookup digest, computed once
 * from `DOMAIN_TAG_BYTES`. Cached on first call.
 */
function lookupDomainTag(): Word {
  if (cachedDomainTag === null) {
    cachedDomainTag = Rpo256.hashElements(new FeltArray(bytesToFelts(DOMAIN_TAG_BYTES)));
  }
  return cachedDomainTag;
}

/**
 * Compute the digest a lookup-bound signer signs.
 *
 * Layout (must mirror `LookupAuthMessage::to_word` in `crates/shared`):
 *
 * ```text
 * RPO256_hash([
 *   DOMAIN_TAG_W0, DOMAIN_TAG_W1, DOMAIN_TAG_W2, DOMAIN_TAG_W3,
 *   timestamp_ms_felt,
 *   key_commitment_W0, key_commitment_W1,
 *   key_commitment_W2, key_commitment_W3,
 * ])
 * ```
 *
 * @param timestampMs Unix milliseconds. Reinterpreted as `u64` to match the
 *                    Rust `as u64` cast (so negative inputs wrap into the
 *                    high range, then are reduced mod the Goldilocks prime by
 *                    the Felt constructor).
 * @param keyCommitmentHex `0x`-prefixed 32-byte hex string for the queried
 *                         commitment.
 */
export function lookupAuthDigest(timestampMs: number | bigint, keyCommitmentHex: string): Word {
  const tag = lookupDomainTag().toFelts();
  const kc = Word.fromHex(keyCommitmentHex).toFelts();
  const tsBigInt = typeof timestampMs === 'bigint' ? timestampMs : BigInt(timestampMs);
  const timestampU64 = BigInt.asUintN(64, tsBigInt);
  const timestampFelt = new Felt(timestampU64);

  const message = new FeltArray([
    tag[0],
    tag[1],
    tag[2],
    tag[3],
    timestampFelt,
    kc[0],
    kc[1],
    kc[2],
    kc[3],
  ]);

  return Rpo256.hashElements(message);
}

/**
 * Reset the cached domain tag. Tests use this to exercise the cache miss path;
 * production callers should not need it.
 */
export function _resetLookupDomainTagCacheForTesting(): void {
  cachedDomainTag = null;
}
