# Guardian Specification

Private state manager is a system that allows a device, or a group of devices, to backup and sync their state securely without trust assumptions about other participants or the server operator.

It consists of 2 main elements:

- State: canonical representation of the state of an entity.
- Delta: valid changes applied to the state.


## Definitions

### State

A state is a data structure that represents the current state of a local account, contract, or any other entity that lives in the local device and has to be kept private and in sync with other devices and valid against some network that asserts its validity.

Example:
```json
{
    "account_id": "1234567890",
    "commitment": "0x1234567890",
    "nonce": 10,
    "assets": [
      {
        "balance": 12000,
        "asset_id": "USDC",
      },
      {
        "balance": 2,
        "asset_id": "ETH",
      }
    ],
}
```

### Delta

A delta is whatever changes you apply to that state in append-only operations. The change on the state is also validated against some network state and acknowledged (signed) by the Guardian.

Example:
```json
{
    "account_id": "1234567890",
    "prev_commitment": "0x1234567890",
    "nonce": 10,
    "ops": [
      { 
        "type": "transfer",
        "asset_id": "USDC",
        "amount": 100,
      }
    ],
}
```

### Account ID

Is the unique identifier of an account holding a state, the Guardian can host multiple accounts and route authenticated requests to each.

### Delta Proposal

Multi-party accounts use delta proposals to coordinate before publishing a canonical delta. A proposal bundles a network-validated delta payload plus optional cosigner signatures. Proposals stay in `pending` status until enough cosigners have signed and someone promotes the payload via `push_delta`; once the corresponding delta becomes canonical, the proposal is deleted.

### Commitment

Is the commitment of the state, it's a hash, nonce, or any other identifier that serves as the unique identifier of the current state of the account. It's used to certify that the state is not forked or corrupted. Each new delta includes a prev_commitment field that references the commitment of the base state in which the delta is applied.

### Nonce

In most networks, the nonce is an incremental counter that serves as a protection mechanism against replay attacks, in this system, we also use the nonce to identify and index deltas.

## Basic principles

- Represent state and deltas as append-only, canonical records.
- Preserve integrity: avoid forks, each delta references the prior commitment and is validated against prior state and, when applicable, external consensus layer.
- Preserve privacy: only authorized account participants can read or mutate state.
- Be consistent across interfaces: the same semantics apply regardless of the transport (HTTP or gRPC).
- Be extensible: network, storage, authentication, and acknowledgement concerns are pluggable without changing core semantics.


## Related documents

- API (HTTP/gRPC): [api.md](./api.md)
- Processes and canonicalization: [processes.md](./processes.md)
- Delta proposal workflow: detailed across [api.md](./api.md) and [processes.md](./processes.md)

## Components

See [components.md](./components.md) for API, Metadata, Auth, Acknowledger, Network, and Storage component details.

 
