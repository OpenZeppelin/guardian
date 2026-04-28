# Research: Add generic EVM proposal sharing and signing support

## Decision 1: Move network selection from server-global configuration to persisted account metadata

- Decision: network selection becomes per-account `network_config` stored in
  account metadata.
- Rationale: the current `AppState.network_client` and `NetworkType` setup only
  supports one network behavior per server process, which prevents Miden and
  feature-enabled EVM accounts from coexisting safely.
- Alternatives considered:
  - Keep a server-global network type and switch behavior from request payloads.
    Rejected because it leaks global assumptions into per-account workflows.
  - Keep separate servers per network. Rejected because the feature explicitly
    requires mixed-account support in one system.
  - Add a backward-compatibility fallback for missing `network_config`.
    Rejected because the project decision for this feature is to require
    explicit account-level network configuration.

## Decision 2: Introduce account-level network dispatch through focused capabilities

- Decision: replace the single network client abstraction with account-aware
  dispatch and smaller network capabilities.
- Rationale: the current `NetworkClient` trait is dominated by Miden
  delta/state/canonicalization concerns. EVM v1 only needs account validation,
  signer authorization, and proposal support.
- Alternatives considered:
  - Extend the existing `NetworkClient` trait with EVM methods. Rejected because
    it would turn unsupported EVM behavior into a large trait full of dead or
    dummy methods.
  - Add EVM branches directly inside services. Rejected because it would embed
    network-specific business logic back into transport-oriented service code.

## Decision 3: Separate cryptographic request verification from signer authorization

- Decision: split request-signature verification from signer authorization source
  lookup, especially for EVM.
- Rationale: current `Auth::verify` depends on stored cosigner commitments, but
  EVM v1 requires signer authority to be re-validated from RPC on every relevant
  action. Those are different responsibilities and need different data sources.
- Alternatives considered:
  - Treat stored auth commitments as the EVM source of truth. Rejected because
    signer changes on-chain would drift silently.
  - Refresh auth only during account configuration. Rejected because it does not
    satisfy the requirement to re-check signer authority on each relevant action.

## Decision 4: Keep proposal endpoints stable in v1 and move network-specific proposal logic behind strategies

- Decision: keep the current proposal endpoints and client method names in v1,
  while moving proposal normalization and proposal-id generation behind
  network-specific strategy interfaces.
- Rationale: this preserves the current surface area and lets the refactor focus
  on account/network behavior instead of replacing the full proposal API.
- Alternatives considered:
  - Introduce a brand-new proposal domain and endpoints now. Rejected because it
    expands blast radius before the EVM proposal contract shape is fully known.

## Decision 5: Use RPC-only signer validation in v1 and treat the RPC endpoint as part of account trust configuration

- Decision: EVM signer validation in v1 depends only on the configured RPC
  endpoint. No indexer is used in this feature.
- Rationale: this matches the current product decision and avoids a second
  external dependency while the contract/read model is still forming.
- Alternatives considered:
  - Use an indexer as the primary or fallback authority. Rejected because the
    product direction for v1 moved away from indexers.
  - Allow silent fallback from RPC to another authority. Rejected by the
    constitution's no-silent-fallback rule.

## Decision 6: Make unsupported EVM delta/state/canonicalization flows fail explicitly

- Decision: `push_delta`, `get_delta`, `get_delta_since`, `get_state`, and
  canonicalization-related behavior remain unsupported for EVM accounts in v1
  and must return explicit errors.
- Rationale: the feature scope is intentionally limited to account configuration
  plus proposal sharing/signing, and silent degradation would create ambiguous
  semantics.
- Alternatives considered:
  - Reuse Miden state/delta flows for EVM accounts. Rejected because EVM does
    not share Miden state or canonicalization semantics.
  - Return empty values or no-ops. Rejected because it hides unsupported
    behavior instead of surfacing it.

## Decision 7: Use a hash-based proposal identifier and defer only the normalized input set

- Decision: the EVM proposal identifier is a deterministic hash-based Guardian value.
- Rationale: this gives cross-language determinism and avoids collisions that
  raw concatenation could allow once the executable payload becomes richer than
  `chain_id + address + nonce`.
- Alternatives considered:
  - Concatenate `chain_id + account_address + nonce`. Rejected because it is
    too weak once multiple draft payload shapes or resubmissions exist.

## Decision 8: Plan around pending-only EVM proposals until lifecycle reconciliation is explicitly designed

- Decision: the refactor plan treats EVM proposals as pending-only in v1 and
  reserves sync or execution tracking for a follow-up once the contract team
  defines the readable on-chain state.
- Rationale: execution-state reconciliation is a separate design problem and is
  not needed to start the account/network refactor.
- Alternatives considered:
  - Force a sync operation into v1 now. Rejected because the contract-readable
    state needed for correct reconciliation is not agreed yet.

## Decision 9: Gate EVM support behind an explicit server-side feature flag

- Decision: EVM account configuration, EVM auth, EVM proposal payloads, and EVM
  RPC validation are available only when the server-side `evm` feature is
  enabled. Default Guardian builds and deployments remain Miden-only and reject
  EVM-shaped requests with `evm_support_disabled` before storage mutation.
- Rationale: enabling EVM by default would broaden dependency, validation, and
  operational risk and could be misread as support for every EVM-compatible
  chain. The feature flag keeps the architecture network-aware while making EVM
  rollout explicit.
- Alternatives considered:
  - Enable EVM for all deployments by default. Rejected because it creates an
    implicit all-chains support posture and increases the default attack and
    dependency surface.
  - Use separate feature flags per EVM chain. Rejected for v1 because chain
    support is account-configured through `chain_id`, `account_address`,
    `multisig_module_address`, and `rpc_endpoint`; deployments may add a chain
    allowlist without fragmenting the code architecture.
  - Fork EVM services behind the flag. Rejected because the feature should share
    the same account resolution, auth, and proposal service architecture as
    Miden once enabled.

## Deferred Topics

- RPC endpoint replacement or rotation policy remains deferred in v1.
- On-chain execution reuse and explicit proposal reconciliation remain deferred
  follow-up features.
- Deployment-specific EVM chain allowlists remain optional policy on top of the
  `evm` feature gate.
