# Build stage
# For reproducible builds across machines, specify --platform:
#   docker build --platform linux/amd64 ...
FROM rust:1.94.1-bookworm@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55 as base-builder

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
FROM debian:bookworm-slim@sha256:4724b8cc51e33e398f0e2e15e18d5ec2851ff0c2280647e1310bc1642182655d as benchmark-runner

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=benchmark-builder /app/target/release/guardian-prod-benchmarks /app/guardian-prod-benchmarks
COPY --from=benchmark-builder /app/crates/contracts/masm /app/crates/contracts/masm

ENTRYPOINT ["/app/guardian-prod-benchmarks"]

# Runtime stage
FROM debian:bookworm-slim@sha256:4724b8cc51e33e398f0e2e15e18d5ec2851ff0c2280647e1310bc1642182655d

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
