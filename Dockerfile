# Build stage
FROM rust:1.88 as builder

WORKDIR /app

# Copy workspace manifests
COPY Cargo.toml Cargo.lock ./

# Copy all crates
COPY crates ./crates

# Build for release (only server)
RUN cargo build --release --package private-state-manager-server --bin server

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the binary from builder
COPY --from=builder /app/target/release/server /app/server

EXPOSE 3000

CMD ["/app/server"]
