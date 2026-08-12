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

## Select the stack explicitly

Load shared credentials and deployment settings from `.env`, then override every
value that identifies the stack. Do not rely on the current values in `.env`
when selecting a state file manually.

For testnet:

```bash
set -a && source .env && set +a

export AWS_REGION=us-east-1
export STACK_NAME=guardian-prod
export DEPLOY_STAGE=prod
export GUARDIAN_NETWORK_TYPE=MidenTestnet
export DOMAIN_NAME=openzeppelin.com
export SUBDOMAIN=guardian-testnet
export ALIAS_SUBDOMAIN=guardian
export TF_STATE_PATH="$(pwd)/infra/terraform.guardian-prod.prod.tfstate"
```

For devnet:

```bash
set -a && source .env && set +a

export AWS_REGION=us-east-1
export STACK_NAME=guardian
export DEPLOY_STAGE=dev
export GUARDIAN_NETWORK_TYPE=MidenDevnet
export DOMAIN_NAME=openzeppelin.com
export SUBDOMAIN=guardian-devnet
export ALIAS_SUBDOMAIN=guardian-stg
export TF_STATE_PATH="$(pwd)/infra/terraform.guardian.dev.tfstate"
```

Set `ACM_CERTIFICATE_ARN` to an issued certificate covering the canonical
hostname. If that certificate does not also cover the legacy hostname, set
`ALIAS_ACM_CERTIFICATE_ARN` to its existing certificate. Both certificates must
be in the ALB's AWS region.

Before continuing, authenticate and confirm that the selected state belongs to
the intended deployment:

```bash
aws sts get-caller-identity
terraform -chdir=infra output -state="$TF_STATE_PATH" deployment_stage
terraform -chdir=infra output -state="$TF_STATE_PATH" ecs_cluster_name
terraform -chdir=infra output -state="$TF_STATE_PATH" custom_domain_url
```

Stop if these outputs do not match the selected stack, stage, and legacy
hostname.

## Determine whether a state move is needed

A state move is not part of a normal hostname addition and is unnecessary in
most deployments. It is needed only when all of the following are true:

- the legacy hostname is already managed by this Terraform state;
- it is tracked at the primary DNS resource address; and
- the same apply must make the new hostname primary while retaining the legacy
  hostname as the secondary record.

This is the expected shape of the existing OpenZeppelin devnet and testnet
stacks. The move changes only Terraform's local ownership address; it does not
modify live DNS.

Inspect the selected state:

```bash
terraform -chdir=infra state list -state="$TF_STATE_PATH" |
  rg '^(cloudflare_dns_record\.service|aws_route53_record\.service_alias)'
```

Skip the state move when the legacy record is not present at either primary
address below. Examples include a fresh stack, DNS managed outside Terraform,
or a state that has already been migrated. Do not import or move an unfamiliar
record without first reconciling who owns it.

If a primary address is present, back up the state:

```bash
chmod 600 "$TF_STATE_PATH"
install -m 600 "$TF_STATE_PATH" "${TF_STATE_PATH}.before-domain-migration"
```

Move only the record type shown by `state list`; move both only if the stack
actually manages both providers. Run each matching block separately. The
subshell keeps a failed guard from terminating the operator's shell, and each
block fails before the move unless the tracked record is the expected legacy
hostname.

For Cloudflare-managed DNS, provider state may store `name` as either the
relative record name or the full hostname:

```bash
(
  EXPECTED_LEGACY_FQDN="${ALIAS_SUBDOMAIN}.${DOMAIN_NAME}"
  CURRENT_LEGACY_NAME=$(terraform -chdir=infra state show -state="$TF_STATE_PATH" \
    'cloudflare_dns_record.service[0]' |
    sed -nE 's/^[[:space:]]*name[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p')
  case "$CURRENT_LEGACY_NAME" in
    "$ALIAS_SUBDOMAIN"|"$EXPECTED_LEGACY_FQDN") ;;
    *)
      echo "Refusing state move: expected ${ALIAS_SUBDOMAIN} or ${EXPECTED_LEGACY_FQDN}, found ${CURRENT_LEGACY_NAME:-<empty>}" >&2
      exit 1
      ;;
  esac
  terraform -chdir=infra state mv -state="$TF_STATE_PATH" \
    'cloudflare_dns_record.service[0]' \
    'cloudflare_dns_record.service_secondary[0]'
)
```

For Route 53-managed DNS, the record's alias block holds a second `name`
attribute for the ALB, so the guard reads the top-level `fqdn` instead:

```bash
(
  EXPECTED_LEGACY_FQDN="${ALIAS_SUBDOMAIN}.${DOMAIN_NAME}"
  CURRENT_LEGACY_FQDN=$(terraform -chdir=infra state show -state="$TF_STATE_PATH" \
    'aws_route53_record.service_alias[0]' |
    sed -nE 's/^[[:space:]]*fqdn[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p')
  test "$CURRENT_LEGACY_FQDN" = "$EXPECTED_LEGACY_FQDN" || {
    echo "Refusing state move: expected ${EXPECTED_LEGACY_FQDN}, found ${CURRENT_LEGACY_FQDN:-<empty>}" >&2
    exit 1
  }
  terraform -chdir=infra state mv -state="$TF_STATE_PATH" \
    'aws_route53_record.service_alias[0]' \
    'aws_route53_record.service_secondary[0]'
)
```

The hosted devnet and testnet states currently use Cloudflare rather than Route
53, but always trust `terraform state list` for the selected state. After a
move, run it again and confirm the record appears only at the corresponding
`service_secondary[0]` address.

When DNS is managed outside Terraform, skip both state-move blocks and create or
retain the canonical and legacy records with that provider. Terraform still
attaches the required ACM certificates to the ALB.

## Plan, deploy, and verify

Run the plan with the same `STACK_NAME`, `DEPLOY_STAGE`, `SUBDOMAIN`, and
`ALIAS_SUBDOMAIN` used for the state move:

```bash
./scripts/aws-deploy.sh plan
```

For Terraform-managed DNS, the plan must retain the legacy secondary record and
create the new canonical record. An in-place comment update on the legacy
Cloudflare record is harmless. With external DNS, the plan should not add DNS
resources; confirm both records separately with that provider. Certificate and
output changes are expected in either case. Do not apply if the plan destroys or
replaces the legacy DNS record, ALB, ECS service, or database. After reviewing
the plan:

```bash
./scripts/aws-deploy.sh deploy --skip-build
```

Verify HTTP, gRPC, and certificate hostname matching on both names. This example
shows testnet; repeat it for `guardian-devnet` and `guardian-stg`:

```bash
curl --fail-with-body https://guardian-testnet.openzeppelin.com/pubkey
curl --fail-with-body https://guardian.openzeppelin.com/pubkey
grpcurl -import-path crates/server/proto -proto guardian.proto -d '{}' guardian-testnet.openzeppelin.com:443 guardian.Guardian/GetPubkey
grpcurl -import-path crates/server/proto -proto guardian.proto -d '{}' guardian.openzeppelin.com:443 guardian.Guardian/GetPubkey
openssl s_client -connect guardian-testnet.openzeppelin.com:443 -servername guardian-testnet.openzeppelin.com -verify_hostname guardian-testnet.openzeppelin.com -verify_return_error </dev/null
openssl s_client -connect guardian.openzeppelin.com:443 -servername guardian.openzeppelin.com -verify_hostname guardian.openzeppelin.com -verify_return_error </dev/null
```

Keep SDK, smoke-test, benchmark, and operational defaults on the legacy names
during the observation period. Switch consumers in a follow-up change only after
both canonical hostnames have been confirmed stable.

## Remove the legacy hostname

After consumers have moved and monitoring shows no required traffic on the
legacy hostname, unset `ALIAS_SUBDOMAIN` and `ALIAS_ACM_CERTIFICATE_ARN` while
keeping all canonical stack values pinned as above:

```bash
unset ALIAS_SUBDOMAIN ALIAS_ACM_CERTIFICATE_ARN
./scripts/aws-deploy.sh plan
```

The cleanup plan must delete only the legacy DNS record, the optional secondary
listener certificate attachment, and migration-only outputs. Apply it with
`./scripts/aws-deploy.sh deploy --skip-build`, then verify the canonical HTTP and
gRPC endpoints again.
