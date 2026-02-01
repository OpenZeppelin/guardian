# Build stage
# For reproducible builds across machines, specify --platform:
#   docker build --platform linux/amd64 ...
FROM rust:1.93.0-bookworm@sha256:812df42b4a866cf7165934691a0a89061281679a145b857dc679be8132e709b9 as builder

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
FROM debian:bookworm-slim@sha256:56ff6d36d4eb3db13a741b342ec466f121480b5edded42e4b7ee850ce7a418ee

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
