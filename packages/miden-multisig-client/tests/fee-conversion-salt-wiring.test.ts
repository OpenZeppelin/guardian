import { TransactionRequest, TransactionRequestBuilder, Word } from '@miden-sdk/miden-sdk';
import { describe, expect, it } from 'vitest';

/**
 * Carries the declared fee-conversion salt through a real `TransactionRequest` and
 * reads it back out of the built object, which is as close to the VM as this package
 * gets without a node.
 *
 * The builder tests in `src/transaction/feeWiring.test.ts` record calls against a fake
 * builder, so they prove what arguments the builders pass and nothing about whether the
 * SDK stores them — a salt the WASM silently dropped passes there and then fails in
 * `pay_fee`, because miden-client commits no conversion info for a request that declares
 * none. That gap is the whole reason this layer exists; it replaces the equivalent
 * assertions for the auth-arg mechanism this package used to build by hand.
 */
const SALT = Word.fromHex('0x' + '11'.repeat(32));

describe('fee conversion salt survives a real TransactionRequest', () => {
  it('stores the declared salt', () => {
    const request = new TransactionRequestBuilder().withFeeConversionSalt(SALT).build();

    expect(request.feeConversionSalt()?.toHex()).toBe(SALT.toHex());
  });

  it('leaves the auth arg unset, so the client still commits the conversion info', () => {
    // The two are mutually exclusive and each setter silently clears the other. An auth
    // arg here would opt the request out of the client's fee machinery entirely, and the
    // failure would surface as an abort in `pay_fee` rather than anything nearer.
    const request = new TransactionRequestBuilder().withFeeConversionSalt(SALT).build();

    expect(request.authArg()).toBeUndefined();
  });

  it('carries the salt across serialization', () => {
    // Proposals ship serialized to co-signers, who rebuild the request on the other side.
    // A salt lost on the wire would rebuild under the client's default and commit a
    // different auth arg than the one the summary was signed over.
    const request = new TransactionRequestBuilder().withFeeConversionSalt(SALT).build();

    const roundTripped = TransactionRequest.deserialize(request.serialize());

    expect(roundTripped.feeConversionSalt()?.toHex()).toBe(SALT.toHex());
  });
});
