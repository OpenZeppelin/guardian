import { useEffect, useState, useCallback } from 'react';
import { toast } from 'sonner';
import { useModal } from '@getpara/react-sdk-lite';
import { MidenWalletAdapter } from '@demox-labs/miden-wallet-adapter-miden';

import {
  type Multisig,
  type MultisigClient,
  type AccountState,
  type DetectedMultisigConfig,
  type Proposal,
  type ProcedureName,
  type SignatureScheme,
} from '@openzeppelin/miden-multisig-client';
import { PsmHttpError } from '@openzeppelin/psm-client';

import { WebClient, AccountId } from '@miden-sdk/miden-sdk';

import {
  Header,
  WelcomeView,
  CreateMultisigDialog,
  LoadMultisigDialog,
  ImportProposalDialog,
  MultisigDashboard,
} from '@/components';

import { normalizeCommitment } from '@/lib/helpers';
import { classifyWalletError, formatError } from '@/lib/errors';
import { clearMidenDatabase, createWebClient, initializeSigner as initSigner } from '@/lib/initClient';
import {
  initMultisigClient,
  createMultisigAccount,
  loadMultisigAccount,
  resolveLocalSigner,
  resolveMidenWalletSigner,
  resolveParaSigner,
  registerOnPsm,
  switchMultisigPsm,
  fetchAccountState,
  syncAll,
  verifyStateCommitment,
  createAddSignerProposal,
  createRemoveSignerProposal,
  createChangeThresholdProposal,
  createUpdateProcedureThresholdProposal,
  createConsumeNotesProposal,
  createP2idProposal,
  createSwitchPsmProposal,
  signProposal,
  executeProposal,
  exportProposalToJson,
  signProposalOffline,
  importProposal,
} from '@/lib/multisigApi';
import { MIDEN_RPC_URL, PSM_ENDPOINT } from '@/config';
import { useParaSession } from '@/hooks/useParaSession';
import { useMidenWallet } from '@/hooks/useMidenWallet';
import type { SignerInfo } from '@/types';
import type { WalletSource } from '@/wallets/types';

// Helper to check if an error is related to pending candidate delta
function isPendingCandidateError(error: unknown): boolean {
  const errorStr = error instanceof Error ? error.message : String(error);
  return (
    errorStr.includes('non-canonical delta pending') ||
    errorStr.includes('ConflictPendingDelta')
  );
}

const CREATE_PROPOSAL_PENDING_WARNING =
  'A previous transaction is still being processed on-chain. Please wait for it to be confirmed before creating new proposals.';
const EXECUTE_PROPOSAL_PENDING_WARNING =
  'A previous transaction is still being processed on-chain. Please wait for it to be confirmed before executing proposals.';

function preferredWalletSource(
  paraConnected: boolean,
  midenWalletConnected: boolean,
): WalletSource {
  if (midenWalletConnected) {
    return 'miden-wallet';
  }

  if (paraConnected) {
    return 'para';
  }

  return 'local';
}

function localCommitmentForScheme(
  signer: SignerInfo | null,
  signatureScheme: SignatureScheme | null,
): string | null {
  if (!signer) {
    return null;
  }

  return signatureScheme === 'ecdsa'
    ? signer.ecdsa.commitment
    : signer.falcon.commitment;
}

function activeWalletSchemeForSource(
  walletSource: WalletSource,
  signer: SignerInfo | null,
  paraScheme: SignatureScheme | null,
  midenWalletScheme: SignatureScheme | null,
): SignatureScheme | null {
  if (walletSource === 'para') {
    return paraScheme ?? 'ecdsa';
  }

  if (walletSource === 'miden-wallet') {
    return midenWalletScheme;
  }

  return signer?.activeScheme ?? null;
}

function activeWalletCommitmentForSource(
  walletSource: WalletSource,
  signer: SignerInfo | null,
  paraCommitment: string | null,
  midenWalletCommitment: string | null,
): string | null {
  if (walletSource === 'para') {
    return paraCommitment;
  }

  if (walletSource === 'miden-wallet') {
    return midenWalletCommitment;
  }

  return localCommitmentForScheme(signer, signer?.activeScheme ?? null);
}

export default function App() {
  // Core state
  const [webClient, setWebClient] = useState<WebClient | null>(null);
  const [multisigClient, setMultisigClient] = useState<MultisigClient | null>(null);
  const [signer, setSigner] = useState<SignerInfo | null>(null);
  const [generatingSigner, setGeneratingSigner] = useState(false);
  const [multisig, setMultisig] = useState<Multisig | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pendingCandidateWarning, setPendingCandidateWarning] = useState<string | null>(null);
  const [walletSource, setWalletSource] = useState<WalletSource>('local');

  // PSM state
  const [psmUrl, setPsmUrl] = useState(PSM_ENDPOINT);
  const [psmStatus, setPsmStatus] = useState<'connected' | 'connecting' | 'error'>('connecting');
  const [psmPubkey, setPsmPubkey] = useState('');
  const [psmState, setPsmState] = useState<AccountState | null>(null);

  // Dialog state
  const [createDialogOpen, setCreateDialogOpen] = useState(false);
  const [loadDialogOpen, setLoadDialogOpen] = useState(false);
  const [importDialogOpen, setImportDialogOpen] = useState(false);
  const [importJson, setImportJson] = useState('');

  // Operation state
  const [creating, setCreating] = useState(false);
  const [registeringOnPsm, setRegisteringOnPsm] = useState(false);
  const [loadingAccount, setLoadingAccount] = useState(false);
  const [detectedConfig, setDetectedConfig] = useState<DetectedMultisigConfig | null>(null);
  const [syncingState, setSyncingState] = useState(false);
  const [verifyingState, setVerifyingState] = useState(false);
  const [verificationStatus, setVerificationStatus] = useState<string | null>(null);

  // Proposal state
  const [proposals, setProposals] = useState<Proposal[]>([]);
  const [creatingProposal, setCreatingProposal] = useState(false);
  const [signingProposal, setSigningProposal] = useState<string | null>(null);
  const [executingProposal, setExecutingProposal] = useState<string | null>(null);

  // Notes state
  const [consumableNotes, setConsumableNotes] = useState<Array<{ id: string; assets: Array<{ faucetId: string; amount: bigint }> }>>([]);

  const falconCommitment = signer?.falcon.commitment ?? null;
  const ecdsaCommitment = signer?.ecdsa.commitment ?? null;
  const activeScheme = signer?.activeScheme ?? null;
  const { session: paraSession, paraClient, walletId: paraWalletId } = useParaSession();
  const [midenWalletAdapter] = useState(
    () => new MidenWalletAdapter({ appName: 'Miden Multisig' }),
  );
  const {
    session: midenWalletSession,
    connect: connectMidenWallet,
    disconnect: disconnectMidenWallet,
    signBytes,
    connectError: midenWalletConnectError,
  } = useMidenWallet(midenWalletAdapter);
  const { openModal } = useModal();
  const preferredSource = preferredWalletSource(
    paraSession.connected,
    midenWalletSession.connected,
  );

  const activeWalletCommitment = activeWalletCommitmentForSource(
    walletSource,
    signer,
    paraSession.commitment,
    midenWalletSession.commitment,
  );
  const activeWalletScheme = activeWalletSchemeForSource(
    walletSource,
    signer,
    paraSession.scheme,
    midenWalletSession.scheme,
  );

  const resolveSignerContext = useCallback(
    (
      source: WalletSource,
      signatureScheme: SignatureScheme = activeWalletSchemeForSource(
        source,
        signer,
        paraSession.scheme,
        midenWalletSession.scheme,
      ) ?? 'falcon',
    ) => {
      if (source === 'para') {
        if (!paraClient || !paraSession.commitment || !paraSession.publicKey) {
          throw new Error('Para wallet is not connected');
        }

        if (!paraWalletId) {
          throw new Error('Para wallet did not expose a wallet id');
        }

        return resolveParaSigner({
          paraClient,
          walletId: paraWalletId,
          commitment: paraSession.commitment,
          publicKey: paraSession.publicKey,
        });
      }

      if (source === 'miden-wallet') {
        if (!midenWalletSession.commitment || !midenWalletSession.publicKey || !midenWalletSession.scheme) {
          throw new Error('Miden Wallet is not connected');
        }

        return resolveMidenWalletSigner({
          wallet: { signBytes },
          commitment: midenWalletSession.commitment,
          publicKey: midenWalletSession.publicKey,
          scheme: midenWalletSession.scheme,
        });
      }

      if (!signer) {
        throw new Error('Local signers are still initializing');
      }

      return resolveLocalSigner(signer, signatureScheme);
    },
    [
      midenWalletSession.commitment,
      midenWalletSession.publicKey,
      midenWalletSession.scheme,
      paraClient,
      paraSession.commitment,
      paraSession.publicKey,
      paraSession.scheme,
      paraWalletId,
      signBytes,
      signer,
    ],
  );

  const handleWalletSourceChange = useCallback(
    async (nextSource: WalletSource) => {
      if (nextSource === walletSource) {
        return;
      }

      if (!multisig || !multisigClient) {
        setWalletSource(nextSource);
        return;
      }

      setLoadingAccount(true);
      setError(null);
      try {
        const signerContext = resolveSignerContext(nextSource);
        const reloaded = await loadMultisigAccount(multisigClient, multisig.accountId, signerContext);
        setMultisig(reloaded);

        const { state, config } = await fetchAccountState(reloaded);
        setDetectedConfig(config);
        setPsmState(state);

        const { proposals: synced, notes } = await syncAll(reloaded);
        setProposals(synced);
        setConsumableNotes(notes);
        setWalletSource(nextSource);
      } catch (err) {
        setError(`Failed to switch wallet source: ${formatError(err)}`);
      } finally {
        setLoadingAccount(false);
      }
    },
    [
      fetchAccountState,
      loadMultisigAccount,
      multisig,
      multisigClient,
      resolveSignerContext,
      syncAll,
      walletSource,
    ],
  );

  // Connect to PSM server
  const connectToPsm = useCallback(
    async (url: string, client?: WebClient): Promise<void> => {
      setPsmStatus('connecting');
      setError(null);
      try {
        const wc = client ?? webClient;
        if (!wc) {
          // Fallback when no WebClient - just fetch pubkey
          const response = await fetch(`${url}/pubkey`);
          const data = await response.json();
          setPsmPubkey(data.commitment || '');
          setPsmStatus('connected');
          return;
        }

        const { client: msClient, psmPubkey: pubkey } = await initMultisigClient(
          wc,
          url,
          MIDEN_RPC_URL
        );
        setPsmPubkey(pubkey);
        setMultisigClient(msClient);
        setPsmStatus('connected');

        // If there's an active multisig, try to load or register on the new PSM
        if (multisig && psmState?.stateDataBase64) {
          setRegisteringOnPsm(true);
          try {
            const signerContext = resolveSignerContext(walletSource);
            // First, try to load from the new PSM
            const reloadedMs = await loadMultisigAccount(msClient, multisig.accountId, signerContext);
            setMultisig(reloadedMs);

            const { state, config } = await fetchAccountState(reloadedMs);
            setPsmState(state);
            setDetectedConfig(config);

            toast.success('Account loaded from PSM');
          } catch (loadErr) {
            // Check if it's a 404 (account not found on this PSM)
            const isNotFound = loadErr instanceof PsmHttpError && loadErr.status === 404;

            if (isNotFound) {
              try {
                const accountId = AccountId.fromHex(multisig.accountId);
                const currentAccount = await wc.getAccount(accountId);
                if (!currentAccount) {
                  throw new Error('Account not found in local client');
                }
                const freshStateBytes = currentAccount.serialize();
                const freshStateBase64 = btoa(String.fromCharCode(...freshStateBytes));

                await switchMultisigPsm(msClient, multisig, freshStateBase64);

                const { state, config } = await fetchAccountState(multisig);
                setPsmState(state);
                setDetectedConfig(config);

                toast.success('Account registered on new PSM');
              } catch (registerErr) {
                setError(`Failed to register account on new PSM: ${formatError(registerErr)}`);
              }
            } else {
              setError(`Failed to load account from PSM: ${formatError(loadErr)}`);
            }
          } finally {
            setRegisteringOnPsm(false);
          }
        }
      } catch (err) {
        const msg = formatError(err);
        console.error('Failed to connect to PSM:', msg);
        setPsmStatus('error');
        setPsmPubkey('');
        setError(`Failed to connect to PSM: ${msg}`);
      }
    },
    [webClient, multisig, psmState, resolveSignerContext, walletSource]
  );

  // Initialize on mount
  useEffect(() => {
    const init = async () => {
      try {
        // Clear IndexedDB to start fresh on each page load
        await clearMidenDatabase();

        const client = await createWebClient();
        setWebClient(client);

        await connectToPsm(psmUrl, client);

        setGeneratingSigner(true);
        const signerInfo = await initSigner(client);
        setSigner(signerInfo);
      } catch (err) {
        setError(formatError(err, 'Initialization failed'));
      } finally {
        setGeneratingSigner(false);
      }
    };
    init();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (midenWalletConnectError) {
      toast.error(classifyWalletError(midenWalletConnectError));
    }
  }, [midenWalletConnectError]);

  useEffect(() => {
    if (multisig) {
      return;
    }

    if (walletSource === 'local') {
      if (preferredSource !== 'local') {
        setWalletSource(preferredSource);
        return;
      }
    }

    if (!midenWalletSession.connected && walletSource === 'miden-wallet') {
      setWalletSource(preferredSource);
      return;
    }

    if (!paraSession.connected && walletSource === 'para') {
      setWalletSource(preferredSource);
    }
  }, [
    midenWalletSession.connected,
    multisig,
    paraSession.connected,
    preferredSource,
    walletSource,
  ]);

  const handleConnectMidenWallet = useCallback(async () => {
    try {
      await connectMidenWallet();
    } catch (err) {
      setError(classifyWalletError(err));
    }
  }, [connectMidenWallet]);

  const handleDisconnectMidenWallet = useCallback(async () => {
    try {
      await disconnectMidenWallet();
      if (walletSource !== 'miden-wallet') {
        return;
      }

      if (!multisig) {
        setWalletSource(preferredSource);
        return;
      }

      await handleWalletSourceChange(preferredSource);
    } catch (err) {
      setError(classifyWalletError(err));
    }
  }, [
    disconnectMidenWallet,
    handleWalletSourceChange,
    multisig,
    preferredSource,
    walletSource,
  ]);

  // Create multisig
  const handleCreate = async (
    otherSignerCommitments: string[],
    threshold: number,
    procedureThresholds?: import('@openzeppelin/miden-multisig-client').ProcedureThreshold[],
    signatureScheme: SignatureScheme = activeWalletScheme ?? 'falcon',
  ) => {
    if (!multisigClient) {
      setError('Client not initialized. Try reconnecting to PSM.');
      return;
    }
    if (!psmPubkey) {
      setPsmStatus('error');
      setError('Missing PSM commitment. Reconnect to the PSM endpoint and try again.');
      return;
    }

    setCreating(true);
    setError(null);
    try {
      const signerContext = resolveSignerContext(walletSource, signatureScheme);
      const { commitment: schemePsmCommitment } = await multisigClient.psmClient.getPubkey(signatureScheme);
      const ms = await createMultisigAccount(
        multisigClient,
        signerContext,
        otherSignerCommitments,
        threshold,
        schemePsmCommitment,
        procedureThresholds,
        signatureScheme,
      );
      setSigner((current) =>
        current
          ? {
              ...current,
              activeScheme: signatureScheme,
            }
          : current,
      );
      setMultisig(ms);

      // Auto-register on PSM
      setRegisteringOnPsm(true);
      try {
        await registerOnPsm(ms);

        // Fetch account state to populate detectedConfig with procedure thresholds
        const { state, config } = await fetchAccountState(ms);
        setPsmState(state);
        setDetectedConfig(config);
      } catch (psmErr) {
        setError(`Created but failed to register on PSM: ${psmErr instanceof Error ? psmErr.message : 'Unknown'}`);
      } finally {
        setRegisteringOnPsm(false);
      }

      setCreateDialogOpen(false);
    } catch (err) {
      setError(formatError(err, 'Failed to create'));
    } finally {
      setCreating(false);
    }
  };

  // Load multisig from PSM
  const handleLoad = async (
    accountId: string,
    signatureScheme: SignatureScheme = activeWalletScheme ?? 'falcon',
  ) => {
    if (!multisigClient) {
      setError('Client not initialized. Try reconnecting to PSM.');
      return;
    }
    if (!psmPubkey) {
      setPsmStatus('error');
      setError('Not connected to PSM. Check the endpoint and try again.');
      return;
    }

    let normalizedId = accountId;
    if (!normalizedId.startsWith('0x')) {
      normalizedId = `0x${normalizedId}`;
    }

    setLoadingAccount(true);
    setError(null);
    setDetectedConfig(null);
    try {
      const signerContext = resolveSignerContext(walletSource, signatureScheme);
      const ms = await loadMultisigAccount(
        multisigClient,
        normalizedId,
        signerContext,
      );
      setSigner((current) =>
        current
          ? {
              ...current,
              activeScheme: signatureScheme,
            }
          : current,
      );
      setMultisig(ms);

      const { state, config } = await fetchAccountState(ms);
      setDetectedConfig(config);
      setPsmState(state);

      setLoadDialogOpen(false);
    } catch (err) {
      const message = err instanceof Error ? err.message : 'Unknown';
      if (message.includes('404') || message.includes('not found')) {
        setError('Account not found on PSM');
      } else {
        setError(`Failed to load: ${message}`);
      }
    } finally {
      setLoadingAccount(false);
    }
  };

  // Sync state and proposals
  const handleSync = async () => {
    if (!multisig || !webClient) return;

    setSyncingState(true);
    setError(null);
    setPendingCandidateWarning(null);
    setVerificationStatus(null);
    try {
      // Sync miden client state first (with retry for IndexedDB race conditions)
      try {
        await webClient.syncState();
      } catch (syncErr) {
        // IndexedDB can have PrematureCommitError - retry once after a short delay
        console.warn('First syncState attempt failed, retrying...', syncErr);
        await new Promise(resolve => setTimeout(resolve, 500));
        await webClient.syncState();
      }

      const { state, config } = await fetchAccountState(multisig);
      setPsmState(state);
      setDetectedConfig(config);

      const { proposals: synced, notes } = await syncAll(multisig);
      setProposals(synced);
      setConsumableNotes(notes);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (message.includes('account nonce is too low to import')) {
        setPendingCandidateWarning(
          'Sync warning: local state is ahead of the on-chain state. ' +
          'This can happen right after executing a transaction. Please wait a moment and sync again.'
        );
        setError(null);
      } else {
        setError(formatError(err, 'Sync failed'));
      }
    } finally {
      setSyncingState(false);
    }
  };

  const handleVerifyState = async () => {
    if (!multisig) return;

    setVerifyingState(true);
    setError(null);
    setVerificationStatus(null);

    try {
      const result = await verifyStateCommitment(multisig);
      setVerificationStatus(
        `Verified local state against on-chain commitment (${result.onChainCommitment.slice(0, 10)}...)`
      );
      toast.success('State verification passed');
    } catch (err) {
      setError(`State verification failed: ${err instanceof Error ? err.message : 'Unknown'}`);
    } finally {
      setVerifyingState(false);
    }
  };

  const handleCreateProposalError = useCallback((err: unknown) => {
    if (isPendingCandidateError(err)) {
      setPendingCandidateWarning(CREATE_PROPOSAL_PENDING_WARNING);
      return;
    }

    setError(`Failed to create proposal: ${formatError(err)}`);
  }, []);

  const runProposalCreation = useCallback(
    async (
      createProposal: () => Promise<{ proposals: Proposal[] }>,
      successMessage: string,
    ) => {
      setCreatingProposal(true);
      setError(null);
      setPendingCandidateWarning(null);
      try {
        const { proposals: nextProposals } = await createProposal();
        setProposals(nextProposals);
        toast.success(successMessage);
      } catch (err) {
        handleCreateProposalError(err);
      } finally {
        setCreatingProposal(false);
      }
    },
    [handleCreateProposalError],
  );

  // Create add signer proposal
  const handleCreateAddSignerProposal = async (commitment: string, increaseThreshold: boolean) => {
    if (!multisig) return;

    let normalizedCommitment: string;
    try {
      normalizedCommitment = normalizeCommitment(commitment);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : 'Invalid commitment');
      return;
    }

    await runProposalCreation(
      () => createAddSignerProposal(multisig, normalizedCommitment, increaseThreshold),
      'Add signer proposal created',
    );
  };

  // Create remove signer proposal
  const handleCreateRemoveSignerProposal = async (signerToRemove: string, newThreshold?: number) => {
    if (!multisig) return;

    await runProposalCreation(
      () => createRemoveSignerProposal(multisig, signerToRemove, newThreshold),
      'Remove signer proposal created',
    );
  };

  // Create change threshold proposal
  const handleCreateChangeThresholdProposal = async (newThreshold: number) => {
    if (!multisig) return;

    await runProposalCreation(
      () => createChangeThresholdProposal(multisig, newThreshold),
      'Change threshold proposal created',
    );
  };

  const handleCreateUpdateProcedureThresholdProposal = async (
    procedure: ProcedureName,
    threshold: number,
  ) => {
    if (!multisig) return;

    await runProposalCreation(
      () => createUpdateProcedureThresholdProposal(multisig, procedure, threshold),
      'Procedure threshold proposal created',
    );
  };

  // Create consume notes proposal
  const handleCreateConsumeNotesProposal = async (noteIds: string[]) => {
    if (!multisig) return;

    await runProposalCreation(
      () => createConsumeNotesProposal(multisig, noteIds),
      'Consume notes proposal created',
    );
  };

  // Create P2ID (send payment) proposal
  const handleCreateP2idProposal = async (recipientId: string, faucetId: string, amount: bigint) => {
    if (!multisig) return;

    await runProposalCreation(
      () => createP2idProposal(multisig, recipientId, faucetId, amount),
      'Send payment proposal created',
    );
  };

  // Create switch PSM proposal
  const handleCreateSwitchPsmProposal = async (newEndpoint: string, newPubkey: string) => {
    if (!multisig) return;

    await runProposalCreation(
      () => createSwitchPsmProposal(multisig, newEndpoint, newPubkey),
      'Switch PSM proposal created',
    );
  };

  // Sign proposal
  const handleSignProposal = async (proposalId: string) => {
    if (!multisig) return;

    setSigningProposal(proposalId);
    setError(null);
    try {
      const proposals = await signProposal(multisig, proposalId);
      setProposals(proposals);
    } catch (err) {
      setError(`Failed to sign: ${err instanceof Error ? err.message : 'Unknown'}`);
    } finally {
      setSigningProposal(null);
    }
  };

  // Execute proposal
  const handleExecuteProposal = async (proposalId: string) => {
    if (!multisig) return;

    setExecutingProposal(proposalId);
    setError(null);
    setPendingCandidateWarning(null);
    try {
      await executeProposal(multisig, proposalId);
      toast.success('Proposal executed successfully');

      // Sync to reload account state and proposals
      await handleSync();
    } catch (err) {
      console.error('[Execute] Execution failed:', err);
      if (isPendingCandidateError(err)) {
        setPendingCandidateWarning(EXECUTE_PROPOSAL_PENDING_WARNING);
      } else {
        setError(`Failed to execute: ${err instanceof Error ? err.message : 'Unknown'}`);
      }
    } finally {
      setExecutingProposal(null);
    }
  };

  // Export proposal to clipboard
  const handleExportProposal = (proposalId: string) => {
    if (!multisig) return;

    try {
      const json = exportProposalToJson(multisig, proposalId);
      navigator.clipboard.writeText(json);
      toast.success('Proposal JSON copied to clipboard');
    } catch (err) {
      setError(`Failed to export: ${err instanceof Error ? err.message : 'Unknown'}`);
    }
  };

  // Sign proposal offline and copy to clipboard
  const handleSignProposalOffline = async (proposalId: string) => {
    if (!multisig) return;

    try {
      const { json, proposals } = await signProposalOffline(multisig, proposalId);
      navigator.clipboard.writeText(json);
      setProposals(proposals);
      toast.success('Signed! Updated proposal JSON copied to clipboard');
    } catch (err) {
      setError(`Failed to sign offline: ${err instanceof Error ? err.message : 'Unknown'}`);
    }
  };

  // Import proposal from JSON
  const handleImportProposal = () => {
    setImportJson('');
    setImportDialogOpen(true);
  };

  const handleImportProposalSubmit = async () => {
    if (!multisig || !importJson.trim()) return;

    try {
      const { proposal, proposals } = await importProposal(multisig, importJson.trim());
      setProposals(proposals);
      setImportDialogOpen(false);
      setImportJson('');
      toast.success(`Proposal imported: ${proposal.id.slice(0, 12)}...`);
    } catch (err) {
      setError(`Failed to import: ${err instanceof Error ? err.message : 'Unknown'}`);
    }
  };

  // Disconnect
  const handleDisconnect = () => {
    setMultisig(null);
    setPsmState(null);
    setProposals([]);
    setError(null);
    setVerificationStatus(null);
  };

  // Reset and reload
  const handleResetData = () => {
    toast.success('Reloading with fresh signer key...');
    // Reload the page to start fresh
    setTimeout(() => window.location.reload(), 500);
  };

  const ready = !!webClient && !!signer && !!multisigClient && !!psmPubkey && psmStatus === 'connected';

  return (
    <div className="min-h-screen flex flex-col">
      <Header
        falconCommitment={falconCommitment}
        ecdsaCommitment={ecdsaCommitment}
        activeScheme={activeScheme}
        generatingSigner={generatingSigner}
        psmStatus={psmStatus}
        psmUrl={psmUrl}
        onPsmUrlChange={setPsmUrl}
        onReconnect={(url) => connectToPsm(url)}
        walletSource={walletSource}
        onWalletSourceChange={handleWalletSourceChange}
        paraConnected={paraSession.connected}
        paraCommitment={paraSession.commitment}
        midenWalletConnected={midenWalletSession.connected}
        midenWalletCommitment={midenWalletSession.commitment}
        onConnectMidenWallet={() => {
          void handleConnectMidenWallet();
        }}
        onDisconnectMidenWallet={() => {
          void handleDisconnectMidenWallet();
        }}
        onOpenParaModal={() => openModal()}
      />

      <main className="flex-1">
        {!multisig ? (
          <WelcomeView
            ready={ready}
            onCreateClick={() => setCreateDialogOpen(true)}
            onLoadClick={() => { setError(null); setLoadDialogOpen(true); }}
            onResetData={handleResetData}
          />
        ) : signer ? (
          <MultisigDashboard
            multisig={multisig}
            signatureScheme={activeWalletScheme ?? signer.activeScheme}
            psmState={psmState}
            proposals={proposals}
            consumableNotes={consumableNotes}
            vaultBalances={detectedConfig?.vaultBalances ?? []}
            procedureThresholds={detectedConfig?.procedureThresholds}
            walletSource={walletSource}
            activeSignerCommitment={multisig.signerCommitment}
            creatingProposal={creatingProposal}
            syncing={syncingState}
            verifying={verifyingState}
            signingProposal={signingProposal}
            executingProposal={executingProposal}
            error={error}
            verificationStatus={verificationStatus}
            pendingCandidateWarning={pendingCandidateWarning}
            onDismissWarning={() => setPendingCandidateWarning(null)}
            onCreateAddSigner={handleCreateAddSignerProposal}
            onCreateRemoveSigner={handleCreateRemoveSignerProposal}
            onCreateChangeThreshold={handleCreateChangeThresholdProposal}
            onCreateUpdateProcedureThreshold={handleCreateUpdateProcedureThresholdProposal}
            onCreateConsumeNotes={handleCreateConsumeNotesProposal}
            onCreateP2id={handleCreateP2idProposal}
            onCreateSwitchPsm={handleCreateSwitchPsmProposal}
            onSync={handleSync}
            onVerify={handleVerifyState}
            onSignProposal={handleSignProposal}
            onExecuteProposal={handleExecuteProposal}
            onExportProposal={handleExportProposal}
            onSignProposalOffline={handleSignProposalOffline}
            onImportProposal={handleImportProposal}
            onDisconnect={handleDisconnect}
          />
        ) : null}
      </main>

      {/* Dialogs */}
      {signer && (
        <>
          <CreateMultisigDialog
            open={createDialogOpen}
            onOpenChange={setCreateDialogOpen}
            falconCommitment={signer.falcon.commitment}
            ecdsaCommitment={signer.ecdsa.commitment}
            defaultScheme={activeWalletScheme ?? signer.activeScheme}
            creating={creating}
            registeringOnPsm={registeringOnPsm}
            onCreate={handleCreate}
            walletSource={walletSource}
            walletCommitment={activeWalletCommitment}
          />
          <LoadMultisigDialog
            open={loadDialogOpen}
            onOpenChange={setLoadDialogOpen}
            loading={loadingAccount}
            detectedConfig={detectedConfig}
            error={error}
            defaultScheme={activeWalletScheme ?? signer.activeScheme}
            onLoad={handleLoad}
            walletSource={walletSource}
          />
          <ImportProposalDialog
            open={importDialogOpen}
            onOpenChange={setImportDialogOpen}
            importJson={importJson}
            onImportJsonChange={setImportJson}
            onImport={handleImportProposalSubmit}
          />
        </>
      )}
    </div>
  );
}
