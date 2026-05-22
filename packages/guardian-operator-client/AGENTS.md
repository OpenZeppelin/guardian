# guardian-operator-client — Agent notes

This is the TypeScript SDK that the operator dashboard (and any external
operator-side tooling) talks to the server through. It is **distinct from**
`@openzeppelin/guardian-client` — that package targets the cosigner gRPC/HTTP
surface; this one targets the operator HTTP surface backed by
`crates/server/src/dashboard/` and friends.

For repo-wide rules (style, change rules, contract-change workflow) see
repo root `AGENTS.md`. `crates/server/AGENTS.md` describes the server side
of this contract under "Auth surfaces" (operator dashboard session).

## Structure

- `src/server-types.ts` — wire types matching server JSON exactly. **Source of
  truth for the HTTP contract**. Keep aligned with server response shapes.
- `src/types.ts` — domain types exposed to consumers. Mapped from wire types
  in `http.ts`.
- `src/http.ts` — typed parsing layer with strict contract validation.
  Wire-form mapping lives here. Errors are surfaced as
  `GuardianOperatorHttpError` (with `GuardianOperatorHttpErrorData` for
  structured details) using stable error codes.
- `src/index.ts` — public exports. Anything not exported here is internal.

## Contract change checklist

When the server changes a response shape:

1. Update `server-types.ts` first.
2. Update `types.ts` if the domain type also changes.
3. Update mapping + error handling in `http.ts`.
4. Add/extend tests covering the new shape.
5. If a new error code, extend `DashboardErrorCode` and
   `GuardianOperatorHttpErrorData` with any new detail fields. The thrown
   type stays `GuardianOperatorHttpError`.

## Conventions

- No `any` in core modules; use type narrowing not `!`.
- Validate at the boundary: wire-form → domain type conversion is the only
  place that should branch on optional/missing fields.
- Error codes are stable strings (e.g. `GUARDIAN_ACCOUNT_PAUSED`).
- Prefer mandatory fields in domain types; convert optional protobuf/JSON
  fields at the parse boundary.

## Tests

```bash
npm test          # vitest
npm run typecheck # tsc --noEmit
```

No `npm run lint` script in this package — skip it if a task asks for lint.
