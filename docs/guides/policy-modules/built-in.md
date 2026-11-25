# Built-in policies

Four policy kinds run inside the Guardian process: no extra service, no network
hop. Each enforces one concern, so an instance is configured with only the
params of that rule. One kind backs any number of instances, each with its own
params, hooks and scope. Do the [setup](./README.md#setup) first; the catalog
is [`built-in.toml`](./built-in.toml).

| Kind | Judges | Hooks it supports |
|---|---|---|
| `spending-limit` | Which assets a transaction moves and how much of each it sends | `before_proposal_created`, `before_transaction_approved` |
| `recipient-policy` | Who a transaction pays | `before_proposal_created`, `before_transaction_approved` |
| `signer-policy` | Who may sign for the account | all three, including `before_account_registered` |
| `code-policy` | Which account code the account may run | all three, including `before_account_registered` |

A catalog that subscribes a kind to a hook it does not support does not start:
a spending or recipient rule has nothing to judge at registration. A kind
refuses params that belong to another, so a signer list put into a spending
instance is caught when the params are written.

## Ids

Write a signer or code commitment in `0x` hex form. Write a faucet or a
recipient either in `0x` hex form or as its bech32 account address (as a wallet
shows it, such as `mtst1...`, with or without the routing part after `_`, which
must be valid but is otherwise ignored). Params written through the operator
API, and catalog params on their first start, are stored with the hex id the
Guardian reports, so the stored params, their history and `get_policies` show
hex; an instance whose params stay in the catalog file shows them as written
there, and matches the same accounts. The address's network prefix is not
checked, since an account id is the same on every network. Params with any
other form are refused, since such an id would never match and its rule would
not apply: a placeholder left in an example catalog, a bech32 string that is
not an account address, and an id of the wrong length (a faucet or a recipient
is an account id of 30 hex digits, a signer or code commitment is 64), so an id
cut short when it was pasted, or an account id where a commitment belongs, is
caught when the params are validated. Case does not matter.

All params are optional; an instance with none allows everything, which is
what an account layer or assignment of `{}` means. A param that only modifies
another is refused without it (`deny_unknown` needs `denylist`, `check` needs
a list), since an account layer is evaluated with its own params and such a
layer alone would do nothing.

## `spending-limit`

| Param | Value | Refuses with | Notes |
|---|---|---|---|
| `max_per_transaction` | `{ "<faucet id>": "<amount>" }` | `amount_cap_exceeded` | Gross amount per asset across the transaction's payment notes, in base units. The fee note is not a payment, and an asset that is not listed is not capped. A faucet named twice in different case is refused. |
| `allowed_assets` | `["<faucet id>", ...]` | `asset_not_allowed` | Assets sent in payment notes and received from consumed notes. Unset, any asset; empty, none. |

Limits over a period (per day, per week) need state the Guardian does not keep
yet; they belong to this kind when they arrive.

## `recipient-policy`

| Param | Value | Refuses with | Notes |
|---|---|---|---|
| `allowlist` | `["<account id>", ...]` | `recipient_not_allowed` | The only accounts a payment note may go to: P2ID and P2IDE targets, and the reclaimer of a P2IDE note whose reclaim is enabled (unless the reclaimer is the paying account itself), since that account can take the note too. A note whose recipient cannot be identified is refused. Excludes `denylist`. |
| `denylist` | `["<account id>", ...]` | `recipient_blocked` | Accounts no payment note may go to, as its target or as the reclaimer of a reclaimable P2IDE note. Excludes `allowlist`. |
| `deny_unknown` | `true` / `false`; unset, `true` once `denylist` names anyone | `recipient_blocked` | With `denylist`: also refuses payment notes whose recipient the Guardian cannot identify (a partial note, or one that is not P2ID or P2IDE), since a blocked recipient could otherwise be paid through a note the Guardian cannot read. It is read from the same params as the list, so it cannot be relaxed per account: an account layer only adds restrictions. For accounts that must create custom-script notes, such as swaps, give them their own `assigned` instance with `deny_unknown = false` instead of the shared denylist. With `deny_unknown = false` the denylist checks only notes whose target it can read: a partial note, or a P2ID or P2IDE note whose target cannot be decoded, passes even when it pays a listed account, so that instance's accounts should be ones you trust not to route payments around it. |

The `blocklist` instance in the guide's catalog starts with an empty
`denylist`, which refuses nothing. Once a recipient is listed, globally or in
an account's layer, notes without a checkable recipient are refused for the
accounts that list applies to, unless the same params set `deny_unknown` to
`false`.

## `signer-policy`

| Param | Value | Refuses with | Notes |
|---|---|---|---|
| `allowlist` | `["<signer commitment>", ...]` | `signer_not_allowed` | The only signers the account may have. Excludes `denylist`. |
| `denylist` | `["<signer commitment>", ...]` | `signer_not_allowed` | Signers the account may not have. Excludes `allowlist`. |
| `check` | `"added"` (default) or `"all"` | | `added` checks only signers a registration or transaction adds; `all` checks every signer the account has after it. |

## `code-policy`

| Param | Value | Refuses with | Notes |
|---|---|---|---|
| `allowed` | `["<code commitment>", ...]` | `code_not_allowed` | Pins accounts to account code you reviewed. The Guardian does not check an account's code itself, so a module verdict is only as good as the code that requires the Guardian's signature. |
| `check` | `"changed"` (default) or `"all"` | | `changed` checks the code an account registers with and code a transaction replaces it with, so accounts already on other code keep working until they change it; `all` checks the code the account runs at every hook, so an account on unlisted code can only upgrade to listed code. |

## Refusals

Each refusal carries a fixed `client_message` naming the rule (for example
"The transaction sends more than this Guardian allows per transaction."),
never a list entry or a limit, so a `client_reason = "full"` instance tells
users what to change without disclosing params. The exact amounts and
entries go to the server log. A request a kind cannot judge (a transaction
without a Miden projection, say) is not an allow: it is refused with
`GUARDIAN_MODULE_UNAVAILABLE` whatever the instance's `on_error` says.

## The example catalog

| Instance | Scope | Rule |
|---|---|---|
| `caps` (`spending-limit`) | all | At most 5 of one asset per transaction; can be switched off; discloses its params to cosigners |
| `blocklist` (`recipient-policy`) | all | Recipient denylist, empty until you fill it; denies generically and skips dry-run checks, so the list cannot be probed without submitting (checks then report `complete: false`) |
| `signers` (`signer-policy`) | all | One signer may never be added; the catalog protects it from being switched off and its list from being changed through the API |
| `treasury-allowlist` (`recipient-policy`) | assigned | Opt-in: applies only to accounts you assign it to |

## Try it

Use your account id from the demo's "Show account" as `ACCOUNT`, and a second
account as `RECIPIENT`.

**Cap.** Propose a payment of 10 of the capped asset. It is refused when the
proposal is created: `GUARDIAN_MODULE_DENIED`, module `caps`, reason
`amount_cap_exceeded`, layer `global`. A payment of 3 goes through.

**Global blocklist.** Block the recipient for every account:

```sh
admin PUT /modules/blocklist/params '{"params":{"denylist":["RECIPIENT"]},"reason":"test"}'
```

A payment to it is refused, but the client only sees
`reason_code: "policy_denied"`: `blocklist` sets `client_reason = "generic"`,
so neither the module nor `recipient_blocked` reaches the user. The server log
has both.

**A stricter rule for one account.** Add a lower cap for one account only:

```sh
admin PUT /accounts/ACCOUNT/modules/caps/params \
  '{"params":{"max_per_transaction":{"<faucet id in 0x hex>":"2"}},"reason":"test"}'
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
  '{"modules":[{"module_id":"treasury-allowlist","params":{"allowlist":["RECIPIENT"]}}],"reason":"test"}'
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

The catalog's `params` are a seed for most instances. After the first start the
stored params apply, and editing the catalog shows up as drift
(`params_drift: true` in `GET /modules`) without changing behaviour. Params that
hold secret references, and params of an instance that sets
`operator_can_set_params = false` (`signers` in this catalog), stay managed in
the file: the API cannot change them, and an edit to the file applies at the
next start.
