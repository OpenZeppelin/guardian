#!/usr/bin/env bash
# Registers an undeployed account with a Miden node so the node's funding
# service pays it a public P2ID note in the fee asset (0.01 USDCx on devnet).
#
# The miden-client wrapper (Client::register_account) skips the RPC when the
# node reports the account as already allowed, which is always the case on a
# node without allowlist enforcement (devnet). This script calls the RPC
# directly instead.
#
# Usage: scripts/devnet-register-account.sh <account-id-hex | bech32-address> [invitation-code]
# Env:   MIDEN_RPC (default rpc.devnet.miden.io:443)
#
# Write RPCs require an accept header naming the node's genesis commitment;
# the script reads it (and the node version) from the node's Status RPC.
#
# After it succeeds, sync the client until the note shows up; the account's
# first transaction consumes it, creates the account on chain and pays its fee.

set -euo pipefail

if [[ $# -lt 1 ]]; then
  sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'
  exit 64
fi

command -v grpcurl >/dev/null || { echo "grpcurl is required (brew install grpcurl)" >&2; exit 69; }

rpc="${MIDEN_RPC:-rpc.devnet.miden.io:443}"
code="${2:-guardian}"

read -r id_hex prefix suffix <<<"$(python3 - "$1" <<'PY'
import sys

value = sys.argv[1].strip()
if value[:2].lower() == "0x":
    raw = value[2:].lower()
    if len(raw) != 30 or any(c not in "0123456789abcdef" for c in raw):
        sys.exit("account id must be 15 bytes of hex")
    ident = bytes.fromhex(raw)
else:
    charset = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"
    hrp, _, data = value.lower().rpartition("1")
    if not hrp or len(data) < 7 or any(c not in charset for c in data):
        sys.exit("not a hex account id or bech32 address")
    acc = bits = 0
    out = bytearray()
    for c in data[:-6]:
        acc = (acc << 5) | charset.index(c)
        bits += 5
        if bits >= 8:
            bits -= 8
            out.append((acc >> bits) & 0xFF)
    if len(out) < 16:
        sys.exit("address does not carry an account id")
    ident = bytes(out[1:16])
print(ident.hex(), int.from_bytes(ident[:8], "big"), int.from_bytes(ident[8:] + b"\0", "big"))

PY
)"
[[ -n "${suffix:-}" ]] || exit 65

account="{\"v1\":{\"prefix\":{\"value\":\"${prefix}\"},\"suffix\":{\"value\":\"${suffix}\"}}}"

read -r version genesis <<<"$(grpcurl -max-time 30 -d '{}' "${rpc}" rpc.Api/Status | python3 -c '
import base64, json, sys
status = json.load(sys.stdin)
print(status["version"], "0x" + base64.b64decode(status["genesisCommitment"]["encoded"]).hex())
')"
[[ -n "${genesis:-}" ]] || { echo "could not read the node status" >&2; exit 69; }
accept="accept: application/vnd.miden; version=${version}; genesis=${genesis}"

echo "node:    ${rpc} (${version}, genesis ${genesis})"
echo "account: 0x${id_hex}"

echo "allowed before:"
grpcurl -max-time 30 -d "{\"account_id\":${account}}" "${rpc}" rpc.Api/IsAccountAllowed

echo "register:"
grpcurl -max-time 60 -H "${accept}" -d "{\"invitation_code\":\"${code}\",\"account_id\":${account}}" "${rpc}" rpc.Api/RegisterAccount
