# Built-in modules

`basic-policy` runs inside the Guardian process: no extra service, no network
hop. One kind backs any number of instances, each with its own params, hooks
and scope. Do the [setup](./README.md#setup) first; the catalog is
[`built-in.toml`](./built-in.toml).

## `basic-policy` params

All params are optional; an instance with none allows everything.

| Param | Value | Refuses with | Notes |
|---|---|---|---|
| `max_amount_per_tx` | `{ "<faucet id>": "<amount>" }` | `amount_cap_exceeded` | Gross amount per asset across the transaction's payment notes, in base units. The fee note is not a payment. |
| `asset_allowlist` | `["<faucet id>", ...]` | `asset_not_allowed` | Assets sent in payment notes and received in input notes. |
| `recipient_allowlist` | `["<account id>", ...]` | `recipient_not_allowed` | P2ID and P2IDE targets. |
| `recipient_denylist` | `["<account id>", ...]` | `recipient_blocked` | |
| `deny_unknown_recipients` | `true` / `false` | `recipient_not_allowed` | Refuses payment notes whose recipient the Guardian cannot identify (not P2ID or P2IDE). |
| `signer_allowlist` or `signer_denylist` | `["<signer commitment>", ...]` | `signer_not_allowed` | Mutually exclusive. |
| `signer_check` | `"added"` (default) or `"all"` | | `added` checks only signers a transaction adds; `all` checks every signer after it. |

Signer rules apply at all three hooks, including registration. Transaction
rules (caps, assets, recipients) apply to proposals and approvals.

## The example catalog

| Instance | Scope | Rule |
|---|---|---|
| `caps` | all | At most 5 of one asset per transaction; can be switched off; discloses its params to cosigners |
| `blocklist` | all | Recipient denylist, empty until you fill it; denies generically |
| `signers` | all | One signer may never be added; the catalog protects it from being switched off |
| `treasury-allowlist` | assigned | Opt-in: applies only to accounts you assign it to |

## Try it

Use your account id from the demo's "Show account" as `ACCOUNT`, and a second
account as `RECIPIENT`.

**Cap.** Propose a payment of 10 of the capped asset. It is refused when the
proposal is created: `GUARDIAN_MODULE_DENIED`, module `caps`, reason
`amount_cap_exceeded`, layer `global`. A payment of 3 goes through.

**Global blocklist.** Block the recipient for every account:

```sh
admin PUT /modules/blocklist/params '{"params":{"recipient_denylist":["RECIPIENT"]},"reason":"test"}'
```

A payment to it is refused, but the client only sees
`reason_code: "policy_denied"`: `blocklist` sets `client_reason = "generic"`,
so neither the module nor `recipient_blocked` reaches the user. The server log
has both.

**A stricter rule for one account.** Add a lower cap for one account only:

```sh
admin PUT /accounts/ACCOUNT/modules/caps/params \
  '{"params":{"max_amount_per_tx":{"0xFAUCET":"2"}},"reason":"test"}'
admin GET /accounts/ACCOUNT/modules
```

The second call shows the global params and the account's layer side by side.
From this account a payment of 3 is now refused with layer `account`; other
accounts still have the cap of 5. Set the account's cap to 100 and a payment of
10 is still refused, by the global layer: an account layer only adds
restrictions. Remove it with:

```sh
admin DELETE /accounts/ACCOUNT/modules/caps/params '{"reason":"done"}'
```

**Opt-in module.** Give one account an allowlist of recipients:

```sh
admin PUT /accounts/ACCOUNT/modules \
  '{"modules":[{"module_id":"treasury-allowlist","params":{"recipient_allowlist":["RECIPIENT"]}}],"reason":"test"}'
```

A payment from that account to any other recipient is refused with
`recipient_not_allowed`. The call replaces the account's whole assignment
list; send `"modules":[]` to clear it.

**Signer rule.** Propose adding the cosigner
`0xdededededededededededededededededededededededededededededededede`. It is
refused with `signer_not_allowed`, at proposal creation and again at approval
if the transaction was signed offline.

**Switch.** `admin POST /modules/caps/disable '{"reason":"test"}'` stops the
cap for every account until `/enable`. `signers` returns `409
GUARDIAN_MODULE_NOT_DISABLEABLE`: its catalog entry does not set
`operator_can_disable`.

**History.** `admin GET /modules/caps/params/history` lists every change with
its version, operator and reason, including the seed and each enable or
disable.

## What persists

The catalog's `params` are only a seed. After the first start the stored
params apply, and editing the catalog shows up as drift (`params_drift: true`
in `GET /modules`) without changing behaviour. Params that hold secret
references stay managed in the file and cannot be changed through the API.
