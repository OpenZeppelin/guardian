import { useState } from 'react';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Separator } from '@/components/ui/separator';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { normalizeCommitment, truncateHex } from '@/lib/helpers';
import type { OtherSigner } from '@/types';

interface CreateMultisigDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  signerCommitment: string;
  creating: boolean;
  registeringOnPsm: boolean;
  onCreate: (otherSigners: string[], threshold: number) => void;
}

export function CreateMultisigDialog({
  open,
  onOpenChange,
  signerCommitment,
  creating,
  registeringOnPsm,
  onCreate,
}: CreateMultisigDialogProps) {
  const [otherSigners, setOtherSigners] = useState<OtherSigner[]>([]);
  const [otherCommitmentInput, setOtherCommitmentInput] = useState('');
  const [threshold, setThreshold] = useState(1);
  const [error, setError] = useState<string | null>(null);

  const totalSigners = 1 + otherSigners.length;

  const handleAddOtherSigner = () => {
    let normalizedCommitment: string;
    try {
      normalizedCommitment = normalizeCommitment(otherCommitmentInput);
    } catch (e: any) {
      setError(e?.message ?? 'Invalid commitment');
      return;
    }

    if (normalizedCommitment === signerCommitment.toLowerCase()) {
      setError("That's your own commitment");
      return;
    }

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

  const handleRemoveOtherSigner = (id: string) => {
    setOtherSigners((prev) => prev.filter((s) => s.id !== id));
  };

  const handleCreate = () => {
    if (threshold < 1 || threshold > totalSigners) {
      setError(`Threshold must be between 1 and ${totalSigners}`);
      return;
    }
    setError(null);
    onCreate(otherSigners.map((s) => s.commitment), threshold);
  };

  const handleClose = () => {
    if (!creating) {
      setOtherSigners([]);
      setOtherCommitmentInput('');
      setThreshold(1);
      setError(null);
      onOpenChange(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={handleClose}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Create Multisig</DialogTitle>
          <DialogDescription>
            Configure signers and threshold for your new multisig account.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          {/* Signers list */}
          <div className="space-y-2">
            <Label>Signers ({totalSigners} total)</Label>
            <div className="bg-muted/50 rounded-lg p-3 space-y-2 text-sm">
              <div className="flex items-center justify-between">
                <span>
                  <strong>You:</strong>{' '}
                  <code className="text-xs">{truncateHex(signerCommitment, 12, 6)}</code>
                </span>
              </div>
              {otherSigners.map((s, i) => (
                <div key={s.id} className="flex items-center justify-between">
                  <span>
                    Signer {i + 2}:{' '}
                    <code className="text-xs">{truncateHex(s.commitment, 12, 6)}</code>
                  </span>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-6 px-2 text-destructive hover:text-destructive"
                    onClick={() => handleRemoveOtherSigner(s.id)}
                  >
                    Remove
                  </Button>
                </div>
              ))}
            </div>
          </div>

          {/* Add other signer */}
          <div className="flex gap-2">
            <Input
              placeholder="Add signer commitment (0x...)"
              value={otherCommitmentInput}
              onChange={(e) => setOtherCommitmentInput(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && handleAddOtherSigner()}
              className="flex-1 text-sm"
            />
            <Button
              variant="outline"
              size="sm"
              onClick={handleAddOtherSigner}
              disabled={!otherCommitmentInput.trim()}
            >
              Add
            </Button>
          </div>

          <Separator />

          {/* Threshold */}
          <div className="flex items-center gap-3">
            <Label htmlFor="threshold">Threshold:</Label>
            <Input
              id="threshold"
              type="number"
              min={1}
              max={totalSigners}
              value={threshold}
              onChange={(e) => setThreshold(Math.max(1, parseInt(e.target.value) || 1))}
              className="w-16"
            />
            <span className="text-sm text-muted-foreground">of {totalSigners} required</span>
          </div>

          {error && (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          )}

          {/* Create button */}
          <Button onClick={handleCreate} className="w-full" disabled={creating}>
            {creating
              ? registeringOnPsm
                ? 'Registering on PSM...'
                : 'Creating...'
              : `Create ${threshold}-of-${totalSigners} Multisig`}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
