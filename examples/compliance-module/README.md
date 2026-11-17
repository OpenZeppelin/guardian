# Compliance module example

A KYC and sanctions policy for the Guardian, served as its own process over
gRPC with [`guardian-module-sdk`](../../crates/guardian-module-sdk):

- every signer of an account must be KYC-verified: at registration, and on
  every transaction with `signers = "all"` (only the signers a transaction adds
  with `signers = "added"`);
- a payment may not go to a sanctioned account (blocklist), or may go only to
  a verified one (allowlist), checking each payment note's target and its
  reclaimer.

The lists come from a JSON file read at startup
([`lists.example.json`](./lists.example.json)) to keep the example
self-contained; a real module would read them from its own database, a
screening vendor or an on-chain registry, depending on its requirements (see
the guide's "Using your own data"). Each catalog instance's params choose which
checks apply. The walkthrough, catalog and production notes are
in the [compliance guide](../../docs/guides/policy-modules/compliance.md).

```sh
mkdir -p .guardian
(umask 077; openssl rand -hex 32 > .guardian/compliance.token)
cp examples/compliance-module/lists.example.json .guardian/compliance-lists.json
cargo run -p guardian-compliance-module -- \
  --data .guardian/compliance-lists.json --listen 127.0.0.1:7445 \
  --instance compliance .guardian/compliance.token
```

Flags: `--data FILE` (required), `--listen ADDR` (default `127.0.0.1:7445`),
`--instance ID TOKEN_FILE` (repeatable, at least one), `--tls-cert PEM
--tls-key PEM [--client-ca PEM]`, and `--insecure` to allow plain text on a
non-loopback address.

```sh
cargo test -p guardian-compliance-module
```

runs the conformance suite over every rule.
