import { toast } from 'sonner';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { ProposalCard } from './ProposalCard';
import { CreateProposalForm } from './CreateProposalForm';
import { copyToClipboard } from '@/lib/helpers';
import type { Multisig, Proposal, AccountState } from '@openzeppelin/miden-multisig-client';
import type { SignerInfo } from '@/types';

interface MultisigDashboardProps {
  multisig: Multisig;
  signer: SignerInfo;
  psmState: AccountState | null;
  proposals: Proposal[];
  creatingProposal: boolean;
  syncing: boolean;
  signingProposal: string | null;
  executingProposal: string | null;
  error: string | null;
  onCreateAddSigner: (commitment: string, increaseThreshold: boolean) => void;
  onCreateRemoveSigner: (signerToRemove: string, newThreshold?: number) => void;
  onCreateChangeThreshold: (newThreshold: number) => void;
  onSync: () => void;
  onSignProposal: (proposalId: string) => void;
  onExecuteProposal: (proposalId: string) => void;
  onDisconnect: () => void;
}

export function MultisigDashboard({
  multisig,
  signer,
  psmState,
  proposals,
  creatingProposal,
  syncing,
  signingProposal,
  executingProposal,
  error,
  onCreateAddSigner,
  onCreateRemoveSigner,
  onCreateChangeThreshold,
  onSync,
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
            <Button variant="outline" size="sm" onClick={onSync} disabled={syncing}>
              {syncing ? 'Syncing...' : 'Sync'}
            </Button>
          </div>
        </CardContent>
      </Card>

      {error && (
        <Alert variant="destructive">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}

      {/* Create Proposal Form */}
      <CreateProposalForm
        currentThreshold={multisig.threshold}
        signerCommitments={multisig.signerCommitments}
        creatingProposal={creatingProposal}
        onCreateAddSigner={onCreateAddSigner}
        onCreateRemoveSigner={onCreateRemoveSigner}
        onCreateChangeThreshold={onCreateChangeThreshold}
      />

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
