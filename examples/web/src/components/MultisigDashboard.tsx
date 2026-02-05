import { toast } from 'sonner';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { ProposalCard } from './ProposalCard';
import { CreateProposalForm } from './CreateProposalForm';
import { CandidateWarningBanner } from './CandidateWarningBanner';
import { copyToClipboard } from '@/lib/helpers';
import { USER_PROCEDURES } from '@/lib/procedures';
import type { Multisig, TransactionProposal, AccountState, ConsumableNote, VaultBalance, ProcedureName } from '@openzeppelin/miden-multisig-client';
import type { SignerInfo } from '@/types';

interface MultisigDashboardProps {
  multisig: Multisig;
  signer: SignerInfo;
  psmState: AccountState | null;
  proposals: TransactionProposal[];
  consumableNotes: ConsumableNote[];
  vaultBalances: VaultBalance[];
  procedureThresholds?: Map<ProcedureName, number>;
  detectedThreshold?: number;
  detectedSignerCommitments?: string[];
  creatingProposal: boolean;
  syncing: boolean;
  signingProposal: string | null;
  executingProposal: string | null;
  error: string | null;
  pendingCandidateWarning: string | null;
  onDismissWarning: () => void;
  onCreateAddSigner: (commitment: string, increaseThreshold: boolean) => void;
  onCreateRemoveSigner: (signerToRemove: string, newThreshold?: number) => void;
  onCreateChangeThreshold: (newThreshold: number) => void;
  onCreateConsumeNotes: (noteIds: string[]) => void;
  onCreateP2id: (recipientId: string, faucetId: string, amount: bigint) => void;
  onCreateSwitchPsm: (newEndpoint: string, newPubkey: string) => void;
  onSync: () => void;
  onSignProposal: (proposalId: string) => void;
  onExecuteProposal: (proposalId: string) => void;
  onExportProposal: (proposalId: string) => void;
  onSignProposalOffline: (proposalId: string) => void;
  onImportProposal: () => void;
  onDisconnect: () => void;
}

export function MultisigDashboard({
  multisig,
  signer,
  psmState,
  proposals,
  consumableNotes,
  vaultBalances,
  procedureThresholds,
  detectedThreshold,
  detectedSignerCommitments,
  creatingProposal,
  syncing,
  signingProposal,
  executingProposal,
  error,
  pendingCandidateWarning,
  onDismissWarning,
  onCreateAddSigner,
  onCreateRemoveSigner,
  onCreateChangeThreshold,
  onCreateConsumeNotes,
  onCreateP2id,
  onCreateSwitchPsm,
  onSync,
  onSignProposal,
  onExecuteProposal,
  onExportProposal,
  onSignProposalOffline,
  onImportProposal,
  onDisconnect,
}: MultisigDashboardProps) {
  const threshold = detectedThreshold ?? multisig.threshold;
  const signerCommitments = detectedSignerCommitments ?? multisig.signerCommitments;
  const activeProposals = proposals.filter((p) => p.status.type !== 'finalized');

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
                  {threshold}-of-{signerCommitments.length}
                </Badge>
              </div>
            </div>
          </div>

          {/* Procedure Threshold Overrides */}
          {procedureThresholds && procedureThresholds.size > 0 && (
            <div className="pt-2 border-t">
              <span className="text-sm text-muted-foreground">Threshold Overrides</span>
              <div className="mt-1 flex flex-wrap gap-2">
                {USER_PROCEDURES.filter((proc) => procedureThresholds.has(proc.name)).map((proc) => (
                  <Badge key={proc.name} variant="secondary" className="text-xs">
                    {proc.label}: {procedureThresholds.get(proc.name)}-of-{signerCommitments.length}
                  </Badge>
                ))}
              </div>
            </div>
          )}

          {psmState && (
            <div className="pt-2 border-t text-xs text-muted-foreground">
              Last synced: {new Date(psmState.updatedAt).toLocaleString()}
            </div>
          )}

          <div className="flex gap-2 pt-2">
            <Button variant="outline" size="sm" onClick={onSync} disabled={syncing}>
              {syncing ? 'Syncing...' : 'Sync'}
            </Button>
            <Button variant="outline" size="sm" onClick={onImportProposal}>
              Import Proposal
            </Button>
          </div>
        </CardContent>
      </Card>

      {/* Vault Balances */}
      {vaultBalances.length > 0 && (
        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-lg">Vault Balances</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="space-y-2">
              {vaultBalances.map((balance) => (
                <div
                  key={balance.faucetId}
                  className="flex items-center justify-between text-sm border-b pb-2 last:border-0 last:pb-0"
                >
                  <code
                    onClick={() => copyToClipboard(balance.faucetId, () => toast.success('Faucet ID copied'))}
                    className="text-xs bg-muted px-2 py-1 rounded cursor-pointer hover:bg-muted/80 truncate max-w-[200px]"
                    title={balance.faucetId}
                  >
                    {balance.faucetId.slice(0, 10)}...{balance.faucetId.slice(-6)}
                  </code>
                  <Badge variant="secondary" className="font-mono">
                    {balance.amount.toString()}
                  </Badge>
                </div>
              ))}
            </div>
          </CardContent>
        </Card>
      )}

      {pendingCandidateWarning && (
        <CandidateWarningBanner
          message={pendingCandidateWarning}
          onDismiss={onDismissWarning}
        />
      )}

      {error && (
        <Alert variant="destructive">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}

      {/* Create Proposal Form */}
      <CreateProposalForm
        currentThreshold={threshold}
        signerCommitments={signerCommitments}
        creatingProposal={creatingProposal}
        consumableNotes={consumableNotes}
        vaultBalances={vaultBalances}
        onCreateAddSigner={onCreateAddSigner}
        onCreateRemoveSigner={onCreateRemoveSigner}
        onCreateChangeThreshold={onCreateChangeThreshold}
        onCreateConsumeNotes={onCreateConsumeNotes}
        onCreateP2id={onCreateP2id}
        onCreateSwitchPsm={onCreateSwitchPsm}
      />

      {/* Proposals List */}
      {activeProposals.length > 0 && (
        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-lg">Proposals ({activeProposals.length})</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            {activeProposals.map((proposal) => (
              <ProposalCard
                key={proposal.id}
                proposal={proposal}
                signer={signer}
                defaultThreshold={threshold}
                procedureThresholds={procedureThresholds}
                signingProposal={signingProposal}
                executingProposal={executingProposal}
                onSign={onSignProposal}
                onExecute={onExecuteProposal}
                onExport={onExportProposal}
                onSignOffline={onSignProposalOffline}
              />
            ))}
          </CardContent>
        </Card>
      )}
    </div>
  );
}
