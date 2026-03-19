import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';

interface WelcomeViewProps {
  ready: boolean;
  onCreateClick: () => void;
  onLoadClick: () => void;
  onResetData: () => void;
}

export function WelcomeView({ ready, onCreateClick, onLoadClick, onResetData }: WelcomeViewProps) {
  return (
    <div className="flex flex-col items-center justify-center min-h-[60vh] gap-8">
      <div className="flex gap-6">
        <Card className="w-64 hover:border-primary/50 transition-colors">
          <CardHeader className="text-center">
            <CardTitle>Create Multisig</CardTitle>
            <CardDescription>
              Create a new multisig account with custom signers and threshold.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Button className="w-full" onClick={onCreateClick} disabled={!ready}>
              Create New
            </Button>
          </CardContent>
        </Card>

        <Card className="w-64 hover:border-primary/50 transition-colors">
          <CardHeader className="text-center">
            <CardTitle>Load Multisig</CardTitle>
            <CardDescription>
              Load an existing multisig account from GUARDIAN by account ID.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Button className="w-full" variant="outline" onClick={onLoadClick} disabled={!ready}>
              Load Existing
            </Button>
          </CardContent>
        </Card>
      </div>

      <Button
        variant="ghost"
        size="sm"
        className="text-muted-foreground"
        onClick={onResetData}
      >
        Reset Data (Clear Key & Account)
      </Button>
    </div>
  );
}
