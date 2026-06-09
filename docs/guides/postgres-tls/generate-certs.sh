#!/usr/bin/env bash
# Generate a throwaway CA and a Postgres server certificate for the
# postgres-tls guide. The server cert's SAN is `postgres` — the Compose service
# name the Guardian server connects to — so sslmode=verify-full matches.
#
# Output (this directory): certs/ca.pem, certs/server.crt, certs/server.key
# (gitignored). ca.pem is the trust anchor the server verifies against.
#
# Uses an -extfile for the SAN so it works on both OpenSSL 3.x and the LibreSSL
# that ships on macOS.
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p certs

openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout certs/ca.key -out certs/ca.pem \
  -subj "/CN=Guardian Postgres-TLS Guide CA" -days 3650

printf 'subjectAltName = DNS:postgres\n' > certs/san.cnf

openssl req -newkey rsa:2048 -nodes \
  -keyout certs/server.key -out certs/server.csr \
  -subj "/CN=postgres"

openssl x509 -req -in certs/server.csr \
  -CA certs/ca.pem -CAkey certs/ca.key -CAcreateserial \
  -out certs/server.crt -days 3650 -extfile certs/san.cnf

chmod 600 certs/server.key
rm -f certs/server.csr certs/ca.key certs/ca.srl certs/san.cnf

echo "Wrote certs/ca.pem, certs/server.crt, certs/server.key (SAN=postgres)."
