#!/usr/bin/env bash
set -euo pipefail

RPC_URL="${EVM_RPC_URL:-http://127.0.0.1:8545}"
ACCOUNT_ADDRESS="${EVM_ACCOUNT_ADDRESS:-0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC}"
SIGNER_ONE="${EVM_SIGNER_ONE:-0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266}"
SIGNER_TWO="${EVM_SIGNER_TWO:-0x70997970C51812dc3A010C7d01b50e0d17dc79C8}"
THRESHOLD="${EVM_THRESHOLD:-2}"
DEPLOYER="${EVM_DEPLOYER_ADDRESS:-$SIGNER_ONE}"
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

cat > "$WORKDIR/src/GuardianEvmSmokeModule.sol" <<'EOF'
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

contract GuardianEvmSmokeModule {
    bytes32 private constant EIP712_DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
    bytes32 private constant GUARDIAN_PROPOSAL_TYPEHASH =
        keccak256("GuardianProposal(bytes32 mode,bytes32 execution_calldata_hash)");
    bytes32 private constant NAME_HASH = keccak256("Guardian EVM Proposal");
    bytes32 private constant VERSION_HASH = keccak256("1");

    mapping(address => address[]) private accountSigners;
    mapping(address => mapping(address => bool)) private signerSet;
    mapping(address => uint64) private accountThresholds;

    mapping(bytes32 => bool) public submitted;
    mapping(bytes32 => uint256) public submittedSignatureCounts;

    event ProposalSubmitted(
        address indexed account,
        bytes32 indexed proposalId,
        bytes32 mode,
        bytes executionCalldata,
        uint256 signatureCount
    );

    constructor(address account, address signerOne, address signerTwo, uint64 threshold_) {
        require(account != address(0), "account");
        require(threshold_ > 0, "threshold");
        _addSigner(account, signerOne);
        _addSigner(account, signerTwo);
        require(threshold_ <= accountSigners[account].length, "unreachable");
        accountThresholds[account] = threshold_;
    }

    function getSignerCount(address account) external view returns (uint256) {
        return accountSigners[account].length;
    }

    function getSigners(
        address account,
        uint256 start,
        uint256 end
    ) external view returns (bytes[] memory result) {
        address[] storage signers = accountSigners[account];
        require(start <= end && end <= signers.length, "range");
        result = new bytes[](end - start);
        for (uint256 i = 0; i < result.length; i++) {
            result[i] = abi.encodePacked(signers[start + i]);
        }
    }

    function isSigner(address account, bytes calldata signer) external view returns (bool) {
        address decoded = _addressFromBytes(signer);
        return decoded != address(0) && signerSet[account][decoded];
    }

    function threshold(address account) external view returns (uint64) {
        return accountThresholds[account];
    }

    function submitProposal(
        address account,
        bytes32 mode,
        bytes calldata executionCalldata,
        bytes[] calldata signatures
    ) external returns (bytes32 proposalId) {
        bytes32 executionCalldataHash = keccak256(executionCalldata);
        proposalId = keccak256(abi.encode(block.chainid, account, mode, executionCalldataHash));
        require(!submitted[proposalId], "submitted");

        bytes32 digest = _proposalDigest(account, mode, executionCalldataHash);
        address[] memory seen = new address[](signatures.length);
        uint256 validCount = 0;
        for (uint256 i = 0; i < signatures.length; i++) {
            address signer = _recover(digest, signatures[i]);
            require(signerSet[account][signer], "not signer");
            for (uint256 j = 0; j < validCount; j++) {
                require(seen[j] != signer, "duplicate");
            }
            seen[validCount] = signer;
            validCount++;
        }

        require(validCount >= accountThresholds[account], "threshold");
        submitted[proposalId] = true;
        submittedSignatureCounts[proposalId] = validCount;
        emit ProposalSubmitted(account, proposalId, mode, executionCalldata, validCount);
    }

    function _addSigner(address account, address signer) private {
        require(signer != address(0), "signer");
        require(!signerSet[account][signer], "duplicate signer");
        signerSet[account][signer] = true;
        accountSigners[account].push(signer);
    }

    function _proposalDigest(
        address account,
        bytes32 mode,
        bytes32 executionCalldataHash
    ) private view returns (bytes32) {
        bytes32 domainSeparator = keccak256(
            abi.encode(EIP712_DOMAIN_TYPEHASH, NAME_HASH, VERSION_HASH, block.chainid, account)
        );
        bytes32 structHash = keccak256(
            abi.encode(GUARDIAN_PROPOSAL_TYPEHASH, mode, executionCalldataHash)
        );
        return keccak256(abi.encodePacked("\x19\x01", domainSeparator, structHash));
    }

    function _recover(bytes32 digest, bytes calldata signature) private pure returns (address) {
        require(signature.length == 65, "signature length");
        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly {
            r := calldataload(signature.offset)
            s := calldataload(add(signature.offset, 32))
            v := byte(0, calldataload(add(signature.offset, 64)))
        }
        if (v < 27) {
            v += 27;
        }
        require(v == 27 || v == 28, "signature v");
        address recovered = ecrecover(digest, v, r, s);
        require(recovered != address(0), "recover");
        return recovered;
    }

    function _addressFromBytes(bytes calldata value) private pure returns (address decoded) {
        if (value.length != 20) {
            return address(0);
        }
        assembly {
            decoded := shr(96, calldataload(value.offset))
        }
    }
}
EOF

cd "$WORKDIR"
OUTPUT="$(
  forge create src/GuardianEvmSmokeModule.sol:GuardianEvmSmokeModule \
    --rpc-url "$RPC_URL" \
    --unlocked \
    --broadcast \
    --from "$DEPLOYER" \
    --constructor-args "$ACCOUNT_ADDRESS" "$SIGNER_ONE" "$SIGNER_TWO" "$THRESHOLD" \
    --json
)"

printf '%s\n' "$OUTPUT"
if command -v jq >/dev/null 2>&1; then
  MODULE_ADDRESS="$(printf '%s\n' "$OUTPUT" | awk 'BEGIN { emit = 0 } /^\{/ { emit = 1 } emit { print }' | jq -r '.deployedTo // empty' 2>/dev/null || true)"
else
  MODULE_ADDRESS=""
fi

if [ -z "${MODULE_ADDRESS:-}" ]; then
  MODULE_ADDRESS="$(
    printf '%s\n' "$OUTPUT" |
      sed -n \
        -e 's/.*"deployedTo"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
        -e 's/^Deployed to:[[:space:]]*\(0x[0-9a-fA-F]*\).*/\1/p' |
      tail -n 1
  )"
fi

if [ -n "${MODULE_ADDRESS:-}" ]; then
  printf 'EVM_MODULE_ADDRESS=%s\n' "$MODULE_ADDRESS"
fi
