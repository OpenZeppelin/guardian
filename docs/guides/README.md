# Guides

Task-oriented, end-to-end walkthroughs for running Guardian in a specific mode.
Each guide assembles a complete, copy-pasteable configuration in one place and
links to [`CONFIGURATION.md`](../CONFIGURATION.md) for the authoritative meaning
of each variable.

These differ from the other docs by intent:

- **Guides** (here) — "how do I run it set up like *this*?" end to end.
- [`CONFIGURATION.md`](../CONFIGURATION.md) — flat reference for every env var.
- [`SERVER_AWS_DEPLOY.md`](../SERVER_AWS_DEPLOY.md) — the ECS/Terraform deploy procedure.
- [`runbooks/`](../runbooks/) — operational procedures (secrets, incidents).

Guides use Docker Compose unless a guide says otherwise, so directory names
describe the *configuration* a guide demonstrates rather than repeating the
runner. Name a guide after what makes it distinct (its signer backends,
storage, or network), not after Compose.

## Available guides

| Guide | Mode |
|---|---|
| [AWS-managed ACK signers](./aws-signers/README.md) | Self-hosted Compose: Postgres + Secrets Manager (Falcon) + KMS (ECDSA) |
| [Miden Dashboard UI](./miden-dashboard/README.md) | Self-hosted Compose: Postgres + Guardian server + the Miden Dashboard operator UI |

## Adding a guide

Give each guide its own subdirectory holding a `README.md` and its committed,
runnable artifacts (e.g. `docker-compose.yml` + `.env.example`), so the guide
and the config you copy live together and the config can be smoke-tested. Keep
variable explanations in `CONFIGURATION.md` rather than restating them here.
