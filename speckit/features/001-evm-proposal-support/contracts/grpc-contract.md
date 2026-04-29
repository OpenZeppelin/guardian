# gRPC Contract: Domain-separated EVM proposal support

EVM v1 is HTTP-only. The existing Guardian gRPC service remains Miden-oriented:

- `Configure`
- `PushDelta`
- `GetDelta`
- `GetDeltaSince`
- `GetState`
- `GetPubkey`
- `PushDeltaProposal`
- `GetDeltaProposals`
- `GetDeltaProposal`
- `SignDeltaProposal`

If an EVM account ID or EVM-shaped account configuration reaches a Miden gRPC
method, the method returns an explicit application error code such as
`unsupported_for_network`. `/evm/*` session, account, proposal, executable, and
cancel routes are not represented in gRPC for v1.

Schema variants may still expose EVM-shaped metadata for compatibility with the
shared metadata model, but EVM behavior is not available through gRPC.
