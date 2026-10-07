import { describe, expect, it } from 'vitest';

import { requireConfigValue, requireMidenRpcEndpoint } from './config.js';

describe('config', () => {
  it.each([undefined, null, 42, {}, []])(
    'rejects non-string configuration value %j consistently',
    (value) => {
      expect(() => requireConfigValue('guardianEndpoint', value)).toThrow(
        'missing required configuration: guardianEndpoint',
      );
    },
  );

  it('trims surrounding whitespace from configuration values', () => {
    expect(requireConfigValue('guardianEndpoint', '  http://localhost:3000\n')).toBe(
      'http://localhost:3000',
    );
  });

  it.each([undefined, '', '   '])('rejects a missing Miden RPC endpoint %j', (endpoint) => {
    expect(() => requireMidenRpcEndpoint(endpoint)).toThrow(
      'missing required configuration: midenRpcEndpoint',
    );
  });
});
