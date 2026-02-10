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
import type { WalletSource } from '@/wallets/types';

interface HeaderProps {
  falconCommitment: string | null;
  ecdsaCommitment: string | null;
  activeScheme: SignatureScheme | null;
  generatingSigner: boolean;
  psmStatus: 'connected' | 'connecting' | 'error';
  psmUrl: string;
  onPsmUrlChange: (url: string) => void;
  onReconnect: (url: string) => void;
  walletSource: WalletSource;
  onWalletSourceChange: (source: WalletSource) => void;
  paraConnected: boolean;
  paraCommitment: string | null;
  midenWalletConnected: boolean;
  midenWalletCommitment: string | null;
  onConnectMidenWallet: () => void;
  onDisconnectMidenWallet: () => void;
  onOpenParaModal: () => void;
}

const SOURCE_LABELS: Record<WalletSource, string> = {
  local: 'Local',
  para: 'Para',
  'miden-wallet': 'Miden Wallet',
};

export function Header({
  falconCommitment,
  ecdsaCommitment,
  activeScheme,
  generatingSigner,
  psmStatus,
  psmUrl,
  onPsmUrlChange,
  onReconnect,
  walletSource,
  onWalletSourceChange,
  paraConnected,
  paraCommitment,
  midenWalletConnected,
  midenWalletCommitment,
  onConnectMidenWallet,
  onDisconnectMidenWallet,
  onOpenParaModal,
}: HeaderProps) {
  const [urlInput, setUrlInput] = useState(psmUrl);
  const [popoverOpen, setPopoverOpen] = useState(false);

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
        {/* Wallet Source Selector */}
        <Popover>
          <PopoverTrigger asChild>
            <Button variant="ghost" className="p-0 h-auto">
              <Badge variant="outline" className="cursor-pointer">
                {SOURCE_LABELS[walletSource]}
              </Badge>
            </Button>
          </PopoverTrigger>
          <PopoverContent className="w-80" align="end">
            <div className="space-y-3">
              <h4 className="font-medium">Wallet Source</h4>

              <div className="space-y-2">
                <Button
                  variant={walletSource === 'local' ? 'default' : 'outline'}
                  size="sm"
                  className="w-full justify-start"
                  onClick={() => onWalletSourceChange('local')}
                >
                  Local Keys
                </Button>

                <div className="space-y-1">
                  {paraConnected ? (
                    <Button
                      variant={walletSource === 'para' ? 'default' : 'outline'}
                      size="sm"
                      className="w-full justify-start"
                      onClick={() => onWalletSourceChange('para')}
                    >
                      Para Wallet (connected)
                    </Button>
                  ) : (
                    <Button
                      variant="outline"
                      size="sm"
                      className="w-full justify-start"
                      onClick={onOpenParaModal}
                    >
                      Connect Para Wallet
                    </Button>
                  )}
                  {paraConnected && paraCommitment && (
                    <code
                      onClick={() => copyToClipboard(paraCommitment, () => toast.success('Para commitment copied'))}
                      className="block text-xs bg-muted px-2 py-1 rounded cursor-pointer hover:bg-muted/80 font-mono truncate"
                    >
                      {truncateHex(paraCommitment, 10, 6)}
                    </code>
                  )}
                </div>

                <div className="space-y-1">
                  {midenWalletConnected ? (
                    <Button
                      variant={walletSource === 'miden-wallet' ? 'default' : 'outline'}
                      size="sm"
                      className="w-full justify-between"
                      onClick={() => onWalletSourceChange('miden-wallet')}
                    >
                      <span>Miden Wallet (connected)</span>
                      <span
                        className="text-xs underline ml-2"
                        onClick={(e) => { e.stopPropagation(); onDisconnectMidenWallet(); }}
                      >
                        Disconnect
                      </span>
                    </Button>
                  ) : (
                    <Button
                      variant="outline"
                      size="sm"
                      className="w-full justify-start"
                      onClick={onConnectMidenWallet}
                    >
                      Connect Miden Wallet
                    </Button>
                  )}
                  {midenWalletConnected && midenWalletCommitment && (
                    <code
                      onClick={() => copyToClipboard(midenWalletCommitment, () => toast.success('Wallet commitment copied'))}
                      className="block text-xs bg-muted px-2 py-1 rounded cursor-pointer hover:bg-muted/80 font-mono truncate"
                    >
                      {truncateHex(midenWalletCommitment, 10, 6)}
                    </code>
                  )}
                </div>
              </div>
            </div>
          </PopoverContent>
        </Popover>

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
                <h4 className="font-medium">Local Signer Keys</h4>
                {falconCommitment && (
                  <div className="space-y-1">
                    <div className="flex items-center gap-2">
                      <Badge variant={activeScheme === 'falcon' && walletSource === 'local' ? 'default' : 'secondary'} className="text-xs">
                        Falcon
                      </Badge>
                      {activeScheme === 'falcon' && walletSource === 'local' && (
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
                      <Badge variant={activeScheme === 'ecdsa' && walletSource === 'local' ? 'default' : 'secondary'} className="text-xs">
                        ECDSA
                      </Badge>
                      {activeScheme === 'ecdsa' && walletSource === 'local' && (
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
