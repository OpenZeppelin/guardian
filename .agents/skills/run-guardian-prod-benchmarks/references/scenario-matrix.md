# Scenario Matrix

These are the only supported scenarios for this skill.

## Profiles

### `ecdsa-burst-scale.toml`

Use for the pure write-admission ceiling with unique accounts.

- signer mix: `100%` ECDSA
- workload: `push_delta` only
- `reads_per_push = 0`
- `4096` users
- `1` account per user
- benchmark shape: burst acceptance, not sustained write reuse

### `ecdsa-mixed-burst-scale.toml`

Use for the ECDSA-only mixed workload.

- signer mix: `100%` ECDSA
- workload: `1 push_delta : 4 get_state`
- `retire_after_first_successful_push = true`
- `4096` users
- `1` account per user

### `falcon-mixed-burst-scale.toml`

Use for the Falcon-only mixed workload.

- signer mix: `100%` Falcon
- workload: `1 push_delta : 4 get_state`
- `retire_after_first_successful_push = true`
- `4096` users
- `1` account per user

### `falcon-ecdsa-mixed-burst-scale.toml`

Use for the mixed-signer planning run.

- signer mix: `50%` Falcon, `50%` ECDSA
- workload: `1 push_delta : 4 get_state`
- `retire_after_first_successful_push = true`
- `4096` users
- `1` account per user

## Worker Defaults

The reference runs used:

- `16` ECS/Fargate worker tasks
- `2 vCPU / 4 GB` per worker
- region `us-east-1`

## Selection Rules

- Use `ecdsa-burst-scale` when the question is pure write admission.
- Use `ecdsa-mixed-burst-scale` when the question is mixed load with only ECDSA signers.
- Use `falcon-mixed-burst-scale` when the question is mixed load with only Falcon signers.
- Use `falcon-ecdsa-mixed-burst-scale` when the question is production-like mixed signer traffic.
- Do not use deleted smoke or baseline profiles as examples or recommendations.
