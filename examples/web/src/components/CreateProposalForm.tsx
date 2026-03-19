import { useState, useEffect, useRef } from 'react';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Checkbox } from '@/components/ui/checkbox';
import { Label } from '@/components/ui/label';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import type {
  ConsumableNote,
  VaultBalance,
  ProcedureName,
  SignatureScheme,
} from '@openzeppelin/miden-multisig-client';
import { USER_PROCEDURES } from '@/lib/procedures';

type ProposalType = 'add_signer' | 'remove_signer' | 'change_threshold' | 'update_procedure_threshold' | 'consume_notes' | 'p2id' | 'switch_guardian';

interface CreateProposalFormProps {
  currentThreshold: number;
  signerCommitments: string[];
  signatureScheme: SignatureScheme;
  procedureThresholds?: Map<ProcedureName, number>;
  creatingProposal: boolean;
  consumableNotes: ConsumableNote[];
  vaultBalances: VaultBalance[];
  onCreateAddSigner: (commitment: string, increaseThreshold: boolean) => void;
  onCreateRemoveSigner: (signerToRemove: string, newThreshold?: number) => void;
  onCreateChangeThreshold: (newThreshold: number) => void;
  onCreateUpdateProcedureThreshold: (procedure: ProcedureName, threshold: number) => void;
  onCreateConsumeNotes: (noteIds: string[]) => void;
  onCreateP2id: (recipientId: string, faucetId: string, amount: bigint) => void;
  onCreateSwitchGuardian: (newEndpoint: string, newPubkey: string) => void;
}

export function CreateProposalForm({
  currentThreshold,
  signerCommitments,
  signatureScheme,
  procedureThresholds,
  creatingProposal,
  consumableNotes,
  vaultBalances,
  onCreateAddSigner,
  onCreateRemoveSigner,
  onCreateChangeThreshold,
  onCreateUpdateProcedureThreshold,
  onCreateConsumeNotes,
  onCreateP2id,
  onCreateSwitchGuardian,
}: CreateProposalFormProps) {
  const [proposalType, setProposalType] = useState<ProposalType>('add_signer');

  // Add signer state
  const [newSignerCommitment, setNewSignerCommitment] = useState('');
  const [increaseThreshold, setIncreaseThreshold] = useState(false);

  // Remove signer state
  const [signerToRemove, setSignerToRemove] = useState('');
  const [adjustThresholdOnRemove, setAdjustThresholdOnRemove] = useState(true);

  // Change threshold state
  const [newThreshold, setNewThreshold] = useState(currentThreshold);

  // Procedure-threshold override state
  const [targetProcedure, setTargetProcedure] = useState<ProcedureName>('send_asset');
  const [procedureThreshold, setProcedureThreshold] = useState('');

  // Consume notes state
  const [selectedNoteIds, setSelectedNoteIds] = useState<string[]>([]);

  // P2ID state
  const [recipientId, setRecipientId] = useState('');
  const [selectedFaucetId, setSelectedFaucetId] = useState('');
  const [sendAmount, setSendAmount] = useState('');

  // Switch GUARDIAN state
  const [newGuardianEndpoint, setNewGuardianEndpoint] = useState('');
  const [newGuardianPubkey, setNewGuardianPubkey] = useState('');
  const [fetchingPubkey, setFetchingPubkey] = useState(false);
  const [pubkeyError, setPubkeyError] = useState<string | null>(null);
  const debounceRef = useRef<number | null>(null);

  // Auto-fetch pubkey when endpoint changes
  useEffect(() => {
    if (!newGuardianEndpoint.trim()) {
      setNewGuardianPubkey('');
      setPubkeyError(null);
      return;
    }

    // Clear previous timeout
    if (debounceRef.current) {
      clearTimeout(debounceRef.current);
    }

    // Debounce the fetch
    debounceRef.current = window.setTimeout(async () => {
      setFetchingPubkey(true);
      setPubkeyError(null);
      setNewGuardianPubkey('');

      try {
        const response = await fetch(
          `${newGuardianEndpoint.trim()}/pubkey?scheme=${signatureScheme}`,
        );
        if (!response.ok) {
          throw new Error(`HTTP ${response.status}`);
        }
        const data = await response.json();
        if (data.commitment) {
          setNewGuardianPubkey(data.commitment);
        } else {
          throw new Error('No commitment in response');
        }
      } catch (err) {
        setPubkeyError(err instanceof Error ? err.message : 'Failed to fetch');
      } finally {
        setFetchingPubkey(false);
      }
    }, 500);

    return () => {
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
      }
    };
  }, [newGuardianEndpoint, signatureScheme]);

  const handleCreate = () => {
    switch (proposalType) {
      case 'add_signer':
        if (newSignerCommitment.trim()) {
          onCreateAddSigner(newSignerCommitment.trim(), increaseThreshold);
          setNewSignerCommitment('');
          setIncreaseThreshold(false);
        }
        break;
      case 'remove_signer':
        if (signerToRemove) {
          // If adjustThresholdOnRemove is true, let SDK auto-adjust; otherwise pass current threshold
          const thresholdArg = adjustThresholdOnRemove ? undefined : currentThreshold;
          onCreateRemoveSigner(signerToRemove, thresholdArg);
          setSignerToRemove('');
          setAdjustThresholdOnRemove(true);
        }
        break;
      case 'change_threshold':
        if (newThreshold !== currentThreshold) {
          onCreateChangeThreshold(newThreshold);
        }
        break;
      case 'update_procedure_threshold':
        if (targetProcedure && procedureThreshold !== '') {
          onCreateUpdateProcedureThreshold(targetProcedure, Number(procedureThreshold));
          setProcedureThreshold('');
        }
        break;
      case 'consume_notes':
        if (selectedNoteIds.length > 0) {
          onCreateConsumeNotes(selectedNoteIds);
          setSelectedNoteIds([]);
        }
        break;
      case 'p2id':
        if (recipientId.trim() && selectedFaucetId && sendAmount) {
          const amount = BigInt(sendAmount);
          if (amount > 0n) {
            onCreateP2id(recipientId.trim(), selectedFaucetId, amount);
            setRecipientId('');
            setSelectedFaucetId('');
            setSendAmount('');
          }
        }
        break;
      case 'switch_guardian':
        if (newGuardianEndpoint.trim() && newGuardianPubkey) {
          onCreateSwitchGuardian(newGuardianEndpoint.trim(), newGuardianPubkey);
          setNewGuardianEndpoint('');
          setNewGuardianPubkey('');
          setPubkeyError(null);
        }
        break;
    }
  };

  const isValid = () => {
    switch (proposalType) {
      case 'add_signer':
        return newSignerCommitment.trim().length > 0;
      case 'remove_signer':
        return signerToRemove.length > 0 && signerCommitments.length > 1;
      case 'change_threshold':
        return newThreshold !== currentThreshold && newThreshold >= 1 && newThreshold <= signerCommitments.length;
      case 'update_procedure_threshold': {
        if (!targetProcedure || procedureThreshold === '') {
          return false;
        }

        const threshold = Number(procedureThreshold);
        return Number.isInteger(threshold) && threshold >= 0 && threshold <= signerCommitments.length;
      }
      case 'consume_notes':
        return selectedNoteIds.length > 0;
      case 'p2id': {
        if (!recipientId.trim() || !selectedFaucetId || !sendAmount) return false;
        try {
          const amount = BigInt(sendAmount);
          const balance = vaultBalances.find((b) => b.faucetId === selectedFaucetId);
          return amount > 0n && (!balance || amount <= balance.amount);
        } catch {
          return false;
        }
      }
      case 'switch_guardian':
        return newGuardianEndpoint.trim().length > 0 && newGuardianPubkey.length > 0 && !fetchingPubkey && !pubkeyError;
    }
  };

  const getDescription = () => {
    switch (proposalType) {
      case 'add_signer':
        return 'Create a proposal to add a new signer to the multisig.';
      case 'remove_signer':
        return 'Create a proposal to remove an existing signer from the multisig.';
      case 'change_threshold':
        return 'Create a proposal to change the required signature threshold.';
      case 'update_procedure_threshold':
        return 'Create a proposal to set or clear a per-procedure threshold override.';
      case 'consume_notes':
        return 'Create a proposal to consume notes sent to the multisig account.';
      case 'p2id':
        return 'Create a proposal to send funds to another account.';
      case 'switch_guardian':
        return 'Create a proposal to switch the GUARDIAN provider for this multisig.';
    }
  };

  const toggleNoteSelection = (noteId: string) => {
    setSelectedNoteIds((prev) =>
      prev.includes(noteId) ? prev.filter((id) => id !== noteId) : [...prev, noteId]
    );
  };

  const formatAmount = (amount: bigint): string => {
    return amount.toString();
  };

  // Calculate what threshold would be after removing a signer
  const thresholdAfterRemove = Math.min(currentThreshold, signerCommitments.length - 1);

  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="text-lg">Create Proposal</CardTitle>
        <CardDescription>{getDescription()}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {/* Proposal Type Selector */}
        <div className="space-y-2">
          <Label>Proposal Type</Label>
          <Select
            value={proposalType}
            onValueChange={(value: ProposalType) => setProposalType(value)}
          >
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="add_signer">Add Signer</SelectItem>
              <SelectItem value="remove_signer">Remove Signer</SelectItem>
              <SelectItem value="change_threshold">Change Threshold</SelectItem>
              <SelectItem value="update_procedure_threshold">Update Procedure Threshold</SelectItem>
              <SelectItem value="consume_notes">Consume Notes</SelectItem>
              <SelectItem value="p2id">Send Payment</SelectItem>
              <SelectItem value="switch_guardian">Switch GUARDIAN</SelectItem>
            </SelectContent>
          </Select>
        </div>

        {/* Add Signer Form */}
        {proposalType === 'add_signer' && (
          <div className="space-y-3">
            <div className="space-y-2">
              <Label>New Signer Commitment</Label>
              <Input
                placeholder="0x..."
                value={newSignerCommitment}
                onChange={(e) => setNewSignerCommitment(e.target.value)}
              />
            </div>

            <div className="flex items-center space-x-2">
              <Checkbox
                id="increase-threshold"
                checked={increaseThreshold}
                onCheckedChange={(checked) => setIncreaseThreshold(checked === true)}
              />
              <Label htmlFor="increase-threshold" className="text-sm">
                Increase threshold to {currentThreshold + 1}
              </Label>
            </div>
          </div>
        )}

        {/* Remove Signer Form */}
        {proposalType === 'remove_signer' && (
          <div className="space-y-3">
            {signerCommitments.length <= 1 ? (
              <p className="text-sm text-muted-foreground">
                Cannot remove the last signer from the multisig.
              </p>
            ) : (
              <>
                <div className="space-y-2">
                  <Label>Signer to Remove</Label>
                  <Select value={signerToRemove} onValueChange={setSignerToRemove}>
                    <SelectTrigger>
                      <SelectValue placeholder="Select a signer..." />
                    </SelectTrigger>
                    <SelectContent>
                      {signerCommitments.map((commitment) => (
                        <SelectItem key={commitment} value={commitment}>
                          <span className="font-mono text-xs">
                            {commitment.slice(0, 10)}...{commitment.slice(-8)}
                          </span>
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>

                {currentThreshold > signerCommitments.length - 1 && (
                  <div className="flex items-center space-x-2">
                    <Checkbox
                      id="adjust-threshold"
                      checked={adjustThresholdOnRemove}
                      onCheckedChange={(checked) => setAdjustThresholdOnRemove(checked === true)}
                      disabled
                    />
                    <Label htmlFor="adjust-threshold" className="text-sm text-muted-foreground">
                      Threshold will be reduced to {thresholdAfterRemove} (required)
                    </Label>
                  </div>
                )}

                {currentThreshold <= signerCommitments.length - 1 && (
                  <p className="text-sm text-muted-foreground">
                    New configuration: {currentThreshold}-of-{signerCommitments.length - 1}
                  </p>
                )}
              </>
            )}
          </div>
        )}

        {/* Change Threshold Form */}
        {proposalType === 'change_threshold' && (
          <div className="space-y-3">
            <div className="space-y-2">
              <Label>New Threshold</Label>
              <Select
                value={newThreshold.toString()}
                onValueChange={(value) => setNewThreshold(parseInt(value, 10))}
              >
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {Array.from({ length: signerCommitments.length }, (_, i) => i + 1).map((t) => (
                    <SelectItem key={t} value={t.toString()}>
                      {t}-of-{signerCommitments.length}
                      {t === currentThreshold && ' (current)'}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            {newThreshold !== currentThreshold && (
              <p className="text-sm text-muted-foreground">
                Change from {currentThreshold}-of-{signerCommitments.length} to {newThreshold}-of-{signerCommitments.length}
              </p>
            )}
          </div>
        )}

        {proposalType === 'update_procedure_threshold' && (
          <div className="space-y-3">
            <div className="space-y-2">
              <Label>Target Procedure</Label>
              <Select
                value={targetProcedure}
                onValueChange={(value: ProcedureName) => setTargetProcedure(value)}
              >
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {USER_PROCEDURES.map((procedure) => (
                    <SelectItem key={procedure.name} value={procedure.name}>
                      {procedure.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            <div className="space-y-2">
              <Label>Override Threshold</Label>
              <Input
                type="number"
                min={0}
                max={signerCommitments.length}
                value={procedureThreshold}
                onChange={(e) => setProcedureThreshold(e.target.value)}
                placeholder="0 clears the override"
              />
              <p className="text-xs text-muted-foreground">
                Current override: {procedureThresholds?.get(targetProcedure) ?? 'none'}
              </p>
              <p className="text-xs text-muted-foreground">
                Use 0 to clear the override. Non-zero values must be between 1 and {signerCommitments.length}.
              </p>
            </div>
          </div>
        )}

        {/* Consume Notes Form */}
        {proposalType === 'consume_notes' && (
          <div className="space-y-3">
            {consumableNotes.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                No consumable notes found. Sync to check for new notes.
              </p>
            ) : (
              <>
                <div className="space-y-2">
                  <Label>Select Notes to Consume</Label>
                  <div className="border rounded-md p-3 space-y-2 max-h-48 overflow-y-auto">
                    {consumableNotes.map((note) => (
                      <div
                        key={note.id}
                        className="flex items-center space-x-3 p-2 hover:bg-muted rounded cursor-pointer"
                        onClick={() => toggleNoteSelection(note.id)}
                      >
                        <Checkbox
                          checked={selectedNoteIds.includes(note.id)}
                          onCheckedChange={() => toggleNoteSelection(note.id)}
                        />
                        <div className="flex-1 min-w-0">
                          <p className="font-mono text-xs truncate">
                            {note.id.slice(0, 16)}...{note.id.slice(-8)}
                          </p>
                          {note.assets.length > 0 && (
                            <p className="text-xs text-muted-foreground">
                              {note.assets.map((a, i) => (
                                <span key={i}>
                                  {formatAmount(a.amount)} from {a.faucetId.slice(0, 10)}...
                                  {i < note.assets.length - 1 && ', '}
                                </span>
                              ))}
                            </p>
                          )}
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
                {selectedNoteIds.length > 0 && (
                  <p className="text-sm text-muted-foreground">
                    {selectedNoteIds.length} note(s) selected
                  </p>
                )}
              </>
            )}
          </div>
        )}

        {/* P2ID (Send Payment) Form */}
        {proposalType === 'p2id' && (
          <div className="space-y-3">
            {vaultBalances.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                No assets in vault. Consume notes first to receive funds.
              </p>
            ) : (
              <>
                <div className="space-y-2">
                  <Label>Recipient Account ID</Label>
                  <Input
                    placeholder="0x..."
                    value={recipientId}
                    onChange={(e) => setRecipientId(e.target.value)}
                  />
                </div>

                <div className="space-y-2">
                  <Label>Asset</Label>
                  <Select value={selectedFaucetId} onValueChange={setSelectedFaucetId}>
                    <SelectTrigger>
                      <SelectValue placeholder="Select an asset..." />
                    </SelectTrigger>
                    <SelectContent>
                      {vaultBalances.map((balance) => (
                        <SelectItem key={balance.faucetId} value={balance.faucetId}>
                          <span className="font-mono text-xs">
                            {balance.faucetId.slice(0, 10)}...{balance.faucetId.slice(-6)}
                          </span>
                          <span className="ml-2 text-muted-foreground">
                            (Balance: {balance.amount.toString()})
                          </span>
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>

                <div className="space-y-2">
                  <Label>Amount</Label>
                  <Input
                    type="number"
                    placeholder="0"
                    value={sendAmount}
                    onChange={(e) => setSendAmount(e.target.value)}
                    min="1"
                  />
                  {selectedFaucetId && (
                    <p className="text-xs text-muted-foreground">
                      Available: {vaultBalances.find((b) => b.faucetId === selectedFaucetId)?.amount.toString() ?? '0'}
                    </p>
                  )}
                </div>
              </>
            )}
          </div>
        )}

        {/* Switch GUARDIAN Form */}
        {proposalType === 'switch_guardian' && (
          <div className="space-y-3">
            <div className="space-y-2">
              <Label>New GUARDIAN Endpoint</Label>
              <Input
                placeholder="http://localhost:3000"
                value={newGuardianEndpoint}
                onChange={(e) => setNewGuardianEndpoint(e.target.value)}
              />
            </div>

            <div className="space-y-2">
              <Label>GUARDIAN Commitment</Label>
              {fetchingPubkey ? (
                <p className="text-sm text-muted-foreground">Fetching commitment...</p>
              ) : pubkeyError ? (
                <p className="text-sm text-destructive">Failed to fetch: {pubkeyError}</p>
              ) : newGuardianPubkey ? (
                <code className="block text-xs bg-muted px-2 py-1 rounded font-mono break-all">
                  {newGuardianPubkey}
                </code>
              ) : (
                <p className="text-sm text-muted-foreground">
                  Enter an endpoint URL to auto-fetch the commitment
                </p>
              )}
            </div>
          </div>
        )}

        {/* Create Button */}
        <Button onClick={handleCreate} disabled={creatingProposal || !isValid()}>
          {creatingProposal ? 'Creating...' : 'Create Proposal'}
        </Button>
      </CardContent>
    </Card>
  );
}
