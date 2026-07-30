import type {
  AccountId,
  ProvenTransaction,
  TransactionProver,
  TransactionRequest,
  TransactionResult,
  WasmWebClient,
} from '@miden-sdk/miden-sdk';
import { describe, expect, it, vi } from 'vitest';
import type { ResolvedProverConfig } from './config.js';
import type { RetryRuntime } from './retry.js';
import { ProverWorkflow } from './workflow.js';

function asType<T>(value: unknown): T {
  return value as T;
}

describe('ProverWorkflow', () => {
  it('executes once, retries proof with fresh provers, submits once, and applies once', async () => {
    const transient = Object.assign(new Error('temporarily unavailable'), {
      code: 'Unavailable',
    });
    const result = asType<TransactionResult>({ marker: 'unchanged' });
    const proof = asType<ProvenTransaction>({});
    const client = {
      executeTransaction: vi.fn().mockResolvedValue(result),
      proveTransaction: vi.fn().mockRejectedValueOnce(transient).mockResolvedValueOnce(proof),
      submitProvenTransaction: vi.fn().mockResolvedValue(42),
      applyTransaction: vi.fn().mockResolvedValue({}),
    };
    const provers: TransactionProver[] = [];
    const config: ResolvedProverConfig = {
      kind: 'remote',
      url: 'https://prover.example/',
      maxAttempts: 2,
      createProver: () => {
        const prover = asType<TransactionProver>({});
        provers.push(prover);
        return prover;
      },
    };
    const runtime: RetryRuntime = {
      sleep: vi.fn().mockResolvedValue(undefined),
      unitRandom: () => 0.5,
    };
    const workflow = new ProverWorkflow(
      Promise.resolve(asType<WasmWebClient>(client)),
      config,
      runtime,
    );

    await workflow.submit(
      asType<AccountId>({}),
      asType<TransactionRequest>({}),
    );

    expect(client.executeTransaction).toHaveBeenCalledTimes(1);
    expect(client.proveTransaction).toHaveBeenCalledTimes(2);
    expect(client.proveTransaction.mock.calls[0]?.[0]).toBe(result);
    expect(client.proveTransaction.mock.calls[1]?.[0]).toBe(result);
    expect(provers).toHaveLength(2);
    expect(provers[0]).not.toBe(provers[1]);
    expect(client.submitProvenTransaction).toHaveBeenCalledTimes(1);
    expect(client.applyTransaction).toHaveBeenCalledTimes(1);
    expect(runtime.sleep).toHaveBeenCalledTimes(1);
  });

  it('returns the final original error without sleeping after exhaustion', async () => {
    const first = Object.assign(new Error('unavailable'), { code: 'Unavailable' });
    const final = Object.assign(new Error('deadline exceeded'), {
      code: 'DeadlineExceeded',
    });
    const client = {
      executeTransaction: vi.fn().mockResolvedValue({}),
      proveTransaction: vi.fn().mockRejectedValueOnce(first).mockRejectedValueOnce(final),
      submitProvenTransaction: vi.fn(),
      applyTransaction: vi.fn(),
    };
    const runtime: RetryRuntime = {
      sleep: vi.fn().mockResolvedValue(undefined),
      unitRandom: () => 0.5,
    };
    const workflow = new ProverWorkflow(
      Promise.resolve(asType<WasmWebClient>(client)),
      {
        kind: 'remote',
        maxAttempts: 2,
        createProver: () => asType<TransactionProver>({}),
      },
      runtime,
    );

    await expect(
      workflow.submit(asType<AccountId>({}), asType<TransactionRequest>({})),
    ).rejects.toBe(final);
    expect(runtime.sleep).toHaveBeenCalledTimes(1);
    expect(client.submitProvenTransaction).not.toHaveBeenCalled();
  });
});
