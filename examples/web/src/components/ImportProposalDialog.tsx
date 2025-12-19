import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { Label } from '@/components/ui/label';

interface ImportProposalDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  importJson: string;
  onImportJsonChange: (value: string) => void;
  onImport: () => void;
}

export function ImportProposalDialog({
  open,
  onOpenChange,
  importJson,
  onImportJsonChange,
  onImport,
}: ImportProposalDialogProps) {
  const handleClose = () => {
    onOpenChange(false);
  };

  const handleImport = () => {
    if (importJson.trim()) {
      onImport();
    }
  };

  return (
    <Dialog open={open} onOpenChange={handleClose}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>Import Proposal</DialogTitle>
          <DialogDescription>
            Paste proposal JSON received from another signer via side channel.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="proposal-json">Proposal JSON</Label>
            <textarea
              id="proposal-json"
              className="flex min-h-[200px] w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 font-mono"
              placeholder='{"accountId": "0x...", "nonce": 123, ...}'
              value={importJson}
              onChange={(e) => onImportJsonChange(e.target.value)}
            />
          </div>

          <div className="flex gap-2 justify-end">
            <Button variant="outline" onClick={handleClose}>
              Cancel
            </Button>
            <Button onClick={handleImport} disabled={!importJson.trim()}>
              Import
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
