# Policy Modules Runbook

What to do when a policy module stops transactions it should let through, and
how to prepare so that the way out is known before an outage. Background: the
[policy modules guide](../guides/policy-modules/README.md) and
[RFC 0002](../rfcs/0002-guardian-modules.md) section 4.

> **Audience:** operators of a Guardian started with a module catalog
> (`GUARDIAN_MODULES_CONFIG`), with an operator key for the dashboard API, and
> deploy access for changes the API cannot make.

A module can only refuse; it can never make the Guardian sign. The risk this
runbook covers is the opposite one: a fail-closed module that is down, slow or
misconfigured stops approvals for every account it applies to, and Guardian
executions fail with `GUARDIAN_EXECUTION_MODULE_*` until it is fixed.

## Before an outage

For every instance in the catalog, know which path stops it:

| Catalog entry | Break-glass path | Time to effect |
|---|---|---|
| `operator_can_disable = true` | `POST /dashboard/modules/{id}/disable` with a reason | Next request, on every replica |
| `operator_can_disable = false`, `on_error = "allow"` | None needed for an outage: failures continue (recorded on the delta). A module that denies wrongly needs a params change or a deploy | Next request |
| `operator_can_disable = false`, `on_error = "deny"` | A deploy: change the catalog and restart | A rollout |

- `on_error = "allow"` skips the instance only when the module itself fails
  (timeout, error, invalid decision, open circuit), which the delta records in
  `modules.failed_open` (`GET /dashboard/accounts/{account_id}/deltas/{nonce}`).
  A request the Guardian cannot build (an account view it cannot read, a failed
  read), one over `GUARDIAN_MODULE_REQUEST_MAX_BYTES`, or one the module
  rejects as one it cannot judge is refused whatever `on_error` says, since a
  request decides each of them; all three are counted in
  `guardian_module_request_refused_total{module, reason}`.
  A module that rejects every request (after a protocol or version mismatch,
  say) therefore refuses every request even with `on_error = "allow"`, and its
  breaker stays closed, since a rejection is an answer: disable the instance
  (below) until the module is fixed. `reason="rejected"` on that counter shows
  it.
- Give the on-call operator `modules:disable`. It allows disabling instances
  marked `operator_can_disable` and nothing else, so it is safe to hand out
  more widely than `policies:write_global`, which enabling the instance again
  needs ([permissions](../DASHBOARD.md#permission-vocabulary)).
- Mark `operator_can_disable = false` only for instances that must hold even
  when they block everything (sanctions screening, for example), and agree in
  advance that their outage is resolved by fixing the module or by a deploy.
  Set `operator_can_set_params = false` on them too, or an operator session
  with `policies:write_global` can still empty their rule.
- Watch `guardian_module_up`, `guardian_module_circuit_open`,
  `guardian_module_fail_open_total` and the p99 of
  `guardian_module_evaluate_duration_seconds` per `module` (see
  [`spec/api.md`](../../spec/api.md) for the full list). On AWS the stack
  alarms on two of them once `guardian_modules_config` is set:
  `<stack>-module-circuit-open` (any circuit open for 10 minutes) and
  `<stack>-module-fail-open` (any module evaluation skipped under
  `on_error = allow`, threshold `alarm_module_fail_open_threshold`; it counts
  skips, dry-run checks included, not approved requests). Elsewhere, alert on
  `max(guardian_module_circuit_open) > 0` for 10 minutes and on any increase of
  `guardian_module_fail_open_total`. A breaker opens only once a health probe
  fails too, so a module that answers its probe but fails every evaluation
  keeps its circuit closed: watch
  `guardian_module_evaluate_duration_seconds_count{outcome="error"}` for it.

Every API change requires a `reason` and is recorded in the instance's params
history (`GET /dashboard/modules/{id}/params/history`), with the operator and
time.

The examples use the guide's `admin` helper
(`admin METHOD PATH [JSON]` against `/dashboard/PATH`, see the
[guide setup](../guides/policy-modules/README.md#4-the-operator-cli)); the same
routes are in [`DASHBOARD.md`](../DASHBOARD.md#policy-modules) and in
`@openzeppelin/guardian-operator-client`.

## A module is down

Symptoms: clients get `503 GUARDIAN_MODULE_UNAVAILABLE` with `meta.module`
(no module for an instance with `client_reason = "generic"`; the `warn` log names it);
`guardian_module_up{module}` is 0; `Policy module health probe failed` and
`Policy module failed; refusing` at `warn`;
`guardian_module_circuit_open{module}` is 1 once a replica has seen
`GUARDIAN_MODULE_BREAKER_FAILURES` (5) failures in a row (failed calls and
unanswered probes count, dry-run checks do not) and at least one health probe
has failed, or at once when a probe finds
the module incompatible with this Guardian, after which the
module is skipped without waiting for its timeout. It reads 0 again once the
instance is disabled, since a disabled instance is not called, so the alarm
clears after the break-glass step and can fire for another instance.

1. Check the module itself (its process, endpoint, certificate, token). A
   module that rejects the Guardian's token fails `Describe`, so it looks
   down.
2. If it cannot be fixed quickly and the catalog allows it, disable it:

   ```sh
   admin POST /modules/risk-vendor/disable '{"reason":"vendor outage INC-123"}'
   ```

   Requests stop calling it on every replica at once. Deltas signed meanwhile
   name the catalog and the two params versions they were approved under in the
   `modules` field of `GET /dashboard/accounts/{account_id}/deltas/{nonce}`, so
   the gap is auditable.
3. When the module is healthy again, enable it. Enabling runs `Describe` and
   `ValidateParams` first and returns `503 GUARDIAN_MODULE_UNAVAILABLE`
   without changing anything if they fail:

   ```sh
   admin POST /modules/risk-vendor/enable '{"reason":"vendor recovered INC-123"}'
   ```

4. If the catalog does not allow disabling, change it and roll the replicas:
   set `on_error = "allow"`, or `operator_can_disable = true` and then
   disable through the API, or remove the instance. Editing `enabled` in the
   catalog does nothing once the instance has started once: the stored flag
   applies and `guardian_module_config_drift` reports the difference. The one
   exception tightens policy: an instance the catalog declares enabled and
   not `operator_can_disable` is switched on at startup even when it is
   stored as disabled, with a params history entry by `catalog`, so making an
   instance non-disableable in the catalog also ends an earlier disable.
   Removing an instance leaves its account assignments in place, marked
   `orphaned` and skipped, so adding it back restores them. Changing an
   instance's `scope` does the same to the rows the new scope does not read
   (layers once it is no longer `all`, assignment entries once it is):
   startup logs how many, and `guardian_module_orphaned_assignments` counts
   them. Changing the scope back restores them.

The breaker closes by itself: a successful health probe
(`GUARDIAN_MODULE_PROBE_INTERVAL_MS`, 5 s) lets the next real call through, and
a successful call closes it. Breakers are per replica, so replicas can
disagree for a few seconds; `GET /dashboard/modules` reports the `circuit` of
the replica that answered.

## A module refuses what it should allow

Symptoms: `403 GUARDIAN_MODULE_DENIED` with a `reason_code` that should not
apply; the `info` log line `Policy module denied the request` names the
`module`, `layer`, `reason_code`, the module's `message`, and the
`params_version` and `account_params_version` it decided under.

1. Find the change: `admin GET /modules/{id}/params/history` lists every
   params write with its previous and new value, operator and reason. A
   denial in the `account` layer comes from the account's own rule: see
   `admin GET /accounts/{account_id}/modules`.
2. Put the previous params back, which the module validates before storing:

   ```sh
   admin PUT /modules/caps/params '{"params":{...previous...},"reason":"revert bad cap INC-124"}'
   ```

   An account's layer is removed with
   `admin DELETE /accounts/{account_id}/modules/{id}/params '{"reason":"..."}'`.
3. If the module's code is wrong rather than its params, disable the instance
   as above, or for a remote module roll back its deployment; the Guardian
   needs no restart.

## Requests are slow or time out

Symptoms: `timed out` failures, the p99 of
`guardian_module_evaluate_duration_seconds{module}` near the instance's
`timeout_ms`, client requests that take up to
`GUARDIAN_MODULE_EVALUATION_BUDGET_MS` (1 s) longer than usual.

- One hook invocation spends at most the budget in module calls. Once it is
  spent, the remaining modules count as timed out and their `on_error`
  applies; these cuts do not count against the breaker.
- `guardian_module_over_budget_accounts` counts accounts whose modules could
  exceed the budget (logged at startup with their worst hook). Remove
  assignments or account layers from those accounts, or lower the
  instances' `timeout_ms` with a deploy. The count, like
  `guardian_module_orphaned_assignments`, is taken again every minute, so it
  drops within a minute of the fix.
- Raising `GUARDIAN_MODULE_EVALUATION_BUDGET_MS` needs a restart and must stay
  below the execution reservation lease (the prod stage refuses to start
  otherwise), and client request timeouts must exceed it.

## The server does not start

An enabled instance that cannot be created or fails its startup checks stops
startup: an unknown kind, a token the module rejects, a module that answers
with a protocol version or hooks the Guardian cannot use, a secret the secrets
backend cannot read, params the module refuses, or an unreachable module of an
instance that has never started on this Guardian. The log names the instance
(`module <id>: ...`).

A module that is merely down does not stop a restart once its instance has
started here before. The server logs `Enabled module did not answer its
startup checks` and starts with that instance unavailable: its circuit is
open, so its accounts get `GUARDIAN_MODULE_UNAVAILABLE` (or skip it with
`on_error = "allow"`, recorded as a fail-open skip), and every other account
is served. Each health probe then runs the full startup checks, and the
instance comes back on its own once they pass (`Policy module passed the
startup checks it could not be reached for`). If the module returns with
params it now refuses, the instance stays unavailable and the log says why:
write params it accepts through the operator API.

- Fix the cause (start the module, fix the secret, see
  [secrets](./secrets.md#policy-module-secrets)), or
- remove the instance from the catalog and restart; its stored params,
  history and assignments are kept for when it comes back.

A disabled instance that fails its checks only logs `Disabled module failed
its startup checks` and stays disabled. Each replica creates its instances when
it starts: if one replica could not create a disabled instance (a secret it
could not read at the time, say), enabling the instance through another replica
does not repair it there, and its calls on that replica fail and follow
`on_error`. `guardian_module_up` is 0 for the instance on that replica; restart
it after enabling.

## Rolling back to a build without modules

A server build from before policy modules does not know the module
permissions, so it refuses to load an operator allowlist that grants
`policies:write_global` or `modules:disable` (`policies:write` is older and
stays). A Guardian execution a module refused is stored with an execution
failure code the older build cannot decode, so its execution status
(`GET /delta/proposal/execution`) fails there with a storage error. Before
deploying such a build:

1. Restart the current build without the catalog
   (`GUARDIAN_MODULES_DISABLED=true` in production) or with
   `GUARDIAN_PROVING_ENABLED=false`, so no new refusal is stored. Refusals
   stored before stay unreadable to the older build
   ([compatibility notes](../MIDEN_COMPATIBILITY.md)).
2. Remove `policies:write_global` and `modules:disable` from the allowlist (the
   entries keep their other permissions).
3. Unset `GUARDIAN_MODULES_CONFIG`. The older build ignores it, but leaving it
   set means the next upgrade starts with the catalog again, which is what you
   want only if you roll forward.

The module tables stay in the database and are ignored by the older build;
rolling forward again picks them up, with the operator params, layers and
assignments as they were. On a prod-stage build with modules, starting without
the catalog while those tables hold state is refused unless
`GUARDIAN_MODULES_DISABLED=true` (see
[CONFIGURATION](../CONFIGURATION.md)).

## After an incident

- Re-enable what was disabled, with a reason that references the incident.
- Check `guardian_module_fail_open_total` for the window: deltas signed while
  a fail-open module was failing carry `modules.failed_open` naming it on the
  dashboard delta detail. Every request let past a failing fail-open module,
  approvals included, is in the audit log as a `modules.failed_open` event.
- If the outage showed that an instance should not stop approvals, change its
  `on_error` or `operator_can_disable` in the catalog now, not during the
  next one.
