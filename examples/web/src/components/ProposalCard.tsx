import { toast } from 'sonner';
import { Card, CardContent } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import { Separator } from '@/components/ui/separator';
import { copyToClipboard } from '@/lib/helpers';
import type { Proposal, ProposalType } from '@openzeppelin/miden-multisig-client';
import type { SignerInfo } from '@/types';

interface ProposalCardProps {
  proposal: Proposal;
  signer: SignerInfo | null;
  threshold: number;
  signingProposal: string | null;
  executingProposal: string | null;
  onSign: (proposalId: string) => void;
  onExecute: (proposalId: string) => void;
  onExport: (proposalId: string) => void;
  onSignOffline: (proposalId: string) => void;
}

function getProposalTypeLabel(type?: ProposalType): string {
  switch (type) {
    case 'add_signer':
      return 'Add Signer';
    case 'remove_signer':
      return 'Remove Signer';
    case 'change_threshold':
      return 'Change Threshold';
    case 'switch_psm':
      return 'Switch PSM';
    case 'custom':
      return 'Custom';
    default:
      return 'Unknown';
  }
}

function getProposalTypeVariant(type?: ProposalType): 'default' | 'secondary' | 'destructive' | 'outline' {
  switch (type) {
    case 'add_signer':
      return 'default';
    case 'remove_signer':
      return 'destructive';
    case 'change_threshold':
      return 'secondary';
    case 'switch_psm':
      return 'secondary';
    default:
      return 'outline';
  }
}

export function ProposalCard({
  proposal,
  signer,
  threshold,
  signingProposal,
  executingProposal,
  onSign,
  onExecute,
  onExport,
  onSignOffline,
}: ProposalCardProps) {
  const userSigned = signer
    ? proposal.signatures.some(
        (sig) => sig.signerId.toLowerCase() === signer.commitment.toLowerCase()
      )
    : false;

  const canSign = proposal.status.type === 'pending' && !userSigned;
  const canExecute =
    proposal.status.type === 'ready' ||
    (proposal.status.type === 'pending' && proposal.signatures.length >= threshold);
  const isSigningThis = signingProposal === proposal.id;
  const isExecutingThis = executingProposal === proposal.id;

  const statusVariant =
    proposal.status.type === 'ready'
      ? 'default'
      : proposal.status.type === 'finalized'
        ? 'secondary'
        : 'outline';

  const proposalType = proposal.metadata?.proposalType;
  const description = proposal.metadata?.description;

  return (
    <Card>
      <CardContent className="pt-4 space-y-3">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Badge variant={getProposalTypeVariant(proposalType)}>
              {getProposalTypeLabel(proposalType)}
            </Badge>
            <code
              onClick={() => copyToClipboard(proposal.id, () => toast.success('Proposal ID copied'))}
              className="text-xs bg-muted px-2 py-1 rounded cursor-pointer hover:bg-muted/80"
              title="Click to copy full ID"
            >
              {proposal.id.slice(0, 12)}...
            </code>
          </div>
          <Badge variant={statusVariant} className="uppercase">
            {proposal.status.type}
          </Badge>
        </div>

        {description && (
          <p className="text-sm text-muted-foreground">{description}</p>
        )}

        <div className="flex gap-4 text-sm">
          <div>
            <span className="text-muted-foreground">Nonce:</span> {proposal.nonce}
          </div>
          <div>
            <span className="text-muted-foreground">Signatures:</span>{' '}
            {proposal.signatures.length} / {threshold}
            {userSigned && <span className="text-green-600 ml-1 font-medium">You signed</span>}
          </div>
        </div>

        {proposal.signatures.length > 0 && (
          <div className="text-sm">
            <span className="text-muted-foreground">Signers:</span>
            <div className="flex flex-wrap gap-1 mt-1">
              {proposal.signatures.map((sig) => {
                const isYou =
                  signer && sig.signerId.toLowerCase() === signer.commitment.toLowerCase();
                return (
                  <Badge
                    key={sig.signerId}
                    variant={isYou ? 'default' : 'outline'}
                    className="font-mono text-xs"
                    title={sig.signerId}
                  >
                    {sig.signerId.slice(0, 8)}...
                  </Badge>
                );
              })}
            </div>
          </div>
        )}

        <Separator />

        <div className="flex gap-2 flex-wrap">
          {canSign && (
            <>
              <Button
                onClick={() => onSign(proposal.id)}
                disabled={isSigningThis || !!signingProposal}
              >
                {isSigningThis ? 'Signing...' : 'Sign'}
              </Button>
              <Button
                variant="outline"
                onClick={() => onSignOffline(proposal.id)}
                title="Sign offline and copy to clipboard"
              >
                Sign & Copy
              </Button>
            </>
          )}
          {canExecute && (
            <Button
              variant="default"
              className="bg-green-600 hover:bg-green-700"
              onClick={() => onExecute(proposal.id)}
              disabled={isExecutingThis || !!executingProposal}
            >
              {isExecutingThis ? 'Executing...' : 'Execute'}
            </Button>
          )}
          <Button
            variant="ghost"
            size="sm"
            onClick={() => onExport(proposal.id)}
            title="Export proposal JSON to clipboard"
          >
            Export
          </Button>
          {!canSign && !canExecute && proposal.status.type === 'finalized' && (
            <span className="text-sm text-muted-foreground italic">Finalized</span>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
