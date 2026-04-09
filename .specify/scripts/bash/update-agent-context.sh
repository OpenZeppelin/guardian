#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
source "${SCRIPT_DIR}/common.sh"

agent_name="${1:-claude}"
ensure_engine_dirs

feature_key=""
if feature_key="$(resolve_feature_key 2>/dev/null)"; then
  set_active_feature "${feature_key}"
fi

feature_title="No active feature"
plan_file=""
if [ -n "${feature_key}" ]; then
  plan_file="$(feature_dir_from_key "${feature_key}")/plan.md"
  feature_title="$(humanize_feature_key "${feature_key}")"
fi

case "${agent_name}" in
  claude)
    mkdir -p "${ROOT}/.claude"
    target_file="${ROOT}/.claude/spec-kit-context.md"
    ;;
  cursor)
    mkdir -p "${ROOT}/.cursor"
    target_file="${ROOT}/.cursor/spec-kit-context.md"
    ;;
  *)
    mkdir -p "${ROOT}/.specify/agent-context"
    target_file="${ROOT}/.specify/agent-context/${agent_name}.md"
    ;;
esac

active_technologies=$'- Rust monorepo centered on crates/server, crates/client, and crates/miden-multisig-client\n- TypeScript SDK packages: packages/guardian-client and packages/miden-multisig-client\n- Dual transport surface: HTTP + gRPC with semantic parity requirements\n- Examples as validation surfaces: examples/demo, examples/web, examples/rust'
commands=$'- `cargo test -p private-state-manager-server`\n- `cargo test -p private-state-manager-client`\n- `cargo test -p miden-multisig-client`\n- `cd packages/guardian-client && npm test`\n- `cd packages/miden-multisig-client && npm test`'
recent_changes=$'- Constitution v1.0.0: bottom-up propagation, transport parity, append-only integrity, explicit auth, evidence-driven validation.\n- Feature workspaces live under `speckit/features/`.\n- Branch creation is manual; scripts only record the suggested feature branch.'

if [ -n "${plan_file}" ] && [ -f "${plan_file}" ]; then
  summary_line="$(awk 'BEGIN{found=0} /^## Summary/{found=1; next} found && NF {print; exit}' "${plan_file}")"
  if [ -n "${summary_line}" ]; then
    recent_changes+=$'\n- Active plan summary: '"${summary_line}"
  fi
fi

cat > "${target_file}" <<EOF
# Guardian Agent Context

Auto-generated from the active feature plan.

**Updated**: $(today_iso)  
**Active Feature**: ${feature_title} (\`${feature_key:-none}\`)

## Active Technologies

${active_technologies}

## Project Structure

\`\`\`text
crates/{server,client,shared,miden-multisig-client,miden-rpc-client,miden-keystore}
packages/{guardian-client,miden-multisig-client}
examples/{demo,web,rust}
speckit/features/${feature_key:-[###-feature-name]}
\`\`\`

## Validation Commands

${commands}

## Working Rules

- Preserve HTTP/gRPC semantic parity unless a spec documents intentional divergence.
- Preserve Rust/TypeScript behavioral parity for equivalent workflows.
- Propagate lower-layer changes upward through clients, multisig SDKs, and examples.
- Prefer targeted validation first, then widen only if blast radius grows.

## Current Constraints

${recent_changes}
EOF

echo "Updated agent context: ${target_file}"
