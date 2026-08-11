# Guardian Domain Migration

This runbook is only for the one-time OpenZeppelin hostname migration in
[issue #341](https://github.com/OpenZeppelin/guardian/issues/341). Normal
Guardian deployments should leave `ALIAS_SUBDOMAIN` unset.

The migration keeps each legacy hostname available on the same ALB while the new
canonical hostname is verified. Both names route directly; there is no hostname
redirect.

| Deployment | Network | Canonical `SUBDOMAIN` | Temporary `ALIAS_SUBDOMAIN` |
|---|---|---|---|
| Devnet | `MidenDevnet` | `guardian-devnet` | `guardian-stg` |
| Testnet | `MidenTestnet` | `guardian-testnet` | `guardian` |

The ACM certificate configured through `ACM_CERTIFICATE_ARN` must cover both
names, or the legacy hostname's certificate must be supplied separately through
`ALIAS_ACM_CERTIFICATE_ARN` for SNI.

## Readdress the existing DNS record

Do not change `SUBDOMAIN` and apply immediately. Terraform would update the
existing primary record while also trying to create the same legacy hostname as
the secondary record. Move the existing record's state address first; this does
not modify live DNS.

Load the existing stack configuration, select its state file, and back it up:

```bash
set -a && source .env && set +a

export SUBDOMAIN=guardian-testnet
export ALIAS_SUBDOMAIN=guardian
export TF_STATE_PATH="$(pwd)/infra/terraform.guardian-prod.prod.tfstate"

cp "$TF_STATE_PATH" "${TF_STATE_PATH}.before-domain-migration"
terraform -chdir=infra state list -state="$TF_STATE_PATH" | rg '^(cloudflare_dns_record\.service|aws_route53_record\.service_alias)'
```

Move only the record type shown by `state list`; move both if the stack manages
both providers:

```bash
# Cloudflare-managed DNS
terraform -chdir=infra state mv -state="$TF_STATE_PATH" \
  'cloudflare_dns_record.service[0]' \
  'cloudflare_dns_record.service_secondary[0]'

# Route 53-managed DNS
terraform -chdir=infra state mv -state="$TF_STATE_PATH" \
  'aws_route53_record.service_alias[0]' \
  'aws_route53_record.service_secondary[0]'
```

For devnet, use `SUBDOMAIN=guardian-devnet`,
`ALIAS_SUBDOMAIN=guardian-stg`, and the devnet stack's state file. The hosted
staging deployment currently uses Cloudflare rather than Route 53, but always
trust `terraform state list` for the stack being migrated.

## Plan, deploy, and verify

Run the plan with the same `STACK_NAME`, `DEPLOY_STAGE`, `SUBDOMAIN`, and
`ALIAS_SUBDOMAIN` used for the state move:

```bash
./scripts/aws-deploy.sh plan
```

The plan must retain the legacy secondary record and create only the new
canonical record. Do not apply if it destroys or replaces the legacy record.
After reviewing the plan:

```bash
./scripts/aws-deploy.sh deploy --skip-build
```

Verify HTTP, gRPC, and certificate hostname matching on both names. This example
shows testnet; repeat it for `guardian-devnet` and `guardian-stg`:

```bash
curl https://guardian-testnet.openzeppelin.com/pubkey
curl https://guardian.openzeppelin.com/pubkey
grpcurl -import-path crates/server/proto -proto guardian.proto -d '{}' guardian-testnet.openzeppelin.com:443 guardian.Guardian/GetPubkey
grpcurl -import-path crates/server/proto -proto guardian.proto -d '{}' guardian.openzeppelin.com:443 guardian.Guardian/GetPubkey
openssl s_client -connect guardian-testnet.openzeppelin.com:443 -servername guardian-testnet.openzeppelin.com -verify_hostname guardian-testnet.openzeppelin.com </dev/null
openssl s_client -connect guardian.openzeppelin.com:443 -servername guardian.openzeppelin.com -verify_hostname guardian.openzeppelin.com </dev/null
```

Keep SDK, smoke-test, benchmark, and operational defaults on the legacy names
during the observation period. Switch consumers in a follow-up change only after
both canonical hostnames have been confirmed stable.
