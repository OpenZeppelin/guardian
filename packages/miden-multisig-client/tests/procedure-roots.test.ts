import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

import { PROCEDURE_ROOTS } from '../src/procedures.js';

interface GeneratedProcedureRoot {
  name: keyof typeof PROCEDURE_ROOTS;
  rust_hex: string;
  typescript_hex: string;
}

interface GeneratedProcedureRoots {
  procedure_roots: GeneratedProcedureRoot[];
}

let cachedRoots: GeneratedProcedureRoots | null = null;

function loadGeneratedProcedureRoots(): GeneratedProcedureRoots {
  if (cachedRoots) {
    return cachedRoots;
  }

  const repoRoot = fileURLToPath(new URL('../../../', import.meta.url));
  const output = execFileSync(
    'cargo',
    ['run', '--quiet', '--example', 'procedure_roots', '-p', 'miden-multisig-client', '--', '--json'],
    {
      cwd: repoRoot,
      encoding: 'utf8',
    },
  );

  cachedRoots = JSON.parse(output) as GeneratedProcedureRoots;
  return cachedRoots;
}

describe('procedure roots', () => {
  it('match the compiled account procedure hashes in SDK hex format', () => {
    const generated = loadGeneratedProcedureRoots();
    const expected = Object.fromEntries(
      generated.procedure_roots.map((procedure) => [procedure.name, procedure.typescript_hex]),
    );

    expect(PROCEDURE_ROOTS).toEqual(expected);
  });

  it('do not use the Rust display encoding', () => {
    const generated = loadGeneratedProcedureRoots();
    const sendAsset = generated.procedure_roots.find(
      (procedure) => procedure.name === 'send_asset',
    );

    expect(sendAsset).toBeDefined();
    expect(PROCEDURE_ROOTS.send_asset).toBe(sendAsset?.typescript_hex);
    expect(PROCEDURE_ROOTS.send_asset).not.toBe(sendAsset?.rust_hex);
  });
});
