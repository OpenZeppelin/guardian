import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Separator } from '@/components/ui/separator';
import type { SignerInfo, OtherSigner } from '@/types';

interface CreateMultisigSectionProps {
  signer: SignerInfo | null;
  otherSigners: OtherSigner[];
  otherCommitmentInput: string;
  threshold: number;
  totalSigners: number;
  creating: boolean;
  registeringOnPsm: boolean;
  multisigClientReady: boolean;
  psmPubkey: string;
  onOtherCommitmentChange: (value: string) => void;
  onAddOtherSigner: () => void;
  onRemoveOtherSigner: (id: string) => void;
  onThresholdChange: (value: number) => void;
  onCreateAccount: () => void;
}

export function CreateMultisigSection({
  signer,
  otherSigners,
  otherCommitmentInput,
  threshold,
  totalSigners,
  creating,
  registeringOnPsm,
  multisigClientReady,
  psmPubkey,
  onOtherCommitmentChange,
  onAddOtherSigner,
  onRemoveOtherSigner,
  onThresholdChange,
  onCreateAccount,
}: CreateMultisigSectionProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Create Multisig Account</CardTitle>
      </CardHeader>
      <CardContent className="space-y-6">
        {/* Step 1: Signers */}
        <div className="space-y-4">
          <h3 className="font-medium">1. Configure Signers</h3>
          <p className="text-sm text-muted-foreground">
            Add commitments from other signers to include them in the multisig.
          </p>

          <div className="bg-muted/50 rounded-lg p-4 space-y-2">
            <h4 className="text-sm font-medium">Signers ({totalSigners} total):</h4>
            <ul className="space-y-1">
              {signer && (
                <li className="text-sm">
                  <strong>You</strong>:{' '}
                  <code className="text-xs bg-muted px-1 rounded">
                    {signer.commitment.slice(0, 16)}...
                  </code>
                </li>
              )}
              {otherSigners.map((s, index) => (
                <li key={s.id} className="flex items-center justify-between text-sm">
                  <span>
                    Signer {index + 2}:{' '}
                    <code className="text-xs bg-muted px-1 rounded">
                      {s.commitment.slice(0, 16)}...
                    </code>
                  </span>
                  <Button
                    variant="destructive"
                    size="sm"
                    onClick={() => onRemoveOtherSigner(s.id)}
                  >
                    Remove
                  </Button>
                </li>
              ))}
            </ul>
          </div>

          <div className="flex gap-2">
            <Input
              placeholder="Paste commitment from another signer (64-char hex)"
              value={otherCommitmentInput}
              onChange={(e) => onOtherCommitmentChange(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && onAddOtherSigner()}
              className="flex-1"
            />
            <Button onClick={onAddOtherSigner} disabled={!otherCommitmentInput.trim()}>
              Add Signer
            </Button>
          </div>
        </div>

        <Separator />

        {/* Step 2: Threshold */}
        <div className="space-y-4">
          <h3 className="font-medium">2. Set Threshold</h3>
          <div className="flex items-center gap-3">
            <Label htmlFor="threshold">Required signatures:</Label>
            <Input
              id="threshold"
              type="number"
              min={1}
              max={Math.max(1, totalSigners)}
              value={threshold}
              onChange={(e) => onThresholdChange(Math.max(1, parseInt(e.target.value) || 1))}
              className="w-20"
            />
            <span className="text-sm text-muted-foreground">
              of {totalSigners} signer(s) required to approve
            </span>
          </div>
        </div>

        <Separator />

        {/* Step 3: Create */}
        <div className="space-y-4">
          <h3 className="font-medium">3. Create Account</h3>
          <Button
            onClick={onCreateAccount}
            size="lg"
            disabled={!multisigClientReady || !psmPubkey || creating || !signer || totalSigners === 0}
          >
            {creating
              ? registeringOnPsm
                ? 'Registering on PSM...'
                : 'Creating Account...'
              : `Create ${threshold}-of-${totalSigners} Multisig`}
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}
