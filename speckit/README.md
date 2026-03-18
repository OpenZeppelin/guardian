# Spec Kit Workflow

This repository keeps Spec Kit artifacts under `speckit/` and keeps the helper
engine under `.specify/`.

## Layout

```text
speckit/
├── constitution.md
├── README.md
└── features/
    └── 001-example-feature/
        ├── spec.md
        ├── plan.md
        ├── research.md
        ├── data-model.md
        ├── quickstart.md
        ├── contracts/
        ├── tasks.md
        └── checklists/
```

## Working Model

- `spec/` remains the system and protocol reference.
- `speckit/constitution.md` defines hard project invariants for feature work.
- `speckit/features/` holds tracked feature-specific artifacts.
- `.specify/state/active-feature` is local runtime state and is intentionally not tracked.

## Branching

Branch creation is manual. The helper scripts only suggest a branch name that
matches the feature key, such as `001-auth-replay-tightening`.

## Core Commands

Create a feature workspace:

```bash
.specify/scripts/bash/create-new-feature.sh --json --short-name auth-replay-tightening "Tighten replay protection validation for signed requests"
```

Create or retrieve the plan workspace for the active feature:

```bash
.specify/scripts/bash/setup-plan.sh --json
```

Resolve the active feature workspace paths:

```bash
.specify/scripts/bash/check-prerequisites.sh --json
```

Update agent guidance from the active feature plan:

```bash
.specify/scripts/bash/update-agent-context.sh claude
```

## Active Feature Resolution

The helper scripts resolve the active feature in this order:

1. `--feature <key>` when explicitly provided
2. Current git branch if it matches an existing feature key
3. `.specify/state/active-feature`
4. The only existing feature workspace, when exactly one exists

If multiple workspaces exist and none of the sources above identify one, the
script stops and asks for `--feature`.
