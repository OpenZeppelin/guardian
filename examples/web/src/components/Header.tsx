import { useState } from 'react';
import { toast } from 'sonner';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover';
import { copyToClipboard, truncateHex } from '@/lib/helpers';

interface HeaderProps {
  signerCommitment: string | null;
  generatingSigner: boolean;
  psmStatus: 'connected' | 'connecting' | 'error';
  psmUrl: string;
  onPsmUrlChange: (url: string) => void;
  onReconnect: () => void;
}

export function Header({
  signerCommitment,
  generatingSigner,
  psmStatus,
  psmUrl,
  onPsmUrlChange,
  onReconnect,
}: HeaderProps) {
  const [urlInput, setUrlInput] = useState(psmUrl);
  const [popoverOpen, setPopoverOpen] = useState(false);

  const handleSave = () => {
    onPsmUrlChange(urlInput);
    onReconnect();
    setPopoverOpen(false);
  };

  return (
    <header className="flex items-center justify-between py-4 px-6 border-b">
      <h1 className="text-xl font-bold">Miden Multisig</h1>

      <div className="flex items-center gap-4">
        {/* Signer commitment */}
        {generatingSigner ? (
          <span className="text-sm text-muted-foreground">Generating signer...</span>
        ) : signerCommitment ? (
          <div className="flex items-center gap-2">
            <span className="text-sm text-muted-foreground">Signer:</span>
            <code
              onClick={() => copyToClipboard(signerCommitment, () => toast.success('Commitment copied'))}
              className="text-xs bg-muted px-2 py-1 rounded cursor-pointer hover:bg-muted/80 font-mono"
              title="Click to copy full commitment"
            >
              {truncateHex(signerCommitment, 8, 4)}
            </code>
          </div>
        ) : null}

        {/* PSM Status Badge with Popover */}
        <Popover open={popoverOpen} onOpenChange={setPopoverOpen}>
          <PopoverTrigger asChild>
            <Button variant="ghost" className="p-0 h-auto">
              <Badge
                variant={psmStatus === 'connected' ? 'default' : 'destructive'}
                className={`cursor-pointer ${
                  psmStatus === 'connected'
                    ? 'bg-green-600 hover:bg-green-700'
                    : psmStatus === 'connecting'
                      ? 'bg-yellow-500 hover:bg-yellow-600'
                      : ''
                }`}
              >
                PSM {psmStatus === 'connected' ? '●' : psmStatus === 'connecting' ? '◐' : '○'}
              </Badge>
            </Button>
          </PopoverTrigger>
          <PopoverContent className="w-80" align="end">
            <div className="space-y-4">
              <div className="space-y-2">
                <h4 className="font-medium">PSM Configuration</h4>
                <p className="text-sm text-muted-foreground">
                  {psmStatus === 'connected'
                    ? 'Connected to PSM server'
                    : psmStatus === 'connecting'
                      ? 'Connecting...'
                      : 'Failed to connect to PSM server'}
                </p>
              </div>
              <div className="space-y-2">
                <Label htmlFor="psm-url">Endpoint URL</Label>
                <Input
                  id="psm-url"
                  value={urlInput}
                  onChange={(e) => setUrlInput(e.target.value)}
                  placeholder="http://localhost:3000"
                />
              </div>
              <div className="flex gap-2">
                <Button onClick={handleSave} size="sm">
                  Save & Reconnect
                </Button>
              </div>
            </div>
          </PopoverContent>
        </Popover>
      </div>
    </header>
  );
}
