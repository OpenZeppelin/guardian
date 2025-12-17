import { useEffect, useState, useCallback } from 'react';

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
  StatusSection,
  SignerSection,
  CreateMultisigSection,
  LoadAccountSection,
  AccountInfoSection,
  ProposalSection,
  PsmSyncSection,
} from '@/components';

import { normalizeCommitment } from '@/lib/helpers';
import type { SignerInfo, OtherSigner } from '@/types';

const DEFAULT_PSM_URL = 'http://localhost:3000';

export default function App() {
  // Connection state
  const [psmUrl, setPsmUrl] = useState<string>(DEFAULT_PSM_URL);
  const [psmStatus, setPsmStatus] = useState<string>('Connecting...');
  const [psmPubkey, setPsmPubkey] = useState<string>('');
  const [clientReady, setClientReady] = useState<boolean>(false);
  const [webClient, setWebClient] = useState<WebClient | null>(null);
  const [error, setError] = useState<string | null>(null);

  // This tab's signer (single key per tab)
  const [signer, setSigner] = useState<SignerInfo | null>(null);
  const [generatingKey, setGeneratingKey] = useState<boolean>(false);

  // Other signers' commitments (from other tabs)
  const [otherSigners, setOtherSigners] = useState<OtherSigner[]>([]);
  const [otherCommitmentInput, setOtherCommitmentInput] = useState<string>('');

  // Multisig creation state
  const [threshold, setThreshold] = useState<number>(1);
  const [creating, setCreating] = useState<boolean>(false);

  // Multisig state
  const [multisig, setMultisig] = useState<Multisig | null>(null);
  const [multisigClient, setMultisigClient] = useState<MultisigClient | null>(null);

  // PSM sync state
  const [configuredOnPsm, setConfiguredOnPsm] = useState<boolean>(false);
  const [registeringOnPsm, setRegisteringOnPsm] = useState<boolean>(false);
  const [psmState, setPsmState] = useState<AccountState | null>(null);
  const [syncingState, setSyncingState] = useState<boolean>(false);

  // Proposals
  const [proposals, setProposals] = useState<Proposal[]>([]);
  const [newSignerCommitment, setNewSignerCommitment] = useState<string>('');
  const [increaseThreshold, setIncreaseThreshold] = useState<boolean>(false);
  const [creatingProposal, setCreatingProposal] = useState<boolean>(false);
  const [signingProposal, setSigningProposal] = useState<string | null>(null);
  const [executingProposal, setExecutingProposal] = useState<string | null>(null);

  // Load existing account state
  const [loadAccountIdInput, setLoadAccountIdInput] = useState<string>('');
  const [loadingAccount, setLoadingAccount] = useState<boolean>(false);
  const [detectedConfig, setDetectedConfig] = useState<DetectedMultisigConfig | null>(null);

  // Connect to PSM server (creates MultisigClient when webClient is available)
  const connectToPsm = useCallback(
    async (url: string, client?: WebClient) => {
      setPsmStatus('Connecting...');
      try {
        const wc = client ?? webClient;
        if (wc) {
          const msClient = new MultisigClient(wc, { psmEndpoint: url });
          const pubkey = await msClient.psmClient.getPubkey();
          setPsmPubkey(pubkey);
          setMultisigClient(msClient);
        } else {
          // Fetch pubkey directly without full client if webClient not ready
          const response = await fetch(`${url}/pubkey`);
          const data = await response.json();
          setPsmPubkey(data.commitment || '');
        }
        setPsmStatus('Connected');
        setError(null);
      } catch {
        setPsmStatus('Disconnected');
        setPsmPubkey('');
      }
    },
    [webClient]
  );

  const loadMidenClient = async () => {
    try {
      const client = await WebClient.createClient('https://rpc.testnet.miden.io:443');
      await client.syncState();
      setWebClient(client);
      setClientReady(true);
      connectToPsm(psmUrl, client);
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : 'Unknown error';
      setError(`Failed to initialize Miden client: ${errorMessage}`);
    }
  };

  const handleGenerateSigner = async () => {
    if (!clientReady || !webClient) {
      setError('Miden client not initialized');
      return;
    }

    setGeneratingKey(true);
    setError(null);

    try {
      // Generate a random seed
      const seed = new Uint8Array(32);
      crypto.getRandomValues(seed);

      // Create the Falcon secret key
      const secretKey = SecretKey.rpoFalconWithRNG(seed);

      // Store in miden-sdk's IndexedDB keystore
      await webClient.addAccountSecretKeyToWebStore(secretKey);

      // Get commitment for display
      const publicKey = secretKey.publicKey();
      const commitment = publicKey.toCommitment().toHex();

      setSigner({
        commitment,
        secretKey,
      });
    } catch (err) {
      setError(`Failed to generate key: ${err instanceof Error ? err.message : 'Unknown error'}`);
    } finally {
      setGeneratingKey(false);
    }
  };

  // Add another signer's commitment (from another signer)
  const handleAddOtherSigner = () => {
    let normalizedCommitment: string;
    try {
      normalizedCommitment = normalizeCommitment(otherCommitmentInput);
    } catch (e: any) {
      setError(e?.message ?? 'Invalid commitment');
      return;
    }

    // Check if it's this signer's own commitment
    const ownCommitment = signer?.commitment.toLowerCase();
    if (ownCommitment && normalizedCommitment === ownCommitment) {
      setError("That's your own commitment");
      return;
    }

    // Check for duplicates
    if (otherSigners.some((s) => s.commitment.toLowerCase() === normalizedCommitment)) {
      setError('This commitment has already been added');
      return;
    }

    setOtherSigners((prev) => [
      ...prev,
      { id: `other-${Date.now()}`, commitment: normalizedCommitment },
    ]);
    setOtherCommitmentInput('');
    setError(null);
  };

  // Create "add signer" proposal
  const handleCreateAddSignerProposal = async () => {
    if (!multisig || !webClient) {
      setError('Multisig not created/loaded');
      return;
    }
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
      // If increaseThreshold is checked, set newThreshold to current + 1
      const newThreshold = increaseThreshold ? multisig.threshold + 1 : undefined;
      const proposal = await multisig.createAddSignerProposal(
        webClient,
        commitment,
        undefined,
        newThreshold
      );
      // Refresh proposals list
      const synced = await multisig.syncProposals();
      setProposals(synced);
      setNewSignerCommitment('');
      setIncreaseThreshold(false);
      if (!synced.find((p) => p.id === proposal.id)) {
        setProposals([...synced, proposal]);
      }
    } catch (err: any) {
      setError(err?.message ?? 'Failed to create proposal');
    } finally {
      setCreatingProposal(false);
    }
  };

  const handleSyncProposals = async () => {
    if (!multisig) {
      setError('Multisig not created/loaded');
      return;
    }
    setSyncingState(true);
    try {
      const synced = await multisig.syncProposals();
      setProposals(synced);
      setError(null);
    } catch (err: any) {
      setError(err?.message ?? 'Failed to sync proposals');
    } finally {
      setSyncingState(false);
    }
  };

  const handleSignProposal = async (proposalId: string) => {
    if (!multisig) {
      setError('Multisig not created/loaded');
      return;
    }
    setSigningProposal(proposalId);
    setError(null);
    try {
      await multisig.signProposal(proposalId);
      const synced = await multisig.syncProposals();
      setProposals(synced);
    } catch (err: any) {
      setError(err?.message ?? 'Failed to sign proposal');
    } finally {
      setSigningProposal(null);
    }
  };

  const handleExecuteProposal = async (proposalId: string) => {
    if (!multisig || !webClient || !multisigClient || !signer) {
      setError('Multisig not created/loaded or webClient not ready');
      return;
    }
    setExecutingProposal(proposalId);
    setError(null);
    try {
      await multisig.executeProposal(proposalId, webClient);

      // Reload the multisig with updated state from PSM
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

      // Sync proposals and remove executed one
      const synced = await reloadedMs.syncProposals();
      // Filter out finalized proposals
      setProposals(synced.filter((p) => p.status.type !== 'finalized'));
    } catch (err: any) {
      setError(err?.message ?? 'Failed to execute proposal');
    } finally {
      setExecutingProposal(null);
    }
  };

  const handleRemoveOtherSigner = (id: string) => {
    setOtherSigners((prev) => prev.filter((s) => s.id !== id));
  };

  const totalSigners = (signer ? 1 : 0) + otherSigners.length;

  const handleCreateAccount = async () => {
    if (!clientReady || !multisigClient) {
      setError('Miden client not initialized');
      return;
    }

    if (!psmPubkey) {
      setError('Please connect to PSM server first');
      return;
    }

    if (!signer) {
      setError('Please generate your signer key first');
      return;
    }

    if (totalSigners === 0) {
      setError('At least one signer is required');
      return;
    }

    if (threshold < 1 || threshold > totalSigners) {
      setError(`Threshold must be between 1 and ${totalSigners}`);
      return;
    }

    setCreating(true);
    setError(null);

    try {
      const signerCommitments = [signer.commitment, ...otherSigners.map((s) => s.commitment)];
      const config: MultisigConfig = {
        threshold,
        signerCommitments,
        psmCommitment: psmPubkey,
        psmEnabled: true,
      };
      const falconSigner = new FalconSigner(signer.secretKey);
      const ms = await multisigClient.create(config, falconSigner);
      setMultisig(ms);

      // Automatically register on PSM
      setRegisteringOnPsm(true);
      try {
        await ms.registerOnPsm();
        setConfiguredOnPsm(true);
      } catch (psmErr) {
        const psmMessage = psmErr instanceof Error ? psmErr.message : String(psmErr);
        setError(`Account created but failed to register on PSM: ${psmMessage}`);
      } finally {
        setRegisteringOnPsm(false);
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setError(`Failed to create account: ${message}`);
    } finally {
      setCreating(false);
    }
  };

  const handleLoadFromPsm = async () => {
    if (!clientReady || !multisigClient) {
      setError('Miden client not initialized');
      return;
    }

    if (!signer) {
      setError('Please generate your signer key first');
      return;
    }

    let accountId = loadAccountIdInput.trim();
    if (!accountId) {
      setError('Please enter an account ID');
      return;
    }

    if (!accountId.startsWith('0x')) {
      accountId = `0x${accountId}`;
    }

    setLoadingAccount(true);
    setError(null);
    setDetectedConfig(null);

    try {
      const falconSigner = new FalconSigner(signer.secretKey);

      // Use temporary config to fetch state for inspection
      const tempConfig: MultisigConfig = {
        threshold: 1,
        signerCommitments: [signer.commitment],
        psmCommitment: psmPubkey,
        psmEnabled: true,
      };

      const tempMs = await multisigClient.load(accountId, tempConfig, falconSigner);
      const state = await tempMs.fetchState();

      const detected = AccountInspector.fromBase64(state.stateDataBase64);
      setDetectedConfig(detected);

      const config: MultisigConfig = {
        threshold: detected.threshold,
        signerCommitments: detected.signerCommitments,
        psmCommitment: detected.psmCommitment || psmPubkey,
        psmEnabled: detected.psmEnabled,
      };

      const ms = await multisigClient.load(accountId, config, falconSigner);
      setMultisig(ms);
      setConfiguredOnPsm(true);
      setPsmState(state);
      setThreshold(detected.threshold);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (message.includes('404') || message.includes('not found')) {
        setError(`Account not found on PSM. Make sure the account has been registered first.`);
      } else {
        setError(`Failed to load account: ${message}`);
      }
    } finally {
      setLoadingAccount(false);
    }
  };

  const handleSyncState = async () => {
    if (!clientReady || !multisig || !multisigClient || !signer) {
      setError('Account not created');
      return;
    }

    setSyncingState(true);
    setError(null);

    try {
      const state = await multisig.fetchState();
      setPsmState(state);

      // Reload the multisig with the latest config from PSM
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
      const message = err instanceof Error ? err.message : String(err);
      setError(`Failed to sync state: ${message}`);
    } finally {
      setSyncingState(false);
    }
  };

  useEffect(() => {
    loadMidenClient();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="max-w-3xl mx-auto p-8 space-y-6">
      <h1 className="text-2xl font-bold">Miden Multisig Web Example</h1>

      <StatusSection
        clientReady={clientReady}
        psmStatus={psmStatus}
        psmUrl={psmUrl}
        psmPubkey={psmPubkey}
        error={error}
        onPsmUrlChange={setPsmUrl}
        onReconnect={() => connectToPsm(psmUrl)}
      />

      <SignerSection
        signer={signer}
        clientReady={clientReady}
        generatingKey={generatingKey}
        onGenerateSigner={handleGenerateSigner}
      />

      {!multisig && (
        <CreateMultisigSection
          signer={signer}
          otherSigners={otherSigners}
          otherCommitmentInput={otherCommitmentInput}
          threshold={threshold}
          totalSigners={totalSigners}
          creating={creating}
          registeringOnPsm={registeringOnPsm}
          multisigClientReady={!!multisigClient}
          psmPubkey={psmPubkey}
          onOtherCommitmentChange={setOtherCommitmentInput}
          onAddOtherSigner={handleAddOtherSigner}
          onRemoveOtherSigner={handleRemoveOtherSigner}
          onThresholdChange={setThreshold}
          onCreateAccount={handleCreateAccount}
        />
      )}

      {!multisig && (
        <LoadAccountSection
          loadAccountIdInput={loadAccountIdInput}
          loadingAccount={loadingAccount}
          detectedConfig={detectedConfig}
          multisigClientReady={!!multisigClient}
          signerReady={!!signer}
          onLoadAccountIdChange={setLoadAccountIdInput}
          onLoadFromPsm={handleLoadFromPsm}
        />
      )}

      {multisig && <AccountInfoSection multisig={multisig} />}

      {multisig && (
        <ProposalSection
          multisig={multisig}
          signer={signer}
          proposals={proposals}
          newSignerCommitment={newSignerCommitment}
          increaseThreshold={increaseThreshold}
          creatingProposal={creatingProposal}
          syncingState={syncingState}
          signingProposal={signingProposal}
          executingProposal={executingProposal}
          webClientReady={!!webClient}
          onNewSignerCommitmentChange={setNewSignerCommitment}
          onIncreaseThresholdChange={setIncreaseThreshold}
          onCreateProposal={handleCreateAddSignerProposal}
          onSyncProposals={handleSyncProposals}
          onSignProposal={handleSignProposal}
          onExecuteProposal={handleExecuteProposal}
        />
      )}

      {multisig && (
        <PsmSyncSection
          registeringOnPsm={registeringOnPsm}
          configuredOnPsm={configuredOnPsm}
          syncingState={syncingState}
          psmState={psmState}
          onSyncState={handleSyncState}
        />
      )}
    </div>
  );
}
