import { Felt, FeltArray, Rpo256, type Word } from '@miden-sdk/miden-sdk';

/**
 * Goldilocks field order (`2^64 - 2^32 + 1`), matching `Felt::ORDER` in
 * `miden-protocol`.
 */
const FELT_ORDER = 18446744069414584321n;

/**
 * Maps an arbitrary `u64` onto a canonical field element by reducing modulo the
 * field order, mirroring `guardian_shared::felt::felt_from_u64_reduced`.
 *
 * Miden 0.15's `Felt` constructor rejects non-canonical inputs (values
 * `>= FELT_ORDER`), whereas 0.14 reduced silently. Byte-packed digest inputs are
 * arbitrary `u64`s, so reducing here preserves the original digest layout, keeps
 * construction infallible, and stays byte-identical to server-side signing.
 */
export function feltFromU64Reduced(value: bigint): Felt {
  return new Felt(BigInt.asUintN(64, value) % FELT_ORDER);
}

/**
 * RPO hash of `bytes` packed as 8-byte little-endian chunks (the last one
 * zero-padded), mirroring `guardian_shared::felt::domain_tag_word`. The
 * convention every Guardian message domain tag uses.
 */
export function domainTagWord(bytes: Uint8Array): Word {
  const felts: Felt[] = [];
  for (let offset = 0; offset < bytes.length; offset += 8) {
    let value = 0n;
    for (let i = 0; i < 8; i += 1) {
      const byte = offset + i < bytes.length ? bytes[offset + i] : 0;
      value |= BigInt(byte) << BigInt(8 * i);
    }
    felts.push(feltFromU64Reduced(value));
  }
  return Rpo256.hashElements(new FeltArray(felts));
}
