# Contract: Run Report JSON

Produced as `reports/<run-id>/run-report.json` by `aggregate`. Extends
`BenchmarkRunReport` (`benchmarks/prod-server/src/report.rs`). Marked **new**
where it extends today's output.

## Shape

```jsonc
{
  "run_id": "20260729T101500Z-ab12cd34",
  "profile_name": "target-read-20k",
  "target_id": "scalability-317",            // new
  "target_version": "1.0.0",                 // new
  "started_at": "2026-07-29T10:15:00Z",
  "completed_at": "2026-07-29T10:27:00Z",
  "measurement_seconds": 600.0,
  "guardian_endpoint": "https://guardian.openzeppelin.com",
  "deployment_shape": "prod-single-task-arm64-rds-proxy",

  "build_identity": {                         // new — FR-A08
    "start": { "version": "0.16.0", "git_commit": "6e7c6263e9ac",
               "environment": "testnet", "started_at": "2026-07-24T12:33:15Z" },
    "end":   { "version": "0.16.0", "git_commit": "6e7c6263e9ac",
               "environment": "testnet", "started_at": "2026-07-24T12:33:15Z" },
    "spanned_restart": false
  },

  "concurrency": {                            // new — FR-003b
    "load_model": "paced",
    "declared_users": 20000,
    "declared_rate_per_user_per_sec": 0.1,
    "offered_ops_per_sec": 2000.0,
    "achieved_ops_per_sec": 1987.4,
    "observed_peak_in_flight": 1043
  },

  "saturation": {                             // new — FR-013
    "attribution": "server_bound",            // "server_bound" | "generator_bound"
    "worker_cpu_max_percent": 41.2,
    "worker_headroom_percent": 58.8,
    "evidence": "no worker exceeded 45% CPU; achieved rate within 0.7% of offered"
  },

  "phase_durations": {                        // new — FR-010
    "provisioning_seconds": 0.0,              // 0 when binding to an existing population
    "warmup_seconds": 60.0,
    "measured_seconds": 600.0,
    "teardown_seconds": 34.1
  },

  "population": {                             // new
    "population_id": "pop-100k-v1",
    "total_accounts": 100000,
    "real_path_accounts": 20100,
    "bulk_accounts": 79900,
    "accounts_reached_by_load": 20000,
    "equivalence_check": { "result": "equivalent", "sampled_real": 50, "sampled_bulk": 50 }
  },

  "shard_recovery": {                         // new — FR-A06
    "expected": 16, "recovered": 16,
    "offered_load_reduction_percent": 0.0,
    "aggregate_usable": true
  },

  "clock_divergence_ms": { "max_abs": 38, "samples": 16 },   // new — FR-014

  "rate_limiting": {                          // new — FR-018
    "applies_to_transport": false,
    "note": "gRPC does not traverse the HTTP rate-limit layer",
    "rate_limited_count": 0
  },

  "scheme_distribution": { "falcon_percent": 50, "ecdsa_percent": 50 },

  "operations": [
    {
      "operation": "get_state",
      "scope": "all",
      "attempted": 1192440, "succeeded": 1192440, "failed": 0,
      "throughput_ops_per_sec": 1987.4,
      "latency_ms": { "p50": 61.0, "p95": 214.0, "p99": 402.0, "max": 1881.0 },
      "server_service_time_ms": { "p50": 12.0, "p95": 39.0, "p99": 88.0, "max": 410.0 },  // new, when available
      "failure_breakdown": {},
      "unclassified_failures": 0               // new — FR-009
    }
  ],

  "canonicalization": {
    "sampled": 60, "canonical": 58, "discarded": 0,
    "timed_out": 2, "observation_failed": 0, "timeout_seconds": 180,
    "wait_ms": { "p50": 9100.0, "p95": 22400.0, "p99": 28900.0, "max": 31200.0 },
    "includes_chain_time": true                // new — FR-005
  },

  "verdicts": [                                // new — FR-021
    { "criterion": "read_p95", "measured_value": 214.0, "target_value": 1000.0,
      "distance": -786.0, "verdict": "pass" },
    { "criterion": "canonicalization_p95", "measured_value": 22400.0, "target_value": 30000.0,
      "distance": -7600.0, "verdict": "pass" },
    { "criterion": "auth_window_failures", "measured_value": 0, "target_value": 0,
      "distance": 0, "verdict": "pass" }
  ],

  "capacity_estimate": { "target_push_tps": 500.0, "sustained_push_tps": 352.4,
                         "headroom_percent": 30.0, "required_instances": 3 },
  "cleanup": { "manifest_path": "…", "status": "completed" },
  "artifacts": { "summary_markdown": "…", "report_json": "…",
                 "canonicalization_samples": "…" }
}
```

## Invariants

| # | Invariant | Source |
|---|---|---|
| 1 | Every failed operation appears in exactly one `failure_breakdown` category | FR-009 |
| 2 | `unclassified_failures` is always present, including as `0` | FR-009 |
| 3 | `auth_window` is its own category, never folded into `auth` | FR-008 |
| 4 | Any nonzero `auth_window` count → `auth_window_failures` verdict is `fail` | FR-008 |
| 5 | `build_identity.spanned_restart == true` → every verdict is `not_measured` with `withheld_reason` | FR-A08 |
| 6 | `saturation.attribution == "generator_bound"` → latency-derived verdicts are `not_measured` | FR-013 |
| 7 | Account-dimension verdict requires `equivalence_check.result == "equivalent"` | FR-016b |
| 8 | `concurrency` reports declared users, declared rate, and observed in-flight as three distinct fields | FR-003b |
| 9 | Headline metrics derive from `measured_seconds` only | FR-010 |
| 10 | `rate_limiting.applies_to_transport` distinguishes "no 429s occurred" from "the limiter does not apply" | FR-018 |
| 11 | `verdicts` cites `target_version`; a version mismatch is an error, not a warning | FR-001 |

## Backwards compatibility

Existing consumers read `operations`, `canonicalization`, `capacity_estimate`,
`cleanup`, `artifacts` — all retained with unchanged shapes so the April reports
stay parseable for the US1 comparison. New fields are additive. Replay runs of the
pinned April profiles populate the new fields where they can (build identity,
classification, shard recovery) and emit `not_measured` verdicts for criteria the
April profile shape cannot address.
