/**
 * HTTP client for PSM server communication.
 */
import type { ConfigureRequest, ConfigureResponse, DeltaObject, DeltaProposalRequest, DeltaProposalResponse, SignProposalRequest, Signer, StateObject } from './types.js';
/**
 * Error thrown by the PSM HTTP client.
 */
export declare class PsmHttpError extends Error {
    readonly status: number;
    readonly statusText: string;
    readonly body: string;
    constructor(status: number, statusText: string, body: string);
}
/**
 * HTTP client for PSM server.
 */
export declare class PsmHttpClient {
    private readonly baseUrl;
    private signer;
    constructor(baseUrl: string);
    /**
     * Set the signer for authenticated requests.
     */
    setSigner(signer: Signer): void;
    /**
     * Get the PSM server's public key.
     */
    getPubkey(): Promise<string>;
    /**
     * Configure/register an account on PSM.
     */
    configure(request: ConfigureRequest): Promise<ConfigureResponse>;
    /**
     * Get account state from PSM.
     */
    getState(accountId: string): Promise<StateObject>;
    /**
     * Get all delta proposals for an account.
     */
    getDeltaProposals(accountId: string): Promise<DeltaObject[]>;
    /**
     * Push a new delta proposal.
     */
    pushDeltaProposal(request: DeltaProposalRequest): Promise<DeltaProposalResponse>;
    /**
     * Sign a delta proposal.
     */
    signDeltaProposal(request: SignProposalRequest): Promise<DeltaObject>;
    /**
     * Push a delta (execute a proposal).
     */
    pushDelta(delta: DeltaObject): Promise<DeltaObject>;
    /**
     * Get a specific delta by nonce.
     */
    getDelta(accountId: string, nonce: number): Promise<DeltaObject>;
    /**
     * Get merged delta since a nonce.
     */
    getDeltaSince(accountId: string, fromNonce: number): Promise<DeltaObject>;
    private fetch;
    private fetchAuthenticated;
}
//# sourceMappingURL=http.d.ts.map