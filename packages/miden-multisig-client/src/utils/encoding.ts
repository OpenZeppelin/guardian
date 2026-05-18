export function ensureHexPrefix(hex: string): string {
  return hex.startsWith('0x') || hex.startsWith('0X') ? hex : `0x${hex}`;
}

export function normalizeHexWord(hex: string): string {
  let clean = ensureHexPrefix(hex).slice(2).toLowerCase();
  clean = clean.padStart(64, '0');
  return `0x${clean}`;
}

export function bytesToHex(bytes: Uint8Array): string {
  let hex = '0x';
  for (let i = 0; i < bytes.length; i++) {
    hex += bytes[i].toString(16).padStart(2, '0');
  }
  return hex;
}

export function hexToBytes(hex: string): Uint8Array {
  const cleanHex = ensureHexPrefix(hex).slice(2);
  const bytes = new Uint8Array(cleanHex.length / 2);
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(cleanHex.substring(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

export function uint8ArrayToBase64(bytes: Uint8Array): string {
  if (typeof btoa === 'function') {
    let binary = '';
    for (let i = 0; i < bytes.length; i++) {
      binary += String.fromCharCode(bytes[i]);
    }
    return btoa(binary);
  }
  const buf: any = (globalThis as any).Buffer;
  if (buf) {
    return buf.from(bytes).toString('base64');
  }
  throw new Error('No base64 encoder available in this environment');
}

/** Serialize a Miden `Note` to base64 (v2 `consume_notes` metadata, issue #229). */
export function noteToBase64(note: { serialize(): Uint8Array }): string {
  return uint8ArrayToBase64(note.serialize());
}

/** Decode a base64 note. `noteCtor` is `Note` from `@miden-sdk/miden-sdk`. */
export function noteFromBase64<TNote>(
  base64: string,
  noteCtor: { deserialize(bytes: Uint8Array): TNote },
): TNote {
  return noteCtor.deserialize(base64ToUint8Array(base64));
}

export function base64ToUint8Array(base64: string): Uint8Array {
  const binary =
    typeof atob === 'function'
      ? atob(base64)
      : (() => {
          const buf: any = (globalThis as any).Buffer;
          if (buf) {
            return buf.from(base64, 'base64').toString('binary');
          }
          throw new Error('No base64 decoder available in this environment');
        })();
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

