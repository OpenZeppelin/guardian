# Quickstart: Memory-Resident Secret Hygiene

**Feature**: `008-zeroize-secrets`
**Audience**: developers adding code that touches secrets in `crates/server`; reviewers auditing the secret surface.

---

## TL;DR for new code

If you are adding a field that holds a long-lived secret value, do these three things:

1. **Pick a wrapper** from `crates/server/src/secret/`:
   - 32-byte (or other fixed-size) symmetric key → `FixedKey<N>`
   - opaque variable-length bytes → `SecretBytes`
   - secret string (token, password) → `SecretString`
   - URL that may embed credentials (userinfo or query) → `CredentialUrl`
2. **Construct via `Wrapper::new(...)`** at the point where the bytes first arrive from the environment / disk / network. **For env vars (FR-012)**: fold the `std::env::var(...)` call and the wrapper constructor into a *single expression* — no intermediate `String` local, no `let raw = env::var(...)?;` two-step.

   ```rust
   // ✅ Right
   let db_url = CredentialUrl::new(std::env::var("DATABASE_URL")?);

   // ❌ Wrong — `raw` lives un-zeroized on the heap
   let raw = std::env::var("DATABASE_URL")?;
   let db_url = CredentialUrl::new(raw);
   ```
3. **Read via `.expose_secret()` only at the single point of use** (sign, HMAC, build pool, etc.). Drop the wrapper as soon as possible.

> **Note on env-var protection limits**: this feature protects the Rust-side destination of an env-var read. It does **not** protect the OS process environment block (`/proc/<pid>/environ`, coredumps, fork-inherited env, ECS task-definition env). For values that must not appear in the env block at all, use the AWS Secrets Manager runtime-fetch pattern (see `ack/secrets_manager.rs`), which is already how production Falcon/ECDSA keys are loaded.

Do **not**:
- Log or `{}`-format the wrapper. The compiler will refuse (`Display` is not implemented).
- Serialize the wrapper. The compiler will refuse (`Serialize` is not implemented).
- Add `Serialize` to an enclosing struct that holds a wrapper. The derive will fail to compile.
- Compare a wrapper byte-by-byte against untrusted input with `==`. Use `secret::ct::eq` instead.

---

## Running the tests for this feature

```bash
cargo test -p guardian-server --lib secret
cargo test -p guardian-server --lib dashboard::cursor
cargo test -p guardian-server --lib dashboard::state
cargo test -p guardian-server --lib evm::session
cargo test -p miden-keystore
```

The compile-time `static_assertions::assert_not_impl_any!` checks run on every `cargo build` — no special step required.

---

## Verifying the security posture (audit recipe)

A reviewer can confirm the feature is intact in ~30 seconds:

```bash
# 1. Enumerate every legitimate exposure of a wrapped secret.
rg -n 'expose_secret\(' crates/server/src crates/miden-keystore/src

# 2. Confirm no wrapper crosses the serialization boundary.
rg -nE '#\[derive\([^)]*Serialize' crates/server/src | rg -i 'secret|credential|fixed_?key'
# Expected: zero hits.

# 3. Confirm no Display impl on any wrapper.
rg -nE 'impl\s+(std::fmt::)?Display\s+for\s+(FixedKey|SecretBytes|SecretString|CredentialUrl)' crates/server/src
# Expected: zero hits.

# 4. Enumerate constant-time equality sites.
rg -n 'secret::ct::eq|secret::ct_eq|ct::eq\(' crates/server/src

# 5. Confirm every secret-bearing env-var read wraps in a single expression (FR-012).
rg -n 'env::var\("(DATABASE_URL|GUARDIAN_DASHBOARD_CURSOR_SECRET|GUARDIAN_EVM_RPC_URLS)"\)' crates/server/src
# Each hit should be on a line that also contains `CredentialUrl::new(` / `FixedKey::<32>::new(` /
# similar — no intermediate `let` binding between env::var and the wrapper constructor.

# 6. Confirm no one introduced `unsafe { env::remove_var(...) }` to "wipe" the env block.
rg -n 'unsafe\s*\{\s*std::env::(remove_var|set_var)' crates/server/src crates/miden-keystore/src
# Expected: zero hits outside of test setup/teardown code.
```

This grep recipe plus reviewer attention is the FR-007 review process. The compile-time `assert_not_impl_any!` /
`assert_impl_all!` checks in `secret/tests.rs` are the hard enforcement.

### Baseline counts (at landing)

| Check | Expected count |
|---|---|
| `expose_secret(` call sites across `crates/server` + `crates/miden-keystore` | ~35 |
| `impl Display for <wrapper>` | 0 |
| `#[derive(Serialize)]` on a struct that contains a wrapper | 0 |
| `secret::ct::eq` call sites (tests only — no production callers in this feature) | ~11 |
| `unsafe { std::env::(remove_var\|set_var) }` outside test scaffolding | 0 (all hits are in `EnvVarGuard` test helpers and `network::mod::tests`) |

A meaningful drift in any of the first three is worth investigating.

If step (1) returns more results than the implementation tracker expects, or steps (2)–(3) return any results, something has regressed.

---

## Adding a new in-scope secret site

If a future change introduces a new long-lived secret (e.g. a future bearer-token cache, an in-memory KMS handle that holds raw material):

1. **Amend `spec.md`'s "In-Scope Inventory"** to add the site by name. The spec is the canonical scope.
2. **Pick a wrapper** per the TL;DR above.
3. **Update `data-model.md`'s site mapping table**.
4. **Add the site to the audit recipe expected counts** (if a follow-on tracker exists).
5. **Re-run the audit recipe above** and update the expected counts if the new site adds an `expose_secret()` call or another reviewed secret-bearing field.

If the new secret needs to cross a serialization boundary (e.g. it must be persisted to disk encrypted-at-rest), this feature is the wrong layer — file a separate spec because the disclosure model is different.

---

## Removing a secret site

If a secret becomes unnecessary (e.g. a session-token store is replaced by a JWT scheme):

1. Remove the field and the surrounding code.
2. Remove the site from `data-model.md`'s mapping table.
3. Update `spec.md`'s In-Scope Inventory.
4. Leave the wrapper types in the `secret` module — they are reusable.

---

## Local development tips

- `tracing::info!(?cfg, ...)` will compile and render redacted output for fields that are wrappers; for fields that are plain `String`/`Vec<u8>`, the value renders verbatim. So if you see a real secret in logs after this feature lands, the field is the bug — not the logging call.
- The `Debug` impl includes a length where it is safe to (e.g. `SecretBytes(len=32)`). This is intentional and useful for debugging without leaking content.
- `CredentialUrl::scheme_and_host()` is the right thing to log when you want to know *which* RPC endpoint or DB host you connected to. Never log the full URL.

---

## When to break glass

If you genuinely need the raw secret value (signing a payload, computing an HMAC, constructing a connection pool):

```rust
let pool = build_pool(database_url.expose_secret())?;
```

That single call is the audit-visible exposure. Reviewers grep for `expose_secret(` and read each site. Keep these call sites few and obvious.

If you find yourself wanting the raw secret in more than one place, consider whether the work that needs the secret can be moved closer to the wrapper construction (smaller exposure window) or expressed as a method on the wrapper itself.
