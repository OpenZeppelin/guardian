# A custom Guardian build: the official server with your module kinds compiled in, on the
# official image's toolchain and base. Build it from the template of a release, in the
# template's directory:
#   docker build -t my-guardian .
# The release's Cargo.lock pins every dependency; --locked refuses to build if it would change.
# The image has no ack-keygen: mint acknowledgement keys with the official image of the release.
ARG MIN_CA_CERTIFICATES_VERSION=20250419~deb12u2

FROM rust:1.98.1-bookworm AS builder
ARG MIN_CA_CERTIFICATES_VERSION

RUN apt-get update && apt-get install -y \
    ca-certificates \
    "protobuf-compiler=3.21.12-*" \
    && dpkg --compare-versions "$(dpkg-query -W -f='${Version}' ca-certificates)" ge "$MIN_CA_CERTIFICATES_VERSION" \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
ENV SOURCE_DATE_EPOCH=0
ENV RUSTFLAGS="--remap-path-prefix /app=. --remap-path-prefix $HOME=~"
ARG GUARDIAN_SERVER_FEATURES=postgres
# The commit in the server's version info. Unset, it is the Guardian release commit, read from
# the git dependency's checkout.
ARG GUARDIAN_GIT_SHA

COPY . .

RUN cargo build --release --locked --bin custom-guardian --features "$GUARDIAN_SERVER_FEATURES"

FROM debian:bookworm-slim@sha256:7e490910eea2861b9664577a96b54ce68ea3e02ce7f51d89cb0103a6f9c386e0
ARG MIN_CA_CERTIFICATES_VERSION

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libpq5 \
    && dpkg --compare-versions "$(dpkg-query -W -f='${Version}' ca-certificates)" ge "$MIN_CA_CERTIFICATES_VERSION" \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/custom-guardian /app/server

EXPOSE 3000 50051

CMD ["/app/server"]
