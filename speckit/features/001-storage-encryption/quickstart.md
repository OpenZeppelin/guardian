# Quickstart: Storage Encryption at Rest

Encryption is **opt-in by key-source presence** — configuring a key turns it on;
with no key configured Guardian behaves exactly as before (SC-008). There is no
separate enable flag. The steps below configure a key.

## Development (filesystem or local Postgres)

1. Generate a base64-encoded 32-byte key:

   ```bash
   openssl rand -base64 32
   ```

2. Configure the key (this is what enables encryption):

   ```bash
   export GUARDIAN_STORAGE_ENCRYPTION_KEY=<base64-32-bytes>
   ```

   That single variable is all you need. (`GUARDIAN_STORAGE_ENCRYPTION_KEY_ID` is
   an optional id label for this key, defaulting to `k1` — leave it unset unless
   you have a reason to pin a specific id.)

3. Start the server. On a fresh/empty store, every state, delta, and proposal is
   written encrypted.

4. Verify ciphertext at rest:
   - **Filesystem**: open a state/delta JSON file under `GUARDIAN_STORAGE_PATH`;
     the payload field is an envelope (`{"v":1,"alg":"AES-256-GCM",...}`), while
     `account_id`/`nonce`/commitments remain readable.
   - **Postgres**: `SELECT state_json FROM states LIMIT 1;` returns an envelope
     object, not the original account state.

   Reading the same records back through Guardian returns the original objects.

## Production (Postgres + AWS Secrets Manager)

1. Create the storage encryption key secret as a **structured document** (an
   active `kid` plus a `kid → base64-32-byte-key` map, so rotation just adds
   entries):

   ```bash
   aws secretsmanager create-secret \
     --name guardian-prod/server/storage-encryption-key \
     --secret-string "$(jq -nc --arg k "$(openssl rand -base64 32)" \
       '{active:"k1", keys:{k1:$k}}')"
   ```

   (Grant the Guardian task role `secretsmanager:GetSecretValue` on this secret —
   same IAM pattern as the ACK key secrets.)

2. Configure the server (setting the secret id is what enables encryption):

   ```bash
   export GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID=guardian-prod/server/storage-encryption-key
   # AWS_REGION is already required for ACK keys and is reused here.
   ```

3. **Enable against an empty store.** The recommended window is the Miden 0.15
   cutover, which truncates account data — so encryption applies from the first
   write with no backfill. Do **not** configure a key for a store that already
   holds plaintext records: that requires an explicit re-encryption migration
   (out of scope). The store's encryption-state marker enforces this — startup
   fails fast on a state mismatch rather than mixing plaintext and ciphertext
   (FR-015).

4. Startup is fail-fast: if a key source is configured but the key cannot be
   resolved, is malformed, or is not 32 bytes — or if more than one key source
   is configured — the server refuses to start (FR-009) rather than writing
   plaintext.

## Verifying the behavior (tests)

- Cipher unit tests: encrypt→decrypt roundtrip; tampered ciphertext rejected;
  wrong key rejected; AAD/identity mismatch rejected; unknown `kid` rejected.
- Decorator integration test (filesystem backend): write via `EncryptedStorage`,
  read raw file → envelope present, no plaintext; read back via decorator →
  original object. Includes a proposal (AAD bound to `commitment`).
- Parity test: service-layer reads are identical with a key configured vs not
  (SC-004).
- Startup tests: configured-but-invalid/short key → error; more than one key
  source → error; marker guard (FR-015) → error for: key absent but an
  encryption marker is present (any record count); key configured against a
  non-empty store with no marker; provider missing the marker's `init_kid`.
- Service-path fail-closed test: a decryption failure on the proposal read path
  propagates as a failed request, not an empty list (FR-010, R9).

## Configuration reference

| Variable | Default | Purpose |
|---|---|---|
| `GUARDIAN_STORAGE_ENCRYPTION_KEY` | — | Dev key source: base64-encoded 32-byte key (presence enables encryption) |
| `GUARDIAN_STORAGE_ENCRYPTION_KEY_ID` | `k1` (literal) | Dev: `kid` label for the dev key (optional) |
| `GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID` | — | Prod key source: Secrets Manager secret id, structured `{active,keys}` (presence enables encryption) |
| `AWS_REGION` | — | Prod: region for Secrets Manager (shared with ACK) |

Enablement is inferred from key-source presence: none → off; exactly one → on;
more than one → startup error. There is no enable/disable flag.
