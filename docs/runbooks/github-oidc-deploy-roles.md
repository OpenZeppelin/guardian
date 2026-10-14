# GitHub OIDC Deploy Roles Runbook

The IAM setup behind OpenZeppelin's **AWS Deploy** GitHub Actions workflow
(`.github/workflows/aws-deploy.yml`, described in
[Deploying a published image from GitHub Actions](../SERVER_AWS_DEPLOY.md#deploying-a-published-image-from-github-actions)).
It is **off by default** (`github_oidc_enabled = false`) and is **not needed**
to deploy with `scripts/aws-deploy.sh`; enable it only if you deploy from
GitHub Actions.

> **Audience:** operators with IAM write access in the AWS account that hosts
> the GitHub OIDC identity provider and in the account that runs the Guardian
> stacks.

## How it works

`infra/oidc.tf` manages two IAM roles that a workflow job assumes in sequence:

1. **Bootstrap role** (`github_oidc_role_name`, default
   `github-actions-solutions-account-guardian-oidc-role`) lives in the account
   that hosts the GitHub OIDC identity provider (the organization's root
   account at OpenZeppelin). GitHub's provider may assume it with
   `sts:AssumeRoleWithWebIdentity` only when the token's subject claim is
   **exactly** one of `github_oidc_subjects`, i.e. a job running under one of
   the listed GitHub environments. The role can do one thing: assume the deploy
   role (`sts:AssumeRole` + `sts:TagSession`).
2. **Deploy role** (`github_deploy_role_name`, default `GithubOIDCGuardianRole`)
   lives in the account that runs the stacks and trusts only the bootstrap
   role. Its inline policy `guardian-aws-deploy` is scoped to the stacks in
   `github_deploy_stack_names`, using the default resource naming from
   `infra/data.tf`:
   - ECR: `ecr:GetAuthorizationToken`, plus push/pull and describe on
     `<stack>-server`;
   - ECS: `DescribeServices` / `UpdateService` on `<stack>-server` in
     `<stack>-cluster`, `DescribeTaskDefinition` / `RegisterTaskDefinition`
     (IAM offers no resource-level scoping for these two), and `TagResource` on
     the `<stack>-server` task-definition family;
   - `iam:PassRole` on `<stack>-ecs-task` and `<stack>-ecs-task-execution`,
     limited to `ecs-tasks.amazonaws.com`.

The roles are shared by every stack in the account, so `github_oidc_enabled`
is set on **exactly one stack per account**; every other stack leaves it
`false`, or they would all try to create roles with the same names.

Because the trust policy admits *environments*, each listed GitHub environment
**must restrict deployment branches to `main`**. Otherwise anyone able to
dispatch the workflow could run an edited copy of it from a feature branch and
obtain the environment's AWS identity.

## OpenZeppelin deployment

| Item | Value |
|---|---|
| Managing stack (`github_oidc_enabled = true`) | `guardian-prod` (state `infra/terraform.guardian-prod.prod.tfstate`) |
| Bootstrap role | `github-actions-solutions-account-guardian-oidc-role` in the root account |
| Deploy role | `GithubOIDCGuardianRole` in the stacks account |
| Admitted subject claims (`github_oidc_subjects`) | `repo:OpenZeppelin/guardian:environment:devnet`, `repo:OpenZeppelin/guardian:environment:testnet` |
| Deployable stacks (`github_deploy_stack_names`) | `guardian` (devnet), `guardian-prod` (testnet) |

Export these before running `scripts/aws-deploy.sh plan` / `deploy` for the
`guardian-prod` stack (the script passes `TF_VAR_*` through to Terraform):

```bash
export TF_VAR_github_oidc_enabled=true
export TF_VAR_github_oidc_provider_arn="arn:aws:iam::<root-account>:oidc-provider/token.actions.githubusercontent.com"
export TF_VAR_github_oidc_root_account_profile="<aws-cli-profile-that-resolves-to-the-root-account>"
```

The bootstrap role is managed through the `aws.root_account` provider alias,
which needs root-account credentials while everything else (including the
script's own AWS CLI calls) runs as the stacks account. The stacks-account
`Terraform` role cannot assume the root-account one, so point the alias at a
named profile that resolves to the root account. If your stack credentials
*can* assume a root-account role, set `github_oidc_root_account_role_arn`
instead. A precondition fails the plan if the alias resolves to a different
account than the one in `github_oidc_provider_arn`.

The two outputs are the values for the GitHub environment variables the
workflow reads:

| Terraform output | GitHub environment variable |
|---|---|
| `github_oidc_role_arn` | `ROLE_FOR_OIDC` |
| `github_deploy_role_arn` | `ROLE_TO_ASSUME` |

## Adopting the existing roles

The roles already exist in AWS (originally created from the retired
oz-terraform repository). Import them into the `guardian-prod` state once,
before the first apply with `github_oidc_enabled = true`, so the apply updates
them in place instead of failing on a name collision. `import` requires every
required variable to be set but applies nothing, so `server_image_uri` can be
a placeholder; the subsequent `plan` resolves the real image as usual.

```bash
cd infra
STATE=terraform.guardian-prod.prod.tfstate
VARS=(-var "deployment_stage=prod" -var "stack_name=guardian-prod" -var "server_image_uri=import-placeholder")

terraform import -state="$STATE" "${VARS[@]}" 'aws_iam_role.github_oidc[0]' \
  github-actions-solutions-account-guardian-oidc-role
terraform import -state="$STATE" "${VARS[@]}" 'aws_iam_role_policy.github_oidc_assume_deploy[0]' \
  github-actions-solutions-account-guardian-oidc-role:github-actions-solutions-guardian-assume-role-policy
terraform import -state="$STATE" "${VARS[@]}" 'aws_iam_role.github_deploy[0]' \
  GithubOIDCGuardianRole
```

Then, with the `TF_VAR_*` exports above:

1. `DEPLOY_STAGE=prod STACK_NAME=guardian-prod ./scripts/aws-deploy.sh plan`
   and confirm the OIDC changes are limited to: the bootstrap role's trust
   policy updated in place (`StringEquals` subjects, default tags) and the
   scoped inline policy `guardian-aws-deploy` created. The deploy role itself
   and the bootstrap role's assume policy are no-ops.
2. Apply. If the plan also carries unrelated stack drift you do not intend to
   roll out, target the OIDC resources:

   ```bash
   terraform -chdir=infra apply -state=terraform.guardian-prod.prod.tfstate \
     -target='aws_iam_role.github_oidc[0]' \
     -target='aws_iam_role_policy.github_deploy[0]' ...
   ```

3. **Only after** the apply has created `guardian-aws-deploy`, detach the
   `AdministratorAccess` policy the original definition attached. Terraform
   does not manage attachments it did not create, and detaching first would
   leave the role with no permissions at all:

   ```bash
   aws iam detach-role-policy --role-name GithubOIDCGuardianRole \
     --policy-arn arn:aws:iam::aws:policy/AdministratorAccess
   ```

4. Finally, remove the resources from the retired oz-terraform `prod`
   workspace so two states no longer claim the same roles (`state rm` only
   forgets them; nothing in AWS changes):

   ```bash
   terraform workspace select prod
   terraform state rm 'aws_iam_role.oidc_role[0]' \
     'aws_iam_role_policy.oidc_assume_role_policy[0]' \
     'aws_iam_role.guardian_deploy_role[0]'
   ```

## Adding an environment or a stack

- **New GitHub environment** (for example `mainnet`): create the environment
  with the workflow's variables, restrict its deployment branches to `main`,
  add `repo:OpenZeppelin/guardian:environment:<name>` to
  `github_oidc_subjects`, add the name to the workflow's `environment` choice
  list, and apply.
- **New deployable stack**: add its `stack_name` to
  `github_deploy_stack_names` and apply; the deploy role then covers that
  stack's ECR repository, ECS service, task-definition family, and task roles.
  Stacks that override the default resource names in Terraform are not
  covered.

## Reusing it for your own deployment

The variable defaults are OpenZeppelin's. To run the same workflow from a fork
in your own AWS account(s), override:

- `github_oidc_provider_arn`: the ARN of *your* GitHub OIDC identity provider
  (`token.actions.githubusercontent.com`). `oidc.tf` does not create the
  provider; create it once per account (IAM console or
  `aws_iam_openid_connect_provider`).
- `github_oidc_subjects`: `repo:<owner>/<repo>:environment:<name>` for each
  GitHub environment that may deploy.
- `github_deploy_stack_names`: the `stack_name` values the workflow may roll
  out.
- Optionally `github_oidc_role_name` / `github_deploy_role_name`.

Root-account layout:

- **Separate root account** (provider and bootstrap role in one account, stacks
  in another): set `github_oidc_root_account_profile` to a profile that
  resolves to the provider's account, or `github_oidc_root_account_role_arn`
  if your stack credentials can assume a role there.
- **Single account**: point `github_oidc_root_account_profile` at the same
  profile you deploy with; both roles are then created in that account.

The workflow itself carries three OpenZeppelin-specific settings a fork must
edit in `.github/workflows/aws-deploy.yml`: the
`github.repository_owner == 'OpenZeppelin'` guard on the `validate` job, the
`environment` input's choice list, and the `GHCR_IMAGE` the deploy mirrors
(`ghcr.io/openzeppelin/guardian`). Provenance verification uses
`github.repository`, so it follows the fork automatically.
