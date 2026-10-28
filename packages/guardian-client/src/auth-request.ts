type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

/**
 * An operation whose signed payload is domain-separated from every other request's, so a
 * signature over one never authenticates another request with the same body. Mirrors the Rust
 * `SignedOperation`.
 */
export type SignedOperation = 'ExecuteDeltaProposal';

function operationTag(operation: SignedOperation): string {
  switch (operation) {
    case 'ExecuteDeltaProposal':
      return 'guardian.ExecuteDeltaProposal\0';
    default: {
      const unreachable: never = operation;
      throw new Error(`Unknown signed operation: ${JSON.stringify(unreachable)}`);
    }
  }
}

export class RequestAuthPayload {
  private constructor(
    private readonly canonicalJson: string,
    private readonly tag: string,
  ) {}

  static fromRequest(requestPayload: unknown): RequestAuthPayload {
    return new RequestAuthPayload(RequestAuthPayload.canonicalJsonOf(requestPayload), '');
  }

  /**
   * The payload of `requestPayload` signed as `operation`: the bytes are the operation's tag
   * followed by the canonical JSON, as the Rust `AuthRequestPayload::for_operation` builds them.
   */
  static forOperation(operation: SignedOperation, requestPayload: unknown): RequestAuthPayload {
    return new RequestAuthPayload(RequestAuthPayload.canonicalJsonOf(requestPayload), operationTag(operation));
  }

  toCanonicalJson(): string {
    return this.canonicalJson;
  }

  toBytes(): Uint8Array {
    return new TextEncoder().encode(this.tag + this.canonicalJson);
  }

  private static canonicalJsonOf(requestPayload: unknown): string {
    const normalized = RequestAuthPayload.normalizeJson(requestPayload);
    return JSON.stringify(RequestAuthPayload.canonicalizeJson(normalized));
  }

  private static normalizeJson(value: unknown): JsonValue {
    if (value === undefined) {
      return null;
    }
    return JSON.parse(JSON.stringify(value)) as JsonValue;
  }

  private static canonicalizeJson(value: JsonValue): JsonValue {
    if (Array.isArray(value)) {
      return value.map((item) => RequestAuthPayload.canonicalizeJson(item));
    }

    if (value && typeof value === 'object') {
      const entries = Object.entries(value).sort(([left], [right]) => left.localeCompare(right));
      const normalized: { [key: string]: JsonValue } = {};
      for (const [key, item] of entries) {
        normalized[key] = RequestAuthPayload.canonicalizeJson(item);
      }
      return normalized;
    }

    return value;
  }
}
