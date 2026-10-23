/**
 * The envelope a Guardian-executable proposal stores its serialized `TransactionRequest` in.
 * Mirrors `guardian_shared::request_envelope`; the same bytes seal to the same envelope in both
 * SDKs.
 */

export const ENVELOPE_FORMAT_VERSION = 1;

/** The Miden protocol line requests are serialized for, as `MAJOR.MINOR`. */
export const PROTOCOL_LINE = '0.17';

/** Wire form: snake_case, exactly as the server stores it. */
export interface TransactionRequestEnvelope {
  format_version: number;
  protocol_line: string;
  /** `0x`-prefixed lowercase SHA-256 of the raw bytes. */
  checksum: string;
  /** Base64 of the raw bytes. */
  bytes: string;
}

function toBase64(bytes: Uint8Array): string {
  let binary = '';
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}

async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = new Uint8Array(await globalThis.crypto.subtle.digest('SHA-256', new Uint8Array(bytes)));
  return `0x${Array.from(digest, (byte) => byte.toString(16).padStart(2, '0')).join('')}`;
}

/** Wraps freshly serialized request bytes for storage with a proposal. */
export async function sealTransactionRequest(bytes: Uint8Array): Promise<TransactionRequestEnvelope> {
  return {
    format_version: ENVELOPE_FORMAT_VERSION,
    protocol_line: PROTOCOL_LINE,
    checksum: await sha256Hex(bytes),
    bytes: toBase64(bytes),
  };
}

function envelopeField<T>(record: Record<string, unknown>, key: keyof TransactionRequestEnvelope, accepts: (value: unknown) => value is T): T {
  const value = record[key];
  if (!accepts(value)) {
    throw new Error(`Guardian returned a transaction request envelope with an invalid ${key}: ${JSON.stringify(value)}`);
  }
  return value;
}

const isString = (value: unknown): value is string => typeof value === 'string';
const isFormatVersion = (value: unknown): value is number => Number.isSafeInteger(value) && (value as number) >= 0;

/** Decodes a stored envelope as the server returns it, refusing a missing or mistyped field. */
export function decodeTransactionRequestEnvelope(value: unknown): TransactionRequestEnvelope {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new Error(`Guardian returned a transaction request envelope that is not an object: ${JSON.stringify(value)}`);
  }
  const record = value as Record<string, unknown>;
  return {
    format_version: envelopeField(record, 'format_version', isFormatVersion),
    protocol_line: envelopeField(record, 'protocol_line', isString),
    checksum: envelopeField(record, 'checksum', isString),
    bytes: envelopeField(record, 'bytes', isString),
  };
}
