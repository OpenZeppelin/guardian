import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import type { DetectedMultisigConfig } from '@openzeppelin/miden-multisig-client';

interface LoadAccountSectionProps {
  loadAccountIdInput: string;
  loadingAccount: boolean;
  detectedConfig: DetectedMultisigConfig | null;
  multisigClientReady: boolean;
  signerReady: boolean;
  onLoadAccountIdChange: (value: string) => void;
  onLoadFromPsm: () => void;
}

export function LoadAccountSection({
  loadAccountIdInput,
  loadingAccount,
  detectedConfig,
  multisigClientReady,
  signerReady,
  onLoadAccountIdChange,
  onLoadFromPsm,
}: LoadAccountSectionProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Or Load Existing Account</CardTitle>
        <CardDescription>
          Load an existing multisig account from PSM. The configuration will be automatically
          detected.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex gap-2">
          <Input
            placeholder="Account ID (0x...)"
            value={loadAccountIdInput}
            onChange={(e) => onLoadAccountIdChange(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && onLoadFromPsm()}
            className="flex-1"
          />
          <Button
            onClick={onLoadFromPsm}
            disabled={!multisigClientReady || !signerReady || loadingAccount || !loadAccountIdInput.trim()}
          >
            {loadingAccount ? 'Loading...' : 'Load from PSM'}
          </Button>
        </div>

        {detectedConfig && (
          <div className="bg-blue-50 dark:bg-blue-950 rounded-lg p-4 space-y-2">
            <h4 className="font-medium text-blue-900 dark:text-blue-100">Detected Configuration:</h4>
            <div className="space-y-1 text-sm">
              <div>
                <span className="text-muted-foreground">Type:</span>{' '}
                <code className="bg-blue-100 dark:bg-blue-900 px-1 rounded">
                  {detectedConfig.threshold}-of-{detectedConfig.numSigners} multisig
                </code>
              </div>
              <div>
                <span className="text-muted-foreground">PSM Enabled:</span>{' '}
                <code className="bg-blue-100 dark:bg-blue-900 px-1 rounded">
                  {detectedConfig.psmEnabled ? 'Yes' : 'No'}
                </code>
              </div>
              <div>
                <span className="text-muted-foreground">Signers:</span>
                <ul className="mt-1 ml-4 space-y-1">
                  {detectedConfig.signerCommitments.map((c, i) => (
                    <li key={i}>
                      <code className="text-xs bg-blue-100 dark:bg-blue-900 px-1 rounded">
                        {c.slice(0, 16)}...{c.slice(-8)}
                      </code>
                    </li>
                  ))}
                </ul>
              </div>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
