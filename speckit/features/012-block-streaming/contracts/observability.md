# Contract: Observability

**Feature**: 012-block-streaming

## Metrics (`crates/server/src/metrics/names.rs`, registered with help text; allowlisted in `infra/observability.tf`)

| Name | Type | Labels | Meaning |
|---|---|---|---|
| `guardian_block_watch_blocks_total` | counter | `transport=stream\|poll` | Blocks processed. |
| `guardian_block_watch_last_block` | gauge | — | Cursor after the last processed block. |
| `guardian_block_watch_lag_blocks` | gauge | — | `committed_chain_tip - last_block` as of the last block (poll: tip read − last block). |
| `guardian_block_watch_account_updates_total` | counter | `outcome=not_guarded\|skipped_pending_candidate\|at_stored\|own_history\|unfinished_configuration\|observed\|undecodable` | Every `updated_accounts` entry, by what the filter and the commitment comparison decided. |
| `guardian_block_watch_observations_pending` | gauge | — | Rows awaiting a visit (store count, refreshed per block and per visit). |
| `guardian_block_watch_observations_long_waiting` | gauge | — | Observations the holder found waiting `LONG_WAIT_BLOCKS` (28,800) or more at their last visit; 0 on step-down (PR #586 review). |
| `guardian_block_watch_observation_visits_total` | counter | `outcome=checked\|deferred\|waiting\|failed\|dropped` | Visitor outcomes. |
| `guardian_block_watch_reconnects_total` | counter | `reason=status_<code>\|transport\|ended\|out_of_sequence\|stall\|malformed\|unavailable_block\|timeout\|skipped_malformed_block\|skipped_unavailable_block` | Source reopen events (`unavailable_block`: a poll's missing block the node then served; `skipped_unavailable_block`: confirmed gone, skipped). |
| `guardian_block_watch_walk_requests_total` | counter | `reason=cursor_init\|cursor_jump\|genesis_changed\|genesis_unknown\|node_behind_too_long\|cursor_ahead_of_tip\|observations_lost\|undecodable_updates\|skipped_malformed_block\|skipped_unavailable_block\|observation_dropped` | Walks of the fleet block watch recorded a request for (PR #586 review). |
| `guardian_block_watch_transport` | gauge | `transport=stream\|poll` | 1 for the active transport, 0 for the other. |
| `guardian_block_watch_block_processing_seconds` | histogram | — | `process(block)` duration (SC-003). |

The existing `guardian_release_sweep_accounts_total{outcome}` keeps counting
detector findings whether a visit came from the rotation, a re-check, or an
observation (add label `source=rotation|recheck|observation` only if the
cardinality rules in `metrics/names.rs` allow a closed set — they do).

## Alarm (`infra/observability.tf`)

`guardian-block-watch-lag`: `guardian_block_watch_lag_blocks > 200` and
shrinking by fewer than 100 blocks per period (`DIFF > -100`) for two
consecutive 5-minute periods, so a replay after a restart (far behind, but
catching up fast) does not page while a feed that barely gains does; same SNS/Slack
routing as the other server alarms (issue #443). `guardian-block-watch-stalled`:
`DIFF(last_block) <= 0` for two 5-minute periods; `last_block` is exported
from the stored cursor before the node is reached, and reset to 0 when a
replica steps down (PR #586 review, M1/M2/M7). Treat `reconnects_total{reason=
status_resource_exhausted}` increments as a dashboard panel, not an alarm
(the fallback covers them). `guardian-block-watch-walk-requests`: more than
10 walk requests (`guardian_block_watch_walk_requests_total`, all reasons)
in one hour, i.e. a lasting cause (an undecodable update format asks once
per block) that quietly turns the rotation into a continuous walk (PR #586
review).

## Log events (stable `event=` keys; list in `docs/TROUBLESHOOTING.md`)

| Event | Level | Fields |
|---|---|---|
| `block_watch_attached` | info | `transport`, `from_block`, `chain_tip` |
| `block_watch_transport` | info/warn | `from`, `to`, `reason`, `retry_in_seconds` |
| `block_watch_reconnect` | info/warn | `reason`, `code`, `backoff_ms`, `from_block` |
| `block_watch_stall` | warn | `last_block`, `chain_tip`, `idle_seconds` |
| `block_watch_cursor_init` | info | `start_block` |
| `block_watch_cursor_jump` | warn | `cursor`, `tip`, `gap`, `max_catchup_blocks` |
| `block_watch_block` | debug | `block_num`, `updates`, `guarded`, `observed`, `duration_ms` |
| `block_watch_observation_recorded` | info | `account_id`, `block_num`, `on_chain`, `stored` |
| `block_watch_observation_visited` | debug | `account_id`, `block_num`, `outcome`, `attempts` |
| `block_watch_observation_dropped` | warn | `account_id`, `block_num`, `attempts` |
| `block_watch_stale_lease` | warn | `holder_id`, `fence_token` |
| `block_watch_walk_owed` | info | `requested_seq`, `completed_seq` |
| `block_watch_walk_request_failed` | warn | `error` |
| `block_watch_undecodable_updates` | warn | `block_num`, `undecodable` |
| `block_watch_block_skipped` | warn | `block_num`, `attempts`, `error` |
| `block_watch_node_behind` | warn | `cursor`, `tip` |
| `block_watch_node_caught_up` | info | — |
| `block_watch_block_not_served` | info | `block_num`, `confirm_seconds` (the block is skipped if it stays unserved that long) |
| `block_watch_observation_waiting` | warn | `account_id`, `block_num`, `waited_blocks` (once per observation and holder) |
| `release_sweep_walk_partial` | info | `unchecked_accounts`, `retry_in_seconds` |
| `release_sweep_walk_retried` | info | `rounds` |
| `release_sweep_walk_unchecked` | warn | `unchecked_accounts`, `rounds` (once, after five rounds) |

Release events keep `release_sweep_released` with `detected_by`, plus
`source=block_watch` and `observed_block_num` when applicable.

## Dashboard

`GET /dashboard/info` → `backend.block_watch` (shape in `data-model.md`).
Operator smoke harness (`examples/operator-smoke-web`) prints it alongside
`backend.release_sweep`.
