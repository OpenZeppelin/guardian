import { toast } from 'sonner';
import { Card, CardContent } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import { Separator } from '@/components/ui/separator';
import { copyToClipboard } from '@/lib/helpers';
import type { Proposal } from '@openzeppelin/miden-multisig-client';
import type { SignerInfo } from '@/types';

interface ProposalCardProps {
  proposal: Proposal;
  signer: SignerInfo | null;
  threshold: number;
  signingProposal: string | null;
  executingProposal: string | null;
  onSign: (proposalId: string) => void;
  onExecute: (proposalId: string) => void;
}

export function ProposalCard({
  proposal,
  signer,
  threshold,
  signingProposal,
  executingProposal,
  onSign,
  onExecute,
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

  return (
    <Card>
      <CardContent className="pt-4 space-y-3">
        <div className="flex items-center justify-between">
          <code
            onClick={() => copyToClipboard(proposal.id, () => toast.success('Proposal ID copied'))}
            className="text-xs bg-muted px-2 py-1 rounded cursor-pointer hover:bg-muted/80"
            title="Click to copy full ID"
          >
            {proposal.id.slice(0, 20)}...
          </code>
          <Badge variant={statusVariant} className="uppercase">
            {proposal.status.type}
          </Badge>
        </div>

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

        <div className="flex gap-2">
          {canSign && (
            <Button
              onClick={() => onSign(proposal.id)}
              disabled={isSigningThis || !!signingProposal}
            >
              {isSigningThis ? 'Signing...' : 'Sign'}
            </Button>
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
          {!canSign && !canExecute && proposal.status.type === 'finalized' && (
            <span className="text-sm text-muted-foreground italic">Finalized</span>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
