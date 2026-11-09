# Policy module example

`basic-policy`, the Guardian's built-in policy kind, served as its own process
over gRPC through the `serve` harness of
[`guardian-module-sdk`](../../crates/guardian-module-sdk). Use it to try a
remote module, or as the starting point for one: replace `BasicPolicy` with
your own `Module`.

The Guardian side is a catalog entry of kind `grpc`; see the
[remote module guide](../../docs/guides/policy-modules/remote.md) and its
catalog, [`remote.toml`](../../docs/guides/policy-modules/remote.toml).

## Run it

Every instance the Guardian calls has an id (the catalog `id`) and its own
bearer token. The module accepts a call only for an instance it was started
with, carrying that instance's token.

```sh
mkdir -p .guardian
(umask 077; openssl rand -hex 32 > .guardian/caps.token)
cargo run -p guardian-policy-module -- \
  --listen 127.0.0.1:7443 --instance caps-remote .guardian/caps.token
```

Repeat `--instance ID TOKEN_FILE` to host several instances in one process, for
example one per list. The Guardian reads the same token through its secrets
backend: `auth = { token_secret = "caps.token" }` with the default `file`
provider and the catalog in `.guardian/`.

## TLS

Without TLS the module listens in plain text, which the Guardian accepts only on
a loopback address (`grpc+insecure://127.0.0.1:7443`) and never in the prod
stage. The module refuses a plain-text listener on any other address unless
`--insecure` is given.

```sh
policy-module --listen 0.0.0.0:7443 \
  --instance caps-remote /run/secrets/caps.token \
  --tls-cert /run/secrets/module.pem --tls-key /run/secrets/module.key
```

On the Guardian, `endpoint = "grpc://<host>:7443"` and, for a private CA,
`tls = { ca_file = "module-ca.pem" }`. The certificate must name the host in
the endpoint and must not be a CA certificate itself.

For mutual TLS add `--client-ca CA_PEM`: the module then refuses a connection
without a client certificate issued by that CA. The Guardian presents its
certificate and key from `tls.client_cert_secret` and `tls.client_key_secret`.

## Docker

Build from the repository root:

```sh
docker build -f examples/policy-module/Dockerfile -t guardian-policy-module .
docker run --rm -p 7443:7443 -v "$PWD/secrets:/run/secrets:ro" guardian-policy-module \
  --listen 0.0.0.0:7443 \
  --instance caps-remote /run/secrets/caps.token \
  --tls-cert /run/secrets/module.pem --tls-key /run/secrets/module.key
```

The container runs as an unprivileged user (uid 10001), so the mounted files
must be readable by it.

## Check a module before deploying it

The SDK's conformance suite runs a module in process and over gRPC and checks
that both give the same answers, that `evaluate` has no side effects, and that
a request at the Guardian's size cap goes through. `basic-policy` runs it in
[`crates/guardian-module-sdk/tests/conformance.rs`](../../crates/guardian-module-sdk/tests/conformance.rs);
the same `Suite` works for your own module with the `conformance` feature.
