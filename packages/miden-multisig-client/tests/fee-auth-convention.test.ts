import { AccountId, Word } from '@miden-sdk/miden-sdk';
import { describe, expect, it } from 'vitest';

import { detectAuthArgConvention, resolveAuthArg } from '../src/transaction/feeAuth.js';

/**
 * Pairs the auth-arg producer with the detector over the real Poseidon2, which
 * is the only way either is checked against the other. `src/multisig.test.ts`
 * mocks the hash, so a detector that disagreed with the producer would pass
 * there while classifying every committed proposal as unexecutable in
 * production — after its signatures were collected.
 *
 * What this file cannot catch is a change to the hash both sides call: swapping
 * its operands moves producer and detector together, so every case below round
 * trips and still passes. Only a digest fixed outside this package can catch
 * that, which is the `Poseidon2::merge` vector computed in Rust and pinned in
 * `src/transaction/feeAuth.test.ts`.
 */
// The first is the account id the Rust cross-SDK parity test pins; the second
// differs only in its prefix, so both are well-formed ids.
const FEE_FAUCET = '0xade67f7701e9e9c12493c6206bc46e';
const OTHER_FAUCET = '0xbde67f7701e9e9c12493c6206bc46e';
const SALT = Word.fromHex('0x' + '11'.repeat(32));

describe('auth-arg convention round trip', () => {
  it('detects the commitment resolveAuthArg produced', () => {
    const { authArg } = resolveAuthArg(SALT, FEE_FAUCET);

    expect(detectAuthArgConvention(authArg, SALT, FEE_FAUCET)).toBe('committed');
  });

  it('detects a bare salt, which is what both SDKs create', () => {
    const { authArg } = resolveAuthArg(SALT);

    expect(authArg.toHex()).toBe(SALT.toHex());
    expect(detectAuthArgConvention(authArg, SALT, FEE_FAUCET)).toBe('bare');
  });

  it('does not accept a commitment under a different fee faucet', () => {
    // The faucet is load-bearing: a rebuild pointed at the wrong chain must not
    // silently reproduce a request the cosigners did not sign.
    const { authArg } = resolveAuthArg(SALT, FEE_FAUCET);

    expect(detectAuthArgConvention(authArg, SALT, OTHER_FAUCET)).toBe('mismatch');
  });

  it('does not accept a commitment to a different salt', () => {
    const { authArg } = resolveAuthArg(SALT, FEE_FAUCET);
    const otherSalt = Word.fromHex('0x' + '22'.repeat(32));

    expect(detectAuthArgConvention(authArg, otherSalt, FEE_FAUCET)).toBe('mismatch');
  });

  it('rejects an auth arg from neither convention', () => {
    const unrelated = Word.fromHex('0x' + '33'.repeat(32));

    expect(detectAuthArgConvention(unrelated, SALT, FEE_FAUCET)).toBe('mismatch');
  });

  it('accepts the AccountId overload as well as hex', () => {
    const faucet = AccountId.fromHex(FEE_FAUCET);
    const { authArg } = resolveAuthArg(SALT, faucet);

    expect(detectAuthArgConvention(authArg, SALT, faucet)).toBe('committed');
  });
});
