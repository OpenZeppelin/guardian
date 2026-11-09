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

- Give the on-call operator `modules:disable`. It allows disabling instances
  marked `operator_can_disable` and nothing else, so it is safe to hand out
  more widely than `policies:write` ([permissions](../DASHBOARD.md#permission-vocabulary)).
- Mark `operator_can_disable = false` only for instances that must hold even
  when they block everything (sanctions screening, for example), and agree in
  advance that their outage is resolved by fixing the module or by a deploy.
- Watch `guardian_module_up`, `guardian_module_circuit_open`,
  `guardian_module_fail_open_total` and the p99 of
  `guardian_module_evaluate_duration_seconds` per `module` (see
  [`spec/api.md`](../../spec/api.md) for the full list).

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
`GUARDIAN_MODULE_BREAKER_FAILURES` (5) failures in a row (dry-run checks do not
count), after which the
module is skipped without waiting for its timeout.

1. Check the module itself (its process, endpoint, certificate, token). A
   module that rejects the Guardian's token fails `Describe`, so it looks
   down.
2. If it cannot be fixed quickly and the catalog allows it, disable it:

   ```sh
   admin POST /modules/risk-vendor/disable '{"reason":"vendor outage INC-123"}'
   ```

   Requests stop calling it on every replica at once. Deltas signed meanwhile
   name the catalog and params version they were approved under in
   `metadata.modules`, so the gap is auditable.
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
   applies and `guardian_module_config_drift` reports the difference.
   Removing an instance leaves its account assignments in place, marked
   `orphaned` and skipped, so adding it back restores them.

The breaker closes by itself: a successful health probe
(`GUARDIAN_MODULE_PROBE_INTERVAL_MS`, 5 s) lets the next real call through, and
a successful call closes it. Breakers are per replica, so replicas can
disagree for a few seconds; `GET /dashboard/modules` reports the `circuit` of
the replica that answered.

## A module refuses what it should allow

Symptoms: `403 GUARDIAN_MODULE_DENIED` with a `reason_code` that should not
apply; the `info` log line `Policy module denied the request` names the
`module`, `layer`, `reason_code`, the module's `message`, and the
`params_version` it decided under.

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
  instances' `timeout_ms` with a deploy.
- Raising `GUARDIAN_MODULE_EVALUATION_BUDGET_MS` needs a restart and must stay
  below the execution reservation lease (the prod stage refuses to start
  otherwise), and client request timeouts must exceed it.

## The server does not start

An enabled instance that cannot be created or fails its startup checks stops
startup: an unknown kind, an unreachable module, a token the module rejects, a
secret the secrets backend cannot read, or params the module refuses. The log
names the instance (`module <id>: ...`).

- Fix the cause (start the module, fix the secret, see
  [secrets](./secrets.md#policy-module-secrets)), or
- remove the instance from the catalog and restart; its stored params,
  history and assignments are kept for when it comes back.

A disabled instance that fails its checks only logs `Disabled module failed
its startup checks` and stays disabled.

## After an incident

- Re-enable what was disabled, with a reason that references the incident.
- Check `guardian_module_fail_open_total` for the window: deltas signed while
  a fail-open module was failing carry `metadata.modules.failed_open` naming
  it.
- If the outage showed that an instance should not stop approvals, change its
  `on_error` or `operator_can_disable` in the catalog now, not during the
  next one.
