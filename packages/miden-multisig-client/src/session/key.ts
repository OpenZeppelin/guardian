import type {
  RequestAuthPayload,
  SessionEndReason,
  SessionRequestSigner,
} from '@openzeppelin/guardian-client';
import type { Word } from '@miden-sdk/miden-sdk';
import { AuthDigest } from '../utils/digest.js';
import { EcdsaFormat } from '../utils/ecdsa.js';
import { bytesToHex } from '../utils/encoding.js';
import { wordToBytes } from '../utils/word.js';
import { sessionLogoutDigest } from './grant.js';

const ALGORITHM = { name: 'ECDSA', namedCurve: 'P-256' } as const;
const SIGNATURE = { name: 'ECDSA', hash: 'SHA-256' } as const;

function subtle(): SubtleCrypto {
  if (!globalThis.crypto?.subtle) {
    throw new Error('Guardian sessions require WebCrypto (crypto.subtle)');
  }
  return globalThis.crypto.subtle;
}

/**
 * A P-256 session key whose private half is a non-extractable WebCrypto key:
 * page scripts can use it to sign while it is loaded, but can never read or
 * export it.
 */
export class WebCryptoSessionKey {
  constructor(
    readonly privateKey: CryptoKey,
    /** Hex SEC1-compressed public key. */
    readonly publicKey: string,
  ) {}

  static async generate(): Promise<WebCryptoSessionKey> {
    const pair = await subtle().generateKey(ALGORITHM, false, ['sign', 'verify']);
    const raw = new Uint8Array(await subtle().exportKey('raw', pair.publicKey));
    return new WebCryptoSessionKey(pair.privateKey, EcdsaFormat.compressPublicKey(bytesToHex(raw)));
  }

  /** Sign the 32 bytes of a word: ECDSA P-256 over SHA-256, `r || s` hex. */
  async signWord(word: Word): Promise<string> {
    const message = new Uint8Array(wordToBytes(word));
    const signature = await subtle().sign(SIGNATURE, this.privateKey, message);
    return bytesToHex(new Uint8Array(signature));
  }
}

export interface GuardianSessionOptions {
  /** The store keeping this session's key; its record is removed when the session ends. */
  store?: SessionKeyStore;
  /**
   * Called once when the Guardian client stops using the session. Requests
   * are then signed by the wallet again. On `expired`, `revoked` or
   * `rejected`, start a new session to stop the prompts; `logout` means the
   * app ended it itself (logout, revoke-all or a replacement session).
   */
  onEnded?: (reason: SessionEndReason, session: GuardianSession) => void;
}

/**
 * Signs Guardian requests for a wallet after its session grant was
 * registered. Plug into `GuardianHttpClient.setSession`.
 */
export class GuardianSession implements SessionRequestSigner {
  constructor(
    private readonly key: WebCryptoSessionKey,
    /** Commitment of the wallet that signed the grant. */
    readonly signerCommitment: string,
    /** Unix seconds. */
    readonly expiresAt: number,
    /** Where a `SessionKeyStore` keeps this session's key. */
    readonly storeId: string,
    private readonly options: GuardianSessionOptions = {},
  ) {}

  get publicKey(): string {
    return this.key.publicKey;
  }

  /**
   * Forgets the stored key (best effort: a dead key left behind fails its
   * first request after a reload and ends then) and tells the app.
   */
  onEnded(reason: SessionEndReason): void {
    if (this.options.store) {
      void forgetSessionKey(this.options.store, this).catch(() => undefined);
    }
    this.options.onEnded?.(reason, this);
  }

  signRequest(
    accountId: string,
    timestamp: number,
    requestPayload: RequestAuthPayload,
  ): Promise<string> {
    return this.key.signWord(AuthDigest.fromRequest(accountId, timestamp, requestPayload));
  }

  signLogout(timestampMs: number): Promise<string> {
    return this.key.signWord(sessionLogoutDigest(this.key.publicKey, timestampMs));
  }
}

/** A session key persisted across page loads. */
export interface StoredSessionKey {
  /** `<guardian key commitment>:<signer commitment>`. */
  id: string;
  privateKey: CryptoKey;
  publicKey: string;
  /** Unix seconds. */
  expiresAt: number;
}

export interface SessionKeyStore {
  load(id: string): Promise<StoredSessionKey | null>;
  save(record: StoredSessionKey): Promise<void>;
  remove(id: string): Promise<void>;
}

/**
 * Remove the stored key of `session`, unless the record already holds the key
 * of a newer session for the same wallet and Guardian.
 */
export async function forgetSessionKey(
  store: SessionKeyStore,
  session: { storeId: string; publicKey: string },
): Promise<void> {
  const record = await store.load(session.storeId);
  if (record?.publicKey === session.publicKey) {
    await store.remove(session.storeId);
  }
}

/**
 * Keeps session keys in IndexedDB so a reload does not need another wallet
 * signature. The browser stores the `CryptoKey` object itself; it stays
 * non-extractable.
 */
export class IndexedDbSessionKeyStore implements SessionKeyStore {
  private static readonly STORE = 'sessions';
  private database: Promise<IDBDatabase> | null = null;

  constructor(private readonly databaseName = 'guardian-sessions') {}

  async load(id: string): Promise<StoredSessionKey | null> {
    const record = await this.request<StoredSessionKey | undefined>('readonly', (store) =>
      store.get(id),
    );
    return record ?? null;
  }

  async save(record: StoredSessionKey): Promise<void> {
    await this.request('readwrite', (store) => store.put(record));
  }

  async remove(id: string): Promise<void> {
    await this.request('readwrite', (store) => store.delete(id));
  }

  private async request<T>(
    mode: IDBTransactionMode,
    run: (store: IDBObjectStore) => IDBRequest,
  ): Promise<T> {
    const database = await this.open();
    return new Promise<T>((resolve, reject) => {
      const transaction = database.transaction(IndexedDbSessionKeyStore.STORE, mode);
      const request = run(transaction.objectStore(IndexedDbSessionKeyStore.STORE));
      transaction.oncomplete = () => resolve(request.result as T);
      transaction.onerror = () => reject(transaction.error ?? request.error);
      transaction.onabort = () => reject(transaction.error ?? request.error);
    });
  }

  /** Opens the database once; a failed open is retried on the next call. */
  private open(): Promise<IDBDatabase> {
    if (!this.database) {
      const opening = new Promise<IDBDatabase>((resolve, reject) => {
        if (!globalThis.indexedDB) {
          reject(new Error('IndexedDB is not available'));
          return;
        }
        const request = globalThis.indexedDB.open(this.databaseName, 1);
        request.onupgradeneeded = () => {
          request.result.createObjectStore(IndexedDbSessionKeyStore.STORE, { keyPath: 'id' });
        };
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
      });
      opening.catch(() => {
        if (this.database === opening) {
          this.database = null;
        }
      });
      this.database = opening;
    }
    return this.database;
  }
}
