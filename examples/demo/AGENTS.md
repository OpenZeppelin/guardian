# examples/demo — Agent notes

See repo root `AGENTS.md` §5 (Examples) and §6 (Fast Validation Matrix). This
is the **CLI smoke harness** (interactive terminal UI via `rustyline`) for
`miden-multisig-client` (Rust). Required manual check alongside
`examples/web` for changes affecting server / Rust client / multisig SDK
behavior.

## Layout

| File | Purpose |
|------|---------|
| `src/main.rs` | Entry point; binary is `guardian-demo` |
| `src/menu.rs` | Interactive menu + input handling (`rustyline`) |
| `src/state.rs` | Session state: GUARDIAN/Miden connections, accounts, keys |
| `src/display.rs` | Print helpers (tables, sections, hex truncation) |
| `src/actions/` | One module per action (create, sign, export, execute, …) |

## Run

```bash
cargo run -p guardian-demo
```

The binary target is `guardian-demo` (not the crate name). At startup it
prompts for Miden RPC and GUARDIAN endpoints; defaults are
`https://rpc.devnet.miden.io` and `http://localhost:50051`.

## Data directory

Each run stores a `miden-client` SQLite store under `~/.guardian-demo`
(configurable from the prompts). To reproduce a stuck-state bug, **delete or
point to a fresh directory** — otherwise prior session data influences the
new run.

```bash
rm -rf ~/.guardian-demo   # nuke local state between repros
```

## Smoke flow (paste into PR notes)

This mirrors `examples/web`'s flow on the CLI side:

1. Generate Falcon keypair; copy your commitment hex.
2. Open a second terminal, run another `cargo run -p guardian-demo`, generate
   that cosigner's commitment.
3. Cosigner A: create 2-of-2 multisig with both commitments; register on
   GUARDIAN.
4. Cosigner B: pull/register account; sign the same proposal A creates.
5. Propose: transfer (P2ID), consume notes, switch GUARDIAN.
6. Export proposal → import in the other terminal → sign offline → re-import.
7. Execute once threshold is met; confirm on Miden explorer.

For an automated harness, prefer the `smoke-test-rust-multisig-sdk` skill —
it drives this demo with the canonical end-to-end sequence.

## Adding a new action

1. New file `src/actions/<name>.rs` exposing one async function callable from
   the menu.
2. Wire it into the menu in `src/menu.rs`.
3. If the action requires new session state, add it to `state.rs`.
4. Display helpers stay in `display.rs` — keep `actions/` modules focused on
   workflow, not printing.

## Conventions

- `guardian-demo` is **not** publishable (`publish = false` in Cargo.toml);
  it's a smoke harness, not a library.
- Keep the demo dependency-light. Adding a heavy dep here slows everyone's
  smoke loop.
- Do not duplicate logic from `miden-multisig-client` — if you find yourself
  rebuilding a flow, surface the missing API on the SDK instead.
