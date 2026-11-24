# Custom Guardian build

The official Guardian server with your own policy module kinds compiled in.
Everything the official `server` binary configures from the environment
(`ServerBuilder::from_env`) is unchanged; `main.rs` adds one
`with_module_kind` call per kind. The example registers `note-policy`, the
module from the [custom modules guide](../../docs/guides/policy-modules/custom.md).

An in-process kind runs inside the process that holds the acknowledgement keys,
storage credentials and every account's state. Register only code you wrote or
audited, and prefer a [remote module](../../docs/guides/policy-modules/remote.md)
for anything else: it needs no custom build and can only refuse.

## Start from a release

Each Guardian release attaches `custom-guardian-<tag>.tar.gz`, this directory
turned into a standalone project:

- `Cargo.toml` depends on `guardian-server` and `guardian-module-sdk` at the
  release tag.
- `Cargo.lock` is the release's own, so every Miden and Plonky3 crate is the
  version the release was tested with. A freshly resolved lockfile can pick
  versions that build but break transaction execution, so always build with
  `--locked`.
- `Dockerfile` builds on the official image's toolchain and base, from the
  whole template directory, so a module crate you keep beside `src` (a path
  dependency such as `modules/acme`) is part of the build. It has no
  `ack-keygen`: mint acknowledgement keys with the official image of the same
  release.
- `rust-toolchain.toml` pins the release's compiler.

```sh
tar xzf custom-guardian-v0.19.0.tar.gz && cd custom-guardian
# add your crate to [dependencies], register its kind in src/main.rs
cargo build --release --locked --features postgres
docker build -t my-guardian .
```

Adding a crate makes `--locked` refuse the build until the lockfile has it.
Run `cargo update --workspace`, which adds the new crates and keeps every
pinned version, then build with `--locked` again; `git diff Cargo.lock` (or a
diff against the archive's copy) should show only your crates. A plain
`cargo update` moves the pinned Guardian dependencies too.

A module crate published to crates.io depends on the crates.io
`guardian-module-sdk`. The template's `[patch.crates-io]` replaces it with the
release's copy, so its factory registers; the crate must accept the release's
SDK version.

The server's version info names the Guardian release commit. Pass
`--build-arg GUARDIAN_GIT_SHA=<commit>` to stamp your own build's commit
instead.

On each Guardian release, start again from that release's template, then
rebuild, sign and redeploy your image. The official image's provenance
attestation does not cover your build.

## Register a kind

```rust
ServerBuilder::from_env()
    .await
    .expect("Failed to configure server")
    .with_module_kind("acme-sanctions", Arc::new(acme_sanctions::Factory))
    .build()
    .await
    .expect("Failed to build server")
    .start()
    .await
    .unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(1)
    });
```

The catalog (`GUARDIAN_MODULES_CONFIG`) then names it with
`kind = "acme-sanctions"`. `build` fails if the name is taken, including by a
built-in policy (`spending-limit`, `recipient-policy`, `signer-policy`,
`code-policy`) or `grpc`, and the server refuses to start with a catalog that
enables an instance of a kind it does not register. Builder methods called
after `from_env` replace what the environment set, but `from_env` has already
opened storage and the acknowledgement keys, and block watch follows the
environment's release sweep.

## In this repository

The example builds against the working tree:

```sh
cargo run -p custom-guardian
cargo build --release -p custom-guardian --features postgres
```

`scripts/custom-guardian-template.sh` turns it into the release template; the
`Custom Guardian Template` workflow runs it on every release and attaches the
result.
