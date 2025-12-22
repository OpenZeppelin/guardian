export function formatError(err: unknown, prefix?: string): string {
  const message =
    err instanceof Error
      ? err.message
      : typeof err === 'string'
        ? err
        : 'Unknown error';
  return prefix ? `${prefix}: ${message}` : message;
}

