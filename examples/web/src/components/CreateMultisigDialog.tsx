import { useEffect, useState } from 'react';
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { normalizeCommitment, truncateHex } from '@/lib/helpers';
import { USER_PROCEDURES } from '@/lib/procedures';
import type { OtherSigner } from '@/types';
import type { ProcedureThreshold, ProcedureName, SignatureScheme } from '@openzeppelin/miden-multisig-client';

interface CreateMultisigDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  falconCommitment: string;
  ecdsaCommitment: string;
  defaultScheme: SignatureScheme;
  creating: boolean;
  registeringOnPsm: boolean;
  onCreate: (
    otherSigners: string[],
    threshold: number,
    procedureThresholds: ProcedureThreshold[] | undefined,
    signatureScheme: SignatureScheme
  ) => void;
}

export function CreateMultisigDialog({
  open,
  onOpenChange,
  falconCommitment,
  ecdsaCommitment,
  defaultScheme,
  creating,
  registeringOnPsm,
  onCreate,
}: CreateMultisigDialogProps) {
  const [otherSigners, setOtherSigners] = useState<OtherSigner[]>([]);
  const [otherCommitmentInput, setOtherCommitmentInput] = useState('');
  const [threshold, setThreshold] = useState(1);
  const [error, setError] = useState<string | null>(null);
  const [signatureScheme, setSignatureScheme] = useState<SignatureScheme>(defaultScheme);

  // Advanced settings state
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [procedureOverrides, setProcedureOverrides] = useState<Map<ProcedureName, number | null>>(
    new Map()
  );

  const totalSigners = 1 + otherSigners.length;
  const activeCommitment = signatureScheme === 'ecdsa' ? ecdsaCommitment : falconCommitment;

  useEffect(() => {
    if (open) {
      setSignatureScheme(defaultScheme);
    }
  }, [open, defaultScheme]);

  const handleAddOtherSigner = () => {
    let normalizedCommitment: string;
    try {
      normalizedCommitment = normalizeCommitment(otherCommitmentInput);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : 'Invalid commitment');
      return;
    }

    if (normalizedCommitment === activeCommitment.toLowerCase()) {
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

  const setOverride = (proc: ProcedureName, value: number | null) => {
    setProcedureOverrides((prev) => {
      const next = new Map(prev);
      if (value === null) {
        next.delete(proc);
      } else {
        next.set(proc, value);
      }
      return next;
    });
  };

  const buildProcedureThresholds = (): ProcedureThreshold[] | undefined => {
    const result: ProcedureThreshold[] = [];
    for (const [procedure, thresholdValue] of procedureOverrides) {
      if (thresholdValue !== null) {
        result.push({ procedure, threshold: thresholdValue });
      }
    }
    return result.length > 0 ? result : undefined;
  };

  const handleCreate = () => {
    if (threshold < 1 || threshold > totalSigners) {
      setError(`Threshold must be between 1 and ${totalSigners}`);
      return;
    }
    setError(null);
    onCreate(
      otherSigners.map((s) => s.commitment),
      threshold,
      buildProcedureThresholds(),
      signatureScheme
    );
  };

  const handleClose = () => {
    if (!creating) {
      setOtherSigners([]);
      setOtherCommitmentInput('');
      setThreshold(1);
      setError(null);
      setShowAdvanced(false);
      setProcedureOverrides(new Map());
      setSignatureScheme(defaultScheme);
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
          {/* Signature Scheme */}
          <div className="space-y-2">
            <Label>Signature Scheme</Label>
            <Select value={signatureScheme} onValueChange={(val) => setSignatureScheme(val as SignatureScheme)}>
              <SelectTrigger className="w-full" size="sm">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="falcon">Falcon</SelectItem>
                <SelectItem value="ecdsa">ECDSA</SelectItem>
              </SelectContent>
            </Select>
          </div>

          {/* Signers list */}
          <div className="space-y-2">
            <Label>Signers ({totalSigners} total)</Label>
            <div className="bg-muted/50 rounded-lg p-3 space-y-2 text-sm">
              <div className="flex items-center justify-between">
                <span>
                  <strong>You:</strong>{' '}
                  <code className="text-xs">{truncateHex(activeCommitment, 12, 6)}</code>
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

          {/* Advanced Settings Toggle */}
          <Button
            variant="ghost"
            size="sm"
            className="w-full justify-start text-muted-foreground"
            onClick={() => setShowAdvanced(!showAdvanced)}
          >
            {showAdvanced ? '▼' : '▶'} Advanced: Threshold Overrides
          </Button>

          {/* Advanced Settings Content */}
          {showAdvanced && (
            <div className="bg-muted/30 rounded-lg p-3 space-y-3">
              <p className="text-xs text-muted-foreground">
                Override thresholds for specific actions (leave as "Default" to use {threshold}).
              </p>
              {USER_PROCEDURES.map((proc) => {
                const currentOverride = procedureOverrides.get(proc.name);
                return (
                  <div key={proc.name} className="flex items-center justify-between gap-2">
                    <div className="flex-1">
                      <Label className="text-sm">{proc.label}</Label>
                      <p className="text-xs text-muted-foreground">{proc.description}</p>
                    </div>
                    <div className="flex items-center gap-2">
                      <Select
                        value={currentOverride?.toString() ?? 'default'}
                        onValueChange={(val) => {
                          if (val === 'default') {
                            setOverride(proc.name, null);
                          } else {
                            setOverride(proc.name, parseInt(val, 10));
                          }
                        }}
                      >
                        <SelectTrigger className="w-20" size="sm">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          <SelectItem value="default">Default</SelectItem>
                          {Array.from({ length: totalSigners }, (_, i) => i + 1).map((n) => (
                            <SelectItem key={n} value={n.toString()}>
                              {n}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                      <span className="text-xs text-muted-foreground">of {totalSigners}</span>
                    </div>
                  </div>
                );
              })}
            </div>
          )}

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
