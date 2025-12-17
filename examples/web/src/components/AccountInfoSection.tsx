import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { copyToClipboard } from '@/lib/helpers';
import type { Multisig } from '@openzeppelin/miden-multisig-client';

interface AccountInfoSectionProps {
  multisig: Multisig;
}

export function AccountInfoSection({ multisig }: AccountInfoSectionProps) {
  return (
    <Card className="border-green-200 bg-green-50 dark:bg-green-950 dark:border-green-900">
      <CardHeader>
        <CardTitle className="text-green-900 dark:text-green-100">Multisig Account</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium text-muted-foreground">Account ID:</span>
          <code
            onClick={() => copyToClipboard(multisig.accountId)}
            className="text-xs bg-green-100 dark:bg-green-900 px-2 py-1 rounded cursor-pointer hover:opacity-80 break-all"
            title="Click to copy"
          >
            {multisig.accountId}
          </code>
        </div>
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium text-muted-foreground">Threshold:</span>
          <code className="text-xs bg-green-100 dark:bg-green-900 px-2 py-1 rounded">
            {multisig.threshold}-of-{multisig.signerCommitments.length}
          </code>
        </div>
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium text-muted-foreground">Your Commitment:</span>
          <code
            onClick={() => copyToClipboard(multisig.signerCommitment)}
            className="text-xs bg-green-100 dark:bg-green-900 px-2 py-1 rounded cursor-pointer hover:opacity-80"
            title="Click to copy"
          >
            {multisig.signerCommitment.slice(0, 16)}...
          </code>
        </div>
      </CardContent>
    </Card>
  );
}
