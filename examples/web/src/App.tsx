import { useEffect, useState, useCallback } from 'react';
import { toast } from 'sonner';

import {
  type Multisig,
  type MultisigClient,
  type AccountState,
  type DetectedMultisigConfig,
  type Proposal,
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
import { formatError } from '@/lib/errors';
import { clearMidenDatabase, createWebClient, initializeSigner as initSigner } from '@/lib/initClient';
import {
  initMultisigClient,
  createMultisigAccount,
  loadMultisigAccount,
  registerOnPsm,
  switchMultisigPsm,
  fetchAccountState,
  syncAll,
  verifyStateCommitment,
  createAddSignerProposal,
  createRemoveSignerProposal,
  createChangeThresholdProposal,
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
import type { SignerInfo } from '@/types';

// Helper to check if an error is related to pending candidate delta
function isPendingCandidateError(error: unknown): boolean {
  const errorStr = error instanceof Error ? error.message : String(error);
  return (
    errorStr.includes('non-canonical delta pending') ||
    errorStr.includes('ConflictPendingDelta')
  );
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
          setPsmPubkey(data.pubkey || '');
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
        if (multisig && signer && psmState?.stateDataBase64) {
          setRegisteringOnPsm(true);
          try {
            // First, try to load from the new PSM
            const reloadedMs = await loadMultisigAccount(msClient, multisig.accountId, signer);
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
    [webClient, multisig, signer, psmState]
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

  // Create multisig
  const handleCreate = async (
    otherSignerCommitments: string[],
    threshold: number,
    procedureThresholds?: import('@openzeppelin/miden-multisig-client').ProcedureThreshold[],
  ) => {
    if (!multisigClient || !signer || !psmPubkey) return;

    setCreating(true);
    setError(null);
    try {
      const ms = await createMultisigAccount(multisigClient, signer, otherSignerCommitments, threshold, psmPubkey, procedureThresholds);
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
  const handleLoad = async (accountId: string) => {
    if (!multisigClient || !signer) {
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
      const ms = await loadMultisigAccount(multisigClient, normalizedId, signer);
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

    setCreatingProposal(true);
    setError(null);
    setPendingCandidateWarning(null);
    try {
      const { proposals } = await createAddSignerProposal(multisig, normalizedCommitment, increaseThreshold);
      setProposals(proposals);
      toast.success('Add signer proposal created');
    } catch (err) {
      if (isPendingCandidateError(err)) {
        setPendingCandidateWarning(
          'A previous transaction is still being processed on-chain. ' +
          'Please wait for it to be confirmed before creating new proposals.'
        );
      } else {
        setError(`Failed to create proposal: ${err instanceof Error ? err.message : 'Unknown'}`);
      }
    } finally {
      setCreatingProposal(false);
    }
  };

  // Create remove signer proposal
  const handleCreateRemoveSignerProposal = async (signerToRemove: string, newThreshold?: number) => {
    if (!multisig) return;

    setCreatingProposal(true);
    setError(null);
    setPendingCandidateWarning(null);
    try {
      const { proposals } = await createRemoveSignerProposal(multisig, signerToRemove, newThreshold);
      setProposals(proposals);
      toast.success('Remove signer proposal created');
    } catch (err) {
      if (isPendingCandidateError(err)) {
        setPendingCandidateWarning(
          'A previous transaction is still being processed on-chain. ' +
          'Please wait for it to be confirmed before creating new proposals.'
        );
      } else {
        setError(`Failed to create proposal: ${err instanceof Error ? err.message : 'Unknown'}`);
      }
    } finally {
      setCreatingProposal(false);
    }
  };

  // Create change threshold proposal
  const handleCreateChangeThresholdProposal = async (newThreshold: number) => {
    if (!multisig) return;

    setCreatingProposal(true);
    setError(null);
    setPendingCandidateWarning(null);
    try {
      const { proposals } = await createChangeThresholdProposal(multisig, newThreshold);
      setProposals(proposals);
      toast.success('Change threshold proposal created');
    } catch (err) {
      if (isPendingCandidateError(err)) {
        setPendingCandidateWarning(
          'A previous transaction is still being processed on-chain. ' +
          'Please wait for it to be confirmed before creating new proposals.'
        );
      } else {
        setError(`Failed to create proposal: ${err instanceof Error ? err.message : 'Unknown'}`);
      }
    } finally {
      setCreatingProposal(false);
    }
  };

  // Create consume notes proposal
  const handleCreateConsumeNotesProposal = async (noteIds: string[]) => {
    if (!multisig) return;

    setCreatingProposal(true);
    setError(null);
    setPendingCandidateWarning(null);
    try {
      const { proposals } = await createConsumeNotesProposal(multisig, noteIds);
      setProposals(proposals);
      toast.success('Consume notes proposal created');
    } catch (err) {
      if (isPendingCandidateError(err)) {
        setPendingCandidateWarning(
          'A previous transaction is still being processed on-chain. ' +
          'Please wait for it to be confirmed before creating new proposals.'
        );
      } else {
        setError(`Failed to create proposal: ${err instanceof Error ? err.message : 'Unknown'}`);
      }
    } finally {
      setCreatingProposal(false);
    }
  };

  // Create P2ID (send payment) proposal
  const handleCreateP2idProposal = async (recipientId: string, faucetId: string, amount: bigint) => {
    if (!multisig) return;

    setCreatingProposal(true);
    setError(null);
    setPendingCandidateWarning(null);
    try {
      const { proposals } = await createP2idProposal(multisig, recipientId, faucetId, amount);
      setProposals(proposals);
      toast.success('Send payment proposal created');
    } catch (err) {
      if (isPendingCandidateError(err)) {
        setPendingCandidateWarning(
          'A previous transaction is still being processed on-chain. ' +
          'Please wait for it to be confirmed before creating new proposals.'
        );
      } else {
        setError(`Failed to create proposal: ${err instanceof Error ? err.message : 'Unknown'}`);
      }
    } finally {
      setCreatingProposal(false);
    }
  };

  // Create switch PSM proposal
  const handleCreateSwitchPsmProposal = async (newEndpoint: string, newPubkey: string) => {
    if (!multisig) return;

    setCreatingProposal(true);
    setError(null);
    setPendingCandidateWarning(null);
    try {
      const { proposals } = await createSwitchPsmProposal(multisig, newEndpoint, newPubkey);
      setProposals(proposals);
      toast.success('Switch PSM proposal created');
    } catch (err) {
      if (isPendingCandidateError(err)) {
        setPendingCandidateWarning(
          'A previous transaction is still being processed on-chain. ' +
          'Please wait for it to be confirmed before creating new proposals.'
        );
      } else {
        setError(`Failed to create proposal: ${err instanceof Error ? err.message : 'Unknown'}`);
      }
    } finally {
      setCreatingProposal(false);
    }
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
        setPendingCandidateWarning(
          'A previous transaction is still being processed on-chain. ' +
          'Please wait for it to be confirmed before executing proposals.'
        );
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

  const ready = !!webClient && !!signer && !!multisigClient && psmStatus === 'connected';

  return (
    <div className="min-h-screen flex flex-col">
      <Header
        signerCommitment={signer?.commitment ?? null}
        generatingSigner={generatingSigner}
        psmStatus={psmStatus}
        psmUrl={psmUrl}
        onPsmUrlChange={setPsmUrl}
        onReconnect={(url) => connectToPsm(url)}
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
            signer={signer}
            psmState={psmState}
            proposals={proposals}
            consumableNotes={consumableNotes}
            vaultBalances={detectedConfig?.vaultBalances ?? []}
            procedureThresholds={detectedConfig?.procedureThresholds}
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
            signerCommitment={signer.commitment}
            creating={creating}
            registeringOnPsm={registeringOnPsm}
            onCreate={handleCreate}
          />
          <LoadMultisigDialog
            open={loadDialogOpen}
            onOpenChange={setLoadDialogOpen}
            loading={loadingAccount}
            detectedConfig={detectedConfig}
            error={error}
            onLoad={handleLoad}
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
