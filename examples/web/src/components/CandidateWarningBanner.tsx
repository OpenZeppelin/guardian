import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { TriangleAlert, X } from 'lucide-react';
import { Button } from '@/components/ui/button';

interface CandidateWarningBannerProps {
  message: string;
  onDismiss?: () => void;
}

export function CandidateWarningBanner({ message, onDismiss }: CandidateWarningBannerProps) {
  return (
    <Alert className="border-yellow-500 bg-yellow-50 dark:bg-yellow-950 relative">
      <TriangleAlert className="h-4 w-4 text-yellow-600" />
      <AlertTitle className="text-yellow-800 dark:text-yellow-200">
        Pending Transaction
      </AlertTitle>
      <AlertDescription className="text-yellow-700 dark:text-yellow-300 pr-8">
        {message}
      </AlertDescription>
      {onDismiss && (
        <Button
          variant="ghost"
          size="sm"
          className="absolute top-2 right-2 h-6 w-6 p-0 text-yellow-700 hover:text-yellow-900 hover:bg-yellow-200 dark:text-yellow-300 dark:hover:text-yellow-100 dark:hover:bg-yellow-800"
          onClick={onDismiss}
        >
          <X className="h-4 w-4" />
          <span className="sr-only">Dismiss</span>
        </Button>
      )}
    </Alert>
  );
}
