# Contract: Configuration & Wire Surface

## New environment variables (operator-facing)

| Variable | Values / Format | Default | Required when |
|---|---|---|---|
| `GUARDIAN_ACK_ECDSA_BACKEND` | `in-memory` \| `aws-kms` | `in-memory` (unset) | never (opt-in) |
| `GUARDIAN_ACK_ECDSA_KMS_KEY_ID` | KMS key id, ARN, or alias | — | `GUARDIAN_ACK_ECDSA_BACKEND=aws-kms` |
| `AWS_REGION` | AWS region | — (existing) | any AWS-backed path (already used by Secrets Manager) |

Behavior:
- Unset/`in-memory`: existing ECDSA path (filesystem keystore, or AWS Secrets
  Manager import when `GUARDIAN_ENV=prod`). **No behavior change.** This selector
  is independent of Falcon, which continues to follow `GUARDIAN_ENV`.
- `aws-kms`: requires the key id + region; credentials via the standard AWS chain.
  Startup validates, in order: reachability + `kms:GetPublicKey` + key spec
  `ECC_SECG_P256K1` + sign-capable usage (via `GetPublicKey`), **then a sign probe
  over a fixed validation word** that proves `kms:Sign` and the conversion path
  (since `GetPublicKey` alone does not prove signing permission). Any failure →
  **fail fast** with an actionable, cause-naming error (FR-007).
  Required IAM: `kms:GetPublicKey` **and** `kms:Sign`.
- Unknown `GUARDIAN_ACK_ECDSA_BACKEND` value → fail fast listing supported ids
  (FR-011).

Required IAM (AWS KMS backend): `kms:GetPublicKey` and `kms:Sign` on the key.

## Unchanged wire surface (asserted, not modified)

These existing surfaces are **unchanged** by this feature; listed to make the
no-contract-change explicit (Constitution I/II):

| Surface | Field | Shape | Change |
|---|---|---|---|
| HTTP/gRPC server identity | `pubkey` (ECDSA) | `0x`-prefixed hex of compressed SEC1 (33 bytes) | none |
| HTTP/gRPC server identity | `commitment` (ECDSA) | `0x`-prefixed 32-byte Poseidon2 commitment hex (66 chars) | none |
| Delta ack | `ack_sig` | hex of 65-byte `r\|\|s\|\|v` ECDSA signature | none |

No Rust/TS client or SDK change is required.

## Documentation updates (Constitution V)

- `docs/CONFIGURATION.md` — add the three variables above with semantics.
- `docs/SERVER_AWS_DEPLOY.md` — KMS key provisioning + IAM for production deploys.
- `docs/runbooks/secrets.md` — operator runbook for the KMS-held ack key
  (provisioning, identity-change-on-migration note, rotation is out-of-band).
