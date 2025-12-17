import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { copyToClipboard } from '@/lib/helpers';
import type { SignerInfo } from '@/types';

interface SignerSectionProps {
  signer: SignerInfo | null;
  clientReady: boolean;
  generatingKey: boolean;
  onGenerateSigner: () => void;
}

export function SignerSection({
  signer,
  clientReady,
  generatingKey,
  onGenerateSigner,
}: SignerSectionProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Your Signer</CardTitle>
        <CardDescription>
          Generate a unique signing key. Share your commitment with other signers to create a
          multisig.
        </CardDescription>
      </CardHeader>
      <CardContent>
        {!signer ? (
          <Button onClick={onGenerateSigner} disabled={!clientReady || generatingKey}>
            {generatingKey ? 'Generating...' : 'Generate Signer Key'}
          </Button>
        ) : (
          <div className="space-y-2">
            <div>
              <span className="text-sm font-medium text-muted-foreground">Your Commitment:</span>
              <code
                onClick={() => copyToClipboard(signer.commitment)}
                className="block mt-1 text-xs bg-muted px-3 py-2 rounded cursor-pointer hover:bg-muted/80 break-all"
                title="Click to copy - share with other signers"
              >
                {signer.commitment}
              </code>
            </div>
            <p className="text-sm text-muted-foreground">
              Copy your commitment above and share it with other signers.
            </p>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
