export class GuardianEvmHttpError extends Error {
  public readonly code?: string;

  constructor(
    public readonly status: number,
    public readonly statusText: string,
    public readonly body: string
  ) {
    super(`Guardian EVM HTTP error ${status}: ${statusText} - ${body}`);
    this.name = 'GuardianEvmHttpError';
    this.code = parseErrorCode(body);
  }
}

function parseErrorCode(body: string): string | undefined {
  try {
    const parsed = JSON.parse(body) as { code?: unknown };
    return typeof parsed.code === 'string' ? parsed.code : undefined;
  } catch {
    return undefined;
  }
}
