export function bytesToHex(bytes: Uint8Array): string {
  let hex = '0x';
  for (let i = 0; i < bytes.length; i += 1) {
    hex += bytes[i].toString(16).padStart(2, '0');
  }
  return hex;
}

export function normalizeHexWord(hex: string): string {
  const clean = hex.replace(/^0x/i, '').toLowerCase().padStart(64, '0');
  return `0x${clean}`;
}

export function hexToBytes(hex: string): Uint8Array {
  const clean = hex.replace(/^0x/i, '');
  if (clean.length % 2 !== 0) {
    throw new Error('Hex string must have an even number of characters');
  }

  const bytes = new Uint8Array(clean.length / 2);
  for (let i = 0; i < bytes.length; i += 1) {
    bytes[i] = Number.parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

