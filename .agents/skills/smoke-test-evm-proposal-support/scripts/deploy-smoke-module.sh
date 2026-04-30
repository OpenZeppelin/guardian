#!/usr/bin/env bash
set -euo pipefail

RPC_URL="${EVM_RPC_URL:-http://127.0.0.1:8545}"
SIGNER_ONE="${EVM_SIGNER_ONE:-0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266}"
SIGNER_TWO="${EVM_SIGNER_TWO:-0x70997970C51812dc3A010C7d01b50e0d17dc79C8}"
THRESHOLD="${EVM_THRESHOLD:-2}"
DEPLOYER="${EVM_DEPLOYER_ADDRESS:-$SIGNER_ONE}"
PRIVATE_KEY="${EVM_PRIVATE_KEY:-}"
ENTRYPOINT_ADDRESS="${EVM_ENTRYPOINT_ADDRESS:-0x433709009b8330fda32311df1c2afa402ed8d009}"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/guardian-evm-module.XXXXXX")"

cleanup() {
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

mkdir -p "$WORKDIR/src"

cat > "$WORKDIR/foundry.toml" <<'EOF'
[profile.default]
src = "src"
out = "out"
libs = []
solc_version = "0.8.24"
optimizer = true
optimizer_runs = 200
evm_version = "cancun"
EOF

cat > "$WORKDIR/src/GuardianEvmSmoke.sol" <<'EOF'
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

contract GuardianEvmSmokeAccount {
    address public immutable validator;

    constructor(address validator_) {
        validator = validator_;
    }

    function isModuleInstalled(
        uint256 moduleTypeId,
        address module,
        bytes calldata
    ) external view returns (bool) {
        return moduleTypeId == 1 && module == validator;
    }
}

contract GuardianEvmSmokeValidator {
    mapping(address => address[]) private accountSigners;
    mapping(address => uint64) private accountThresholds;

    function configure(address account, address signerOne, address signerTwo, uint64 threshold_) external {
        require(account != address(0), "account");
        require(accountSigners[account].length == 0, "configured");
        require(threshold_ > 0 && threshold_ <= 2, "threshold");
        accountSigners[account].push(signerOne);
        accountSigners[account].push(signerTwo);
        accountThresholds[account] = threshold_;
    }

    function getSignerCount(address account) external view returns (uint256) {
        return accountSigners[account].length;
    }

    function getSigners(address account, uint256 start, uint256 end) external view returns (bytes[] memory result) {
        address[] storage signers = accountSigners[account];
        require(start <= end && end <= signers.length, "range");
        result = new bytes[](end - start);
        for (uint256 i = 0; i < result.length; i++) {
            result[i] = abi.encodePacked(signers[start + i]);
        }
    }

    function threshold(address account) external view returns (uint64) {
        return accountThresholds[account];
    }
}

contract GuardianEvmSmokeEntryPoint {
    mapping(address => mapping(uint192 => uint256)) private accountNonces;

    function getNonce(address sender, uint192 key) external view returns (uint256) {
        return accountNonces[sender][key];
    }

    function setNonce(address sender, uint192 key, uint256 nonce) external {
        accountNonces[sender][key] = nonce;
    }
}
EOF

cd "$WORKDIR"

forge build >/dev/null

deploy_contract() {
  local contract="$1"
  shift
  local auth_args=()
  if [ -n "$PRIVATE_KEY" ]; then
    auth_args=(--private-key "$PRIVATE_KEY")
  else
    auth_args=(--unlocked --from "$DEPLOYER")
  fi

  forge create "src/GuardianEvmSmoke.sol:$contract" \
    --rpc-url "$RPC_URL" \
    --broadcast \
    "${auth_args[@]}" \
    --json \
    "$@"
}

extract_address() {
  if command -v jq >/dev/null 2>&1; then
    awk 'BEGIN { emit = 0 } /^\{/ { emit = 1 } emit { print }' | jq -r '.deployedTo // empty' 2>/dev/null || true
  else
    sed -n \
      -e 's/.*"deployedTo"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
      -e 's/^Deployed to:[[:space:]]*\(0x[0-9a-fA-F]*\).*/\1/p' |
      tail -n 1
  fi
}

install_entrypoint_code() {
  local runtime_bytecode
  local existing_code
  runtime_bytecode="$(forge inspect "src/GuardianEvmSmoke.sol:GuardianEvmSmokeEntryPoint" deployedBytecode | tr -d '\n')"
  if [ -z "$runtime_bytecode" ] || [ "$runtime_bytecode" = "0x" ]; then
    printf 'Failed to read mock EntryPoint deployed bytecode\n' >&2
    return 1
  fi

  if cast rpc anvil_setCode "$ENTRYPOINT_ADDRESS" "$runtime_bytecode" --rpc-url "$RPC_URL" >/dev/null 2>&1; then
    printf 'Installed local mock EntryPoint code at %s\n' "$ENTRYPOINT_ADDRESS"
    return 0
  fi

  existing_code="$(cast code "$ENTRYPOINT_ADDRESS" --rpc-url "$RPC_URL" 2>/dev/null || true)"
  if [ -z "$existing_code" ] || [ "$existing_code" = "0x" ]; then
    printf 'EntryPoint address %s has no code and RPC did not accept anvil_setCode\n' "$ENTRYPOINT_ADDRESS" >&2
    return 1
  fi

  printf 'Using existing EntryPoint code at %s\n' "$ENTRYPOINT_ADDRESS"
}

VALIDATOR_OUTPUT="$(deploy_contract GuardianEvmSmokeValidator)"
VALIDATOR_ADDRESS="$(printf '%s\n' "$VALIDATOR_OUTPUT" | extract_address)"

ACCOUNT_OUTPUT="$(deploy_contract GuardianEvmSmokeAccount --constructor-args "$VALIDATOR_ADDRESS")"
ACCOUNT_ADDRESS="$(printf '%s\n' "$ACCOUNT_OUTPUT" | extract_address)"

install_entrypoint_code

send_args=()
if [ -n "$PRIVATE_KEY" ]; then
  send_args=(--private-key "$PRIVATE_KEY")
else
  send_args=(--unlocked --from "$DEPLOYER")
fi

cast send "$VALIDATOR_ADDRESS" \
  "configure(address,address,address,uint64)" \
  "$ACCOUNT_ADDRESS" "$SIGNER_ONE" "$SIGNER_TWO" "$THRESHOLD" \
  --rpc-url "$RPC_URL" \
  "${send_args[@]}" >/dev/null

printf '%s\n' "$VALIDATOR_OUTPUT"
printf '%s\n' "$ACCOUNT_OUTPUT"
printf 'EVM_ACCOUNT_ADDRESS=%s\n' "$ACCOUNT_ADDRESS"
printf 'EVM_VALIDATOR_ADDRESS=%s\n' "$VALIDATOR_ADDRESS"
printf 'EVM_ENTRYPOINT_ADDRESS=%s\n' "$ENTRYPOINT_ADDRESS"
