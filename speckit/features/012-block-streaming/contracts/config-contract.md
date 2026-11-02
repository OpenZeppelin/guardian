# Contract: Configuration

**Feature**: 012-block-streaming

New `GUARDIAN_BLOCK_WATCH_*` environment variables, parsed in
`crates/server/src/builder/block_watch.rs` with the same rules as
`GUARDIAN_RELEASE_SWEEP_*` (`builder/release_sweep.rs`): values are
trimmed; one that does not parse or lies outside its range stops startup
with a message naming the variable; `docs/CONFIGURATION.md` gets one row
per variable in the existing table.

| Variable | Default | Range | Meaning |
|---|---|---|---|
| `GUARDIAN_BLOCK_WATCH_ENABLED` | `true` | bool | Runs block watch (the committed-block feed that drives switch detection). `false` leaves detection to the push path and the rotation. |
| `GUARDIAN_BLOCK_WATCH_TRANSPORT` | `auto` | `auto` \| `stream` \| `poll` | `auto` tries the node's `BlockSubscription` stream and falls back to `GetBlockByNumber` polling (with cooldowns) when the stream is unavailable, refused, or closed for lag; `stream` never polls (reconnects only); `poll` never subscribes. |
| `GUARDIAN_BLOCK_WATCH_POLL_SECONDS` | `3` | 1–300 | Tip read interval on the polling transport (Miden blocks every 3 s). |
| `GUARDIAN_BLOCK_WATCH_STALL_SECONDS` | `60` | 5–3600 | No block for this long triggers a tip read; a tip ahead of the cursor reopens the source. |
| `GUARDIAN_BLOCK_WATCH_MAX_CATCHUP_BLOCKS` | `10000` | 0–1000000 | Largest gap between the cursor and the tip that is replayed on (re)start; a larger gap (or no cursor) starts at the tip and requests one rotation. `0` always starts at the tip. |
| `GUARDIAN_BLOCK_WATCH_BAN_COOLDOWN_SECONDS` | `600` | 1–86400 | After the node closed the stream as too slow (`RESOURCE_EXHAUSTED`; the node bans the IP for 10 min) block watch polls and does not retry the stream for this long. |
| `GUARDIAN_BLOCK_WATCH_STREAM_RETRY_SECONDS` | `600` | 1–86400 | How long block watch stays on the polling transport after the stream was unavailable (`UNIMPLEMENTED`, `PERMISSION_DENIED`, repeated connect failures) before trying it again. |
| `GUARDIAN_BLOCK_WATCH_MAX_ATTEMPTS` | `20` | 1–1000 | Deferred/failed visits of one observation before it is dropped (warned and counted); the rotation remains the backstop. |

Semantics changes to existing variables:

- `GUARDIAN_RELEASE_SWEEP_ENABLED` now disables **the rotation** only; the
  release sweep task runs when either it or `GUARDIAN_BLOCK_WATCH_ENABLED`
  is `true`. Doc row updated to say so and to recommend raising
  `GUARDIAN_RELEASE_SWEEP_ROTATION_SECONDS` (e.g. 604800, 7 days) once block
  watch is healthy. Defaults unchanged.
- `GUARDIAN_RELEASE_SWEEP_MAX_RATE_PER_SECOND` and
  `GUARDIAN_RELEASE_SWEEP_RECHECK_SECONDS` apply to observation visits too
  (one pacing budget; the re-check delay is also the base of the
  observation retry backoff).

## Terraform (`infra/variables.tf`, `infra/ecs.tf`)

```hcl
variable "guardian_block_watch_enabled" {
  description = <<-EOT
    Whether block watch runs (GUARDIAN_BLOCK_WATCH_ENABLED): the committed-block
    feed that detects guardian switches at block cadence. false leaves
    detection to the push path and the release sweep rotation.
  EOT
  type    = bool
  default = true
}

variable "guardian_block_watch_transport" {
  description = "GUARDIAN_BLOCK_WATCH_TRANSPORT: auto (stream with polling fallback), stream, or poll"
  type        = string
  default     = "auto"
  validation {
    condition     = contains(["auto", "stream", "poll"], var.guardian_block_watch_transport)
    error_message = "guardian_block_watch_transport must be auto, stream, or poll."
  }
}
```

Wired into the task definition environment next to the
`GUARDIAN_RELEASE_SWEEP_*` entries in `infra/ecs.tf`;
`terraform.tfvars.example` and `infra/README.md` updated.

## Dashboard (`GET /dashboard/info`)

Additive, nullable `backend.block_watch` object (shape in
`data-model.md`). `docs/DASHBOARD.md` paragraph next to
`backend.release_sweep`. OpenAPI: `docs/openapi-dashboard.json` regenerated
(`cargo run --features evm --bin gen-openapi -- docs`); the info handler's
`ToSchema` type gains the optional field.
