import { afterAll, describe, it } from 'vitest';

import { loadManifest, selectScenarios } from './manifest.js';
import { blocksConclusion, writeResults } from './report.js';
import { runScenario, type ActionContext } from './runner.js';
import type { Profile, ScenarioResult } from './types.js';

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} must be set to run the qualification driver`);
  return value;
}

const profile = (process.env.QUAL_PROFILE ?? 'deterministic') as Profile;
const outDir = required('QUAL_OUT_DIR');
const runId = required('QUAL_RUN_ID');
const coreOnly = process.env.QUAL_CORE_ONLY === '1';
const ids = (process.env.QUAL_SCENARIOS ?? '').split(/\s+/).filter(Boolean);

const network = process.env.QUAL_NETWORK as 'devnet' | 'testnet' | undefined;

const context: ActionContext = {
  httpEndpoint: required('QUAL_HTTP_ENDPOINT'),
  grpcEndpoint: process.env.QUAL_GRPC_ENDPOINT ?? '',
  imageRevision: process.env.QUAL_IMAGE_REVISION ?? '',
  // The multisig SDK reaches GUARDIAN over HTTP, unlike the Rust SDK which
  // uses gRPC, so the live endpoint here is the HTTP one.
  live: network
    ? {
        network,
        guardianEndpoint: required('QUAL_HTTP_ENDPOINT'),
        midenRpcEndpoint:
          process.env.QUAL_MIDEN_RPC_ENDPOINT ?? `https://rpc.${network}.miden.io`,
        migrationEndpoint: process.env.QUAL_GUARDIAN_MIGRATION_ENDPOINT,
      }
    : undefined,
};

const manifest = loadManifest();
const selected = selectScenarios(manifest, { profile, sdk: 'typescript', ids })
  .filter((scenario) => !coreOnly || scenario.core);

const results: ScenarioResult[] = [];

/**
 * One test per scenario, not one test for the set.
 *
 * A single test carrying the whole matrix means one slow scenario takes the
 * timeout with it, and because the results were only written at the end, a
 * timeout discarded every scenario that had already run. Per scenario, the
 * timeout bounds the scenario, and whatever finished is still reported.
 */
describe('qualification driver', () => {
  // A selection that names only Rust scenarios leaves nothing for this leg, and
  // vitest treats a suite with no tests as an error rather than a no-op.
  if (selected.length === 0) {
    it('has no TypeScript scenarios in this selection', () => {});
  }

  it.each(selected.map((scenario) => [scenario.id, scenario] as const))(
    '%s',
    async (_id, scenario) => {
      const result = await runScenario(scenario, context);
      // eslint-disable-next-line no-console
      console.log(`  ${result.scenario_id} typescript ${result.outcome}${result.reason ? `: ${result.reason}` : ''}`);
      results.push(result);

      // Environment-blocked is deliberately not a failure: a live run whose
      // only non-passing scenarios are blocked by the environment does not
      // conclude as failed, which is what the Rust driver does too. It still
      // costs the run its qualification claim, which the merged report derives
      // from the required set rather than from an exit code. An unimplemented
      // action on a required scenario reports `failed`, so the fail-closed rule
      // is unaffected.
      // Any product or setup failure, required or not. The Rust driver's
      // `blocks_conclusion` treats both the same, and gating on `required` here
      // let a TypeScript failure on an optional scenario leave this leg's exit
      // code at 0, so the run reported success while carrying a failure.
      //
      // An `environment` failure is deliberately not one of them. It keeps
      // `outcome: 'failed'` so the report says the scenario did not complete,
      // but the network is not the product: throwing here would exit this leg
      // non-zero and fail the nightly for a prover timeout, which is both the
      // opposite of `blocks_conclusion` and the opposite of what
      // `docs/QUALIFICATION.md` promises. It still costs the run its claim,
      // because the claim is derived from the required set rather than from an
      // exit code.
      if (blocksConclusion(result)) {
        throw new Error(`${result.scenario_id}: ${result.reason}`);
      }
    },
  );

  // Runs even when a scenario failed or timed out, so the results of everything
  // that did run survive.
  afterAll(() => {
    const path = writeResults(results, outDir, runId);
    // eslint-disable-next-line no-console
    console.log(`wrote ${path} (${results.length} of ${selected.length} scenarios)`);
  });
});
