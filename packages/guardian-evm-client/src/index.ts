import { getAddress, keccak256, toBytes, toHex, type Hex } from 'viem';

type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

type TypedDataField = { name: string; type: string };

type GuardianTypedData = {
  domain: {
    name: string;
    version: string;
    chainId: number;
    verifyingContract: `0x${string}`;
  };
  types: {
    EIP712Domain: TypedDataField[];
    [key: string]: TypedDataField[];
  };
  primaryType: string;
  message: Record<string, string | number>;
};

interface ServerEcdsaSignature {
  scheme: 'ecdsa';
  signature: string;
  public_key?: string;
}

interface ServerProposalSignature {
  signer_id: string;
  signature: ServerEcdsaSignature;
}

type ServerDeltaStatus =
  | { status: 'pending'; timestamp: string; proposer_id: string; cosigner_sigs: ServerCosignerSignature[] }
  | { status: 'candidate'; timestamp: string }
  | { status: 'canonical'; timestamp: string }
  | { status: 'discarded'; timestamp: string };

interface ServerCosignerSignature {
  signer_id: string;
  signature: ServerEcdsaSignature;
  timestamp: string;
}

interface ServerEvmProposalPayload {
  kind: 'evm';
  mode: string;
  execution_calldata: string;
  signatures?: ServerProposalSignature[];
}

interface ServerDeltaObject {
  account_id: string;
  nonce: number;
  prev_commitment: string;
  new_commitment?: string;
  delta_payload: ServerEvmProposalPayload;
  ack_sig?: string;
  ack_pubkey?: string;
  ack_scheme?: string;
  status: ServerDeltaStatus;
}

interface ServerConfigureResponse {
  success: boolean;
  message: string;
  ack_pubkey?: string;
  ack_commitment?: string;
  code?: string;
}

interface ServerStateObject {
  account_id: string;
  commitment: string;
  state_json: { data: string };
  created_at: string;
  updated_at: string;
  auth_scheme?: string;
}

interface ServerDeltaProposalResponse {
  delta: ServerDeltaObject;
  commitment: string;
}

interface ServerProposalsResponse {
  proposals: ServerDeltaObject[];
}

export interface RequestSigner {
  readonly commitment: string;
  readonly publicKey: string;
  readonly scheme: 'ecdsa';
  signRequest(accountId: string, timestamp: number, requestPayload: RequestAuthPayload): Promise<string> | string;
}

export interface EcdsaSignature {
  scheme: 'ecdsa';
  signature: string;
  publicKey?: string;
}

export interface CosignerSignature {
  signerId: string;
  signature: EcdsaSignature;
  timestamp: string;
}

export type DeltaStatus =
  | { status: 'pending'; timestamp: string; proposerId: string; cosignerSigs: CosignerSignature[] }
  | { status: 'candidate'; timestamp: string }
  | { status: 'canonical'; timestamp: string }
  | { status: 'discarded'; timestamp: string };

export interface EvmNetworkConfig {
  kind: 'evm';
  chainId: number;
  accountAddress: string;
  multisigModuleAddress: string;
  rpcEndpoint: string;
}

export interface EvmProposalPayload {
  kind: 'evm';
  mode: string;
  executionCalldata: string;
  signatures: Array<{ signerId: string; signature: EcdsaSignature }>;
}

export interface DeltaObject {
  accountId: string;
  nonce: number;
  prevCommitment: string;
  newCommitment?: string;
  deltaPayload: EvmProposalPayload;
  ackSig?: string;
  ackPubkey?: string;
  ackScheme?: string;
  status: DeltaStatus;
}

export interface ConfigureResponse {
  success: boolean;
  message: string;
  ackPubkey?: string;
  ackCommitment?: string;
  code?: string;
}

export interface DeltaProposalResponse {
  delta: DeltaObject;
  commitment: string;
}

export interface StateObject {
  accountId: string;
  commitment: string;
  stateJson: { data: string };
  createdAt: string;
  updatedAt: string;
  authScheme?: string;
}

export interface Eip1193Provider {
  request(args: { method: string; params?: unknown[] }): Promise<unknown>;
}

export interface GuardianEvmClientOptions {
  guardianUrl: string;
  provider: Eip1193Provider;
  networkConfig: EvmNetworkConfig;
  signerAddress: string;
}

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

export class GuardianEvmHttpError extends Error {
  public readonly code?: string;

  constructor(
    public readonly status: number,
    public readonly statusText: string,
    public readonly body: string
  ) {
    super(`GUARDIAN EVM HTTP error ${status}: ${statusText} - ${body}`);
    this.name = 'GuardianEvmHttpError';
    this.code = GuardianEvmHttpError.parseCode(body);
  }

  private static parseCode(body: string): string | undefined {
    try {
      const parsed = JSON.parse(body) as { code?: unknown };
      return typeof parsed.code === 'string' ? parsed.code : undefined;
    } catch {
      return undefined;
    }
  }
}

export class GuardianEvmClient {
  readonly accountId: string;
  readonly networkConfig: EvmNetworkConfig;
  readonly signer: EvmInjectedWalletSigner;

  private readonly guardianUrl: string;
  private lastTimestamp = 0;

  constructor(options: GuardianEvmClientOptions) {
    this.guardianUrl = options.guardianUrl;
    this.networkConfig = normalizeEvmNetworkConfig(options.networkConfig);
    this.accountId = evmAccountId(this.networkConfig.chainId, this.networkConfig.accountAddress);
    this.signer = new EvmInjectedWalletSigner(options.provider, this.networkConfig, options.signerAddress);
  }

  configure(
    signers: string[] = [this.signer.publicKey],
    initialState: Record<string, never> = {}
  ): Promise<ConfigureResponse> {
    return this.fetchAuthenticated(
      '/configure',
      {
        method: 'POST',
        body: JSON.stringify({
          account_id: this.accountId,
          auth: { EvmEcdsa: { signers: signers.map(normalizeEvmAddress) } },
          network_config: toServerNetworkConfig(this.networkConfig),
          initial_state: initialState,
        }),
      },
      {
        account_id: this.accountId,
        auth: { EvmEcdsa: { signers: signers.map(normalizeEvmAddress) } },
        network_config: toServerNetworkConfig(this.networkConfig),
        initial_state: initialState,
      }
    ).then(async (response) => fromServerConfigureResponse((await response.json()) as ServerConfigureResponse));
  }

  createProposal(payload: EvmProposalPayload, nonce: number): Promise<DeltaProposalResponse> {
    const request = {
      account_id: this.accountId,
      nonce,
      delta_payload: toServerEvmProposalPayload(payload),
    };
    return this.fetchAuthenticated('/delta/proposal', {
      method: 'POST',
      body: JSON.stringify(request),
    }, request).then(async (response) => {
      const server = (await response.json()) as ServerDeltaProposalResponse;
      return {
        delta: fromServerDeltaObject(server.delta),
        commitment: server.commitment,
      };
    });
  }

  async signProposal(commitment: string, payload: EvmProposalPayload): Promise<DeltaObject> {
    const signature = await this.signer.signEvmProposal(payload);
    const request = {
      account_id: this.accountId,
      commitment,
      signature: toServerEcdsaSignature(signature),
    };
    const response = await this.fetchAuthenticated('/delta/proposal', {
      method: 'PUT',
      body: JSON.stringify(request),
    }, request);
    return fromServerDeltaObject((await response.json()) as ServerDeltaObject);
  }

  getProposal(commitment: string): Promise<DeltaObject> {
    const requestQuery = { account_id: this.accountId, commitment };
    const params = new URLSearchParams(requestQuery);
    return this.fetchAuthenticated(`/delta/proposal/single?${params}`, {
      method: 'GET',
    }, requestQuery).then(async (response) => fromServerDeltaObject((await response.json()) as ServerDeltaObject));
  }

  listProposals(): Promise<DeltaObject[]> {
    const requestQuery = { account_id: this.accountId };
    const params = new URLSearchParams(requestQuery);
    return this.fetchAuthenticated(`/delta/proposal?${params}`, {
      method: 'GET',
    }, requestQuery).then(async (response) => {
      const server = (await response.json()) as ServerProposalsResponse;
      return server.proposals.map(fromServerDeltaObject);
    });
  }

  getState(): Promise<StateObject> {
    const requestQuery = { account_id: this.accountId };
    const params = new URLSearchParams(requestQuery);
    return this.fetchAuthenticated(`/state?${params}`, {
      method: 'GET',
    }, requestQuery).then(async (response) => fromServerStateObject((await response.json()) as ServerStateObject));
  }

  private async fetchAuthenticated(path: string, init: RequestInit, requestPayload: unknown, retries = 2): Promise<Response> {
    const now = Date.now();
    const timestamp = now > this.lastTimestamp ? now : this.lastTimestamp + 1;
    this.lastTimestamp = timestamp;
    const signature = await this.signer.signRequest(this.accountId, timestamp, RequestAuthPayload.fromRequest(requestPayload));

    try {
      return await this.fetch(path, {
        ...init,
        headers: {
          ...init.headers,
          'x-pubkey': this.signer.publicKey,
          'x-signature': signature,
          'x-timestamp': timestamp.toString(),
        },
      });
    } catch (err) {
      if (retries > 0 && err instanceof GuardianEvmHttpError && err.body.includes('Replay attack')) {
        await new Promise((resolve) => setTimeout(resolve, 50));
        return this.fetchAuthenticated(path, init, requestPayload, retries - 1);
      }
      throw err;
    }
  }

  private async fetch(path: string, init: RequestInit): Promise<Response> {
    const response = await fetch(`${this.guardianUrl}${path}`, {
      ...init,
      headers: {
        'Content-Type': 'application/json',
        ...init.headers,
      },
    });

    if (!response.ok) {
      const body = await response.text();
      throw new GuardianEvmHttpError(response.status, response.statusText, body);
    }

    return response;
  }
}

export function normalizeEvmAddress(address: string): `0x${string}` {
  return getAddress(address).toLowerCase() as `0x${string}`;
}

export function evmAccountId(chainId: number, accountAddress: string): string {
  return `evm:${chainId}:${normalizeEvmAddress(accountAddress)}`;
}

export function buildEvmRequestTypedData(
  config: EvmNetworkConfig,
  accountId: string,
  timestamp: number,
  requestPayload: RequestAuthPayload
): GuardianTypedData {
  const normalized = normalizeEvmNetworkConfig(config);
  return {
    domain: evmDomain('Guardian EVM Request', normalized),
    types: {
      EIP712Domain: eip712DomainFields(),
      GuardianRequest: [
        { name: 'account_id', type: 'string' },
        { name: 'timestamp', type: 'uint64' },
        { name: 'request_hash', type: 'bytes32' },
      ],
    },
    primaryType: 'GuardianRequest',
    message: {
      account_id: accountId,
      timestamp,
      request_hash: keccak256(toHex(requestPayload.toBytes())),
    },
  };
}

export function buildEvmProposalTypedData(
  config: EvmNetworkConfig,
  payload: EvmProposalPayload
): GuardianTypedData {
  const normalized = normalizeEvmNetworkConfig(config);
  return {
    domain: evmDomain('Guardian EVM Proposal', normalized),
    types: {
      EIP712Domain: eip712DomainFields(),
      GuardianProposal: [
        { name: 'mode', type: 'bytes32' },
        { name: 'execution_calldata_hash', type: 'bytes32' },
      ],
    },
    primaryType: 'GuardianProposal',
    message: {
      mode: normalizeBytes32(payload.mode, 'mode'),
      execution_calldata_hash: keccak256(normalizeHex(payload.executionCalldata, 'execution calldata')),
    },
  };
}

export class EvmInjectedWalletSigner implements RequestSigner {
  readonly commitment: string;
  readonly publicKey: `0x${string}`;
  readonly scheme = 'ecdsa' as const;

  constructor(
    private readonly provider: Eip1193Provider,
    private readonly config: EvmNetworkConfig,
    signerAddress: string
  ) {
    this.publicKey = normalizeEvmAddress(signerAddress);
    this.commitment = this.publicKey;
  }

  async signRequest(
    accountId: string,
    timestamp: number,
    requestPayload: RequestAuthPayload
  ): Promise<string> {
    const typedData = buildEvmRequestTypedData(this.config, accountId, timestamp, requestPayload);
    return signTypedData(this.provider, this.publicKey, typedData);
  }

  async signEvmProposal(payload: EvmProposalPayload): Promise<EcdsaSignature> {
    return signEvmProposal(this.provider, this.config, this.publicKey, payload);
  }
}

export async function signEvmProposal(
  provider: Eip1193Provider,
  config: EvmNetworkConfig,
  signerAddress: string,
  payload: EvmProposalPayload
): Promise<EcdsaSignature> {
  const signature = await signTypedData(
    provider,
    normalizeEvmAddress(signerAddress),
    buildEvmProposalTypedData(config, payload)
  );
  return { scheme: 'ecdsa', signature };
}

function fromServerConfigureResponse(server: ServerConfigureResponse): ConfigureResponse {
  const response: ConfigureResponse = {
    success: server.success,
    message: server.message,
    ackPubkey: server.ack_pubkey,
    ackCommitment: server.ack_commitment,
  };
  if (server.code) {
    response.code = server.code;
  }
  return response;
}

function fromServerDeltaObject(server: ServerDeltaObject): DeltaObject {
  return {
    accountId: server.account_id,
    nonce: server.nonce,
    prevCommitment: server.prev_commitment,
    newCommitment: server.new_commitment,
    deltaPayload: {
      kind: 'evm',
      mode: server.delta_payload.mode,
      executionCalldata: server.delta_payload.execution_calldata,
      signatures: (server.delta_payload.signatures ?? []).map((signature) => ({
        signerId: signature.signer_id,
        signature: fromServerEcdsaSignature(signature.signature),
      })),
    },
    ackSig: server.ack_sig,
    ackPubkey: server.ack_pubkey,
    ackScheme: server.ack_scheme,
    status: fromServerDeltaStatus(server.status),
  };
}

function fromServerDeltaStatus(server: ServerDeltaStatus): DeltaStatus {
  switch (server.status) {
    case 'pending':
      return {
        status: 'pending',
        timestamp: server.timestamp,
        proposerId: server.proposer_id,
        cosignerSigs: server.cosigner_sigs.map((signature) => ({
          signerId: signature.signer_id,
          signature: fromServerEcdsaSignature(signature.signature),
          timestamp: signature.timestamp,
        })),
      };
    case 'candidate':
      return { status: 'candidate', timestamp: server.timestamp };
    case 'canonical':
      return { status: 'canonical', timestamp: server.timestamp };
    case 'discarded':
      return { status: 'discarded', timestamp: server.timestamp };
  }
}

function fromServerEcdsaSignature(signature: ServerEcdsaSignature): EcdsaSignature {
  return {
    scheme: 'ecdsa',
    signature: signature.signature,
    publicKey: signature.public_key,
  };
}

function fromServerStateObject(server: ServerStateObject): StateObject {
  return {
    accountId: server.account_id,
    commitment: server.commitment,
    stateJson: server.state_json,
    createdAt: server.created_at,
    updatedAt: server.updated_at,
    authScheme: server.auth_scheme,
  };
}

function toServerEvmProposalPayload(payload: EvmProposalPayload): ServerEvmProposalPayload {
  return {
    kind: 'evm',
    mode: payload.mode,
    execution_calldata: payload.executionCalldata,
    signatures: payload.signatures.map((signature) => ({
      signer_id: signature.signerId,
      signature: toServerEcdsaSignature(signature.signature),
    })),
  };
}

function toServerEcdsaSignature(signature: EcdsaSignature): ServerEcdsaSignature {
  return {
    scheme: 'ecdsa',
    signature: signature.signature,
    public_key: signature.publicKey,
  };
}

function toServerNetworkConfig(config: EvmNetworkConfig): {
  kind: 'evm';
  chain_id: number;
  account_address: string;
  multisig_module_address: string;
  rpc_endpoint: string;
} {
  return {
    kind: 'evm',
    chain_id: config.chainId,
    account_address: config.accountAddress,
    multisig_module_address: config.multisigModuleAddress,
    rpc_endpoint: config.rpcEndpoint,
  };
}

async function signTypedData(
  provider: Eip1193Provider,
  signerAddress: `0x${string}`,
  typedData: GuardianTypedData
): Promise<string> {
  const signature = await provider.request({
    method: 'eth_signTypedData_v4',
    params: [signerAddress, JSON.stringify(typedData)],
  });
  if (typeof signature !== 'string') {
    throw new Error('Wallet returned a non-string EIP-712 signature');
  }
  return signature;
}

function normalizeEvmNetworkConfig(config: EvmNetworkConfig): EvmNetworkConfig {
  return {
    ...config,
    accountAddress: normalizeEvmAddress(config.accountAddress),
    multisigModuleAddress: normalizeEvmAddress(config.multisigModuleAddress),
  };
}

function evmDomain(name: string, config: EvmNetworkConfig): GuardianTypedData['domain'] {
  return {
    name,
    version: '1',
    chainId: config.chainId,
    verifyingContract: normalizeEvmAddress(config.accountAddress),
  };
}

function eip712DomainFields(): TypedDataField[] {
  return [
    { name: 'name', type: 'string' },
    { name: 'version', type: 'string' },
    { name: 'chainId', type: 'uint256' },
    { name: 'verifyingContract', type: 'address' },
  ];
}

function normalizeBytes32(value: string, field: string): Hex {
  const bytes = toBytes(value as Hex);
  if (bytes.length !== 32) {
    throw new Error(`${field} must be 32 bytes`);
  }
  return value.toLowerCase() as Hex;
}

function normalizeHex(value: string, field: string): Hex {
  if (!value.startsWith('0x')) {
    throw new Error(`${field} must be 0x-prefixed hex`);
  }
  const hex = value.slice(2);
  if (hex.length % 2 !== 0) {
    throw new Error(`${field} must contain an even number of hex characters`);
  }
  return value.toLowerCase() as Hex;
}
