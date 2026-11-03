import { execFile } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { beforeAll, describe, expect, it } from 'vitest';

import { PROCEDURE_ROOTS } from '../src/procedures.js';

interface GeneratedProcedureRoot {
  name: keyof typeof PROCEDURE_ROOTS;
  rust_hex: string;
  typescript_hex: string;
}

interface GeneratedProcedureRoots {
  procedure_roots: GeneratedProcedureRoot[];
}

// `cargo run` compiles the example first when the target dir does not hold it yet: CI prebuilds
// it, but a cold local run takes tens of seconds, far past the default per-test budget.
const FIXTURE_BUILD_TIMEOUT_MS = 600_000;

async function loadGeneratedProcedureRoots(): Promise<GeneratedProcedureRoots> {
  const repoRoot = fileURLToPath(new URL('../../../', import.meta.url));
  const { stdout } = await promisify(execFile)(
    'cargo',
    ['run', '--quiet', '--example', 'procedure_roots', '-p', 'miden-multisig-client', '--', '--json'],
    { cwd: repoRoot, encoding: 'utf8' },
  );
  return JSON.parse(stdout) as GeneratedProcedureRoots;
}

describe('procedure roots', () => {
  let generated: GeneratedProcedureRoots;

  beforeAll(async () => {
    generated = await loadGeneratedProcedureRoots();
  }, FIXTURE_BUILD_TIMEOUT_MS);

  it('match the compiled account procedure hashes in SDK hex format', () => {
    const expected = Object.fromEntries(
      generated.procedure_roots.map((procedure) => [procedure.name, procedure.typescript_hex]),
    );

    expect(PROCEDURE_ROOTS).toEqual(expected);
  }, 120_000);

  it('do not use the Rust display encoding', () => {
    const sendAsset = generated.procedure_roots.find(
      (procedure) => procedure.name === 'send_asset',
    );

    expect(sendAsset).toBeDefined();
    expect(PROCEDURE_ROOTS.send_asset).toBe(sendAsset?.typescript_hex);
    expect(PROCEDURE_ROOTS.send_asset).not.toBe(sendAsset?.rust_hex);
  });
});
