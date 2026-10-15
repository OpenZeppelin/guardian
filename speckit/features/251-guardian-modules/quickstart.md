# Quickstart: Guardian Modules

**Feature**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md)

How to see the feature working locally and which checks prove each slice. Commands are the target shape; exact flags are pinned when `tasks.md` is generated.

## 1. Run a Guardian with a mandatory policy (M2a)

```bash
cat > /tmp/modules.toml <<'EOF'
[[module]]
id = "caps-and-signers"
kind = "basic-policy"
scope = "all"
hooks = ["before_proposal_created", "before_transaction_approved", "before_account_registered"]
timeout_ms = 20
on_error = "deny"
params = { max_amount_per_tx = { "0x<demo faucet>" = "1000" }, signer_check = "added", signer_allowlist = ["0x<demo signer 1>", "0x<demo signer 2>"] }
EOF

GUARDIAN_MODULES_CONFIG=/tmp/modules.toml cargo run -p guardian-server --bin server
```

Filesystem backend is enough for M2a (constitution: local work defaults to filesystem).

## 2. Walk the user stories with the Rust demo

With `examples/demo` against the local server:

| Step | Expected | Spec |
|---|---|---|
| Create a 2-of-2 account with both demo signers | Registered | US3 |
| Create an account with a signer not in the allowlist | `GUARDIAN_MODULE_DENIED`, `reason_code` for signers, nothing stored | US3, FR-003 |
| Propose a transfer of 500 | Created, signed, executed | US1 |
| Propose a transfer of 5,000 | Refused at creation, proposal count unchanged | US2, SC-003 |
| Propose one transaction sending 600 and 600 of the capped asset | Refused: the cap applies to the total sent | US1 |
| Export a 5,000 transfer, sign offline, push directly | Refused at push | US1, SC-002 |
| Propose adding a signer not in the allowlist | Refused at creation | US3 |
| Restart the server with no `GUARDIAN_MODULES_CONFIG` | Every step above behaves as before the feature | FR-006, SC-001 |

## 3. Remote module and failure modes (M2a)

```bash
cargo run -p guardian-module-sdk --example basic-policy-serve -- --listen 127.0.0.1:7443
```

Point a second instance of kind `grpc` at it with `on_error = "deny"`, repeat step 2, then stop the module: pushes return `GUARDIAN_MODULE_UNAVAILABLE`; after five failures they return without waiting for the timeout; `guardian_module_up` drops to 0. Switch to `on_error = "allow"`: pushes proceed and `guardian_module_fail_open_total` increases. (US4, US6)

## 4. Global and per-account rules from the operator API (prototype)

```bash
# once: an operator key and an allowlist entry with the module permissions
cargo run -p guardian-server --example modules_admin -- keygen .guardian/operator.key
#   paste the printed entry into a JSON array in .guardian/operators.json

GUARDIAN_MODULES_CONFIG=.guardian/modules.toml \
GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE=.guardian/operators.json \
  cargo run -p guardian-server --bin server

ADMIN="cargo run -q -p guardian-server --example modules_admin -- --key .guardian/operator.key"
$ADMIN GET /modules
$ADMIN PUT /modules/blocklist/params '{"params":{"recipient_denylist":["0xaaa","0xddd"]},"reason":"extend global list"}'
$ADMIN PUT /accounts/0xACCOUNT/modules/blocklist/params '{"params":{"recipient_denylist":["0xccc"]},"reason":"customer block"}'
$ADMIN GET /accounts/0xACCOUNT/modules
$ADMIN GET /modules/blocklist/params/history
$ADMIN DELETE /accounts/0xACCOUNT/modules/blocklist/params '{"reason":"lift customer block"}'

# runtime switch (disable needs operator_can_disable = true in the catalog entry)
$ADMIN POST /modules/blocklist/disable '{"reason":"emergency stop"}'
$ADMIN POST /modules/blocklist/enable '{"reason":"back on"}'
```

A payment to `0xddd` is then refused for every account (`layer: global`), one to `0xccc` only for `0xACCOUNT` (`layer: account`). Params persist in the module tables (on the filesystem backend one file, `<GUARDIAN_METADATA_PATH>/modules/module_state.json`, written atomically; on Postgres the tables `module_instances`, `module_account_params` and `module_params_history`) and survive restarts; editing `params` in the catalog afterwards shows as drift and does not apply.

## 5. Validation matrix

| Slice | Command | Proves |
|---|---|---|
| M2a | `cargo test -p guardian-module-sdk` | Conformance: identical decisions in process and over gRPC, evaluation purity, params validation, request at the size cap |
| M2a | `cargo test -p guardian-server modules::` | Catalog loading and validation, scopes, ordering, budget, breaker, fail-open continuation followed by a deny, view extraction errors, proposal context |
| M2a | `cargo test -p guardian-server storage::` | First creator kept on duplicate creation on both backends; filesystem create, sign, reload keeps the signature |
| M2a | `cargo test -p guardian-server --features postgres` (proposal storage) | Same create and update semantics on Postgres |
| M2a | `cargo run --features evm --bin gen-openapi -- --check docs` after regenerating with `-- docs` | Operator module routes and module error codes in the OpenAPI documents (AGENTS.md section 4) |
| M2a | `cargo test -p guardian-server` (full) | No regression with no modules (SC-001) |
| M2a | `cargo test -p guardian-client` and `cargo test -p miden-multisig-client` | Typed module errors on proposal creation, push, registration |
| M2a | `cd packages/guardian-client && npm test`, `cd packages/miden-multisig-client && npm test` | Same classification in TypeScript (SC-010) |
| M2a | `cd packages/guardian-operator-client && npm test` | Operator module endpoints |
| M2a | `smoke-test-rust-multisig-sdk`, `smoke-test-ts-multisig-sdk` skills with a catalog | End-to-end refusal at creation, push and registration |
| M2a | `examples/demo` and `examples/web` manual smoke with and without a catalog | Required smoke for server, client and multisig behaviour changes (AGENTS.md) |
| M2a | `smoke-test-operator-dashboard` skill (`examples/operator-smoke-web`) | Operator module routes, permission errors including `modules:disable` |
| M2a | Local A/B of `push_delta` p99 with zero, one in-process and one remote module | SC-001, SC-006 |
| M2b | `cargo test -p guardian-server --features postgres modules::notify` | Outbox atomicity, ordering, fencing, retention, resync |
| M2b | `cargo test -p guardian-module-sdk stateful` | Counting interleavings, delayed notify with per-proposer limits, failover discard |
| M2b | Deployment test: first stateful instance added while a candidate is queued; stateful module assigned to an account later | Activation fence applies, then lifts; later assignment needs no fence |
| M2b | Qualification scenario with a daily volume module | SC-008 |
| M2c | Build `examples/custom-guardian` with `--locked` | Custom kind registered and usable (US8) |
