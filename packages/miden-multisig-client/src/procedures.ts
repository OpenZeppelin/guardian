import type { SignatureScheme } from '@openzeppelin/psm-client';

export const PROCEDURE_ROOTS_FALCON = {
  update_signers: '0x53d0ad381a193de0cf6af3730141e498274103dfc3b8c8e7367bd49d4a66c72b',
  auth_tx: '0x474c613b38001cc36d68557e9d881495d6a461a9027033445c7672c586509026',
  update_psm: '0xb103236807e5bf09c27efc2c5287ca8b03ab9efc1d852b62ebd31f5f1927ec26',
  verify_psm: '0x30727fc23c6105a678fea8b4c1920f35fa85c03f16a0cf98372c8f56701f8a87',
  send_asset: '0x0e406b067ed2bcd7de745ca6517f519fd1a9be245f913347ac673ca1db30c1d6',
  receive_asset: '0x6f4bdbdc4b13d7ed933d590d88ac9dfb98020c9e917697845b5e169395b76a01',
} as const;

export const PROCEDURE_ROOTS_ECDSA = {
  update_signers: '0x930f9ea86d33c7b3b2d23c4e9ac2492add461359344ed25b1acd38f3713dd79c',
  auth_tx: '0x2bc7664a9dd47b36e7c8b8c3df03412798e4410173f36acfe03d191a38add053',
  update_psm: '0x26ec27195f1fd3eb622b851dfc9eab038bca87522cfc7ec209bfe507682303b1',
  verify_psm: '0xbeb9e08a83eca030968f2f137b5673136f21bc16c82fd408c2ce2495ccbdcd15',
  send_asset: '0xd6c130dba13c67ac4733915f24bea9d19f517f51a65c74ded7bcd27e066b400e',
  receive_asset: '0x016ab79593165e5b849776919e0c0298fb9dac880d593d93edd7134bdcdb4b6f',
} as const;

export const PROCEDURE_ROOTS = PROCEDURE_ROOTS_FALCON;

export type ProcedureName = keyof typeof PROCEDURE_ROOTS_FALCON;

export function getProcedureRoot(name: ProcedureName, scheme: SignatureScheme = 'falcon'): string {
  const roots = scheme === 'ecdsa' ? PROCEDURE_ROOTS_ECDSA : PROCEDURE_ROOTS_FALCON;
  return roots[name];
}

export function isProcedureName(name: string): name is ProcedureName {
  return name in PROCEDURE_ROOTS_FALCON;
}

export function getProcedureNames(scheme: SignatureScheme = 'falcon'): ProcedureName[] {
  const roots = scheme === 'ecdsa' ? PROCEDURE_ROOTS_ECDSA : PROCEDURE_ROOTS_FALCON;
  return Object.keys(roots) as ProcedureName[];
}
