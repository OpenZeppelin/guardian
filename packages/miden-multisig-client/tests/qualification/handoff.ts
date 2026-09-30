import { execFile } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const run = promisify(execFile);

const repoRootFromModule = resolve(dirname(fileURLToPath(import.meta.url)), '../../../..');

export interface CosignRequest {
  readonly network: 'devnet' | 'testnet';
  readonly guardianEndpoint: string;
  readonly accountId: string;
  readonly proposalId: string;
  readonly keyFile: string;
}

/**
 * Signs a proposal in the Rust driver's own process.
 *
 * A handoff scenario is only evidence of cross-SDK compatibility if the
 * signature is produced by the other SDK, so this crosses a process boundary
 * rather than reimplementing Rust's signing here.
 */
export async function cosignWithRust(request: CosignRequest): Promise<void> {
  const cwd = process.env.QUAL_REPO_ROOT ?? repoRootFromModule;
  const binary = process.env.QUAL_DRIVER_BIN ?? join(cwd, 'target/debug/qualification-driver');

  await run(
    binary,
    [
      'cosign',
      '--network',
      request.network,
      '--guardian-endpoint',
      request.guardianEndpoint,
      '--account-id',
      request.accountId,
      '--proposal-id',
      request.proposalId,
      '--key-file',
      request.keyFile,
    ],
    { cwd, maxBuffer: 8 * 1024 * 1024 },
  );
}
