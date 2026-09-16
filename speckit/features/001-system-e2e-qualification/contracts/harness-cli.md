# Contract: harness command line

**Feature**: `001-system-e2e-qualification` · Satisfies FR-004, FR-010, FR-025.

One entry point, used identically by a developer and by CI. CI passes the same
arguments a person would; there is no CI-only path.

```
qualification/stack/run.sh [OPTIONS]
```

| Option | Values | Default | Notes |
|---|---|---|---|
| `--profile` | `deterministic` \| `live` | `deterministic` | |
| `--network` | `devnet` \| `testnet` | none | Required for `live`. One network per invocation (FR-026). |
| `--image-source` | `built` \| `pulled` | `built` | |
| `--image-ref` | git ref | current checkout | With `--image-source built`. |
| `--image-tag` | tag or digest | none | With `--image-source pulled`. Resolved to a digest before use. |
| `--pairing` | `branch` \| `release` \| `published` | inferred | Explicit value wins; an inconsistent combination is rejected rather than reconciled (FR-025a). |
| `--scenario` | scenario id | all | Repeatable. Any use marks the run filtered (FR-003e). |
| `--select` | `dimension=value` | none | Repeatable. Same filtering consequence. |
| `--sdk` | `rust` \| `typescript` \| `both` | `both` | |
| `--out` | path | `./qualification-results` | Result files and, on failure, diagnostics. |
| `--keep-stack` | flag | off | Debugging only. Prints the teardown command it skipped. |

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Run concluded successfully. Consult `qualification_claim` for what it proved; a zero exit does not mean full coverage. |
| 1 | Product failure. At least one scenario failed with `classification: product`. |
| 2 | Setup failure. Treasury unusable, scheme policy excludes the run, image identity mismatch, pairing inconsistent. |
| 3 | Environment blocked and nothing else ran. Distinguished from 1 so a schedule does not sit red through an upgrade window (FR-003a). |
| 4 | Usage error. |

## Behaviour requirements

- Teardown runs on success, failure, and signal. `--keep-stack` is the only
  exception and it says so on exit (FR-009).
- Before starting, adopt or remove resources left by a dead earlier run
  (FR-009a).
- Readiness is polled against a deadline on both the HTTP and gRPC ports, which
  bind independently. No fixed sleeps (FR-006).
- A `live` run refuses to start without a usable treasury for the named network
  and reports the distinct setup outcome rather than failing partway (FR-030).
- Secrets arrive by environment only, never as arguments, so they do not reach
  the process table or shell history (FR-027).
