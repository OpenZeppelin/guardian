import { useEffect, useState, useCallback } from 'react';
import { toast } from 'sonner';

import {
  type Multisig,
  type MultisigClient,
  type AccountState,
  type DetectedMultisigConfig,
  type TransactionProposal,
  type SignatureScheme,
} from '@openzeppelin/miden-multisig-client';
import { PsmHttpError } from '@openzeppelin/psm-client';

import { WebClient } from '@demox-labs/miden-sdk';

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
} from '@/lib/multisigApi';
import { PSM_ENDPOINT } from '@/config';
import type { SignerInfo } from '@/types';

function isPendingCandidateError(error: unknown): boolean {
  const errorStr = error instanceof Error ? error.message : String(error);
  return (
    errorStr.includes('non-canonical delta pending') ||
    errorStr.includes('ConflictPendingDelta')
  );
}

export default function App() {
  const [webClient, setWebClient] = useState<WebClient | null>(null);
  const [multisigClient, setMultisigClient] = useState<MultisigClient | null>(null);
  const [signer, setSigner] = useState<SignerInfo | null>(null);
  const [generatingSigner, setGeneratingSigner] = useState(false);
  const [multisig, setMultisig] = useState<Multisig | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pendingCandidateWarning, setPendingCandidateWarning] = useState<string | null>(null);

  const [psmUrl, setPsmUrl] = useState(PSM_ENDPOINT);
  const [psmStatus, setPsmStatus] = useState<'connected' | 'connecting' | 'error'>('connecting');
  const [psmCommitment, setPsmCommitment] = useState('');
  const [psmPublicKey, setPsmPublicKey] = useState<string | undefined>(undefined);
  const [psmState, setPsmState] = useState<AccountState | null>(null);

  const [createDialogOpen, setCreateDialogOpen] = useState(false);
  const [loadDialogOpen, setLoadDialogOpen] = useState(false);
  const [importDialogOpen, setImportDialogOpen] = useState(false);
  const [importJson, setImportJson] = useState('');

  const [creating, setCreating] = useState(false);
  const [registeringOnPsm, setRegisteringOnPsm] = useState(false);
  const [loadingAccount, setLoadingAccount] = useState(false);
  const [detectedConfig, setDetectedConfig] = useState<DetectedMultisigConfig | null>(null);
  const [syncingState, setSyncingState] = useState(false);

  const [proposals, setProposals] = useState<TransactionProposal[]>([]);
  const [creatingProposal, setCreatingProposal] = useState(false);
  const [signingProposal, setSigningProposal] = useState<string | null>(null);
  const [executingProposal, setExecutingProposal] = useState<string | null>(null);

  const [consumableNotes, setConsumableNotes] = useState<Array<{ id: string; assets: Array<{ faucetId: string; amount: bigint }> }>>([]);


  const connectToPsm = useCallback(
    async (url: string, client?: WebClient): Promise<void> => {
      setPsmStatus('connecting');
      setError(null);
      try {
        const wc = client ?? webClient;
        if (!wc) {
          const response = await fetch(`${url}/pubkey`);
          const data = await response.json();
          setPsmCommitment(data.commitment ?? '');
          setPsmPublicKey(data.pubkey);
          setPsmStatus('connected');
          return;
        }

        const { client: msClient, psmCommitment: commitment, psmPubkey: pubkey } =
          await initMultisigClient(wc, url);
        setPsmCommitment(commitment);
        setPsmPublicKey(pubkey);
        setMultisigClient(msClient);
        setPsmStatus('connected');

        if (multisig && signer && psmState?.stateDataBase64) {
          setRegisteringOnPsm(true);
          try {
            let ackPublicKey = pubkey;
            if (signer.activeScheme === 'ecdsa' && !ackPublicKey) {
              const { pubkey: fetched } = await msClient.psmClient.getPubkey('ecdsa');
              ackPublicKey = fetched;
              setPsmPublicKey(fetched);
            }
            const reloadedMs = await loadMultisigAccount(
              msClient,
              multisig.accountId,
              signer,
              ackPublicKey,
              signer.activeScheme
            );
            setMultisig(reloadedMs);

            const { proposals: synced, state, notes, config } = await reloadedMs.syncAll();
            setPsmState(state);
            setDetectedConfig(config);
            setProposals(synced);
            setConsumableNotes(notes);

            toast.success('Account loaded from PSM');
          } catch (loadErr) {
            const isNotFound = loadErr instanceof PsmHttpError && loadErr.status === 404;

            if (isNotFound) {
              try {
                await multisig.switchPsm(msClient.psmClient);

                const { proposals: synced, state, notes, config } = await multisig.syncAll();
                setPsmState(state);
                setDetectedConfig(config);
                setProposals(synced);
                setConsumableNotes(notes);

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
        setPsmStatus('error');
        setPsmCommitment('');
        setPsmPublicKey(undefined);
        setError(`Failed to connect to PSM: ${msg}`);
      }
    },
    [webClient, multisig, signer, psmState]
  );

  useEffect(() => {
    const init = async () => {
      try {
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
  }, []);

  const handleCreate = async (
    otherSignerCommitments: string[],
    threshold: number,
    procedureThresholds?: import('@openzeppelin/miden-multisig-client').ProcedureThreshold[],
    signatureScheme: SignatureScheme = 'falcon',
  ) => {
    if (!multisigClient || !signer || !psmCommitment) return;

    setCreating(true);
    setError(null);
    try {
      setSigner((prev) => (prev ? { ...prev, activeScheme: signatureScheme } : prev));
      let ackPublicKey = psmPublicKey;
      let accountPsmCommitment = psmCommitment;
      if (signatureScheme === 'ecdsa') {
        const { pubkey, commitment } = await multisigClient.psmClient.getPubkey('ecdsa');
        if (!ackPublicKey) {
          ackPublicKey = pubkey;
          setPsmPublicKey(pubkey);
        }
        accountPsmCommitment = commitment;
        setPsmCommitment(commitment);
      }
      const ms = await createMultisigAccount(
        multisigClient,
        signer,
        otherSignerCommitments,
        threshold,
        accountPsmCommitment,
        ackPublicKey,
        procedureThresholds,
        signatureScheme
      );
      setMultisig(ms);

      setRegisteringOnPsm(true);
      try {
        await ms.registerOnPsm();
        const { proposals: synced, state, notes, config } = await ms.syncAll();
        setPsmState(state);
        setDetectedConfig(config);
        setProposals(synced);
        setConsumableNotes(notes);
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

  const handleLoad = async (accountId: string, signatureScheme: SignatureScheme = 'falcon') => {
    if (!multisigClient || !signer) {
      setError('Client not initialized. Try reconnecting to PSM.');
      return;
    }
    if (!psmCommitment) {
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
      setSigner((prev) => (prev ? { ...prev, activeScheme: signatureScheme } : prev));
      let ackPublicKey = psmPublicKey;
      if (signatureScheme === 'ecdsa' && !ackPublicKey) {
        const { pubkey } = await multisigClient.psmClient.getPubkey('ecdsa');
        ackPublicKey = pubkey;
        setPsmPublicKey(pubkey);
      }
      const ms = await loadMultisigAccount(
        multisigClient,
        normalizedId,
        signer,
        ackPublicKey,
        signatureScheme
      );
      setMultisig(ms);

      const { proposals: synced, state, notes, config } = await ms.syncAll();
      setDetectedConfig(config);
      setPsmState(state);
      setProposals(synced);
      setConsumableNotes(notes);

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

  const handleSync = async () => {
    if (!multisig || !webClient) return;

    setSyncingState(true);
    setError(null);
    setPendingCandidateWarning(null);
    try {
      try {
        await webClient.syncState();
      } catch {
        await new Promise(resolve => setTimeout(resolve, 500));
        await webClient.syncState();
      }

      const { proposals: synced, state, notes, config } = await multisig.syncAll();
      setPsmState(state);
      setDetectedConfig(config);
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
      const newThreshold = increaseThreshold ? multisig.threshold + 1 : undefined;
      const { proposals } = await multisig.createAddSignerProposal(normalizedCommitment, { newThreshold });
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

  const handleCreateRemoveSignerProposal = async (signerToRemove: string, newThreshold?: number) => {
    if (!multisig) return;

    setCreatingProposal(true);
    setError(null);
    setPendingCandidateWarning(null);
    try {
      const { proposals } = await multisig.createRemoveSignerProposal(signerToRemove, { newThreshold });
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

  const handleCreateChangeThresholdProposal = async (newThreshold: number) => {
    if (!multisig) return;

    setCreatingProposal(true);
    setError(null);
    setPendingCandidateWarning(null);
    try {
      const { proposals } = await multisig.createChangeThresholdProposal(newThreshold);
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

  const handleCreateConsumeNotesProposal = async (noteIds: string[]) => {
    if (!multisig) return;

    setCreatingProposal(true);
    setError(null);
    setPendingCandidateWarning(null);
    try {
      const { proposals } = await multisig.createConsumeNotesProposal(noteIds);
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

  const handleCreateP2idProposal = async (recipientId: string, faucetId: string, amount: bigint) => {
    if (!multisig) return;

    setCreatingProposal(true);
    setError(null);
    setPendingCandidateWarning(null);
    try {
      const { proposals } = await multisig.createSendProposal(recipientId, faucetId, amount);
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

  const handleCreateSwitchPsmProposal = async (newEndpoint: string, newPubkey: string) => {
    if (!multisig) return;

    setCreatingProposal(true);
    setError(null);
    setPendingCandidateWarning(null);
    try {
      const { proposals } = await multisig.createSwitchPsmProposal(newEndpoint, newPubkey);
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

  const handleSignProposal = async (proposalId: string) => {
    if (!multisig) return;

    setSigningProposal(proposalId);
    setError(null);
    try {
      const proposals = await multisig.signTransactionProposal(proposalId);
      setProposals(proposals);
    } catch (err) {
      setError(`Failed to sign: ${err instanceof Error ? err.message : 'Unknown'}`);
    } finally {
      setSigningProposal(null);
    }
  };

  const handleExecuteProposal = async (proposalId: string) => {
    if (!multisig) return;

    setExecutingProposal(proposalId);
    setError(null);
    setPendingCandidateWarning(null);
    try {
      await multisig.executeTransactionProposal(proposalId);
      toast.success('Proposal executed successfully');

      await handleSync();
    } catch (err) {
      const message = formatError(err, 'Execute failed');
      if (isPendingCandidateError(err)) {
        setPendingCandidateWarning(
          'A previous transaction is still being processed on-chain. ' +
          'Please wait for it to be confirmed before executing proposals.'
        );
      } else {
        setError(message);
        toast.error(message);
      }
    } finally {
      setExecutingProposal(null);
    }
  };

  const handleExportProposal = (proposalId: string) => {
    if (!multisig) return;

    try {
      const json = multisig.exportTransactionProposalToJson(proposalId);
      navigator.clipboard.writeText(json);
      toast.success('Proposal JSON copied to clipboard');
    } catch (err) {
      setError(`Failed to export: ${err instanceof Error ? err.message : 'Unknown'}`);
    }
  };

  const handleSignProposalOffline = (proposalId: string) => {
    if (!multisig) return;

    try {
      const json = multisig.signTransactionProposalOffline(proposalId);
      navigator.clipboard.writeText(json);
      setProposals(multisig.listTransactionProposals());
      toast.success('Signed! Updated proposal JSON copied to clipboard');
    } catch (err) {
      setError(`Failed to sign offline: ${err instanceof Error ? err.message : 'Unknown'}`);
    }
  };

  const handleImportProposal = () => {
    setImportJson('');
    setImportDialogOpen(true);
  };

  const handleImportProposalSubmit = () => {
    if (!multisig || !importJson.trim()) return;

    try {
      const { proposal, proposals } = multisig.importTransactionProposal(importJson.trim());
      setProposals(proposals);
      setImportDialogOpen(false);
      setImportJson('');
      toast.success(`Proposal imported: ${proposal.id.slice(0, 12)}...`);
    } catch (err) {
      setError(`Failed to import: ${err instanceof Error ? err.message : 'Unknown'}`);
    }
  };

  const handleDisconnect = () => {
    setMultisig(null);
    setPsmState(null);
    setProposals([]);
    setError(null);
  };

  const handleResetData = () => {
    toast.success('Reloading with fresh signer key...');
    setTimeout(() => window.location.reload(), 500);
  };

  const ready = !!webClient && !!signer && !!multisigClient && psmStatus === 'connected';

  return (
    <div className="min-h-screen flex flex-col">
      <Header
        falconCommitment={signer?.falcon.commitment ?? null}
        ecdsaCommitment={signer?.ecdsa.commitment ?? null}
        activeScheme={signer?.activeScheme ?? null}
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
            detectedThreshold={detectedConfig?.threshold}
            detectedSignerCommitments={detectedConfig?.signerCommitments}
            creatingProposal={creatingProposal}
            syncing={syncingState}
            signingProposal={signingProposal}
            executingProposal={executingProposal}
            error={error}
            pendingCandidateWarning={pendingCandidateWarning}
            onDismissWarning={() => setPendingCandidateWarning(null)}
            onCreateAddSigner={handleCreateAddSignerProposal}
            onCreateRemoveSigner={handleCreateRemoveSignerProposal}
            onCreateChangeThreshold={handleCreateChangeThresholdProposal}
            onCreateConsumeNotes={handleCreateConsumeNotesProposal}
            onCreateP2id={handleCreateP2idProposal}
            onCreateSwitchPsm={handleCreateSwitchPsmProposal}
            onSync={handleSync}
            onSignProposal={handleSignProposal}
            onExecuteProposal={handleExecuteProposal}
            onExportProposal={handleExportProposal}
            onSignProposalOffline={handleSignProposalOffline}
            onImportProposal={handleImportProposal}
            onDisconnect={handleDisconnect}
          />
        ) : null}
      </main>

      {signer && (
        <>
          <CreateMultisigDialog
            open={createDialogOpen}
            onOpenChange={setCreateDialogOpen}
            falconCommitment={signer.falcon.commitment}
            ecdsaCommitment={signer.ecdsa.commitment}
            defaultScheme={signer.activeScheme}
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
            defaultScheme={signer.activeScheme}
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
