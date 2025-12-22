import {
  type Multisig,
  type MultisigClient,
  type Proposal,
  type AccountState,
  type MultisigConfig,
  type ConsumableNote,
} from '@openzeppelin/miden-multisig-client';
import type { WebClient } from '@demox-labs/miden-sdk';
import { FalconSigner } from '@openzeppelin/miden-multisig-client';
import type { SignerInfo } from '@/types';

export async function createMultisigAccount(
  multisigClient: MultisigClient,
  signer: SignerInfo,
  otherCommitments: string[],
  threshold: number,
  psmCommitment: string,
): Promise<Multisig> {
  const signerCommitments = [signer.commitment, ...otherCommitments];
  const config: MultisigConfig = {
    threshold,
    signerCommitments,
    psmCommitment,
    psmEnabled: true,
  };
  const falconSigner = new FalconSigner(signer.secretKey);
  return multisigClient.create(config, falconSigner);
}

export async function loadMultisigAccount(
  multisigClient: MultisigClient,
  accountId: string,
  config: MultisigConfig,
  signer: SignerInfo,
): Promise<Multisig> {
  const falconSigner = new FalconSigner(signer.secretKey);
  return multisigClient.load(accountId, config, falconSigner);
}

export async function syncAll(
  multisig: Multisig,
  webClient: WebClient | null,
): Promise<{ proposals: Proposal[]; state: AccountState | null; notes: ConsumableNote[] }> {
  const proposals = await multisig.syncProposals();
  const state = webClient ? await multisig.fetchState() : null;
  const notes = webClient ? await multisig.getConsumableNotes(webClient) : [];
  return { proposals, state, notes };
}

