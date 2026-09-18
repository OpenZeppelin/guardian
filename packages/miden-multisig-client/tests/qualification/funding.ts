import { execFile } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const run = promisify(execFile);

// Derived from this module rather than the working directory, so the driver
// is found whether vitest is launched from the package or the repo root.
const repoRootFromModule = resolve(dirname(fileURLToPath(import.meta.url)), '../../../..');

/**
 * Funding goes through the Rust driver rather than being reimplemented here.
 *
 * One implementation means the two drivers cannot disagree about what a funded
 * account is, and the treasury key never enters this process: it stays in the
 * environment of the subprocess that needs it.
 */
export interface FundingResult {
  readonly funded: string;
  readonly amount: number;
  readonly faucet: string;
  /** Where the funds came from, and so a real counterparty to send to. */
  readonly treasury: string;
}

export interface FundingOptions {
  readonly network: 'devnet' | 'testnet';
  readonly recipient: string;
  readonly amount: number;
  readonly repoRoot?: string;
}

export async function fundAccount(options: FundingOptions): Promise<FundingResult> {
  const cwd = options.repoRoot ?? process.env.QUAL_REPO_ROOT ?? repoRootFromModule;

  // The built binary directly. Going through `cargo run` costs about three
  // and a half seconds of build-graph checking per call, and can stall on a
  // rebuild in the middle of a chain operation.
  const binary = process.env.QUAL_DRIVER_BIN ?? join(cwd, 'target/debug/qualification-driver');

  const { stdout } = await run(
    binary,
    [
      'fund',
      '--network',
      options.network,
      '--recipient',
      options.recipient,
      '--amount',
      String(options.amount),
    ],
    { cwd, maxBuffer: 8 * 1024 * 1024 },
  );

  const line = stdout
    .split('\n')
    .map((entry) => entry.trim())
    .filter(Boolean)
    .at(-1);

  if (!line) {
    throw new Error('the funding driver produced no result');
  }
  if (line.includes('charges nothing')) {
    return { funded: options.recipient, amount: 0, faucet: '', treasury: '' };
  }
  return JSON.parse(line) as FundingResult;
}
