import { useEffect, useState, useCallback } from 'react';
import { toast } from 'sonner';

import {
  MultisigClient,
  FalconSigner,
  AccountInspector,
  type Multisig,
  type MultisigConfig,
  type AccountState,
  type DetectedMultisigConfig,
  type Proposal,
} from '@openzeppelin/miden-multisig-client';

import { WebClient, SecretKey } from '@demox-labs/miden-sdk';

import {
  Header,
  WelcomeView,
  CreateMultisigDialog,
  LoadMultisigDialog,
  MultisigDashboard,
} from '@/components';

import { normalizeCommitment } from '@/lib/helpers';
import type { SignerInfo } from '@/types';

const DEFAULT_PSM_URL = 'http://localhost:3000';

export default function App() {
  // Core state
  const [webClient, setWebClient] = useState<WebClient | null>(null);
  const [multisigClient, setMultisigClient] = useState<MultisigClient | null>(null);
  const [signer, setSigner] = useState<SignerInfo | null>(null);
  const [generatingSigner, setGeneratingSigner] = useState(false);
  const [multisig, setMultisig] = useState<Multisig | null>(null);
  const [error, setError] = useState<string | null>(null);

  // PSM state
  const [psmUrl, setPsmUrl] = useState(DEFAULT_PSM_URL);
  const [psmStatus, setPsmStatus] = useState<'connected' | 'connecting' | 'error'>('connecting');
  const [psmPubkey, setPsmPubkey] = useState('');
  const [psmState, setPsmState] = useState<AccountState | null>(null);

  // Dialog state
  const [createDialogOpen, setCreateDialogOpen] = useState(false);
  const [loadDialogOpen, setLoadDialogOpen] = useState(false);

  // Operation state
  const [creating, setCreating] = useState(false);
  const [registeringOnPsm, setRegisteringOnPsm] = useState(false);
  const [loadingAccount, setLoadingAccount] = useState(false);
  const [detectedConfig, setDetectedConfig] = useState<DetectedMultisigConfig | null>(null);
  const [syncingState, setSyncingState] = useState(false);

  // Proposal state
  const [proposals, setProposals] = useState<Proposal[]>([]);
  const [newSignerCommitment, setNewSignerCommitment] = useState('');
  const [increaseThreshold, setIncreaseThreshold] = useState(false);
  const [creatingProposal, setCreatingProposal] = useState(false);
  const [signingProposal, setSigningProposal] = useState<string | null>(null);
  const [executingProposal, setExecutingProposal] = useState<string | null>(null);

  // Connect to PSM server
  const connectToPsm = useCallback(
    async (url: string, client?: WebClient) => {
      setPsmStatus('connecting');
      try {
        const wc = client ?? webClient;
        if (wc) {
          const msClient = new MultisigClient(wc, { psmEndpoint: url });
          const pubkey = await msClient.psmClient.getPubkey();
          setPsmPubkey(pubkey);
          setMultisigClient(msClient);
        } else {
          const response = await fetch(`${url}/pubkey`);
          const data = await response.json();
          setPsmPubkey(data.pubkey || '');
        }
        setPsmStatus('connected');
      } catch {
        setPsmStatus('error');
        setPsmPubkey('');
      }
    },
    [webClient]
  );

  // Generate signer key
  const generateSigner = useCallback(async (client: WebClient) => {
    setGeneratingSigner(true);
    try {
      const seed = new Uint8Array(32);
      crypto.getRandomValues(seed);
      const secretKey = SecretKey.rpoFalconWithRNG(seed);
      await client.addAccountSecretKeyToWebStore(secretKey);
      const publicKey = secretKey.publicKey();
      const commitment = publicKey.toCommitment().toHex();
      setSigner({ commitment, secretKey });
    } catch (err) {
      setError(`Failed to generate signer: ${err instanceof Error ? err.message : 'Unknown'}`);
    } finally {
      setGeneratingSigner(false);
    }
  }, []);

  // Initialize on mount
  useEffect(() => {
    const init = async () => {
      try {
        const client = await WebClient.createClient('https://rpc.testnet.miden.io:443');
        await client.syncState();
        setWebClient(client);
        await connectToPsm(psmUrl, client);
        await generateSigner(client);
      } catch (err) {
        setError(`Initialization failed: ${err instanceof Error ? err.message : 'Unknown'}`);
      }
    };
    init();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Create multisig
  const handleCreate = async (otherSignerCommitments: string[], threshold: number) => {
    if (!multisigClient || !signer || !psmPubkey) return;

    setCreating(true);
    setError(null);
    try {
      const signerCommitments = [signer.commitment, ...otherSignerCommitments];
      const config: MultisigConfig = {
        threshold,
        signerCommitments,
        psmCommitment: psmPubkey,
        psmEnabled: true,
      };
      const falconSigner = new FalconSigner(signer.secretKey);
      const ms = await multisigClient.create(config, falconSigner);
      setMultisig(ms);

      // Auto-register on PSM
      setRegisteringOnPsm(true);
      try {
        await ms.registerOnPsm();
      } catch (psmErr) {
        setError(`Created but failed to register on PSM: ${psmErr instanceof Error ? psmErr.message : 'Unknown'}`);
      } finally {
        setRegisteringOnPsm(false);
      }

      setCreateDialogOpen(false);
    } catch (err) {
      setError(`Failed to create: ${err instanceof Error ? err.message : 'Unknown'}`);
    } finally {
      setCreating(false);
    }
  };

  // Load multisig from PSM
  const handleLoad = async (accountId: string) => {
    if (!multisigClient || !signer || !psmPubkey) return;

    let normalizedId = accountId;
    if (!normalizedId.startsWith('0x')) {
      normalizedId = `0x${normalizedId}`;
    }

    setLoadingAccount(true);
    setError(null);
    setDetectedConfig(null);
    try {
      const falconSigner = new FalconSigner(signer.secretKey);

      // Temporary config to fetch state
      const tempConfig: MultisigConfig = {
        threshold: 1,
        signerCommitments: [signer.commitment],
        psmCommitment: psmPubkey,
        psmEnabled: true,
      };

      const tempMs = await multisigClient.load(normalizedId, tempConfig, falconSigner);
      const state = await tempMs.fetchState();
      const detected = AccountInspector.fromBase64(state.stateDataBase64);
      setDetectedConfig(detected);

      const config: MultisigConfig = {
        threshold: detected.threshold,
        signerCommitments: detected.signerCommitments,
        psmCommitment: detected.psmCommitment || psmPubkey,
        psmEnabled: detected.psmEnabled,
      };

      const ms = await multisigClient.load(normalizedId, config, falconSigner);
      setMultisig(ms);
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

  // Sync state
  const handleSyncState = async () => {
    if (!multisig || !multisigClient || !signer) return;

    setSyncingState(true);
    setError(null);
    try {
      const state = await multisig.fetchState();
      setPsmState(state);

      const detected = AccountInspector.fromBase64(state.stateDataBase64);
      const newConfig: MultisigConfig = {
        threshold: detected.threshold,
        signerCommitments: detected.signerCommitments,
        psmCommitment: detected.psmCommitment || psmPubkey,
        psmEnabled: detected.psmEnabled,
      };

      const falconSigner = new FalconSigner(signer.secretKey);
      const reloadedMs = await multisigClient.load(multisig.accountId, newConfig, falconSigner);
      setMultisig(reloadedMs);
    } catch (err) {
      setError(`Sync failed: ${err instanceof Error ? err.message : 'Unknown'}`);
    } finally {
      setSyncingState(false);
    }
  };

  // Sync proposals
  const handleSyncProposals = async () => {
    if (!multisig) return;

    setSyncingState(true);
    try {
      const synced = await multisig.syncProposals();
      setProposals(synced);
      setError(null);
    } catch (err) {
      setError(`Failed to sync proposals: ${err instanceof Error ? err.message : 'Unknown'}`);
    } finally {
      setSyncingState(false);
    }
  };

  // Create proposal
  const handleCreateProposal = async () => {
    if (!multisig || !webClient) return;

    let commitment: string;
    try {
      commitment = normalizeCommitment(newSignerCommitment);
    } catch (e: any) {
      setError(e?.message ?? 'Invalid commitment');
      return;
    }

    setCreatingProposal(true);
    setError(null);
    try {
      const newThreshold = increaseThreshold ? multisig.threshold + 1 : undefined;
      const proposal = await multisig.createAddSignerProposal(webClient, commitment, undefined, newThreshold);
      const synced = await multisig.syncProposals();
      setProposals(synced);
      setNewSignerCommitment('');
      setIncreaseThreshold(false);
      if (!synced.find((p) => p.id === proposal.id)) {
        setProposals([...synced, proposal]);
      }
    } catch (err) {
      setError(`Failed to create proposal: ${err instanceof Error ? err.message : 'Unknown'}`);
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
      await multisig.signProposal(proposalId);
      const synced = await multisig.syncProposals();
      setProposals(synced);
    } catch (err) {
      setError(`Failed to sign: ${err instanceof Error ? err.message : 'Unknown'}`);
    } finally {
      setSigningProposal(null);
    }
  };

  // Execute proposal
  const handleExecuteProposal = async (proposalId: string) => {
    if (!multisig || !webClient || !multisigClient || !signer) return;

    setExecutingProposal(proposalId);
    setError(null);
    try {
      await multisig.executeProposal(proposalId, webClient);

      // Immediately remove the executed proposal from UI
      setProposals((prev) => prev.filter((p) => p.id !== proposalId));
      toast.success('Proposal executed successfully');

      const state = await multisig.fetchState();
      const detected = AccountInspector.fromBase64(state.stateDataBase64);
      const newConfig: MultisigConfig = {
        threshold: detected.threshold,
        signerCommitments: detected.signerCommitments,
        psmCommitment: detected.psmCommitment || psmPubkey,
        psmEnabled: detected.psmEnabled,
      };

      const falconSigner = new FalconSigner(signer.secretKey);
      const reloadedMs = await multisigClient.load(multisig.accountId, newConfig, falconSigner);
      setMultisig(reloadedMs);
      setPsmState(state);

      // Sync remaining proposals (excluding any finalized ones)
      const synced = await reloadedMs.syncProposals();
      setProposals(synced.filter((p) => p.status.type !== 'finalized'));
    } catch (err) {
      setError(`Failed to execute: ${err instanceof Error ? err.message : 'Unknown'}`);
    } finally {
      setExecutingProposal(null);
    }
  };

  // Disconnect
  const handleDisconnect = () => {
    setMultisig(null);
    setPsmState(null);
    setProposals([]);
    setError(null);
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
        onReconnect={() => connectToPsm(psmUrl)}
      />

      <main className="flex-1">
        {!multisig ? (
          <WelcomeView
            ready={ready}
            onCreateClick={() => setCreateDialogOpen(true)}
            onLoadClick={() => setLoadDialogOpen(true)}
          />
        ) : signer ? (
          <MultisigDashboard
            multisig={multisig}
            signer={signer}
            psmState={psmState}
            proposals={proposals}
            newSignerCommitment={newSignerCommitment}
            increaseThreshold={increaseThreshold}
            creatingProposal={creatingProposal}
            syncingState={syncingState}
            signingProposal={signingProposal}
            executingProposal={executingProposal}
            error={error}
            onNewSignerCommitmentChange={setNewSignerCommitment}
            onIncreaseThresholdChange={setIncreaseThreshold}
            onCreateProposal={handleCreateProposal}
            onSyncState={handleSyncState}
            onSyncProposals={handleSyncProposals}
            onSignProposal={handleSignProposal}
            onExecuteProposal={handleExecuteProposal}
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
            onLoad={handleLoad}
          />
        </>
      )}
    </div>
  );
}
