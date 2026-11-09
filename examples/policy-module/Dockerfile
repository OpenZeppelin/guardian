# basic-policy as a standalone Guardian policy module. Build from the repository root:
#   docker build -f examples/policy-module/Dockerfile -t guardian-policy-module .
FROM rust:1.98.1-bookworm AS builder

RUN apt-get update && apt-get install -y "protobuf-compiler=3.21.12-*" \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY benchmarks ./benchmarks
COPY examples ./examples
RUN cargo build --release --locked --package guardian-policy-module

FROM debian:bookworm-slim@sha256:7e490910eea2861b9664577a96b54ce68ea3e02ce7f51d89cb0103a6f9c386e0

RUN useradd --system --no-create-home --uid 10001 module
COPY --from=builder /app/target/release/policy-module /usr/local/bin/policy-module
USER module
EXPOSE 7443
ENTRYPOINT ["/usr/local/bin/policy-module"]
