/**
 * In-memory IndexedDB for tests that exercise the real WASM client store,
 * with per-"device" isolation.
 *
 * The SDK's bundled dexie captures `globalThis.indexedDB` once at module
 * load, so this module must be imported before the SDK (it is the first
 * import in `tests/setup-wasm.ts`). Every WASM mock client opens the same
 * database name; to simulate a new device (fresh, empty store) the global
 * factory is a thin delegate whose backing `IDBFactory` can be swapped at
 * any time via {@link freshDeviceStore}.
 *
 * Lives under `src/` (not `tests/`) only because test files compile with
 * `rootDir: src` and must be able to import it; it is test-only code, like
 * the `*.test.ts` files beside it.
 */
import 'fake-indexeddb/auto';
import { IDBFactory } from 'fake-indexeddb';

let currentFactory = new IDBFactory();

/** Swap in a fresh, empty IndexedDB — the next client opened is a "new device". */
export function freshDeviceStore(): void {
  currentFactory = new IDBFactory();
}

const delegatingFactory = {
  open: (name: string, version?: number) => currentFactory.open(name, version),
  deleteDatabase: (name: string) => currentFactory.deleteDatabase(name),
  databases: () => currentFactory.databases(),
  cmp: (a: unknown, b: unknown) => currentFactory.cmp(a, b),
};

globalThis.indexedDB = delegatingFactory as unknown as IDBFactory;
