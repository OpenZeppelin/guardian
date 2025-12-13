/**
 * Web keystore for managing Falcon keys in the browser.
 *
 * Keys are stored in localStorage and can be used for signing transactions.
 */

import type { SecretKey, Word } from '@demox-labs/miden-sdk';

const KEYSTORE_STORAGE_KEY = 'miden_keystore';

/**
 * A stored key entry in the keystore.
 */
export interface KeyEntry {
  /** Unique identifier for the key */
  id: string;
  /** Human-readable name for the key */
  name: string;
  /** Public key commitment as hex string (64 chars) */
  commitment: string;
  /** Serialized secret key as base64 */
  secretKeyBase64: string;
  /** Creation timestamp */
  createdAt: number;
}

/**
 * SDK types needed for keystore operations.
 */
export interface KeystoreSdkTypes {
  SecretKey: typeof SecretKey;
  Word: typeof Word;
}

/**
 * Generates a unique ID for a new key.
 */
function generateKeyId(): string {
  return crypto.randomUUID();
}

/**
 * Loads all keys from localStorage.
 */
export function loadKeys(): KeyEntry[] {
  try {
    const stored = localStorage.getItem(KEYSTORE_STORAGE_KEY);
    if (!stored) return [];
    return JSON.parse(stored) as KeyEntry[];
  } catch {
    return [];
  }
}

/**
 * Saves keys to localStorage.
 */
function saveKeys(keys: KeyEntry[]): void {
  localStorage.setItem(KEYSTORE_STORAGE_KEY, JSON.stringify(keys));
}

/**
 * Generates a new Falcon key pair and stores it in the keystore.
 *
 * @param sdk - Miden SDK types
 * @param name - Human-readable name for the key
 * @returns The created key entry
 */
export function generateKey(sdk: KeystoreSdkTypes, name: string): KeyEntry {
  // Generate a random seed for the key
  const seed = new Uint8Array(32);
  crypto.getRandomValues(seed);

  // Generate the Falcon secret key
  const secretKey = sdk.SecretKey.rpoFalconWithRNG(seed);

  // Get the public key and its commitment
  const publicKey = secretKey.publicKey();
  const commitment = publicKey.toCommitment();
  const commitmentHex = commitment.toHex();

  // Serialize the secret key for storage
  const secretKeyBytes = secretKey.serialize();
  const secretKeyBase64 = btoa(String.fromCharCode(...secretKeyBytes));

  // Create the key entry
  const entry: KeyEntry = {
    id: generateKeyId(),
    name,
    commitment: commitmentHex,
    secretKeyBase64,
    createdAt: Date.now(),
  };

  // Save to keystore
  const keys = loadKeys();
  keys.push(entry);
  saveKeys(keys);

  return entry;
}

/**
 * Loads a secret key from a key entry.
 *
 * @param sdk - Miden SDK types
 * @param entry - The key entry to load
 * @returns The deserialized secret key
 */
export function loadSecretKey(sdk: KeystoreSdkTypes, entry: KeyEntry): SecretKey {
  const bytes = Uint8Array.from(atob(entry.secretKeyBase64), (c) => c.charCodeAt(0));
  return sdk.SecretKey.deserialize(bytes);
}

/**
 * Deletes a key from the keystore.
 *
 * @param keyId - The ID of the key to delete
 */
export function deleteKey(keyId: string): void {
  const keys = loadKeys();
  const filtered = keys.filter((k) => k.id !== keyId);
  saveKeys(filtered);
}

/**
 * Gets a key by ID.
 *
 * @param keyId - The ID of the key to get
 * @returns The key entry or undefined if not found
 */
export function getKey(keyId: string): KeyEntry | undefined {
  const keys = loadKeys();
  return keys.find((k) => k.id === keyId);
}

/**
 * Renames a key.
 *
 * @param keyId - The ID of the key to rename
 * @param newName - The new name for the key
 */
export function renameKey(keyId: string, newName: string): void {
  const keys = loadKeys();
  const key = keys.find((k) => k.id === keyId);
  if (key) {
    key.name = newName;
    saveKeys(keys);
  }
}

/**
 * Clears all keys from the keystore.
 */
export function clearKeystore(): void {
  localStorage.removeItem(KEYSTORE_STORAGE_KEY);
}
