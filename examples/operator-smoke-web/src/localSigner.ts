import { AuthSecretKey, MidenClient, Word } from '@miden-sdk/miden-sdk';
import { useCallback, useEffect, useRef, useState } from 'react';
import { bytesToHex } from './encoding';

const LOCAL_SIGNER_STORAGE_KEY = 'guardian.operatorSmoke.localFalconSecretKey.v1';

export interface LocalFalconSignerState {
  ready: boolean;
  publicKey: string | null;
  publicKeyLength: number | null;
  commitment: string | null;
  persisted: boolean;
}

let midenRuntimeReady: Promise<void> | null = null;

function emptyLocalSignerState(): LocalFalconSignerState {
  return {
    ready: false,
    publicKey: null,
    publicKeyLength: null,
    commitment: null,
    persisted: false,
  };
}

function getStorage(): Storage | null {
  if (typeof window === 'undefined') {
    return null;
  }

  return window.localStorage;
}

function uint8ArrayToBase64(bytes: Uint8Array): string {
  let binary = '';
  for (let i = 0; i < bytes.length; i += 1) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary);
}

function base64ToUint8Array(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

async function ensureMidenRuntime(): Promise<void> {
  if (midenRuntimeReady) {
    return midenRuntimeReady;
  }

  midenRuntimeReady = MidenClient.createMock()
    .then((client) => {
      client.terminate();
    })
    .catch((error) => {
      midenRuntimeReady = null;
      throw error;
    });

  return midenRuntimeReady;
}

function serializeSecretKey(secretKey: AuthSecretKey): string {
  return uint8ArrayToBase64(secretKey.serialize());
}

function deserializeSecretKey(serialized: string): AuthSecretKey {
  return AuthSecretKey.deserialize(base64ToUint8Array(serialized));
}

function deriveState(secretKey: AuthSecretKey): LocalFalconSignerState {
  const publicKey = secretKey.publicKey();
  const serializedPublicKey = publicKey.serialize();

  return {
    ready: true,
    publicKey: bytesToHex(serializedPublicKey.slice(1)),
    publicKeyLength: serializedPublicKey.length - 1,
    commitment: publicKey.toCommitment().toHex(),
    persisted: true,
  };
}

export function useLocalFalconSigner() {
  const [session, setSession] = useState<LocalFalconSignerState>(emptyLocalSignerState);
  const [sessionError, setSessionError] = useState<string | null>(null);
  const secretKeyRef = useRef<AuthSecretKey | null>(null);

  const restore = useCallback(async (): Promise<LocalFalconSignerState> => {
    const serializedSecretKey = getStorage()?.getItem(LOCAL_SIGNER_STORAGE_KEY);
    if (!serializedSecretKey) {
      secretKeyRef.current = null;
      setSessionError(null);
      const nextState = emptyLocalSignerState();
      setSession(nextState);
      return nextState;
    }

    await ensureMidenRuntime();
    const secretKey = deserializeSecretKey(serializedSecretKey);
    secretKeyRef.current = secretKey;
    const nextState = deriveState(secretKey);
    setSessionError(null);
    setSession(nextState);
    return nextState;
  }, []);

  useEffect(() => {
    void restore().catch((error: unknown) => {
      const message = error instanceof Error ? error.message : String(error);
      secretKeyRef.current = null;
      getStorage()?.removeItem(LOCAL_SIGNER_STORAGE_KEY);
      setSession(emptyLocalSignerState());
      setSessionError(`Failed to restore local Falcon signer: ${message}`);
    });
  }, [restore]);

  const generate = useCallback(async (): Promise<LocalFalconSignerState> => {
    await ensureMidenRuntime();
    const secretKey = AuthSecretKey.rpoFalconWithRNG(undefined);
    getStorage()?.setItem(LOCAL_SIGNER_STORAGE_KEY, serializeSecretKey(secretKey));
    secretKeyRef.current = secretKey;
    const nextState = deriveState(secretKey);
    setSessionError(null);
    setSession(nextState);
    return nextState;
  }, []);

  const clear = useCallback(async (): Promise<LocalFalconSignerState> => {
    getStorage()?.removeItem(LOCAL_SIGNER_STORAGE_KEY);
    secretKeyRef.current = null;
    setSessionError(null);
    const nextState = emptyLocalSignerState();
    setSession(nextState);
    return nextState;
  }, []);

  const signWordHex = useCallback(
    async (wordHex: string): Promise<string> => {
      const secretKey = secretKeyRef.current;
      if (!secretKey) {
        throw new Error('Local Falcon signer is unavailable');
      }

      const word = Word.fromHex(wordHex);
      const signature = secretKey.sign(word);
      return bytesToHex(signature.serialize().slice(1));
    },
    [],
  );

  return {
    session,
    sessionError,
    restore,
    generate,
    clear,
    signWordHex,
  };
}
