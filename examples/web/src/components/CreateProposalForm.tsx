import { useState } from 'react';
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
import type { ConsumableNote } from '@openzeppelin/miden-multisig-client';

type ProposalType = 'add_signer' | 'remove_signer' | 'change_threshold' | 'consume_notes';

interface CreateProposalFormProps {
  currentThreshold: number;
  signerCommitments: string[];
  creatingProposal: boolean;
  consumableNotes: ConsumableNote[];
  onCreateAddSigner: (commitment: string, increaseThreshold: boolean) => void;
  onCreateRemoveSigner: (signerToRemove: string, newThreshold?: number) => void;
  onCreateChangeThreshold: (newThreshold: number) => void;
  onCreateConsumeNotes: (noteIds: string[]) => void;
}

export function CreateProposalForm({
  currentThreshold,
  signerCommitments,
  creatingProposal,
  consumableNotes,
  onCreateAddSigner,
  onCreateRemoveSigner,
  onCreateChangeThreshold,
  onCreateConsumeNotes,
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

  // Consume notes state
  const [selectedNoteIds, setSelectedNoteIds] = useState<string[]>([]);

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
      case 'consume_notes':
        if (selectedNoteIds.length > 0) {
          onCreateConsumeNotes(selectedNoteIds);
          setSelectedNoteIds([]);
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
      case 'consume_notes':
        return selectedNoteIds.length > 0;
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
      case 'consume_notes':
        return 'Create a proposal to consume notes sent to the multisig account.';
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
              <SelectItem value="consume_notes">Consume Notes</SelectItem>
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

        {/* Create Button */}
        <Button onClick={handleCreate} disabled={creatingProposal || !isValid()}>
          {creatingProposal ? 'Creating...' : 'Create Proposal'}
        </Button>
      </CardContent>
    </Card>
  );
}
