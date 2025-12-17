import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import { copyToClipboard } from '@/lib/helpers';
import type { AccountState } from '@openzeppelin/miden-multisig-client';

interface PsmSyncSectionProps {
  registeringOnPsm: boolean;
  configuredOnPsm: boolean;
  syncingState: boolean;
  psmState: AccountState | null;
  onSyncState: () => void;
}

export function PsmSyncSection({
  registeringOnPsm,
  configuredOnPsm,
  syncingState,
  psmState,
  onSyncState,
}: PsmSyncSectionProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>PSM State Sync</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex items-center gap-3">
          {registeringOnPsm ? (
            <Badge variant="secondary">Registering on PSM...</Badge>
          ) : configuredOnPsm ? (
            <Badge variant="default" className="bg-green-600">
              Registered on PSM
            </Badge>
          ) : (
            <Badge variant="destructive">Not registered on PSM</Badge>
          )}

          <Button variant="outline" onClick={onSyncState} disabled={syncingState || !configuredOnPsm}>
            {syncingState ? 'Syncing...' : 'Sync State'}
          </Button>
        </div>

        {psmState && (
          <div className="bg-blue-50 dark:bg-blue-950 rounded-lg p-4 space-y-2">
            <h3 className="font-medium text-blue-900 dark:text-blue-100">PSM State</h3>
            <div className="space-y-2 text-sm">
              <div className="flex items-center gap-2">
                <span className="text-muted-foreground">Account ID:</span>
                <code
                  onClick={() => copyToClipboard(psmState.accountId)}
                  className="text-xs bg-blue-100 dark:bg-blue-900 px-2 py-1 rounded cursor-pointer hover:opacity-80"
                  title="Click to copy"
                >
                  {psmState.accountId}
                </code>
              </div>
              <div className="flex items-center gap-2">
                <span className="text-muted-foreground">Commitment:</span>
                <code
                  onClick={() => copyToClipboard(psmState.commitment)}
                  className="text-xs bg-blue-100 dark:bg-blue-900 px-2 py-1 rounded cursor-pointer hover:opacity-80"
                  title="Click to copy"
                >
                  {psmState.commitment.slice(0, 16)}...{psmState.commitment.slice(-8)}
                </code>
              </div>
              <div className="flex items-center gap-2">
                <span className="text-muted-foreground">Updated:</span>
                <code className="text-xs bg-blue-100 dark:bg-blue-900 px-2 py-1 rounded">
                  {new Date(psmState.updatedAt).toLocaleString()}
                </code>
              </div>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
