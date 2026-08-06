import type {
  AccountId,
  MidenClient,
  TransactionProver,
  TransactionRequest,
} from '@miden-sdk/miden-sdk';
import { describe, expect, it, vi } from 'vitest';
import type { ResolvedProverConfig } from './config.js';
import type { RetryRuntime } from '../retry/runtime.js';
import { ProverWorkflow } from './workflow.js';

function asType<T>(value: unknown): T {
  return value as T;
}

describe('ProverWorkflow', () => {
  it('executes once, retries proof with fresh provers, submits once, and applies once', async () => {
    const transient = Object.assign(new Error('temporarily unavailable'), {
      code: 'Unavailable',
    });
    const apply = vi.fn().mockResolvedValue({});
    const submit = vi.fn().mockResolvedValue({ apply });
    const prove = vi.fn().mockRejectedValueOnce(transient).mockResolvedValueOnce({ submit });
    const execution = { marker: 'unchanged', prove };
    const executeRequest = vi.fn().mockResolvedValue(execution);
    const client = {
      transactions: { executeRequest },
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
      asType<MidenClient>(client),
      config,
      runtime,
    );

    await workflow.submit(
      asType<AccountId>({}),
      asType<TransactionRequest>({}),
    );

    expect(executeRequest).toHaveBeenCalledTimes(1);
    expect(prove).toHaveBeenCalledTimes(2);
    expect(provers).toHaveLength(2);
    expect(provers[0]).not.toBe(provers[1]);
    expect(submit).toHaveBeenCalledTimes(1);
    expect(apply).toHaveBeenCalledTimes(1);
    expect(runtime.sleep).toHaveBeenCalledTimes(1);
  });

  it('returns the final original error without sleeping after exhaustion', async () => {
    const first = Object.assign(new Error('unavailable'), { code: 'Unavailable' });
    const final = Object.assign(new Error('deadline exceeded'), {
      code: 'DeadlineExceeded',
    });
    const prove = vi.fn().mockRejectedValueOnce(first).mockRejectedValueOnce(final);
    const client = { transactions: { executeRequest: vi.fn().mockResolvedValue({ prove }) } };
    const runtime: RetryRuntime = {
      sleep: vi.fn().mockResolvedValue(undefined),
      unitRandom: () => 0.5,
    };
    const workflow = new ProverWorkflow(
      asType<MidenClient>(client),
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
    expect(prove).toHaveBeenCalledTimes(2);
    expect(runtime.sleep).toHaveBeenCalledTimes(1);
  });

  it('never re-submits when submission fails with transient-looking wording', async () => {
    const rateLimited = Object.assign(new Error('Too Many Requests!'), {
      code: 'ResourceExhausted',
    });
    const submit = vi.fn().mockRejectedValue(rateLimited);
    const prove = vi.fn().mockResolvedValue({ submit });
    const client = {
      transactions: { executeRequest: vi.fn().mockResolvedValue({ prove }) },
    };
    const runtime: RetryRuntime = {
      sleep: vi.fn().mockResolvedValue(undefined),
      unitRandom: () => 0.5,
    };
    const workflow = new ProverWorkflow(
      asType<MidenClient>(client),
      {
        kind: 'remote',
        maxAttempts: 5,
        createProver: () => asType<TransactionProver>({}),
      },
      runtime,
    );

    await expect(
      workflow.submit(asType<AccountId>({}), asType<TransactionRequest>({})),
    ).rejects.toBe(rateLimited);
    expect(prove).toHaveBeenCalledTimes(1);
    expect(submit).toHaveBeenCalledTimes(1);
    expect(runtime.sleep).not.toHaveBeenCalled();
  });

  it('uses the injected prover directly when no cloneable remote override exists', async () => {
    const apply = vi.fn().mockResolvedValue({});
    const submit = vi.fn().mockResolvedValue({ apply });
    const prove = vi.fn().mockResolvedValue({ submit });
    const client = {
      transactions: { executeRequest: vi.fn().mockResolvedValue({ prove }) },
    };
    const workflow = new ProverWorkflow(asType<MidenClient>(client), {
      kind: 'injected',
      maxAttempts: 1,
      createProver: () => undefined,
    });

    await workflow.submit(asType<AccountId>({}), asType<TransactionRequest>({}));

    expect(prove).toHaveBeenCalledWith();
  });
});
