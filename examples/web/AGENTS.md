# examples/web — Agent notes

See repo root `AGENTS.md` §5 (Examples) and §6 (Fast Validation Matrix). This
is the **browser smoke harness** for `@openzeppelin/miden-multisig-client`
and is a required manual check for changes affecting server / TS client /
multisig SDK behavior (root `AGENTS.md` §6 Manual policy).

Stack: Vite + React + TypeScript, Para SDK for wallet connect (Miden / EVM /
Solana / Cosmos adapters), shadcn/ui components, sonner for toasts.

## Layout

| Path | Purpose |
|------|---------|
| `index.html`, `src/main.tsx`, `src/App.tsx` | Vite entry; `App.tsx` owns top-level state and screen routing |
| `src/components/` | UI: dashboard, dialogs, proposal cards, forms |
| `src/hooks/` | `useMidenWallet`, `useParaSession` |
| `src/wallets/` | Wallet adapter wiring (Miden via Demox; EVM/Sol/Cosmos via Para) |
| `src/lib/initClient.ts` | `createMidenClient`, `initializeSigner`, **`clearMidenDatabase`** (IndexedDB nuke for stuck state) |
| `src/lib/procedures.ts` | Map UI procedure choice → SDK transaction type |
| `src/lib/multisigApi.ts` | Thin wrappers over `MultisigClient` calls |
| `src/lib/helpers.ts` | `normalizeCommitment`, formatting |
| `src/lib/errors.ts` | `classifyWalletError`, `formatError` — folds `GuardianHttpError` into user-facing strings |
| `src/config.ts` | Default endpoints (GUARDIAN, Miden RPC) |

## Run

```bash
cd examples/web
npm install
npm run dev          # vite
npm run build        # vite build
npm run preview      # serve built bundle
```

There is **no** `test`, `lint`, or `typecheck` script in this package's
`package.json`. Type errors surface at `npm run build`.

## IndexedDB gotcha

The `miden-client` SDK persists state in IndexedDB. A stale or partially-
written DB **will silently break new account creation** and proposal sync
the next time you reload the page.

- Use the **Clear DB** action in the UI (or call `clearMidenDatabase()` from
  `src/lib/initClient.ts`) before reproducing any bug that involves stuck
  state.
- In DevTools: Application → Storage → IndexedDB → delete `miden-client-*`.
- When debugging "I can't see my account", check IndexedDB first; the
  account exists on GUARDIAN but the local store has no record.

## consume_notes and note metadata (issue #229 / M-08 class)

`consume_notes` is the most regression-prone procedure here because it
requires **local note records** to rebuild the transaction the cosigner
signed. Signed metadata must carry the serialized notes; rebuilding solely
from `NoteId`s is the bug class behind issue #229 and audit finding M-08.

If you touch `src/components/CreateProposalForm.tsx`, `src/lib/procedures.ts`,
or the consume-notes path in `ProposalCard.tsx`, exercise the full flow:
create → sign on a second cosigner → execute.

## Wallet wiring

- Falcon signer (Miden-native) is generated and stored in the Miden keystore.
- EVM / Solana / Cosmos signers come through Para (`@getpara/react-sdk-lite`)
  and are used as cosigners under the **ECDSA path** in the SDK.
- Changes to wallet wiring usually only affect `src/wallets/`,
  `src/hooks/useMidenWallet.ts`, and `src/hooks/useParaSession.ts`.

## Smoke flow (paste into PR notes)

1. Para login, Falcon key generated.
2. Create 2-of-N multisig; register on GUARDIAN.
3. Open in a second browser profile, load account via commitment, sign.
4. Propose: transfer (P2ID), consume notes, add signer, switch GUARDIAN.
5. Threshold reached → execute. Confirm on Miden explorer.
6. Export + re-import proposal file (offline path).

For an automated harness, prefer the `smoke-test-ts-multisig-sdk` skill,
which uses `examples/smoke-web` (the headless variant of this app).
