FROM rust:1.93-slim AS build

ARG MIDEN_REMOTE_PROVER_VERSION=0.15.2

RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Pinned rather than floating: the prover and the client must agree on the
# miden-tx line, and 0.15.2 is what miden-client 0.15.0 speaks. A newer prover
# silently fails to prove for a 0.15 client.
RUN cargo install miden-remote-prover \
    --version ${MIDEN_REMOTE_PROVER_VERSION} \
    --locked \
    --root /out

# Must match the builder's Debian release: the rust:1.93 images are trixie
# (glibc 2.41), and a bookworm runtime (2.36) fails at exec with
# "GLIBC_2.38 not found".
FROM debian:trixie-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /out/bin/miden-remote-prover /usr/local/bin/miden-remote-prover

EXPOSE 50051

ENTRYPOINT ["miden-remote-prover"]
