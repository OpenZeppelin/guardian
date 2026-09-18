import { register, verifyCommitment } from './actions/account.js';
import * as live from './actions/live.js';
import { assertIdentity } from './actions/identity.js';
import {
  assertAccounts,
  assertAllowlistReload,
  assertDenial,
  assertLogout,
  assertSession,
} from './actions/operator.js';
import { assertHttpEnvelope } from './actions/errorEnvelope.js';
import { isEnvironmental } from './environment.js';
import { buildResult } from './report.js';
import type { Classification, Runtime, Scenario, ScenarioResult } from './types.js';

export interface LiveContext {
  readonly network: 'devnet' | 'testnet';
  readonly guardianEndpoint: string;
  readonly midenRpcEndpoint: string;
  /** A second GUARDIAN deployment, required only by the migration scenario. */
  readonly migrationEndpoint?: string;
}

export interface ActionContext {
  readonly httpEndpoint: string;
  readonly grpcEndpoint: string;
  readonly imageRevision: string;
  readonly live?: LiveContext;
}

export type ActionOutcome =
  | { kind: 'passed' }
  | { kind: 'failed'; classification: Classification; reason: string }
  | { kind: 'skipped'; reason: string }
  | { kind: 'environment_blocked'; reason: string };

export type ActionHandler = (context: ActionContext) => Promise<ActionOutcome>;

export const HANDLERS: Readonly<Record<string, ActionHandler>> = {
  'status-identity': assertIdentity,
  'error-envelope': assertHttpEnvelope,
  'account-register': register,
  'commitment-verify': verifyCommitment,
  'operator-session': assertSession,
  'operator-accounts': assertAccounts,
  'operator-denial': assertDenial,
  'operator-logout': assertLogout,
  'operator-allowlist-reload': assertAllowlistReload,
};

async function runLiveAction(
  action: string,
  context: ActionContext,
  scenario: Scenario,
): Promise<ActionOutcome | null> {
  switch (action) {
    case 'account-create':
      return live.createAccount(
        context,
        scenario.id,
        scenario.shape,
        scenario.scheme as 'falcon' | 'ecdsa',
      );
    case 'account-register':
      return live.registerAccount(context, scenario.id);
    case 'commitment-verify':
      return live.verifyRegistration(context, scenario.id);
    case 'proposal-create':
      return live.createProposal(context, scenario.id);
    case 'proposal-sign':
      return live.signProposal(context, scenario.id);
    case 'proposal-execute':
      return live.executeProposal(context, scenario.id);
    case 'proposal-reject-below-threshold':
      return live.rejectBelowThreshold(context, scenario.id);
    case 'proposal-reject-duplicate-signature':
      return live.rejectDuplicateSignature(context, scenario.id);
    case 'account-recover-by-cosigner':
      return live.recoverByCosigner(context, scenario.id);
    case 'balance-assert':
      return live.assertBalance(context, scenario.id);
    case 'asset-transfer':
      return live.transferAsset(context, scenario.id);
    case 'note-consume':
      return live.consumeNote(context, scenario.id);
    case 'proposal-export':
      return live.exportProposal(context, scenario.id);
    case 'proposal-sign-external':
      return live.signProposalExternally(context, scenario.id);
    case 'proposal-import':
      return live.importProposal(context, scenario.id);
    case 'proposal-create-offline':
      return live.createProposalOffline(context, scenario.id);
    case 'paused-refuses-execution':
      return live.assertPausedRefusesExecution(context, scenario.id);
    case 'custom-proposal-create':
      return live.createCustomProposal(context, scenario.id);
    case 'custom-proposal-assert':
      return live.assertCustomProposalType(context, scenario.id);
    case 'custom-proposal-prepare':
      return live.prepareCustomExecution(context, scenario.id);
    case 'abandon-and-assert-hidden':
      return live.abandonAndAssertHidden(context, scenario.id);
    case 'p2ide-send':
      return live.sendP2ide(context, scenario.id);
    case 'p2ide-timelock-assert':
      return live.assertP2ideTimelocked(context, scenario.id);
    case 'guardian-switch-online':
      return live.switchGuardianOnline(context, scenario.id);
    case 'guardian-switch-assert':
      return live.assertGuardianSwitched(context, scenario.id);
    case 'guardian-migrate':
      return live.assertGuardianMigration(context, scenario.id);
    case 'handoff-ts-to-rust':
      return live.handoffToRust(context, scenario.id);
    case 'signer-add':
      return live.addSigner(context, scenario.id);
    case 'signer-remove':
      return live.removeSigner(context, scenario.id);
    case 'threshold-change':
      return live.changeThreshold(context, scenario.id);
    case 'signer-set-assert':
      return live.assertSignerSet(context, scenario.id);
    case 'signer-removed-refused':
      return live.assertRemovedSignerRefused(context, scenario.id);
    case 'asset-send':
      return live.sendAsset(context, scenario.id);
    case 'asset-send-assert':
      return live.assertAssetSent(context, scenario.id);
    case 'procedure-threshold-set':
      return live.setProcedureThreshold(context, scenario.id);
    case 'procedure-threshold-assert':
      return live.assertProcedureThreshold(context, scenario.id);
    default:
      return null;
  }
}

/**
 * Folds an action outcome into what the report carries.
 *
 * A live run drives a public network and a remote prover, neither of them
 * under this repository's control, and both fail in ways that read exactly like
 * a scenario failing. Reporting those as product defects is how a nightly
 * schedule stops being read, so a failure whose evidence points at the link is
 * reclassified `environment`, which the conclusion already exempts.
 *
 * It stays a failure rather than becoming `environment_blocked`: that outcome
 * says the scenario never got a verdict, and reading one as the other loses the
 * difference between a night the suite could not start and a night the network
 * broke under it.
 *
 * The deterministic profile is deliberately exempt: it gates pull requests
 * against a stack the suite brings up itself, so a failure there is the
 * product's whatever its wording, and softening it would cost the one gate that
 * has to stay hard. Mirrors `report_as` in the Rust driver.
 */
export function reportAs(outcome: ActionOutcome, isLive: boolean): ActionOutcome {
  if (outcome.kind === 'failed' && isLive && isEnvironmental(outcome.reason)) {
    return { ...outcome, classification: 'environment' };
  }
  return outcome;
}

export async function runScenario(
  scenario: Scenario,
  context: ActionContext,
): Promise<ScenarioResult> {
  const startedAt = Date.now();
  const runtime: Runtime = scenario.runtime ?? 'server-side';
  let outcome: ActionOutcome = { kind: 'passed' };
  const isLive = scenario.profile === 'live';
  if (isLive) live.resetSession(scenario.id);

  for (const action of scenario.actions) {
    // Separate tables per profile. A shared one let a live scenario fall
    // through to a fixture implementation and report coverage the live path
    // never produced.
    const liveHandled = isLive ? await runLiveAction(action, context, scenario) : null;
    if (liveHandled) {
      outcome = liveHandled;
      if (outcome.kind !== 'passed') break;
      continue;
    }

    const handler = isLive ? undefined : HANDLERS[action];
    if (handler) {
      outcome = await handler(context);
    } else if (scenario.required) {
      // Skips do not fail a run, so skipping here would let a required
      // scenario report coverage that does not exist.
      outcome = {
        kind: 'failed',
        classification: 'setup',
        reason: `action ${action} is required but has no driver implementation`,
      };
    } else {
      outcome = { kind: 'skipped', reason: `no driver implementation yet for action ${action}` };
    }
    if (outcome.kind !== 'passed') break;
  }

  const durationMs = Date.now() - startedAt;
  outcome = reportAs(outcome, isLive);
  switch (outcome.kind) {
    case 'passed':
      return buildResult({ scenarioId: scenario.id, runtime, outcome: 'passed', durationMs });
    case 'failed':
      return buildResult({
        scenarioId: scenario.id,
        runtime,
        outcome: 'failed',
        reason: outcome.reason,
        classification: outcome.classification,
        durationMs,
      });
    case 'skipped':
      return buildResult({
        scenarioId: scenario.id,
        runtime,
        outcome: 'skipped',
        reason: outcome.reason,
        durationMs,
      });
    case 'environment_blocked':
      return buildResult({
        scenarioId: scenario.id,
        runtime,
        outcome: 'environment_blocked',
        reason: outcome.reason,
        durationMs,
      });
  }
}
