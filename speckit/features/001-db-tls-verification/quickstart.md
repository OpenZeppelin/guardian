# Quickstart: Verifying Database TLS

Audience: operators configuring Guardian's Postgres connection, and developers
validating the change locally. Behavior is driven entirely by standard libpq
`sslmode` / `sslrootcert` parameters in `DATABASE_URL`.

## Choose a mode

| You want… | Set in `DATABASE_URL` |
|---|---|
| Local dev, no TLS | (omit `sslmode`, or `sslmode=disable`) |
| Encrypted, no verification (legacy) | `sslmode=require` |
| Encrypted + verify the CA chain | `sslmode=verify-ca&sslrootcert=/path/ca.pem` |
| Encrypted + verify chain **and** hostname (recommended) | `sslmode=verify-full&sslrootcert=/path/ca.pem` |

Notes:
- `sslmode=require` **with** `sslrootcert` set is automatically treated as
  `verify-ca` (matches libpq).
- `allow`, `prefer`, and `sslrootcert=system` are **rejected** with an actionable
  error — pick an explicit mode and an explicit CA file.
- **Omitting `sslmode` means no TLS** (normalized to `disable` on both the
  migration and pool paths). If you want encryption/verification you MUST set
  `sslmode` explicitly — Guardian will not silently negotiate TLS.

## AWS RDS (reference deployment)

1. A **combined CA bundle** is mounted into the container at a fixed path at
   **deploy time** by the infrastructure (the published Guardian image is
   provider-neutral and ships no CA). The bundle MUST contain BOTH the Amazon RDS
   CA roots AND the Amazon Trust Services roots — see the RDS Proxy note below.
2. Terraform sets `DATABASE_URL=...&sslmode=verify-full&sslrootcert=<that-path>`.
3. Deploy. Migrations (libpq) and the runtime pools (rustls) both verify against
   the same bundle. **Rotation**: replace the mounted (multi-root) bundle and
   redeploy; ensure the new roots are present before tightening modes. No image or
   code change is needed.

> **RDS Proxy note (important)**: prod defaults to RDS Proxy enabled, and
> `DATABASE_URL` points at the **proxy** endpoint. The RDS Proxy presents an **AWS
> Certificate Manager** certificate that chains to **Amazon Trust Services** roots
> — NOT the Amazon RDS CA roots used by a direct instance. So the trust bundle
> must include BOTH sets of roots; an RDS-CA-only bundle would fail `verify-full`
> against the proxy. Smoke-test against the proxy endpoint (and, if used, the
> direct endpoint) with the combined bundle.

## Other managed providers (GCP / Azure / Supabase / Neon / …)

Same model — download the provider's CA bundle, mount it into the container, and
set `sslmode=verify-full&sslrootcert=<path>`. No Guardian or provider-specific
code path is involved.

## Local verification (developer)

Run a local Postgres with TLS and a self-signed CA:

```bash
# generate a CA + server cert (CN/SAN = localhost), start postgres with ssl=on, then:
DATABASE_URL="postgres://guardian:guardian@localhost:5432/guardian?sslmode=verify-full&sslrootcert=$PWD/ca.pem" \
  cargo run -p guardian-server --features postgres
```

Expected:
- Correct CA + matching hostname → starts, runs migrations, pool ready.
- Wrong/empty CA file → fails fast with a certificate-verification error.
- `sslmode=verify-full` + cert whose SAN ≠ `localhost` → refused.
- Same cert under `sslmode=verify-ca` → accepted (hostname not checked).

## Tests

```bash
# parser/resolver + verifier + redaction + parity unit tests (the TLS suite)
cargo test -p guardian-server --features postgres storage::postgres::tests

# full server test suite under postgres feature (slow: includes proving-heavy tests)
cargo test -p guardian-server --features postgres
```

> The TLS tests live in `storage::postgres::tests`; there is no test whose name
> contains `tls`, so `cargo test … tls` matches nothing. The live-only checks
> (verify-full/verify-ca against a real TLS Postgres) are `#[ignore]`.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| startup error naming `sslmode` | used `allow`/`prefer` or an unknown value → switch to an explicit mode |
| startup error naming `sslrootcert` | missing/unreadable/empty CA file, or `system` (unsupported) → provide a readable CA bundle file |
| connection refused, cert-verification error | wrong CA for the server, expired cert, or (verify-full) hostname mismatch |
| works under verify-ca, fails under verify-full | hostname/SAN does not match the connection endpoint |

See `docs/TROUBLESHOOTING.md` for the full mapping.
