# Contract: run result

**Feature**: `001-system-e2e-qualification` · Satisfies FR-003, FR-003a to FR-003f, FR-007a, FR-025g, FR-031, FR-037, FR-040.

Both drivers emit this. The reporter merges across drivers and networks. It is
the artifact a release record points at, so it must stay readable after the run
logs expire.

```json
{
  "run_id": "qual-2026-09-15-testnet-a1b2c3",
  "trigger": "schedule",
  "requested_by": null,
  "network": { "name": "testnet", "observed_protocol_version": "0.16.1",
               "historical_window": "PT30M", "fee_asset": "0x..." },
  "artifact_set": {
    "image_digest": "sha256:...",
    "image_revision": "383bc1d4...",
    "pairing": "branch",
    "sdk_versions": { "miden-multisig-client": "0.17.0" },
    "sdk_integrity": {},
    "miden_versions": { "miden-protocol": "0.16.1" }
  },
  "scenario_results": [
    { "scenario_id": "proposal-execute-2of3-falcon", "sdk": "rust",
      "runtime": "native", "outcome": "passed", "duration": "PT48S",
      "embedded_retry": false },
    { "scenario_id": "proposal-execute-2of3-falcon", "sdk": "typescript",
      "runtime": "server-side", "outcome": "environment_blocked",
      "reason": "anchor block pruned before execution",
      "classification": "environment", "embedded_retry": true,
      "duration": "PT151S" }
  ],
  "funding_summary": { "balance_at_start": "...", "spent": "...",
                       "projected_remaining_runs": 42, "required": true },
  "conclusion": "success",
  "qualification_claim": "partial",
  "not_covered": ["evm surface (out of scope)",
                  "operator surface (deferred)",
                  "mixed-scheme accounts (capability gap, FR-017a)"]
}
```

## Derivation rules

**`conclusion`** (FR-003a):
- `failure` if any scenario is `failed` with `classification` `product` or
  `setup`.
- `success` otherwise, including when scenarios are `environment_blocked`.
- Deterministic-profile runs concluding `failure` block a merge (FR-015a).
  Live-profile runs never block publication (FR-041).

**`qualification_claim`** (FR-003d, FR-003e):
- `full` only when every required matrix entry for this network ran and passed.
- `partial` when some required entries did not pass for any reason, including
  environment-blocked and skipped.
- `none` for a filtered run. A filtered run reports which scenarios passed and
  never presents itself as a qualification.

**Cross-network** (FR-003b, FR-026c): results are stored per network and never
merged into one verdict. A summary may state whether every required network
passed, provided each network's result stays separately visible.

## Constraints

- `reason` is required for every outcome other than `passed`.
- `classification` is required when `failed` and is never softened to present a
  cleaner result (FR-041b).
- No field carries a private key, treasury credential, session cookie, or
  signed payload. Diagnostics are attached out of band and bounded (FR-036).
- Error assertions inside scenarios key on stable error codes, never on message
  wording (FR-012a).
