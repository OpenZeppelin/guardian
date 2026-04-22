import { useCallback, useEffect, useRef, useState, type SetStateAction } from 'react';
import { useModal } from '@getpara/react-sdk-lite';
import { MidenWalletAdapter } from '@demox-labs/miden-wallet-adapter-miden';
import type { MidenClient } from '@miden-sdk/miden-sdk';
import {
  AccountInspector,
  type AccountState,
  type ConsumableNote,
  type DetectedMultisigConfig,
  type Multisig,
  type MultisigClient,
  type ProcedureName,
  type ProcedureThreshold,
  type Proposal,
  type SignatureScheme,
} from '@openzeppelin/miden-multisig-client';
import {
  classifyWalletError,
  clearIndexedDbDatabasesByPrefix,
  createAddSignerProposal,
  createChangeThresholdProposal,
  createConsumeNotesProposal,
  createMultisigAccount,
  createP2idProposal,
  createRemoveSignerProposal,
  createSwitchGuardianProposal,
  createUpdateProcedureThresholdProposal,
  createMidenClient,
  executeProposal as executeOnlineProposal,
  exportProposalToJson,
  fetchAccountState,
  importProposal as importStoredProposal,
  initMultisigClient,
  initializeLocalSigners,
  listVisibleProposals,
  loadMultisigAccount,
  normalizeCommitment,
  normalizeError,
  registerOnGuardian as registerOnlineAccount,
  registerOnGuardianWithState,
  resolveLocalSigner,
  resolveMidenWalletSigner,
  resolveParaSigner,
  serializeConsumableNote,
  serializeDetectedMultisigConfig,
  serializeExternalWalletState,
  serializeProposal,
  serializeSignerInfo,
  signProposal as signOnlineProposal,
  signProposalOffline as signOfflineProposal,
  syncAll,
  useMidenWallet,
  useParaSession,
  verifyStateCommitment,
  type BrowserSessionSnapshot,
  type ExternalWalletState,
  type ResolvedSigner,
  type SignerInfo,
  type SmokeBootStatus,
  type SmokeEventEntry,
  type WalletSource,
} from '@multisig-browser/index';
import {
  DEFAULT_APP_NAME,
  DEFAULT_BROWSER_LABEL,
  DEFAULT_GUARDIAN_ENDPOINT,
  DEFAULT_MIDEN_DB_NAME,
  DEFAULT_MIDEN_RPC_URL,
} from './config';

export interface SessionConfig {
  guardianEndpoint: string;
  midenRpcEndpoint: string;
  signerSource: WalletSource;
  signatureScheme: SignatureScheme;
  browserLabel: string;
}

export interface InitSessionInput {
  guardianEndpoint?: string;
  midenRpcEndpoint?: string;
  signerSource?: WalletSource;
  signatureScheme?: SignatureScheme;
  browserLabel?: string;
}

export interface CreateAccountInput {
  threshold: number;
  otherCommitments: string[];
  guardianCommitment?: string;
  procedureThresholds?: ProcedureThreshold[];
}

export type CreateProposalInput =
  | { type: 'add_signer'; commitment: string; increaseThreshold?: boolean }
  | { type: 'remove_signer'; signerCommitment: string; newThreshold?: number }
  | { type: 'change_threshold'; newThreshold: number }
  | { type: 'update_procedure_threshold'; procedure: ProcedureName; threshold: number }
  | { type: 'consume_notes'; noteIds: string[] }
  | { type: 'p2id'; recipientId: string; faucetId: string; amount: string | number }
  | { type: 'switch_guardian'; newGuardianEndpoint: string; newGuardianPubkey: string };

export interface SignProposalOfflineInput {
  proposalId?: string;
  json?: string;
}

export interface SmokeApi {
  initSession(input: InitSessionInput): Promise<BrowserSessionSnapshot>;
  connectPara(): Promise<BrowserSessionSnapshot>;
  connectMidenWallet(): Promise<BrowserSessionSnapshot>;
  status(): Promise<BrowserSessionSnapshot>;
  createAccount(input: CreateAccountInput): Promise<BrowserSessionSnapshot>;
  loadAccount(input: { accountId: string }): Promise<BrowserSessionSnapshot>;
  registerOnGuardian(input?: { stateDataBase64?: string }): Promise<BrowserSessionSnapshot>;
  sync(): Promise<BrowserSessionSnapshot>;
  fetchState(): Promise<{
    state: AccountState;
    config: ReturnType<typeof serializeDetectedMultisigConfig>;
    status: BrowserSessionSnapshot;
  }>;
  verifyStateCommitment(): Promise<{
    accountId: string;
    localCommitment: string;
    onChainCommitment: string;
  }>;
  listConsumableNotes(): Promise<ReturnType<typeof serializeConsumableNote>[]>;
  listProposals(): Promise<ReturnType<typeof serializeProposal>[]>;
  createProposal(input: CreateProposalInput): Promise<{
    proposal: ReturnType<typeof serializeProposal>;
    proposals: Array<ReturnType<typeof serializeProposal>>;
  }>;
  signProposal(input: { proposalId: string }): Promise<Array<ReturnType<typeof serializeProposal>>>;
  executeProposal(input: { proposalId: string }): Promise<BrowserSessionSnapshot>;
  exportProposal(input: { proposalId: string }): Promise<{ json: string }>;
  signProposalOffline(input: SignProposalOfflineInput): Promise<{
    proposalId: string;
    json: string;
    proposals: Array<ReturnType<typeof serializeProposal>>;
  }>;
  importProposal(input: { json: string }): Promise<{
    proposal: ReturnType<typeof serializeProposal>;
    proposals: Array<ReturnType<typeof serializeProposal>>;
  }>;
  clearLocalState(): Promise<BrowserSessionSnapshot>;
  events(): Promise<SmokeEventEntry[]>;
}

interface SnapshotState {
  sessionConfig: SessionConfig;
  webClient: MidenClient | null;
  multisigClient: MultisigClient | null;
  bootStatus: SmokeBootStatus;
  bootError: string | null;
  guardianPubkey: string | null;
  localSigners: SignerInfo | null;
  paraSession: ExternalWalletState;
  midenWalletSession: ExternalWalletState;
  multisig: Multisig | null;
  guardianState: AccountState | null;
  detectedConfig: DetectedMultisigConfig | null;
  proposals: Proposal[];
  consumableNotes: ConsumableNote[];
  lastError: string | null;
  busyAction: string | null;
}

const defaultSessionConfig: SessionConfig = {
  guardianEndpoint: DEFAULT_GUARDIAN_ENDPOINT,
  midenRpcEndpoint: DEFAULT_MIDEN_RPC_URL,
  signerSource: 'local',
  signatureScheme: 'falcon',
  browserLabel: DEFAULT_BROWSER_LABEL,
};

const BOOT_TIMEOUT_MS = 30_000;

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    window.setTimeout(resolve, ms);
  });
}

async function withTimeout<T>(
  promise: Promise<T>,
  timeoutMs: number,
  timeoutMessage: string,
): Promise<T> {
  let timeoutId: number | null = null;

  try {
    return await Promise.race([
      promise,
      new Promise<T>((_, reject) => {
        timeoutId = window.setTimeout(() => {
          reject(new Error(timeoutMessage));
        }, timeoutMs);
      }),
    ]);
  } finally {
    if (timeoutId !== null) {
      window.clearTimeout(timeoutId);
    }
  }
}

async function waitForCondition(
  condition: () => boolean,
  timeoutMs = 60_000,
  intervalMs = 250,
): Promise<void> {
  const startedAt = Date.now();
  while (!condition()) {
    if (Date.now() - startedAt > timeoutMs) {
      throw new Error('Timed out waiting for wallet connection');
    }
    await sleep(intervalMs);
  }
}

async function syncBrowserClientState(client: MidenClient): Promise<void> {
  try {
    await client.sync();
  } catch {
    await sleep(500);
    await client.sync();
  }
}

function normalizeAccountId(accountId: string): string {
  const trimmed = accountId.trim();
  if (!trimmed) {
    throw new Error('Account ID is required');
  }

  return trimmed.startsWith('0x') ? trimmed : `0x${trimmed}`;
}

function applySignatureScheme(
  signers: SignerInfo | null,
  signatureScheme: SignatureScheme,
): SignerInfo | null {
  if (!signers) {
    return null;
  }

  return {
    ...signers,
    activeScheme: signatureScheme,
  };
}

function buildSnapshot(state: SnapshotState): BrowserSessionSnapshot {
  return {
    browserLabel: state.sessionConfig.browserLabel || null,
    initialized: Boolean(
      state.webClient && state.multisigClient && state.localSigners && state.guardianPubkey,
    ),
    bootStatus: state.bootStatus,
    bootError: state.bootError,
    guardianEndpoint: state.sessionConfig.guardianEndpoint,
    midenRpcEndpoint: state.sessionConfig.midenRpcEndpoint,
    signerSource: state.sessionConfig.signerSource,
    signatureScheme: state.sessionConfig.signatureScheme,
    guardianPubkey: state.guardianPubkey,
    localSigners: state.localSigners ? serializeSignerInfo(state.localSigners) : null,
    para: serializeExternalWalletState(state.paraSession),
    midenWallet: serializeExternalWalletState(state.midenWalletSession),
    multisig: state.multisig
      ? {
          accountId: state.multisig.accountId,
          signerCommitment: state.multisig.signerCommitment,
          threshold: state.multisig.threshold,
          signerCommitments: [...state.multisig.signerCommitments],
          guardianCommitment: state.multisig.guardianCommitment,
          procedureThresholds: [...state.multisig.procedureThresholds.entries()]
            .map(([procedure, threshold]) => ({ procedure, threshold }))
            .sort((left, right) => left.procedure.localeCompare(right.procedure)),
        }
      : null,
    guardianState: state.guardianState,
    detectedConfig: state.detectedConfig
      ? serializeDetectedMultisigConfig(state.detectedConfig)
      : null,
    proposals: state.proposals.map(serializeProposal),
    consumableNotes: state.consumableNotes.map(serializeConsumableNote),
    lastError: state.lastError,
    busyAction: state.busyAction,
  };
}

function normalizeSessionInput(input: InitSessionInput): SessionConfig {
  return {
    guardianEndpoint: input.guardianEndpoint?.trim() || DEFAULT_GUARDIAN_ENDPOINT,
    midenRpcEndpoint: input.midenRpcEndpoint?.trim() || DEFAULT_MIDEN_RPC_URL,
    signerSource: input.signerSource ?? 'local',
    signatureScheme: input.signatureScheme ?? 'falcon',
    browserLabel: input.browserLabel?.trim() ?? DEFAULT_BROWSER_LABEL,
  };
}

function useStateRef<T>(
  initialValue: T,
): [T, React.MutableRefObject<T>, (value: SetStateAction<T>) => void] {
  const [value, setValueState] = useState(initialValue);
  const valueRef = useRef(initialValue);

  const setValue = useCallback((nextValue: SetStateAction<T>) => {
    const resolvedValue =
      typeof nextValue === 'function'
        ? (nextValue as (current: T) => T)(valueRef.current)
        : nextValue;
    valueRef.current = resolvedValue;
    setValueState(resolvedValue);
  }, []);

  return [value, valueRef, setValue];
}

export function useSmokeHarness(): {
  api: SmokeApi;
  snapshot: BrowserSessionSnapshot;
  events: SmokeEventEntry[];
  midenWalletConnectError: string | null;
  disconnectMidenWallet: () => Promise<void>;
} {
  const [sessionConfig, sessionConfigRef, setSessionConfig] =
    useStateRef<SessionConfig>(defaultSessionConfig);
  const [webClient, webClientRef, setWebClient] = useStateRef<MidenClient | null>(null);
  const [multisigClient, multisigClientRef, setMultisigClient] =
    useStateRef<MultisigClient | null>(null);
  const [bootStatus, bootStatusRef, setBootStatus] = useStateRef<SmokeBootStatus>(
    'initializing',
  );
  const [bootError, bootErrorRef, setBootError] = useStateRef<string | null>(null);
  const [localSigners, localSignersRef, setLocalSigners] = useStateRef<SignerInfo | null>(null);
  const [guardianPubkey, guardianPubkeyRef, setGuardianPubkey] = useStateRef<string | null>(null);
  const [multisig, multisigRef, setMultisig] = useStateRef<Multisig | null>(null);
  const [guardianState, guardianStateRef, setGuardianState] =
    useStateRef<AccountState | null>(null);
  const [detectedConfig, detectedConfigRef, setDetectedConfig] =
    useStateRef<DetectedMultisigConfig | null>(null);
  const [proposals, proposalsRef, setProposals] = useStateRef<Proposal[]>([]);
  const [consumableNotes, consumableNotesRef, setConsumableNotes] =
    useStateRef<ConsumableNote[]>([]);
  const [busyAction, busyActionRef, setBusyAction] = useStateRef<string | null>(null);
  const [lastError, lastErrorRef, setLastError] = useStateRef<string | null>(null);
  const [events, setEvents] = useState<SmokeEventEntry[]>([]);
  const eventIdRef = useRef(0);
  const eventsRef = useRef<SmokeEventEntry[]>([]);
  const bootGenerationRef = useRef(0);
  const bootStartedRef = useRef(false);
  const bootTaskRef = useRef<Promise<void> | null>(null);
  const [midenWalletAdapter] = useState(
    () => new MidenWalletAdapter({ appName: DEFAULT_APP_NAME }),
  );
  const { openModal } = useModal();
  const {
    session: paraSession,
    paraClient,
    walletId: paraWalletId,
  } = useParaSession(sessionConfig.midenRpcEndpoint);
  const {
    session: midenWalletSession,
    connect: connectMidenWallet,
    disconnect: disconnectMidenWallet,
    signBytes,
    connectError: midenWalletConnectError,
  } = useMidenWallet(midenWalletAdapter);
  const paraSessionRef = useRef(paraSession);
  const midenWalletSessionRef = useRef(midenWalletSession);

  useEffect(() => {
    paraSessionRef.current = paraSession;
  }, [paraSession]);

  useEffect(() => {
    midenWalletSessionRef.current = midenWalletSession;
  }, [midenWalletSession]);

  const appendEvent = useCallback(
    (
      action: string,
      outcome: SmokeEventEntry['outcome'],
      error: string | null,
      durationMs: number,
    ): SmokeEventEntry => {
      const entry: SmokeEventEntry = {
        id: ++eventIdRef.current,
        timestamp: new Date().toISOString(),
        action,
        outcome,
        error,
        durationMs: Math.round(durationMs),
      };
      const nextEvents = [...eventsRef.current, entry];
      eventsRef.current = nextEvents;
      setEvents(nextEvents);
      return entry;
    },
    [],
  );

  const buildCurrentSnapshot = useCallback(
    (
      overrides: Partial<SnapshotState> = {},
    ): BrowserSessionSnapshot =>
      buildSnapshot({
        sessionConfig: sessionConfigRef.current,
        webClient: webClientRef.current,
        multisigClient: multisigClientRef.current,
        bootStatus: bootStatusRef.current,
        bootError: bootErrorRef.current,
        guardianPubkey: guardianPubkeyRef.current,
        localSigners: localSignersRef.current,
        paraSession: paraSessionRef.current,
        midenWalletSession: midenWalletSessionRef.current,
        multisig: multisigRef.current,
        guardianState: guardianStateRef.current,
        detectedConfig: detectedConfigRef.current,
        proposals: proposalsRef.current,
        consumableNotes: consumableNotesRef.current,
        lastError: lastErrorRef.current,
        busyAction: busyActionRef.current,
        ...overrides,
      }),
    [
      busyActionRef,
      bootErrorRef,
      bootStatusRef,
      consumableNotesRef,
      detectedConfigRef,
      guardianPubkeyRef,
      guardianStateRef,
      lastErrorRef,
      localSignersRef,
      midenWalletSessionRef,
      multisigClientRef,
      multisigRef,
      paraSessionRef,
      proposalsRef,
      sessionConfigRef,
      webClientRef,
    ],
  );

  const clearLoadedAccountState = useCallback(() => {
    setMultisig(null);
    setGuardianState(null);
    setDetectedConfig(null);
    setProposals([]);
    setConsumableNotes([]);
  }, []);

  const clearSessionCore = useCallback(() => {
    setWebClient(null);
    setMultisigClient(null);
    setLocalSigners(null);
    setGuardianPubkey(null);
    clearLoadedAccountState();
  }, [clearLoadedAccountState]);

  const requireSessionReady = useCallback(() => {
    if (bootStatusRef.current === 'initializing') {
      throw new Error('Session is still booting');
    }

    if (bootStatusRef.current === 'error') {
      throw new Error(bootErrorRef.current ?? 'Session boot failed');
    }

    if (
      !webClientRef.current ||
      !multisigClientRef.current ||
      !localSignersRef.current ||
      !guardianPubkeyRef.current
    ) {
      throw new Error('Session is not ready');
    }
  }, [bootErrorRef, bootStatusRef, guardianPubkeyRef, localSignersRef, multisigClientRef, webClientRef]);

  const resolveSignerContext = useCallback(
    (
      source: WalletSource = sessionConfigRef.current.signerSource,
      signatureScheme: SignatureScheme = sessionConfigRef.current.signatureScheme,
    ): ResolvedSigner => {
      const currentParaSession = paraSessionRef.current;
      const currentMidenWalletSession = midenWalletSessionRef.current;

      if (source === 'para') {
        if (!paraClient || !currentParaSession.commitment || !currentParaSession.publicKey) {
          throw new Error('Para wallet is not connected');
        }

        if (!paraWalletId) {
          throw new Error('Para wallet did not expose a wallet id');
        }

        return resolveParaSigner({
          paraClient,
          walletId: paraWalletId,
          commitment: currentParaSession.commitment,
          publicKey: currentParaSession.publicKey,
        });
      }

      if (source === 'miden-wallet') {
        if (
          !currentMidenWalletSession.commitment ||
          !currentMidenWalletSession.publicKey ||
          !currentMidenWalletSession.scheme
        ) {
          throw new Error('Miden Wallet is not connected');
        }

        return resolveMidenWalletSigner({
          wallet: { signBytes },
          commitment: currentMidenWalletSession.commitment,
          publicKey: currentMidenWalletSession.publicKey,
          scheme: currentMidenWalletSession.scheme,
        });
      }

      if (!localSignersRef.current) {
        throw new Error('Local signers are not initialized');
      }

      return resolveLocalSigner(localSignersRef.current, signatureScheme);
    },
    [
      localSignersRef,
      midenWalletSessionRef,
      paraClient,
      paraSessionRef,
      paraWalletId,
      sessionConfigRef,
      signBytes,
    ],
  );

  const refreshMultisigState = useCallback(
    async (
      targetMultisig: Multisig,
      targetClient?: MidenClient,
    ): Promise<{
      state: AccountState;
      config: DetectedMultisigConfig;
      proposals: Proposal[];
      notes: ConsumableNote[];
    }> => {
      const activeClient = targetClient ?? webClientRef.current;
      if (!activeClient) {
        throw new Error('MidenClient is not initialized');
      }

      await syncBrowserClientState(activeClient);
      const synced = await syncAll(targetMultisig);
      const config = AccountInspector.fromBase64(synced.state.stateDataBase64);
      setGuardianState(synced.state);
      setDetectedConfig(config);
      setProposals(synced.proposals);
      setConsumableNotes(synced.notes);

      return {
        state: synced.state,
        config,
        proposals: synced.proposals,
        notes: synced.notes,
      };
    },
    [webClientRef],
  );

  const withCommand = useCallback(
    async <T,>(action: string, handler: () => Promise<T>): Promise<T> => {
      const startedAt = performance.now();
      setBusyAction(action);
      setLastError(null);

      try {
        const result = await handler();
        appendEvent(action, 'succeeded', null, performance.now() - startedAt);
        return result;
      } catch (err) {
        const message = normalizeError(err);
        setLastError(message);
        appendEvent(action, 'failed', message, performance.now() - startedAt);
        throw err instanceof Error ? err : new Error(message);
      } finally {
        setBusyAction((current) => (current === action ? null : current));
      }
    },
    [appendEvent],
  );

  const bootSession = useCallback(
    async (nextConfig: SessionConfig, action: string): Promise<BrowserSessionSnapshot> => {
      const previousBoot = bootTaskRef.current;
      if (previousBoot) {
        await previousBoot.catch(() => undefined);
      }

      const startedAt = performance.now();
      const generation = ++bootGenerationRef.current;

      setBusyAction(action);
      setLastError(null);
      setBootStatus('initializing');
      setBootError(null);
      setSessionConfig(nextConfig);
      clearSessionCore();

      const currentBoot = (async (): Promise<BrowserSessionSnapshot> => {
        try {
          return await withTimeout(
            (async () => {
              await clearIndexedDbDatabasesByPrefix([DEFAULT_MIDEN_DB_NAME]);

              const nextClient = await createMidenClient(
                nextConfig.midenRpcEndpoint,
                DEFAULT_MIDEN_DB_NAME,
              );
              const {
                client: nextMultisigClient,
                guardianPubkey: nextGuardianPubkey,
              } = await initMultisigClient(
                nextClient,
                nextConfig.guardianEndpoint,
                nextConfig.midenRpcEndpoint,
              );
              const nextSigners = applySignatureScheme(
                await initializeLocalSigners(),
                nextConfig.signatureScheme,
              );

              if (!nextSigners) {
                throw new Error('Failed to initialize local signers');
              }

              if (bootGenerationRef.current !== generation) {
                return buildCurrentSnapshot({
                  sessionConfig: nextConfig,
                  webClient: nextClient,
                  multisigClient: nextMultisigClient,
                  bootStatus: 'ready',
                  bootError: null,
                  guardianPubkey: nextGuardianPubkey,
                  localSigners: nextSigners,
                  multisig: null,
                  guardianState: null,
                  detectedConfig: null,
                  proposals: [],
                  consumableNotes: [],
                  lastError: null,
                });
              }

              setWebClient(nextClient);
              setMultisigClient(nextMultisigClient);
              setGuardianPubkey(nextGuardianPubkey);
              setLocalSigners(nextSigners);
              setBootStatus('ready');
              setBootError(null);
              setLastError(null);

              appendEvent(action, 'succeeded', null, performance.now() - startedAt);

              return buildCurrentSnapshot({
                sessionConfig: nextConfig,
                webClient: nextClient,
                multisigClient: nextMultisigClient,
                bootStatus: 'ready',
                bootError: null,
                guardianPubkey: nextGuardianPubkey,
                localSigners: nextSigners,
                multisig: null,
                guardianState: null,
                detectedConfig: null,
                proposals: [],
                consumableNotes: [],
                lastError: null,
              });
            })(),
            BOOT_TIMEOUT_MS,
            'Session bootstrap timed out',
          );
        } catch (err) {
          const message = normalizeError(err);

          if (bootGenerationRef.current === generation) {
            clearSessionCore();
            setBootStatus('error');
            setBootError(message);
            setLastError(message);
          }

          appendEvent(action, 'failed', message, performance.now() - startedAt);
          throw err instanceof Error ? err : new Error(message);
        } finally {
          if (bootGenerationRef.current === generation) {
            setBusyAction((current) => (current === action ? null : current));
          }
        }
      })();

      const trackedBoot = currentBoot.then(
        () => undefined,
        () => undefined,
      );
      bootTaskRef.current = trackedBoot;

      try {
        return await currentBoot;
      } finally {
        if (bootTaskRef.current === trackedBoot) {
          bootTaskRef.current = null;
        }
      }
    },
    [appendEvent, buildCurrentSnapshot, clearSessionCore],
  );

  const initSession = useCallback(
    async (input: InitSessionInput): Promise<BrowserSessionSnapshot> =>
      bootSession(normalizeSessionInput(input), 'initSession'),
    [bootSession],
  );

  const connectParaSession = useCallback(
    async (): Promise<BrowserSessionSnapshot> =>
      withCommand('connectPara', async () => {
        if (!paraSessionRef.current.connected) {
          openModal();
          await waitForCondition(() => paraSessionRef.current.connected);
        }

        const nextConfig: SessionConfig = {
          ...sessionConfigRef.current,
          signerSource: 'para',
          signatureScheme: 'ecdsa',
        };
        setSessionConfig(nextConfig);

        return buildCurrentSnapshot({
          sessionConfig: nextConfig,
          paraSession: paraSessionRef.current,
          lastError: null,
        });
      }),
    [buildCurrentSnapshot, openModal, sessionConfigRef, withCommand],
  );

  const connectMidenWalletSession = useCallback(
    async (): Promise<BrowserSessionSnapshot> =>
      withCommand('connectMidenWallet', async () => {
        await connectMidenWallet();
        await waitForCondition(() => midenWalletSessionRef.current.connected);
        const scheme = midenWalletSessionRef.current.scheme ?? sessionConfigRef.current.signatureScheme;
        const nextConfig: SessionConfig = {
          ...sessionConfigRef.current,
          signerSource: 'miden-wallet',
          signatureScheme: scheme,
        };
        setSessionConfig(nextConfig);

        return buildCurrentSnapshot({
          sessionConfig: nextConfig,
          midenWalletSession: midenWalletSessionRef.current,
          lastError: null,
        });
      }),
    [buildCurrentSnapshot, connectMidenWallet, sessionConfigRef, withCommand],
  );

  const status = useCallback(
    async (): Promise<BrowserSessionSnapshot> =>
      withCommand('status', async () => buildCurrentSnapshot()),
    [buildCurrentSnapshot, withCommand],
  );

  const createAccount = useCallback(
    async (input: CreateAccountInput): Promise<BrowserSessionSnapshot> =>
      withCommand('createAccount', async () => {
        requireSessionReady();
        const currentMultisigClient = multisigClientRef.current as MultisigClient;

        const signerContext = resolveSignerContext();
        const normalizedGuardianCommitment = input.guardianCommitment
          ? normalizeCommitment(input.guardianCommitment)
          : normalizeCommitment(
              (await currentMultisigClient.guardianClient.getPubkey(signerContext.signatureScheme))
                .commitment,
            );
        const normalizedOtherCommitments = input.otherCommitments.map(normalizeCommitment);
        const nextMultisig = await createMultisigAccount(
          currentMultisigClient,
          signerContext,
          normalizedOtherCommitments,
          input.threshold,
          normalizedGuardianCommitment,
          input.procedureThresholds,
          signerContext.signatureScheme,
        );
        const nextSigners = applySignatureScheme(
          localSignersRef.current,
          signerContext.signatureScheme,
        );

        setMultisig(nextMultisig);
        setLocalSigners(nextSigners);
        setGuardianState(null);
        setDetectedConfig(null);
        setProposals([]);
        setConsumableNotes([]);
        setLastError(null);

        return buildCurrentSnapshot({
          localSigners: nextSigners,
          multisig: nextMultisig,
          guardianState: null,
          detectedConfig: null,
          proposals: [],
          consumableNotes: [],
          lastError: null,
        });
      }),
    [buildCurrentSnapshot, localSignersRef, multisigClientRef, resolveSignerContext, withCommand],
  );

  const loadAccount = useCallback(
    async ({ accountId }: { accountId: string }): Promise<BrowserSessionSnapshot> =>
      withCommand('loadAccount', async () => {
        requireSessionReady();
        const currentMultisigClient = multisigClientRef.current as MultisigClient;

        const signerContext = resolveSignerContext();
        const normalizedId = normalizeAccountId(accountId);
        const loaded = await loadMultisigAccount(currentMultisigClient, normalizedId, signerContext);
        const nextSigners = applySignatureScheme(
          localSignersRef.current,
          signerContext.signatureScheme,
        );
        setMultisig(loaded);
        setLocalSigners(nextSigners);
        const refreshed = await refreshMultisigState(loaded);

        return buildCurrentSnapshot({
          localSigners: nextSigners,
          multisig: loaded,
          guardianState: refreshed.state,
          detectedConfig: refreshed.config,
          proposals: refreshed.proposals,
          consumableNotes: refreshed.notes,
          lastError: null,
        });
      }),
    [
      buildCurrentSnapshot,
      localSignersRef,
      multisigClientRef,
      refreshMultisigState,
      resolveSignerContext,
      withCommand,
    ],
  );

  const registerOnGuardian = useCallback(
    async (
      input: { stateDataBase64?: string } = {},
    ): Promise<BrowserSessionSnapshot> =>
      withCommand('registerOnGuardian', async () => {
        requireSessionReady();
        const currentMultisig = multisigRef.current;
        if (!currentMultisig) {
          throw new Error('No multisig account is loaded');
        }

        const stateDataBase64 = input?.stateDataBase64?.trim();
        if (stateDataBase64) {
          await registerOnGuardianWithState(currentMultisig, stateDataBase64);
        } else {
          await registerOnlineAccount(currentMultisig);
        }

        const refreshed = await refreshMultisigState(currentMultisig);
        return buildCurrentSnapshot({
          guardianState: refreshed.state,
          detectedConfig: refreshed.config,
          proposals: refreshed.proposals,
          consumableNotes: refreshed.notes,
          lastError: null,
        });
      }),
    [buildCurrentSnapshot, multisigRef, refreshMultisigState, withCommand],
  );

  const sync = useCallback(
    async (): Promise<BrowserSessionSnapshot> =>
      withCommand('sync', async () => {
        requireSessionReady();
        const currentMultisig = multisigRef.current;
        if (!currentMultisig) {
          throw new Error('No multisig account is loaded');
        }

        const refreshed = await refreshMultisigState(currentMultisig);
        return buildCurrentSnapshot({
          guardianState: refreshed.state,
          detectedConfig: refreshed.config,
          proposals: refreshed.proposals,
          consumableNotes: refreshed.notes,
          lastError: null,
        });
      }),
    [buildCurrentSnapshot, multisigRef, refreshMultisigState, withCommand],
  );

  const fetchState = useCallback(
    async (): Promise<{
      state: AccountState;
      config: ReturnType<typeof serializeDetectedMultisigConfig>;
      status: BrowserSessionSnapshot;
    }> =>
      withCommand('fetchState', async () => {
        requireSessionReady();
        const currentMultisig = multisigRef.current;
        if (!currentMultisig) {
          throw new Error('No multisig account is loaded');
        }

        const { state, config } = await fetchAccountState(currentMultisig);
        setGuardianState(state);
        setDetectedConfig(config);
        const statusSnapshot = buildCurrentSnapshot({
          guardianState: state,
          detectedConfig: config,
          lastError: null,
        });

        return {
          state,
          config: serializeDetectedMultisigConfig(config),
          status: statusSnapshot,
        };
      }),
    [buildCurrentSnapshot, multisigRef, withCommand],
  );

  const verifyState = useCallback(
    async (): Promise<{
      accountId: string;
      localCommitment: string;
      onChainCommitment: string;
    }> =>
      withCommand('verifyStateCommitment', async () => {
        requireSessionReady();
        const currentMultisig = multisigRef.current;
        if (!currentMultisig) {
          throw new Error('No multisig account is loaded');
        }

        return verifyStateCommitment(currentMultisig);
      }),
    [multisigRef, withCommand],
  );

  const listConsumableNotes = useCallback(
    async (): Promise<Array<ReturnType<typeof serializeConsumableNote>>> =>
      withCommand('listConsumableNotes', async () => {
        requireSessionReady();
        const currentMultisig = multisigRef.current;
        if (!currentMultisig) {
          throw new Error('No multisig account is loaded');
        }

        const notes = await currentMultisig.getConsumableNotes();
        setConsumableNotes(notes);
        return notes.map(serializeConsumableNote);
      }),
    [multisigRef, withCommand],
  );

  const listProposals = useCallback(
    async (): Promise<Array<ReturnType<typeof serializeProposal>>> =>
      withCommand('listProposals', async () => {
        requireSessionReady();
        const currentMultisig = multisigRef.current;
        if (!currentMultisig) {
          throw new Error('No multisig account is loaded');
        }

        const visible = listVisibleProposals(currentMultisig);
        setProposals(visible);
        return visible.map(serializeProposal);
      }),
    [multisigRef, withCommand],
  );

  const createProposal = useCallback(
    async (
      input: CreateProposalInput,
    ): Promise<{
      proposal: ReturnType<typeof serializeProposal>;
      proposals: Array<ReturnType<typeof serializeProposal>>;
    }> =>
      withCommand('createProposal', async () => {
        requireSessionReady();
        const currentMultisig = multisigRef.current;
        if (!currentMultisig) {
          throw new Error('No multisig account is loaded');
        }

        let result:
          | { proposal: Proposal; proposals: Proposal[] }
          | undefined;

        switch (input.type) {
          case 'add_signer':
            result = await createAddSignerProposal(
              currentMultisig,
              normalizeCommitment(input.commitment),
              input.increaseThreshold ?? false,
            );
            break;
          case 'remove_signer':
            result = await createRemoveSignerProposal(
              currentMultisig,
              normalizeCommitment(input.signerCommitment),
              input.newThreshold,
            );
            break;
          case 'change_threshold':
            result = await createChangeThresholdProposal(currentMultisig, input.newThreshold);
            break;
          case 'update_procedure_threshold':
            result = await createUpdateProcedureThresholdProposal(
              currentMultisig,
              input.procedure,
              input.threshold,
            );
            break;
          case 'consume_notes':
            result = await createConsumeNotesProposal(currentMultisig, input.noteIds);
            break;
          case 'p2id':
            result = await createP2idProposal(
              currentMultisig,
              input.recipientId.trim(),
              input.faucetId.trim(),
              BigInt(input.amount),
            );
            break;
          case 'switch_guardian':
            result = await createSwitchGuardianProposal(
              currentMultisig,
              input.newGuardianEndpoint.trim(),
              normalizeCommitment(input.newGuardianPubkey),
            );
            break;
        }

        if (!result) {
          throw new Error('Unsupported proposal type');
        }

        setProposals(result.proposals);

        return {
          proposal: serializeProposal(result.proposal),
          proposals: result.proposals.map(serializeProposal),
        };
      }),
    [multisigRef, withCommand],
  );

  const signProposal = useCallback(
    async ({
      proposalId,
    }: {
      proposalId: string;
    }): Promise<Array<ReturnType<typeof serializeProposal>>> =>
      withCommand('signProposal', async () => {
        requireSessionReady();
        const currentMultisig = multisigRef.current;
        if (!currentMultisig) {
          throw new Error('No multisig account is loaded');
        }

        const nextProposals = await signOnlineProposal(currentMultisig, proposalId);
        setProposals(nextProposals);
        return nextProposals.map(serializeProposal);
      }),
    [multisigRef, withCommand],
  );

  const executeProposal = useCallback(
    async ({
      proposalId,
    }: {
      proposalId: string;
    }): Promise<BrowserSessionSnapshot> =>
      withCommand('executeProposal', async () => {
        requireSessionReady();
        const currentMultisig = multisigRef.current;
        if (!currentMultisig) {
          throw new Error('No multisig account is loaded');
        }

        await executeOnlineProposal(currentMultisig, proposalId);
        const refreshed = await refreshMultisigState(currentMultisig);
        return buildCurrentSnapshot({
          guardianState: refreshed.state,
          detectedConfig: refreshed.config,
          proposals: refreshed.proposals,
          consumableNotes: refreshed.notes,
          lastError: null,
        });
      }),
    [buildCurrentSnapshot, multisigRef, refreshMultisigState, withCommand],
  );

  const exportProposal = useCallback(
    async ({
      proposalId,
    }: {
      proposalId: string;
    }): Promise<{ json: string }> =>
      withCommand('exportProposal', async () => {
        requireSessionReady();
        const currentMultisig = multisigRef.current;
        if (!currentMultisig) {
          throw new Error('No multisig account is loaded');
        }

        return { json: exportProposalToJson(currentMultisig, proposalId) };
      }),
    [multisigRef, withCommand],
  );

  const signProposalOffline = useCallback(
    async (
      input: SignProposalOfflineInput,
    ): Promise<{
      proposalId: string;
      json: string;
      proposals: Array<ReturnType<typeof serializeProposal>>;
    }> =>
      withCommand('signProposalOffline', async () => {
        requireSessionReady();
        const currentMultisig = multisigRef.current;
        if (!currentMultisig) {
          throw new Error('No multisig account is loaded');
        }

        let proposalId = input.proposalId?.trim();
        if (input.json?.trim()) {
          const imported = await importStoredProposal(currentMultisig, input.json.trim());
          setProposals(imported.proposals);
          proposalId = proposalId || imported.proposal.id;
        }

        if (!proposalId) {
          throw new Error('proposalId or json is required');
        }

        const signed = await signOfflineProposal(currentMultisig, proposalId);
        setProposals(signed.proposals);

        return {
          proposalId,
          json: signed.json,
          proposals: signed.proposals.map(serializeProposal),
        };
      }),
    [multisigRef, withCommand],
  );

  const importProposal = useCallback(
    async ({
      json,
    }: {
      json: string;
    }): Promise<{
      proposal: ReturnType<typeof serializeProposal>;
      proposals: Array<ReturnType<typeof serializeProposal>>;
    }> =>
      withCommand('importProposal', async () => {
        requireSessionReady();
        const currentMultisig = multisigRef.current;
        if (!currentMultisig) {
          throw new Error('No multisig account is loaded');
        }

        const imported = await importStoredProposal(currentMultisig, json.trim());
        setProposals(imported.proposals);
        return {
          proposal: serializeProposal(imported.proposal),
          proposals: imported.proposals.map(serializeProposal),
        };
      }),
    [multisigRef, withCommand],
  );

  const clearLocalState = useCallback(
    async (): Promise<BrowserSessionSnapshot> =>
      withCommand('clearLocalState', async () => {
        await clearIndexedDbDatabasesByPrefix([DEFAULT_MIDEN_DB_NAME]);
        clearSessionCore();
        setBootStatus('idle');
        setBootError(null);
        setLastError(null);

        return buildCurrentSnapshot({
          webClient: null,
          multisigClient: null,
          bootStatus: 'idle',
          bootError: null,
          guardianPubkey: null,
          localSigners: null,
          multisig: null,
          guardianState: null,
          detectedConfig: null,
          proposals: [],
          consumableNotes: [],
          lastError: null,
        });
      }),
    [buildCurrentSnapshot, clearSessionCore, withCommand],
  );

  const listEvents = useCallback(async (): Promise<SmokeEventEntry[]> => {
    const startedAt = performance.now();
    appendEvent('events', 'succeeded', null, performance.now() - startedAt);
    return [...eventsRef.current];
  }, [appendEvent]);

  const api: SmokeApi = {
    initSession,
    connectPara: connectParaSession,
    connectMidenWallet: connectMidenWalletSession,
    status,
    createAccount,
    loadAccount,
    registerOnGuardian,
    sync,
    fetchState,
    verifyStateCommitment: verifyState,
    listConsumableNotes,
    listProposals,
    createProposal,
    signProposal,
    executeProposal,
    exportProposal,
    signProposalOffline,
    importProposal,
    clearLocalState,
    events: listEvents,
  };

  useEffect(() => {
    window.smoke = api;
    return () => {
      delete window.smoke;
    };
  }, [api]);

  useEffect(() => {
    if (midenWalletConnectError) {
      setLastError(classifyWalletError(midenWalletConnectError));
    }
  }, [midenWalletConnectError]);

  useEffect(() => {
    if (bootStartedRef.current) {
      return;
    }

    bootStartedRef.current = true;
    void bootSession(defaultSessionConfig, 'bootstrap').catch(() => undefined);
  }, [bootSession]);

  return {
    api,
    snapshot: buildCurrentSnapshot(),
    events,
    midenWalletConnectError,
    disconnectMidenWallet: async () => {
      await disconnectMidenWallet();
      if (sessionConfigRef.current.signerSource === 'miden-wallet') {
        setSessionConfig((current) => ({
          ...current,
          signerSource: 'local',
        }));
      }
    },
  };
}
