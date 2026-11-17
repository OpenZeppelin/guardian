# KYC and sanctions screening

This page builds a compliance policy as your own remote module and runs it with
a Guardian. The module is
[`examples/compliance-module`](../../../examples/compliance-module/README.md),
about 500 lines of Rust on the
[module SDK](../../../crates/guardian-module-sdk/README.md). Copy it as the
starting point for your own. It enforces two rules:

- **KYC on signers.** Every signer of an account must be KYC-verified. The
  module checks this when the account registers (`/configure`, hook
  `before_account_registered`), where it sees every signer the account will
  have. It checks again on every transaction, against the signers the
  account has after the transaction. Without the second check, an account
  could add an unverified signer, or keep one whose verification was
  withdrawn.
- **Screening recipients.** A payment may not go to a sanctioned account
  (blocklist), or may go only to a verified account (allowlist). The module
  checks every account a payment note can reach: its P2ID target and, for a
  P2IDE note that can be reclaimed, the reclaimer. It refuses a note whose
  recipient it cannot read (a partial note, a custom script), since a
  sanctioned recipient could hide there.

The lists come from a JSON file the module reads when it starts. A production
module reads the same lists from its compliance database or vendor; see
[Using your own data](#using-your-own-data).

Do the [setup](./README.md#setup) first (operator key, `.env`, server). This
page replaces its catalog with [`compliance.toml`](./compliance.toml).

## Two kinds of configuration

The module separates *who* from *which rule*:

| | Where it lives | Who changes it | Example |
|---|---|---|---|
| **Data**: who is verified, who is sanctioned | The module's data file (or your database) | Your compliance process | `kyc_signers`, `sanctioned_accounts`, `verified_accounts` |
| **Params**: which checks an instance applies | The Guardian's catalog, then the [operator API](./README.md#operator-api-at-a-glance) | Guardian operators, recorded in the params history | `{ "signers": "all", "recipients": "blocklist" }` |

One module process can serve several catalog instances with different
params. [`compliance.toml`](./compliance.toml) uses two:

- **`compliance`** has `scope = "all"`. It applies to every account: every
  signer must be KYC-verified, and no payment may go to a sanctioned account.
- **`treasury-screening`** has `scope = "assigned"`. It applies only to the
  accounts an operator assigns it to, and allows payments only to verified
  counterparties. It runs in addition to `compliance`, so it can only add
  restrictions. An assigned instance runs the params given when the account is
  assigned, not its catalog params, which are only checked at startup. So
  assign it with `{"signers":"off","recipients":"allowlist"}`: assigned with
  `{}`, it runs the module's defaults (a blocklist), which repeats `compliance`.

### Params

| Param | Values | Default |
|---|---|---|
| `signers` | `all`: every signer, on every request. `added`: every signer at registration, afterwards only the signers a transaction adds (an account keeps working if a signer later loses verification). An account registered before the module was deployed was never checked at registration, so under `added` its unverified signers are never refused; use `all` at least until every existing account has transacted once. `off`. | `all` |
| `recipients` | `blocklist`: no payment to `sanctioned_accounts`. `allowlist`: payments only to `verified_accounts`. `off`. | `blocklist` |

Any other field or value is refused when the params are written, so a typo
cannot quietly switch a check off.

### The data file

```json
{
  "kyc_signers": ["0x<64 hex digits: a signer's public key commitment>"],
  "sanctioned_accounts": ["0x<30 hex digits: a Miden account id>"],
  "verified_accounts": ["0x<30 hex digits>"]
}
```

[`lists.example.json`](../../../examples/compliance-module/lists.example.json)
is a template. Every id must be `0x` hex at its full length, in any case. The
module refuses to start on anything else, including a bech32 address or an id
cut short, because such an entry would never match and its rule would not
apply. Where to find the values:

- **Signer commitments.** These are the commitments the Guardian reports for
  an account's signers. `admin GET /accounts/{account_id}` lists them as
  `authorized_signer_ids`, and the multisig SDKs and the demo show them as
  cosigner commitments. Your KYC process records the commitment of each key a
  verified person or entity controls.
- **Account ids.** Use the `0x` hex form. `admin GET /accounts/{account_id}`
  shows both forms (`account_id` and `account_id_bech32`). Convert an address
  a wallet shows as `mtst1...` before you add it.

## Run it

1. Create a token for each instance:

   ```sh
   mkdir -p .guardian
   (umask 077; openssl rand -hex 32 > .guardian/compliance.token)
   (umask 077; openssl rand -hex 32 > .guardian/treasury-screening.token)
   cp examples/compliance-module/lists.example.json .guardian/compliance-lists.json
   ```

   The Guardian reads the same tokens through its secrets backend. With the
   guide's default `file` provider and the catalog in `.guardian/`, the names
   in the catalog (`compliance.token`, `treasury-screening.token`) are these
   files.

2. Edit `.guardian/compliance-lists.json`. The example ids are placeholders.
   Your demo signers' commitments only exist once the demo has created its
   keys (it prints them as cosigner commitments), and the module refuses to
   register an account whose signers are not listed. So start the demo first,
   copy the commitments into `kyc_signers`, then start or restart the module.
   Put a recipient account id you will refuse in `sanctioned_accounts`.

3. Start the module:

   ```sh
   cargo run -p guardian-compliance-module -- \
     --data .guardian/compliance-lists.json --listen 127.0.0.1:7445 \
     --instance compliance .guardian/compliance.token \
     --instance treasury-screening .guardian/treasury-screening.token
   ```

   It prints how many entries each list holds, and refuses to start if the
   file has an id that could never match.

4. Point the Guardian at the catalog and start it. In the guide's `.env`, set:

   ```sh
   GUARDIAN_MODULES_CONFIG=.guardian/compliance.toml
   ```

   ```sh
   cp docs/guides/policy-modules/compliance.toml .guardian/
   set -a && . docs/guides/policy-modules/.env && set +a
   cargo run -p guardian-server --bin server
   ```

   The log line `Policy modules loaded` lists
   `compliance=compliance 0.1.0` and `treasury-screening=compliance 0.1.0`.
   If the module is not running, startup stops with an error containing
   `module compliance: describe`, followed by the transport error. The
   instance is new to this Guardian, so a wrong endpoint is reported at once.
   Once an instance has started, a later restart with the module down starts
   it unavailable instead (its accounts follow `on_error`).

## Try it

| Step | Expected |
|---|---|
| Register a 2-of-2 account whose signers are both in `kyc_signers` | Registered |
| Register one with a signer that is not in `kyc_signers` | Refused at `/configure`, nothing stored; the client sees `GUARDIAN_MODULE_DENIED` with `reason_code: policy_denied` (generic) |
| Propose adding a signer that is not in `kyc_signers` | Refused at proposal creation |
| Pay a recipient in `sanctioned_accounts` | Refused at proposal creation, and again at the push if the proposal was made elsewhere and signed offline |
| `admin PUT /accounts/{account_id}/modules '{"modules":[{"module_id":"treasury-screening","params":{"signers":"off","recipients":"allowlist"}}],"reason":"treasury"}'`, then pay an account that is not in `verified_accounts` | Refused; paying a verified account is allowed |
| Read why in the Guardian's log | `Policy module denied the request` with `module`, `reason_code` (`signer_not_verified`, `recipient_sanctioned`, `recipient_not_verified` or `recipient_unknown`) and the module's `message`, which names the signer or recipient |

Clients see only `policy_denied`, because the catalog sets
`client_reason = "generic"`: they learn that a policy refused, not which rule.
With `client_reason = "full"`, they would see the reason code and the module's
client message, which names the rule but no list entry.

Neither setting hides *who is on a list*. A cosigner who proposes a payment to
an address of their choosing learns from the answer whether that address is
blocked (or, under the allowlist, verified), and an add-signer proposal tests a
signer against `kyc_signers` the same way. The catalog sets `dry_run = false`
on `compliance`, so cosigners cannot run that test for free with
`checkTransaction` / `check_transaction` ([dry runs](./custom.md#dry-runs)):
each probe is then a signed proposal request, subject to the request rate
limits, and every refusal is logged by the Guardian with the submitter
(`Policy module denied the request`). A refused proposal is not stored, so
watch those log lines rather than the account's history. Treat list membership as
something an account's own cosigners can find out, and do not put on the lists
anything that must stay secret from them.

## Changing the lists

This example reads the file once, at startup. To change a list, edit the file
and restart the module process. The Guardian keeps running. While the module
is down, its calls fail, and with `on_error = "deny"` every gated request is
refused until it answers again. That is the right failure mode for
compliance, but it means a restart is a short outage of transactions. To avoid
it, run two module processes behind a load balancer and restart them one at a
time, or have the module reload its lists in place (see below).

Changing *params* never needs a restart. A params write through the operator
API applies to the next request.

## What the module refuses, and why

| Reason code | When | Notes |
|---|---|---|
| `signer_not_verified` | A signer of the account is not in `kyc_signers` | At registration, and on transactions per `signers` |
| `recipient_sanctioned` | A payment's target or reclaimer is in `sanctioned_accounts` | `recipients = "blocklist"` |
| `recipient_not_verified` | A payment's target or reclaimer is not in `verified_accounts` | `recipients = "allowlist"` |
| `recipient_unknown` | A payment note's recipient cannot be read (partial note, unknown script) | Unless `recipients = "off"` |

A note the account sends back to itself (a P2IDE reclaim by the paying
account) is not a payment to anyone else. The fee note is not a payment.

A request the module cannot judge is an error, never an allow. Examples are a
registration without the account's signers, or a transaction without its
projection. The instance's `on_error` then decides, and `on_error = "deny"`
refuses the request.

## Going to production

- **Transport.** In the prod stage, a remote instance must use TLS
  (`endpoint = "grpc://host:7445"`) and a token. Start the module with
  `--tls-cert` and `--tls-key` (and `--client-ca` for mutual TLS), as the
  [remote module guide](./remote.md#credentials) shows. Store the tokens in
  your secrets backend (`GUARDIAN_MODULE_SECRET_PROVIDER=aws` on AWS).
- **Availability.** `on_error = "deny"` means the module's availability is your
  transaction availability. Run at least two replicas behind a load balancer,
  and alert on `<stack>-module-circuit-open` (see
  [the runbook](../../runbooks/policy-modules.md)).
- **Break-glass, and who can switch screening off.** `compliance` is not
  `operator_can_disable`, so the disable route refuses it. Its params can
  still turn it off: an operator with `policies:write_global` can set
  `{"signers":"off","recipients":"off"}` and screening stops for every account
  on the next request, recorded in the params history with the operator and
  reason. Grant `policies:write_global` only to the people who may make that
  decision, and alert on changes to this instance's params history. If no
  operator should be able to do it, remove `off` from the values your module
  accepts. `treasury-screening` can also be disabled outright by an operator
  with `modules:disable`.
- **Timeouts.** `timeout_ms` (200 here) bounds each call, including the
  connection setup after a reconnect. Both instances together must stay within
  `GUARDIAN_MODULE_EVALUATION_BUDGET_MS` (1,000 by default) at each hook.
- **Know what the Guardian cannot screen.**
  - *Leaving.* An account's signers can move it to another Guardian with
    `SwitchGuardian`, which executes on chain without this Guardian's
    approval. The policy binds only while the account stays here. An account
    whose signer lost verification can leave rather than fix it.
  - *Incoming funds.* The module sees the notes a transaction consumes, but the
    projection does not carry who sent them, so it cannot screen inbound
    payments. Screen deposits elsewhere if your program requires it.
  - *Custom-script notes.* `recipient_unknown` refuses every payment note whose
    recipient the Guardian cannot read, swaps included, for every account under
    `compliance`. An account layer can only add restrictions, so no account
    can be exempted there. If some accounts must make swaps, run them under an
    assigned instance with recipient checks that fit them, and set
    `recipients = "off"` on the shared one.
- **Records.** The Guardian records which configuration approved each delta
  (`admin GET /accounts/{account_id}/deltas/{nonce}`, field `modules`). It
  also keeps every params change in the params history, with operator and
  reason. Keep your list changes under the same kind of control in your own
  system.

## Using your own data

All data access goes through `Lists` in
[`src/lib.rs`](../../../examples/compliance-module/src/lib.rs): three sets,
filled once by `Lists::from_file`. To read from a database or a vendor
instead:

- **Load into memory, refresh in the background.** `evaluate` must answer
  within `timeout_ms` (tens of milliseconds in practice). Do not query a vendor
  API for each request. Keep the lists in memory (for example behind an
  `ArcSwap` or `RwLock`), and refresh them on a timer or on a change
  notification from your database.
- **Fail closed.** If the lists cannot be loaded at startup, exit, as this
  example does. If a refresh fails, keep serving the last good lists and alert;
  if they become too old for your policy, return
  `ModuleError::Unavailable` from `evaluate`. The instance's
  `on_error = "deny"` then refuses requests rather than letting them through
  unscreened.
- **Personal data stays with you.** The Guardian sends account ids, signer
  commitments and transaction contents, never names or documents. Map
  commitments to people in your own systems, and keep the module's logs to ids
  and reason codes.

Everything else stays the same: the params, the hooks, the reason codes and
the conformance tests.

## Testing your changes

[`tests/conformance.rs`](../../../examples/compliance-module/tests/conformance.rs)
runs every rule through the SDK's conformance suite over a real gRPC
connection. The suite checks that the module refuses what it should, allows
what it should, returns an error for what it cannot judge, and validates its
params:

```sh
cargo test -p guardian-compliance-module
```

Add a case for every rule you add. For the protocol and the full request
fields, see [custom modules](./custom.md).
