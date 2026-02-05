import { type WebClient, Account, AccountId } from '@demox-labs/miden-sdk';
import { PsmHttpClient, type SignatureScheme } from '@openzeppelin/psm-client';
import { Multisig } from './multisig.js';
import { createMultisigAccount } from './account/index.js';
import { AccountInspector } from './inspector.js';
import type { MultisigConfig, Signer } from './types.js';

export interface MultisigClientConfig {
  psmEndpoint?: string;
}

export class MultisigClient {
  private readonly webClient: WebClient;
  private _psmClient: PsmHttpClient;

  constructor(webClient: WebClient, config: MultisigClientConfig = {}) {
    this.webClient = webClient;
    this._psmClient = new PsmHttpClient(config.psmEndpoint ?? 'http://localhost:3000');
  }

  setPsmEndpoint(endpoint: string): void {
    this._psmClient = new PsmHttpClient(endpoint);
  }

  async initialize(scheme?: SignatureScheme): Promise<{ psmCommitment: string; psmPublicKey?: string }> {
    const { commitment, pubkey } = await this._psmClient.getPubkey(scheme);
    return { psmCommitment: commitment, psmPublicKey: pubkey };
  }

  get psmClient(): PsmHttpClient {
    return this._psmClient;
  }

  async create(config: MultisigConfig, signer: Signer): Promise<Multisig> {
    this._psmClient.setSigner(signer);

    const { account } = await createMultisigAccount(this.webClient, config);

    return new Multisig(account, config, this._psmClient, signer, this.webClient);
  }

  async load(accountId: string, signer: Signer): Promise<Multisig> {
    this._psmClient.setSigner(signer);

    const stateResponse = await this._psmClient.getState(accountId);

    const accountBase64 = stateResponse.stateJson.data;
    if (!accountBase64) {
      throw new Error('No account data found in PSM state');
    }

    const binaryString = atob(accountBase64);
    const accountBytes = new Uint8Array(binaryString.length);
    for (let i = 0; i < binaryString.length; i++) {
      accountBytes[i] = binaryString.charCodeAt(i);
    }
    const account = Account.deserialize(accountBytes);

    const detectedScheme: SignatureScheme = signer.scheme;

    const detected = AccountInspector.fromAccount(account, detectedScheme);
    const config: MultisigConfig = {
      threshold: detected.threshold,
      signerCommitments: detected.signerCommitments,
      psmCommitment: detected.psmCommitment ?? '',
      psmEnabled: detected.psmEnabled,
      signatureScheme: detected.signatureScheme,
      procedureThresholds: Array.from(detected.procedureThresholds.entries()).map(
        ([procedure, threshold]) => ({ procedure, threshold })
      ),
    };

    const existingAccount = await this.webClient.getAccount(AccountId.fromHex(accountId));
    if (!existingAccount) {
        await this.webClient.newAccount(account, true);
    }

    return new Multisig(null, config, this._psmClient, signer, this.webClient, accountId);
  }
}
