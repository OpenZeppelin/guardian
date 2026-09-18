export type Profile = 'deterministic' | 'live';
export type Sdk = 'rust' | 'typescript';
export type SdkSelector = Sdk | 'both';
export type Runtime = 'native' | 'server-side' | 'browser';
export type Scheme = 'falcon' | 'ecdsa' | 'mixed' | 'n/a';
export type Shape = '1-of-1' | '2-of-3' | '3-of-3' | 'n/a';
export type Mode = 'online' | 'offline' | 'n/a';
export type NetworkName = 'devnet' | 'testnet';

export type Outcome = 'passed' | 'failed' | 'skipped' | 'environment_blocked';
export type Classification = 'product' | 'environment' | 'setup';

export interface Scenario {
  readonly id: string;
  readonly title: string;
  readonly profile: Profile;
  readonly sdk: SdkSelector;
  readonly runtime: Runtime | null;
  readonly scheme: Scheme;
  readonly shape: Shape;
  readonly mode: Mode;
  readonly actions: readonly string[];
  readonly step_budget: string;
  readonly required: boolean;
  readonly core: boolean;
}

export interface Network {
  readonly name: NetworkName;
  readonly rpc_endpoint: string;
  readonly historical_window: string;
}

export interface ExportedManifest {
  readonly scenarios: readonly Scenario[];
  readonly networks: readonly Network[];
}

export interface ScenarioResult {
  readonly scenario_id: string;
  readonly sdk: Sdk;
  readonly runtime: Runtime;
  readonly outcome: Outcome;
  readonly reason?: string;
  readonly classification?: Classification;
  readonly embedded_retry: boolean;
  readonly duration: string;
}
