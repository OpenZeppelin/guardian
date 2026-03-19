type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

export class RequestAuthPayload {
  private constructor(private readonly canonicalJson: string) {}

  static fromRequest(requestPayload: unknown): RequestAuthPayload {
    const normalized = RequestAuthPayload.normalizeJson(requestPayload);
    const canonical = RequestAuthPayload.canonicalizeJson(normalized);
    return new RequestAuthPayload(JSON.stringify(canonical));
  }

  toCanonicalJson(): string {
    return this.canonicalJson;
  }

  toBytes(): Uint8Array {
    return new TextEncoder().encode(this.canonicalJson);
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
