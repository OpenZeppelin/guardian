# Build stage
# For reproducible builds across machines, specify --platform:
#   docker build --platform linux/amd64 ...
FROM rust:1.93.1-bookworm@sha256:7c4ae649a84014c467d79319bbf17ce2632ae8b8be123ac2fb2ea5be46823f31 as builder

# Install protobuf compiler (pinned to specific version)
RUN apt-get update && apt-get install -y \
    protobuf-compiler=3.21.12-3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Set environment variables for reproducible builds
ENV SOURCE_DATE_EPOCH=0
ENV RUSTFLAGS="--remap-path-prefix /app=. --remap-path-prefix $HOME=~"

# Copy workspace manifests
COPY Cargo.toml Cargo.lock ./
COPY rust-toolchain.toml ./

COPY crates ./crates
COPY examples ./examples

# Build for release (only server)
RUN cargo build --release --package private-state-manager-server --bin server --features postgres

# Runtime stage
FROM debian:bookworm-slim@sha256:74d56e3931e0d5a1dd51f8c8a2466d21de84a271cd3b5a733b803aa91abf4421

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libpq5 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the binary from builder
COPY --from=builder /app/target/release/server /app/server

# Expose HTTP and gRPC ports
EXPOSE 3000 50051

CMD ["/app/server"]
