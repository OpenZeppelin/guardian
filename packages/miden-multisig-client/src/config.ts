export function requireConfigValue(field: string, value?: unknown): string {
  if (typeof value !== 'string') {
    throw new Error(`missing required configuration: ${field}`);
  }
  const normalizedValue = value.trim();
  if (normalizedValue === '') {
    throw new Error(`missing required configuration: ${field}`);
  }
  return normalizedValue;
}

export function requireMidenRpcEndpoint(endpoint?: string): string {
  return requireConfigValue('midenRpcEndpoint', endpoint);
}
