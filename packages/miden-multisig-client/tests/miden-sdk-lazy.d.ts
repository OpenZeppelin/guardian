/**
 * The SDK exports `initSync` from its `./lazy` entry at runtime but omits it
 * from that entry's type declarations, so Node consumers who initialize the
 * WASM module by hand have nothing to import.
 */
declare module '@miden-sdk/miden-sdk/lazy' {
  export function initSync(options: { module: BufferSource }): unknown;
}
