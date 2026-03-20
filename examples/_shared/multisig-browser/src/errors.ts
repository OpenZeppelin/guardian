export function normalizeError(err: unknown): string {
  if (err instanceof Error) {
    return err.message;
  }

  if (typeof err === 'string') {
    return err;
  }

  return 'Unknown error';
}

export function formatError(err: unknown, prefix?: string): string {
  const message = normalizeError(err);
  return prefix ? `${prefix}: ${message}` : message;
}

export function classifyWalletError(err: unknown): string {
  const message = normalizeError(err);
  const name = err instanceof Error ? err.name : '';
  const lower = `${message} ${name}`.toLowerCase();

  if (
    lower.includes('user cancelled') ||
    lower.includes('user rejected') ||
    lower.includes('user denied')
  ) {
    return 'Signing was cancelled';
  }

  if (
    lower.includes('walletnotready') ||
    lower.includes('not detected') ||
    lower.includes('not found') ||
    lower.includes('not installed')
  ) {
    return 'Wallet extension not detected. Please install the Miden Wallet browser extension.';
  }

  if (lower.includes('not connected') || lower.includes('no wallet')) {
    return 'Wallet is not connected';
  }

  if (lower.includes('invalid signature') || lower.includes('signature format')) {
    return 'Invalid signature format';
  }

  return message || name || 'Unknown wallet error';
}

export function normalizeCommitment(hex: string): string {
  const trimmed = hex.trim();
  if (!trimmed) {
    throw new Error('Commitment is required');
  }

  const withoutPrefix =
    trimmed.startsWith('0x') || trimmed.startsWith('0X') ? trimmed.slice(2) : trimmed;

  if (!/^[0-9a-fA-F]{64}$/.test(withoutPrefix)) {
    throw new Error('Commitment must be a 64-character hex string');
  }

  return `0x${withoutPrefix.toLowerCase()}`;
}

export async function copyToClipboard(text: string): Promise<void> {
  await navigator.clipboard.writeText(text);
}
