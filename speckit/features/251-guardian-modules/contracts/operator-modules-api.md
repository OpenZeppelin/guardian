# Contract: Operator Module API

**Source**: RFC 0002 section 2.2. Routes live on the `/dashboard/*` surface with session-cookie authentication and are HTTP-only, like the rest of the operator dashboard. Every write requires `reason` and is audited with the operator identity. Every route is added to the generated OpenAPI documents (`docs/openapi*.json`). Client: `packages/guardian-operator-client`, updated in the same change.

## Routes

| Method and path | Permission | Slice | Request body | Success |
|---|---|---|---|---|
| `GET /dashboard/modules` | `dashboard:read` | M2a | none | 200 `ModuleCatalogView` |
| `GET /dashboard/accounts/{account_id}/modules` | `dashboard:read` | M2a | none | 200 `AccountModulesView` |
| `PUT /dashboard/accounts/{account_id}/modules` | `policies:write` | M2a | `AccountModulesUpdate` | 200 `AccountModulesView` (the new effective set) |
| `PUT /dashboard/modules/{module_id}/params` | `policies:write_global` | M2a | `ParamsBody` | 200 `ModuleInstanceView` |
| `PUT /dashboard/accounts/{account_id}/modules/{module_id}/params` | `policies:write` | M2a | `ParamsBody` | 200 `AccountModulesView` |
| `DELETE /dashboard/accounts/{account_id}/modules/{module_id}/params` | `policies:write` | M2a | `ReasonBody` | 200 `AccountModulesView` |
| `GET /dashboard/modules/{module_id}/params/history` | `dashboard:read` | M2a | none | 200 `ParamsHistory` |
| `POST /dashboard/modules/{module_id}/enable` | `policies:write` | M2a | `ReasonBody` | 200 `ModuleInstanceView` |
| `POST /dashboard/modules/{module_id}/disable` | `policies:write` or `modules:disable` | M2a | `ReasonBody` | 200 `ModuleInstanceView` |
| `POST /dashboard/modules/{module_id}/resync` | `policies:write` | M2b | `ResyncBody` | 200 `ResyncResult` |

Enable and disable are idempotent: enabling an enabled instance or disabling a disabled one returns 200 with the unchanged view and writes an audit record.

## Schemas

```text
ReasonBody            { reason: string (non-empty) }
ResyncBody            { reason: string (non-empty), account_ids?: string[] }
AccountModulesUpdate  { modules: [{ module_id: string, params: object }], reason: string (non-empty) }
ParamsBody            { params: object, reason: string (non-empty) }

ModuleInstanceView {
  module_id: string,
  kind: string,
  scope: "all" | "default" | "assigned",
  hooks: string[],
  declared: bool,               // false: a stored flag the catalog no longer declares
  catalog_enabled?: bool,       // seed value from the file; absent when declared = false
  enabled: bool,                // runtime flag
  drift: bool,                  // catalog_enabled != enabled
  operator_can_disable: bool,
  params: object,               // current global params; secret references unresolved
  params_drift: bool,           // catalog params differ from the stored global params
  circuit: "closed" | "open" | "half_open"   // on the replica that answered
}
ModuleCatalogView     { catalog_hash: string, modules: ModuleInstanceView[] }

AccountModuleEntry {
  module_id: string,
  scope: "all" | "default" | "assigned",
  enabled: bool,
  orphaned: bool,               // stored entry naming an instance the catalog no longer declares
  global_params?: object,       // scope = all: the instance's global params
  account_layer?: object,       // scope = all: this account's layer, when set
  params?: object               // default / assigned: this account's own params
}                               // secret references are shown unresolved
AccountModulesView    { account_id: string, modules: AccountModuleEntry[] }   // evaluation order

ResyncResult          { module_id: string, account_ids: string[] }            // pairs cleared
ParamsHistory         { module_id: string, entries: [{ version: number, account_id?: string, previous: object, new: object, operator: string, reason: string, at: string }] }
```

## Errors

All errors use the `{ code, message, meta }` envelope.

| Condition | HTTP | `code` | `meta` |
|---|---|---|---|
| Operator lacks a required permission | 403 | `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` (existing) | `{ missing_permissions: string[] }` |
| Missing or empty `reason` | 400 | `invalid_input` (existing) | none |
| Account layer on an instance that is not `scope = all` | 400 | `invalid_input` | none |
| Global params change on an instance whose catalog params hold secret references | 400 | `invalid_input` | none |
| `PUT` entry names an `all` instance, repeats a module, or params contain `$secret` | 400 | `invalid_input` | none |
| `PUT` params fail `validate_params` | 400 | `invalid_input` | none; `message` names the module |
| `PUT` effective set exceeds the evaluation budget on any hook | 400 | `invalid_input` | none |
| `module_id` not declared and not stored | 404 | `GUARDIAN_MODULE_NOT_FOUND` (new) | `{ module }` |
| Unknown account | 404 | `account_not_found` (existing) | none |
| Disable of an instance without `operator_can_disable` | 409 | `GUARDIAN_MODULE_NOT_DISABLEABLE` (new) | `{ module }` |
| Enable of an instance whose startup checks fail (unreachable, unsupported hook or protocol, invalid params) | 503 | `GUARDIAN_MODULE_UNAVAILABLE` | `{ module }` |
| Resync of a pair not in `resync_required` | 200 | none; the pair is omitted from `ResyncResult.account_ids` | |

Contract tests in `crates/server` and `packages/guardian-operator-client` pin every row.
