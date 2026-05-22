# crates/server — Agent notes

See repo root `AGENTS.md` §5 (Server) for canonical guidance. This file is
local context for the `guardian-server` crate.

## Layout

- `src/api/`: thin transport (HTTP + gRPC). Keep handlers small.
- `src/services/`: business logic, one module per operation
  (e.g. `pause_account.rs`, `push_delta.rs`, `sign_delta_proposal.rs`).
  Every service module is also re-exported from `src/services/mod.rs`.
- `src/metadata/`: `MetadataStore` trait + `filesystem` and `postgres` backends.
  Trait changes require updates to **both** backends and `testing/mocks.rs`.
- `src/jobs/canonicalization/`: pending/candidate/canonical/discarded lifecycle.
- `src/network/`: Miden- vs EVM-specific dispatch (keep shared layers
  network-agnostic per AGENTS.md).
- `migrations/`: Postgres-only. Filesystem backend has no schema.

## Adding a new service

1. New file `src/services/<name>.rs` exposing one public function.
2. `mod <name>;` + `pub use <name>::{...};` in `src/services/mod.rs`.
3. HTTP handler in `src/api/http.rs` or a domain-specific module
   (e.g. `dashboard.rs`); for client IP, reuse `extract_client_ip` from
   `src/middleware/rate_limit.rs` rather than reading headers directly.
4. If state-mutating, decide whether it bypasses normal canonicalization
   (e.g. `pause_account` does — it's a flag, not a delta).
5. Mirror in `crates/client` (Rust SDK) and `packages/guardian-client`
   (TS SDK) per AGENTS.md §4.

## Test running

`cargo test -p guardian-server` has hung silently in past sessions. Preferred order:

```bash
# 1. Kill stragglers
pkill -f 'cargo test' || true

# 2. Targeted module first (fast feedback)
cargo test -p guardian-server <module::path>

# 3. Lib tests, skipping integration
cargo test -p guardian-server --lib

# 4. Full crate, only when needed
cargo test -p guardian-server
```

Build with feature combos to catch drift:

```bash
cargo build -p guardian-server --features postgres
cargo build -p guardian-server --features postgres,evm
```

## Binary targets

There is **no** `guardian-server` binary. Targets are:

```bash
cargo run -p guardian-server --bin server
cargo run -p guardian-server --bin ack-keygen
```

## Auth surfaces

There is **no** single auth middleware. Each surface has its own authenticator
— design and review them independently:

| Surface | Authenticator | Where |
|---------|---------------|-------|
| Cosigner gRPC / HTTP | Per-cosigner proof-of-possession against the registered commitment (Falcon or ECDSA) | `src/metadata/auth/` (`credentials.rs`, `lookup.rs`, `miden_ecdsa.rs`, `miden_falcon_rpo.rs`) |
| Operator dashboard | Session cookie | `src/dashboard/middleware.rs` (`require_dashboard_session`) |
| EVM surface (feature-gated) | Challenge / verify / cookie session | `src/api/evm.rs` (`challenge_evm_session`, `verify_evm_session`, `require_evm_session`) |
| Health / public endpoints | Intentionally unauthenticated | — |

When adding a new endpoint, decide which surface it belongs to and reuse that
surface's authenticator. Do not invent a "shared" middleware that spans
surfaces — the trust model is different in each (cosigner credential vs.
operator role vs. EVM-address session).

For deeper guidance on signature schemes and PoP, see the
`guardian-auth-signature-flows` skill (`.agents/skills/guardian-auth-signature-flows/SKILL.md`).
