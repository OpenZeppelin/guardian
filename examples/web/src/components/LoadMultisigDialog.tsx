import { useState, useEffect } from 'react';
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
import type { DetectedMultisigConfig, SignatureScheme } from '@openzeppelin/miden-multisig-client';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';

interface LoadMultisigDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  loading: boolean;
  detectedConfig: DetectedMultisigConfig | null;
  error: string | null;
  defaultScheme: SignatureScheme;
  onLoad: (accountId: string, scheme: SignatureScheme) => void;
}

export function LoadMultisigDialog({
  open,
  onOpenChange,
  loading,
  detectedConfig,
  error,
  defaultScheme,
  onLoad,
}: LoadMultisigDialogProps) {
  const [accountIdInput, setAccountIdInput] = useState('');
  const [signatureScheme, setSignatureScheme] = useState<SignatureScheme>(defaultScheme);

  useEffect(() => {
    if (open) {
      setSignatureScheme(defaultScheme);
    }
  }, [open, defaultScheme]);

  const handleLoad = () => {
    if (accountIdInput.trim()) {
      onLoad(accountIdInput.trim(), signatureScheme);
    }
  };

  const handleClose = () => {
    if (!loading) {
      setAccountIdInput('');
      setSignatureScheme(defaultScheme);
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

          <div className="space-y-2">
            <Label>Signature Scheme</Label>
            <Select value={signatureScheme} onValueChange={(val) => setSignatureScheme(val as SignatureScheme)}>
              <SelectTrigger className="w-full" size="sm">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="falcon">Falcon</SelectItem>
                <SelectItem value="ecdsa">ECDSA</SelectItem>
              </SelectContent>
            </Select>
          </div>

          {error && (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          )}

          {detectedConfig && (
            <Alert>
              <AlertDescription>
                <div className="space-y-1 text-sm">
                  <div>
                    <strong>Detected:</strong> {detectedConfig.threshold}-of-
                    {detectedConfig.numSigners} multisig
                  </div>
                  <div>
                    <strong>Scheme:</strong> {detectedConfig.signatureScheme}
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
