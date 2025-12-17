import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Badge } from '@/components/ui/badge';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Label } from '@/components/ui/label';
import { copyToClipboard, clearIndexedDB } from '@/lib/helpers';

interface StatusSectionProps {
  clientReady: boolean;
  psmStatus: string;
  psmUrl: string;
  psmPubkey: string;
  error: string | null;
  onPsmUrlChange: (url: string) => void;
  onReconnect: () => void;
}

export function StatusSection({
  clientReady,
  psmStatus,
  psmUrl,
  psmPubkey,
  error,
  onPsmUrlChange,
  onReconnect,
}: StatusSectionProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Status</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex flex-wrap gap-4">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium text-muted-foreground">Miden Client:</span>
            <Badge variant={clientReady ? 'default' : 'secondary'}>
              {clientReady ? 'Ready' : 'Initializing...'}
            </Badge>
          </div>
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium text-muted-foreground">PSM Server:</span>
            <Badge variant={psmStatus === 'Connected' ? 'default' : 'destructive'}>
              {psmStatus}
            </Badge>
          </div>
        </div>

        <div className="flex gap-2 items-end">
          <div className="flex-1">
            <Label htmlFor="psm-url">PSM URL</Label>
            <Input
              id="psm-url"
              value={psmUrl}
              onChange={(e) => onPsmUrlChange(e.target.value)}
              placeholder="http://localhost:3000"
            />
          </div>
          <Button onClick={onReconnect} variant="outline">
            Reconnect
          </Button>
        </div>

        <Button
          variant="secondary"
          onClick={async () => {
            await clearIndexedDB();
            window.location.reload();
          }}
        >
          Reset IndexedDB
        </Button>

        {psmPubkey && (
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium text-muted-foreground">PSM Public Key:</span>
            <code
              onClick={() => copyToClipboard(psmPubkey)}
              className="text-xs bg-muted px-2 py-1 rounded cursor-pointer hover:bg-muted/80"
              title="Click to copy"
            >
              {psmPubkey.slice(0, 16)}...{psmPubkey.slice(-8)}
            </code>
          </div>
        )}

        {error && (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}
      </CardContent>
    </Card>
  );
}
