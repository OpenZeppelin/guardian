import { toast } from 'sonner';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Badge } from '@/components/ui/badge';
import { Checkbox } from '@/components/ui/checkbox';
import { Label } from '@/components/ui/label';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { ProposalCard } from './ProposalCard';
import { copyToClipboard } from '@/lib/helpers';
import type { Multisig, Proposal, AccountState } from '@openzeppelin/miden-multisig-client';
import type { SignerInfo } from '@/types';

interface MultisigDashboardProps {
  multisig: Multisig;
  signer: SignerInfo;
  psmState: AccountState | null;
  proposals: Proposal[];
  newSignerCommitment: string;
  increaseThreshold: boolean;
  creatingProposal: boolean;
  syncingState: boolean;
  signingProposal: string | null;
  executingProposal: string | null;
  error: string | null;
  onNewSignerCommitmentChange: (value: string) => void;
  onIncreaseThresholdChange: (checked: boolean) => void;
  onCreateProposal: () => void;
  onSyncState: () => void;
  onSyncProposals: () => void;
  onSignProposal: (proposalId: string) => void;
  onExecuteProposal: (proposalId: string) => void;
  onDisconnect: () => void;
}

export function MultisigDashboard({
  multisig,
  signer,
  psmState,
  proposals,
  newSignerCommitment,
  increaseThreshold,
  creatingProposal,
  syncingState,
  signingProposal,
  executingProposal,
  error,
  onNewSignerCommitmentChange,
  onIncreaseThresholdChange,
  onCreateProposal,
  onSyncState,
  onSyncProposals,
  onSignProposal,
  onExecuteProposal,
  onDisconnect,
}: MultisigDashboardProps) {
  return (
    <div className="max-w-2xl mx-auto p-6 space-y-6">
      {/* Account Info Card */}
      <Card>
        <CardHeader className="pb-3">
          <div className="flex items-center justify-between">
            <CardTitle className="text-lg">Multisig Account</CardTitle>
            <Button variant="ghost" size="sm" onClick={onDisconnect}>
              Disconnect
            </Button>
          </div>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="grid grid-cols-2 gap-4 text-sm">
            <div>
              <span className="text-muted-foreground">Account ID</span>
              <code
                onClick={() => copyToClipboard(multisig.accountId, () => toast.success('Account ID copied'))}
                className="block mt-1 text-xs bg-muted px-2 py-1 rounded cursor-pointer hover:bg-muted/80 truncate"
                title="Click to copy"
              >
                {multisig.accountId}
              </code>
            </div>
            <div>
              <span className="text-muted-foreground">Configuration</span>
              <div className="mt-1">
                <Badge variant="outline">
                  {multisig.threshold}-of-{multisig.signerCommitments.length}
                </Badge>
              </div>
            </div>
          </div>

          {psmState && (
            <div className="pt-2 border-t text-xs text-muted-foreground">
              Last synced: {new Date(psmState.updatedAt).toLocaleString()}
            </div>
          )}

          <div className="flex gap-2 pt-2">
            <Button variant="outline" size="sm" onClick={onSyncState} disabled={syncingState}>
              {syncingState ? 'Syncing...' : 'Sync State'}
            </Button>
            <Button variant="outline" size="sm" onClick={onSyncProposals} disabled={syncingState}>
              Sync Proposals
            </Button>
          </div>
        </CardContent>
      </Card>

      {error && (
        <Alert variant="destructive">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}

      {/* Create Proposal Card */}
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-lg">Add Signer Proposal</CardTitle>
          <CardDescription>
            Create a proposal to add a new signer to the multisig.
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
                Increase threshold to {multisig.threshold + 1}
              </Label>
            </div>

            <Button
              onClick={onCreateProposal}
              disabled={creatingProposal || !newSignerCommitment.trim()}
            >
              {creatingProposal ? 'Creating...' : 'Create Proposal'}
            </Button>
          </div>
        </CardContent>
      </Card>

      {/* Proposals List */}
      {proposals.length > 0 && (
        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-lg">Proposals ({proposals.length})</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            {proposals.map((proposal) => (
              <ProposalCard
                key={proposal.id}
                proposal={proposal}
                signer={signer}
                threshold={multisig.threshold}
                signingProposal={signingProposal}
                executingProposal={executingProposal}
                onSign={onSignProposal}
                onExecute={onExecuteProposal}
              />
            ))}
          </CardContent>
        </Card>
      )}
    </div>
  );
}
