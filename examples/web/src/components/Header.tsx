import { useState, useEffect } from 'react';
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
import type { SignatureScheme } from '@openzeppelin/miden-multisig-client';

interface HeaderProps {
  falconCommitment: string | null;
  ecdsaCommitment: string | null;
  activeScheme: SignatureScheme | null;
  generatingSigner: boolean;
  psmStatus: 'connected' | 'connecting' | 'error';
  psmUrl: string;
  onPsmUrlChange: (url: string) => void;
  onReconnect: (url: string) => void;
}

export function Header({
  falconCommitment,
  ecdsaCommitment,
  activeScheme,
  generatingSigner,
  psmStatus,
  psmUrl,
  onPsmUrlChange,
  onReconnect,
}: HeaderProps) {
  const [urlInput, setUrlInput] = useState(psmUrl);
  const [popoverOpen, setPopoverOpen] = useState(false);

  // Sync urlInput when psmUrl changes externally
  useEffect(() => {
    setUrlInput(psmUrl);
  }, [psmUrl]);

  const handleSave = () => {
    onPsmUrlChange(urlInput);
    onReconnect(urlInput);
    setPopoverOpen(false);
  };

  return (
    <header className="flex items-center justify-between py-4 px-6 border-b">
      <h1 className="text-xl font-bold">Miden Multisig</h1>

      <div className="flex items-center gap-2">
        {/* Signer Keys Popover */}
        {generatingSigner ? (
          <span className="text-sm text-muted-foreground">Generating signer...</span>
        ) : falconCommitment || ecdsaCommitment ? (
          <Popover>
            <PopoverTrigger asChild>
              <Button variant="ghost" className="p-0 h-auto">
                <Badge variant="outline" className="cursor-pointer">
                  Keys {activeScheme ? `(${activeScheme})` : ''}
                </Badge>
              </Button>
            </PopoverTrigger>
            <PopoverContent className="w-80" align="end">
              <div className="space-y-3">
                <h4 className="font-medium">Signer Keys</h4>
                {falconCommitment && (
                  <div className="space-y-1">
                    <div className="flex items-center gap-2">
                      <Badge variant={activeScheme === 'falcon' ? 'default' : 'secondary'} className="text-xs">
                        Falcon
                      </Badge>
                      {activeScheme === 'falcon' && (
                        <span className="text-xs text-muted-foreground">active</span>
                      )}
                    </div>
                    <code
                      onClick={() =>
                        copyToClipboard(falconCommitment, () => toast.success('Falcon commitment copied'))
                      }
                      className="block text-xs bg-muted px-2 py-1.5 rounded cursor-pointer hover:bg-muted/80 font-mono truncate"
                      title="Click to copy full commitment"
                    >
                      {truncateHex(falconCommitment, 10, 6)}
                    </code>
                  </div>
                )}
                {ecdsaCommitment && (
                  <div className="space-y-1">
                    <div className="flex items-center gap-2">
                      <Badge variant={activeScheme === 'ecdsa' ? 'default' : 'secondary'} className="text-xs">
                        ECDSA
                      </Badge>
                      {activeScheme === 'ecdsa' && (
                        <span className="text-xs text-muted-foreground">active</span>
                      )}
                    </div>
                    <code
                      onClick={() =>
                        copyToClipboard(ecdsaCommitment, () => toast.success('ECDSA commitment copied'))
                      }
                      className="block text-xs bg-muted px-2 py-1.5 rounded cursor-pointer hover:bg-muted/80 font-mono truncate"
                      title="Click to copy full commitment"
                    >
                      {truncateHex(ecdsaCommitment, 10, 6)}
                    </code>
                  </div>
                )}
                <p className="text-xs text-muted-foreground">Click a commitment to copy</p>
              </div>
            </PopoverContent>
          </Popover>
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
