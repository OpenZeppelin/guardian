# guardian-module-sdk

Build policy modules for [Guardian](https://github.com/OpenZeppelin/guardian):
services the Guardian asks before it signs a transaction, creates a proposal or
accepts a new account. A module answers allow or deny; it cannot sign, change
its own configuration, or see the Guardian's secrets.

The crate has:

- `Module`, the trait a module implements, and `ModuleFactory` for kinds
  compiled into a Guardian build.
- The `guardian.module.v1` protocol types (`HookRequest`, `Decision`,
  `ModuleInfo`, the Miden transaction and account projections). Protocol
  version 1 is frozen from this crate's first release (0.20.0): it changes only
  additively, and new features arrive as named capabilities.
- `serve`, a gRPC harness that exposes any `Module` as a remote service with a
  bearer token per instance.
- `policies`, the Guardian's built-in policy kinds, one concern each, as a
  reference: `SpendingLimit` (caps and allowed assets), `RecipientPolicy`,
  `SignerPolicy` and `CodePolicy`.
- `conformance` (feature `conformance`), a suite that checks a module before
  it is deployed.

## A module

```rust
use guardian_module_sdk::proto::hook_request;
use guardian_module_sdk::{
    Decision, DescribeRequest, Hook, HookRequest, Module, ModuleError, ModuleInfo,
    PROTOCOL_VERSION, ParamsVerdict,
};

struct NoPrivateNotes;

#[guardian_module_sdk::async_trait]
impl Module for NoPrivateNotes {
    async fn describe(&self, _guardian: &DescribeRequest) -> Result<ModuleInfo, ModuleError> {
        Ok(ModuleInfo {
            name: "no-private-notes".into(),
            version: "1.0.0".into(),
            protocol_version: PROTOCOL_VERSION,
            hooks: vec![Hook::BeforeTransactionApproved as i32],
            ..Default::default()
        })
    }

    async fn validate_params(&self, params: &[u8]) -> Result<ParamsVerdict, ModuleError> {
        Ok(if params == b"{}" {
            ParamsVerdict::ok()
        } else {
            ParamsVerdict::invalid("takes no params")
        })
    }

    async fn evaluate(&self, request: &HookRequest) -> Result<Decision, ModuleError> {
        let Some(hook_request::Subject::Transaction(subject)) = &request.subject else {
            return Err(ModuleError::InvalidRequest("judges transactions only".into()));
        };
        let miden = subject
            .miden()
            .ok_or_else(|| ModuleError::InvalidRequest("not a Miden transaction".into()))?;
        let private = miden
            .output_notes
            .iter()
            .any(|note| note.note_type == guardian_module_sdk::NoteType::Private as i32);
        Ok(if private {
            Decision::deny("private_note", "creates a private note")
                .with_client_message("Private notes are not allowed for this account")
        } else {
            Decision::allow()
        })
    }
}
```

Rules every module follows:

- `evaluate` has no side effects. The same transaction is evaluated when the
  proposal is created and again before the Guardian signs, clients retry, and
  the Guardian may call it concurrently.
- A deny carries a `reason_code` of 1 to 64 lowercase letters, digits or
  underscores. `message` is logged by the Guardian and never shown to clients;
  `client_message` (plain text, at most 256 characters) is shown only where
  the operator allows it.
- Return `ModuleError::InvalidRequest` rather than allow when the request
  lacks what the module needs: the Guardian refuses it whatever the operator's
  `on_error` says. Return `Unavailable` or `Internal` for the module's own
  failures, which follow `on_error`.
- Params arrive as UTF-8 JSON in `HookRequest.params` on every call: the
  module keeps no params of its own, so operator changes apply on the next
  request.

## Serve it over gRPC

```rust
use guardian_module_sdk::serve::serve;

serve(NoPrivateNotes)
    .instance("no-private-notes", token)
    .run("127.0.0.1:7443".parse()?)
    .await?;
```

Each instance the Guardian calls (its catalog `id`) has its own token; a call
for an unknown instance, or with another instance's token, is refused. A server
with no instance registered refuses every call. For a test or a local experiment
on a loopback address, `serve(module).allow_unauthenticated()` answers without
credentials; never expose such a server, since anyone who reaches it can query
the module. For TLS
add `serve(...).into_service()` to a tonic server configured for TLS, as
[`examples/policy-module`](https://github.com/OpenZeppelin/guardian/tree/main/examples/policy-module)
does. Raise the server's receive limit if you lower it: the Guardian sends
requests up to `DEFAULT_REQUEST_MAX_BYTES` (2 MiB) by default.

The Guardian side is a catalog entry:

```toml
[[module]]
id = "no-private-notes"
kind = "grpc"
endpoint = "grpc://modules.internal:7443"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 200
on_error = "deny"
auth = { token_secret = "guardian/modules/no-private-notes-token" }
```

## Check it

```toml
[dev-dependencies]
guardian-module-sdk = { version = "0.20.0", features = ["conformance"] }
```

```rust
use guardian_module_sdk::conformance::Suite;

#[tokio::test]
async fn conforms() {
    Suite::new(NoPrivateNotes)
        .allows("public payment", public_payment())
        .denies("private payment", private_payment())
        .valid_params(serde_json::json!({}))
        .invalid_params(serde_json::json!({ "x": 1 }))
        .run()
        .await
        .assert_conformant();
}
```

The suite serves the module over loopback gRPC with two instances, and checks
`Describe`, params validation on both transports, identical answers in process
and over gRPC, repeated, reordered and concurrent evaluations, the shape of
every decision, the outcome each request is stated to have (`allows`, `denies`,
`denies_with`, `fails`), per-instance tokens and a request at the size cap. It
needs at least one request the module must deny.

## Further reading

- [Policy module guides](https://github.com/OpenZeppelin/guardian/tree/main/docs/guides/policy-modules):
  built-in, remote and custom modules end to end.
- [RFC 0002](https://github.com/OpenZeppelin/guardian/blob/main/docs/rfcs/0002-guardian-modules.md):
  the design, the protocol and the trust model.
