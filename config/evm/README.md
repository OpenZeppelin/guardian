# Guardian EVM Chain Config

`chains.json` is the deploy-time source of truth for feature-gated EVM chain
support. `scripts/aws-deploy.sh` reads this file when `GUARDIAN_SERVER_FEATURES`
includes `evm` and the explicit `GUARDIAN_EVM_ALLOWED_CHAIN_IDS` /
`GUARDIAN_EVM_RPC_URLS` values are not set.

The deploy script derives:

- `GUARDIAN_EVM_ALLOWED_CHAIN_IDS` from `chains[].chainId`
- `GUARDIAN_EVM_RPC_URLS` from `chains[].chainId` and `chains[].rpcUrl`
- `GUARDIAN_EVM_ENTRYPOINT_ADDRESS` from `entrypointAddress`

RPC URLs are passed to Terraform as a Secrets Manager-backed ECS secret. The
EntryPoint address is passed as a normal ECS environment variable.
