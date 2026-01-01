# Build stage
# For reproducible builds across machines, specify --platform:
#   docker build --platform linux/amd64 ...
FROM rust:1.92.0-bookworm@sha256:3d0d1a335e1d1220d416a1f38f29925d40ec9929d3c83e07a263adf30a7e4aa3 as builder

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
RUN cargo build --release --package private-state-manager-server --bin server

# Runtime stage
FROM debian:bookworm-slim@sha256:d5d3f9c23164ea16f31852f95bd5959aad1c5e854332fe00f7b3a20fcc9f635c

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the binary from builder
COPY --from=builder /app/target/release/server /app/server

# Expose HTTP and gRPC ports
EXPOSE 3000 50051

CMD ["/app/server"]
