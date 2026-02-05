import {
  type Multisig,
  type MultisigClient,
  type MultisigConfig,
  type ProcedureThreshold,
  MultisigClient as MultisigClientClass,
  FalconSigner,
  EcdsaSigner,
  type SignatureScheme,
} from '@openzeppelin/miden-multisig-client';
import type { WebClient } from '@demox-labs/miden-sdk';
import type { SignerInfo } from '@/types';

/**
 * Initialize MultisigClient and fetch PSM pubkey.
 */
export async function initMultisigClient(
  webClient: WebClient,
  psmEndpoint: string,
  scheme?: SignatureScheme,
): Promise<{ client: MultisigClient; psmCommitment: string; psmPubkey?: string }> {
  const client = new MultisigClientClass(webClient, { psmEndpoint });
  const { psmCommitment, psmPublicKey } = await client.initialize(scheme);
  return { client, psmCommitment, psmPubkey: psmPublicKey };
}

/**
 * Create a new multisig account.
 * Handles SignerInfo scheme selection (UI-specific).
 */
export async function createMultisigAccount(
  multisigClient: MultisigClient,
  signer: SignerInfo,
  otherCommitments: string[],
  threshold: number,
  psmCommitment: string,
  psmPublicKey?: string,
  procedureThresholds?: ProcedureThreshold[],
  signatureScheme: SignatureScheme = 'falcon',
): Promise<Multisig> {
  const activeSigner = signatureScheme === 'ecdsa' ? signer.ecdsa : signer.falcon;
  const signerCommitments = [activeSigner.commitment, ...otherCommitments];
  const config: MultisigConfig = {
    threshold,
    signerCommitments,
    psmCommitment,
    psmPublicKey,
    psmEnabled: true,
    procedureThresholds,
    storageMode: 'private',
    signatureScheme,
  };
  const clientSigner =
    signatureScheme === 'ecdsa'
      ? new EcdsaSigner(activeSigner.secretKey)
      : new FalconSigner(activeSigner.secretKey);
  return multisigClient.create(config, clientSigner);
}

/**
 * Load an existing multisig account from PSM.
 * Handles SignerInfo scheme selection (UI-specific).
 */
export async function loadMultisigAccount(
  multisigClient: MultisigClient,
  accountId: string,
  signer: SignerInfo,
  psmPublicKey?: string,
  signatureScheme: SignatureScheme = 'falcon',
): Promise<Multisig> {
  const activeSigner = signatureScheme === 'ecdsa' ? signer.ecdsa : signer.falcon;
  const clientSigner =
    signatureScheme === 'ecdsa'
      ? new EcdsaSigner(activeSigner.secretKey)
      : new FalconSigner(activeSigner.secretKey);
  const multisig = await multisigClient.load(accountId, clientSigner);
  if (psmPublicKey) {
    multisig.setPsmPublicKey(psmPublicKey);
  }
  return multisig;
}
