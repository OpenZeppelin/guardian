# Build stage
# For reproducible builds across machines, specify --platform:
#   docker build --platform linux/amd64 ...
FROM rust:1.93.0-bookworm as base-builder

# Install protobuf compiler (pinned to specific version)
RUN apt-get update && apt-get install -y \
    protobuf-compiler=3.21.12-3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Set environment variables for reproducible builds
ENV SOURCE_DATE_EPOCH=0
ENV RUSTFLAGS="--remap-path-prefix /app=. --remap-path-prefix $HOME=~"
ARG GUARDIAN_SERVER_FEATURES=postgres

# Copy workspace manifests
COPY Cargo.toml Cargo.lock ./
COPY rust-toolchain.toml ./

COPY crates ./crates
COPY benchmarks ./benchmarks
COPY examples ./examples

# Build for release (only server)
FROM base-builder as server-builder

RUN if [ -n "$GUARDIAN_SERVER_FEATURES" ]; then \
      cargo build --release --package guardian-server --bin server --features "$GUARDIAN_SERVER_FEATURES"; \
    else \
      cargo build --release --package guardian-server --bin server; \
    fi

FROM base-builder as benchmark-builder

RUN cargo build --release --package guardian-prod-benchmarks --bin guardian-prod-benchmarks

# Runtime stage
FROM debian:bookworm-slim@sha256:7e490910eea2861b9664577a96b54ce68ea3e02ce7f51d89cb0103a6f9c386e0 as benchmark-runner

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=benchmark-builder /app/target/release/guardian-prod-benchmarks /app/guardian-prod-benchmarks
COPY --from=benchmark-builder /app/crates/contracts/masm /app/crates/contracts/masm

ENTRYPOINT ["/app/guardian-prod-benchmarks"]

# Runtime stage
FROM debian:bookworm-slim@sha256:7e490910eea2861b9664577a96b54ce68ea3e02ce7f51d89cb0103a6f9c386e0

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libpq5 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the binary from builder
COPY --from=server-builder /app/target/release/server /app/server

# Expose HTTP and gRPC ports
EXPOSE 3000 50051

CMD ["/app/server"]
