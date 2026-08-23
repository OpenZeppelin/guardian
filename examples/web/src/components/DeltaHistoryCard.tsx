import { useState } from 'react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import type { HistoryEntry, Multisig } from '@openzeppelin/miden-multisig-client';

const PAGE_SIZE = 10;

function shorten(hex: string): string {
  return hex.length > 14 ? `${hex.slice(0, 8)}…${hex.slice(-4)}` : hex;
}

interface DeltaHistoryCardProps {
  multisig: Multisig;
}

/**
 * Confirmed (canonical) delta history from Guardian (issue #413),
 * newest-first by nonce, loaded one page at a time via the opaque
 * cursor. Self-contained: owns its own paging state so the dashboard
 * only passes the multisig handle.
 */
export function DeltaHistoryCard({ multisig }: DeltaHistoryCardProps) {
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [nextCursor, setNextCursor] = useState<string | undefined>(undefined);
  const [loaded, setLoaded] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadPage = async (cursor?: string) => {
    setLoading(true);
    setError(null);
    try {
      const page = await multisig.deltaHistory({ limit: PAGE_SIZE, cursor });
      setEntries((previous) => (cursor ? [...previous, ...page.entries] : page.entries));
      setNextCursor(page.nextCursor);
      setLoaded(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex items-center justify-between">
          <CardTitle className="text-lg">Delta History</CardTitle>
          <Button
            variant="outline"
            size="sm"
            onClick={() => loadPage()}
            disabled={loading}
          >
            {loading && !loaded ? 'Loading…' : loaded ? 'Refresh' : 'Load'}
          </Button>
        </div>
      </CardHeader>
      <CardContent className="space-y-3">
        {error && <p className="text-sm text-destructive">{error}</p>}
        {!loaded && !error && (
          <p className="text-sm text-muted-foreground">
            Confirmed transactions from Guardian, newest first. Only canonical
            deltas appear; pending proposals are listed below.
          </p>
        )}
        {loaded && entries.length === 0 && (
          <p className="text-sm text-muted-foreground">No confirmed deltas yet.</p>
        )}
        {entries.map((entry) => (
          <div key={entry.nonce} className="rounded-md border p-3 text-sm space-y-1">
            <div className="flex items-center justify-between">
              <span className="font-medium">Nonce {entry.nonce}</span>
              <Badge variant="secondary">{entry.status}</Badge>
            </div>
            <p className="text-muted-foreground">{entry.timestamp}</p>
            {entry.newCommitment && (
              <p className="text-muted-foreground font-mono">
                {shorten(entry.newCommitment)}
              </p>
            )}
            {[...entry.inputNotes.map((note) => ({ note, direction: 'In' })),
              ...entry.outputNotes.map((note) => ({ note, direction: 'Out' }))].map(
              ({ note, direction }) => (
                <p key={`${direction}-${note.noteId}`} className="text-muted-foreground">
                  {direction}: {note.tag} note {shorten(note.noteId)} ({note.noteType})
                  {note.recipient ? ` → ${shorten(note.recipient)}` : ''}
                </p>
              ),
            )}
            {entry.decodeWarnings.length > 0 && (
              <p className="text-muted-foreground italic">
                Note details unavailable: {entry.decodeWarnings[0].reason}
              </p>
            )}
          </div>
        ))}
        {loaded && nextCursor && (
          <Button
            variant="outline"
            size="sm"
            className="w-full"
            onClick={() => loadPage(nextCursor)}
            disabled={loading}
          >
            {loading ? 'Loading…' : 'Load more'}
          </Button>
        )}
      </CardContent>
    </Card>
  );
}
