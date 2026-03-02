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
import type { Multisig, Proposal, AccountState, ConsumableNote, VaultBalance, ProcedureName } from '@openzeppelin/miden-multisig-client';
import type { SignerInfo } from '@/types';

interface MultisigDashboardProps {
  multisig: Multisig;
  signer: SignerInfo;
  psmState: AccountState | null;
  proposals: Proposal[];
  consumableNotes: ConsumableNote[];
  vaultBalances: VaultBalance[];
  procedureThresholds?: Map<ProcedureName, number>;
  creatingProposal: boolean;
  syncing: boolean;
  verifying: boolean;
  signingProposal: string | null;
  executingProposal: string | null;
  error: string | null;
  verificationStatus: string | null;
  pendingCandidateWarning: string | null;
  onDismissWarning: () => void;
  onCreateAddSigner: (commitment: string, increaseThreshold: boolean) => void;
  onCreateRemoveSigner: (signerToRemove: string, newThreshold?: number) => void;
  onCreateChangeThreshold: (newThreshold: number) => void;
  onCreateConsumeNotes: (noteIds: string[]) => void;
  onCreateP2id: (recipientId: string, faucetId: string, amount: bigint) => void;
  onCreateSwitchPsm: (newEndpoint: string, newPubkey: string) => void;
  onSync: () => void;
  onVerify: () => void;
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
  creatingProposal,
  syncing,
  verifying,
  signingProposal,
  executingProposal,
  error,
  verificationStatus,
  pendingCandidateWarning,
  onDismissWarning,
  onCreateAddSigner,
  onCreateRemoveSigner,
  onCreateChangeThreshold,
  onCreateConsumeNotes,
  onCreateP2id,
  onCreateSwitchPsm,
  onSync,
  onVerify,
  onSignProposal,
  onExecuteProposal,
  onExportProposal,
  onSignProposalOffline,
  onImportProposal,
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

          {/* Procedure Threshold Overrides */}
          {procedureThresholds && procedureThresholds.size > 0 && (
            <div className="pt-2 border-t">
              <span className="text-sm text-muted-foreground">Threshold Overrides</span>
              <div className="mt-1 flex flex-wrap gap-2">
                {USER_PROCEDURES.filter((proc) => procedureThresholds.has(proc.name)).map((proc) => (
                  <Badge key={proc.name} variant="secondary" className="text-xs">
                    {proc.label}: {procedureThresholds.get(proc.name)}-of-{multisig.signerCommitments.length}
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
            <Button variant="outline" size="sm" onClick={onVerify} disabled={verifying}>
              {verifying ? 'Verifying...' : 'Verify State'}
            </Button>
            <Button variant="outline" size="sm" onClick={onImportProposal}>
              Import Proposal
            </Button>
          </div>
          {verificationStatus && (
            <div className="text-xs text-muted-foreground">{verificationStatus}</div>
          )}
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
        currentThreshold={multisig.threshold}
        signerCommitments={multisig.signerCommitments}
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
                defaultThreshold={multisig.threshold}
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
