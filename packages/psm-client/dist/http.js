/**
 * HTTP client for PSM server communication.
 */
/**
 * Error thrown by the PSM HTTP client.
 */
export class PsmHttpError extends Error {
    status;
    statusText;
    body;
    constructor(status, statusText, body) {
        super(`PSM HTTP error ${status}: ${statusText} - ${body}`);
        this.status = status;
        this.statusText = statusText;
        this.body = body;
        this.name = 'PsmHttpError';
    }
}
/**
 * HTTP client for PSM server.
 */
export class PsmHttpClient {
    baseUrl;
    signer = null;
    constructor(baseUrl) {
        this.baseUrl = baseUrl;
    }
    /**
     * Set the signer for authenticated requests.
     */
    setSigner(signer) {
        this.signer = signer;
    }
    /**
     * Get the PSM server's public key.
     */
    async getPubkey() {
        const response = await this.fetch('/pubkey', { method: 'GET' });
        const data = (await response.json());
        return data.pubkey;
    }
    /**
     * Configure/register an account on PSM.
     */
    async configure(request) {
        const response = await this.fetchAuthenticated('/configure', {
            method: 'POST',
            body: JSON.stringify(request),
        }, request.account_id);
        return (await response.json());
    }
    /**
     * Get account state from PSM.
     */
    async getState(accountId) {
        const params = new URLSearchParams({ account_id: accountId });
        const response = await this.fetchAuthenticated(`/state?${params}`, {
            method: 'GET',
        }, accountId);
        return (await response.json());
    }
    /**
     * Get all delta proposals for an account.
     */
    async getDeltaProposals(accountId) {
        const params = new URLSearchParams({ account_id: accountId });
        const response = await this.fetchAuthenticated(`/delta/proposal?${params}`, {
            method: 'GET',
        }, accountId);
        const data = (await response.json());
        return data.proposals;
    }
    /**
     * Push a new delta proposal.
     */
    async pushDeltaProposal(request) {
        const response = await this.fetchAuthenticated('/delta/proposal', {
            method: 'POST',
            body: JSON.stringify(request),
        }, request.account_id);
        return (await response.json());
    }
    /**
     * Sign a delta proposal.
     */
    async signDeltaProposal(request) {
        const response = await this.fetchAuthenticated('/delta/proposal', {
            method: 'PUT',
            body: JSON.stringify(request),
        }, request.account_id);
        return (await response.json());
    }
    /**
     * Push a delta (execute a proposal).
     */
    async pushDelta(delta) {
        const response = await this.fetchAuthenticated('/delta', {
            method: 'POST',
            body: JSON.stringify(delta),
        }, delta.account_id);
        return (await response.json());
    }
    /**
     * Get a specific delta by nonce.
     */
    async getDelta(accountId, nonce) {
        const params = new URLSearchParams({
            account_id: accountId,
            nonce: nonce.toString(),
        });
        const response = await this.fetchAuthenticated(`/delta?${params}`, {
            method: 'GET',
        }, accountId);
        return (await response.json());
    }
    /**
     * Get merged delta since a nonce.
     */
    async getDeltaSince(accountId, fromNonce) {
        const params = new URLSearchParams({
            account_id: accountId,
            nonce: fromNonce.toString(),
        });
        const response = await this.fetchAuthenticated(`/delta/since?${params}`, {
            method: 'GET',
        }, accountId);
        return (await response.json());
    }
    // ==========================================================================
    // Private helpers
    // ==========================================================================
    async fetch(path, init) {
        const url = `${this.baseUrl}${path}`;
        const response = await fetch(url, {
            ...init,
            headers: {
                'Content-Type': 'application/json',
                ...init.headers,
            },
        });
        if (!response.ok) {
            const body = await response.text();
            throw new PsmHttpError(response.status, response.statusText, body);
        }
        return response;
    }
    async fetchAuthenticated(path, init, accountId) {
        if (!this.signer) {
            throw new Error('No signer configured. Call setSigner() first.');
        }
        const signature = this.signer.signAccountId(accountId);
        return this.fetch(path, {
            ...init,
            headers: {
                ...init.headers,
                'x-pubkey': this.signer.publicKey,
                'x-signature': signature,
            },
        });
    }
}
//# sourceMappingURL=http.js.map