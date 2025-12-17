import { useState } from 'react';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { truncateHex } from '@/lib/helpers';
import type { DetectedMultisigConfig } from '@openzeppelin/miden-multisig-client';

interface LoadMultisigDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  loading: boolean;
  detectedConfig: DetectedMultisigConfig | null;
  onLoad: (accountId: string) => void;
}

export function LoadMultisigDialog({
  open,
  onOpenChange,
  loading,
  detectedConfig,
  onLoad,
}: LoadMultisigDialogProps) {
  const [accountIdInput, setAccountIdInput] = useState('');

  const handleLoad = () => {
    if (accountIdInput.trim()) {
      onLoad(accountIdInput.trim());
    }
  };

  const handleClose = () => {
    if (!loading) {
      setAccountIdInput('');
      onOpenChange(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={handleClose}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Load Multisig</DialogTitle>
          <DialogDescription>
            Load an existing multisig account from PSM. Configuration will be auto-detected.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="account-id">Account ID</Label>
            <Input
              id="account-id"
              placeholder="0x..."
              value={accountIdInput}
              onChange={(e) => setAccountIdInput(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && handleLoad()}
            />
          </div>

          {detectedConfig && (
            <Alert>
              <AlertDescription>
                <div className="space-y-1 text-sm">
                  <div>
                    <strong>Detected:</strong> {detectedConfig.threshold}-of-
                    {detectedConfig.numSigners} multisig
                  </div>
                  <div>
                    <strong>PSM:</strong> {detectedConfig.psmEnabled ? 'Enabled' : 'Disabled'}
                  </div>
                  <div className="text-xs text-muted-foreground">
                    Signers:{' '}
                    {detectedConfig.signerCommitments.map((c, i) => (
                      <span key={i}>
                        {i > 0 && ', '}
                        {truncateHex(c, 6, 4)}
                      </span>
                    ))}
                  </div>
                </div>
              </AlertDescription>
            </Alert>
          )}

          <Button
            onClick={handleLoad}
            className="w-full"
            disabled={loading || !accountIdInput.trim()}
          >
            {loading ? 'Loading...' : 'Load from PSM'}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
