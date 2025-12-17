import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Checkbox } from '@/components/ui/checkbox';
import { Label } from '@/components/ui/label';
import { ProposalCard } from './ProposalCard';
import type { Multisig, Proposal } from '@openzeppelin/miden-multisig-client';
import type { SignerInfo } from '@/types';

interface ProposalSectionProps {
  multisig: Multisig;
  signer: SignerInfo | null;
  proposals: Proposal[];
  newSignerCommitment: string;
  increaseThreshold: boolean;
  creatingProposal: boolean;
  syncingState: boolean;
  signingProposal: string | null;
  executingProposal: string | null;
  webClientReady: boolean;
  onNewSignerCommitmentChange: (value: string) => void;
  onIncreaseThresholdChange: (checked: boolean) => void;
  onCreateProposal: () => void;
  onSyncProposals: () => void;
  onSignProposal: (proposalId: string) => void;
  onExecuteProposal: (proposalId: string) => void;
}

export function ProposalSection({
  multisig,
  signer,
  proposals,
  newSignerCommitment,
  increaseThreshold,
  creatingProposal,
  syncingState,
  signingProposal,
  executingProposal,
  webClientReady,
  onNewSignerCommitmentChange,
  onIncreaseThresholdChange,
  onCreateProposal,
  onSyncProposals,
  onSignProposal,
  onExecuteProposal,
}: ProposalSectionProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Add Signer Proposal</CardTitle>
        <CardDescription>
          Build a proposal that updates signer set and threshold by executing the multisig
          update_signers script.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="space-y-3">
          <Input
            placeholder="New signer commitment (0x...)"
            value={newSignerCommitment}
            onChange={(e) => onNewSignerCommitmentChange(e.target.value)}
          />

          <div className="flex items-center space-x-2">
            <Checkbox
              id="increase-threshold"
              checked={increaseThreshold}
              onCheckedChange={(checked) => onIncreaseThresholdChange(checked === true)}
            />
            <Label htmlFor="increase-threshold" className="text-sm">
              Increase threshold from {multisig.threshold} to {multisig.threshold + 1}
            </Label>
          </div>

          <div className="flex gap-2">
            <Button onClick={onCreateProposal} disabled={creatingProposal || !webClientReady}>
              {creatingProposal ? 'Creating...' : 'Create Proposal'}
            </Button>
            <Button variant="outline" onClick={onSyncProposals} disabled={syncingState}>
              {syncingState ? 'Syncing...' : 'Sync Proposals'}
            </Button>
          </div>
        </div>

        {proposals.length > 0 && (
          <div className="space-y-3">
            <h3 className="font-medium">Proposals ({proposals.length})</h3>
            <div className="space-y-3">
              {proposals.map((p) => (
                <ProposalCard
                  key={p.id}
                  proposal={p}
                  signer={signer}
                  threshold={multisig.threshold}
                  signingProposal={signingProposal}
                  executingProposal={executingProposal}
                  onSign={onSignProposal}
                  onExecute={onExecuteProposal}
                />
              ))}
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
