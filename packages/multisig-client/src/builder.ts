/**
 * Builder pattern for constructing MultisigClient instances.
 *
 * This mirrors the Rust MultisigClientBuilder pattern, providing a fluent API
 * for client construction.
 */

import { MultisigClient, type MultisigClientConfig } from './client.js';
import type { Signer } from './types.js';
import type { KeyEntry } from './keystore.js';
import { createSigner } from './signer.js';

// =============================================================================
// Builder
// =============================================================================

/**
 * Builder for constructing MultisigClient instances.
 *
 * Provides a fluent API for configuring and creating clients.
 *
 * @example
 * ```typescript
 * const client = await MultisigClientBuilder.create()
 *   .psmEndpoint('http://localhost:8080')
 *   .withKeyEntry(keyEntry)
 *   .build();
 * ```
 */
export class MultisigClientBuilder {
  private _psmEndpoint: string | null = null;
  private _signer: Signer | null = null;

  /**
   * Creates a new builder instance.
   */
  static create(): MultisigClientBuilder {
    return new MultisigClientBuilder();
  }

  /**
   * Sets the PSM server endpoint.
   *
   * @param endpoint - The PSM server URL (e.g., 'http://localhost:8080')
   */
  psmEndpoint(endpoint: string): this {
    this._psmEndpoint = endpoint;
    return this;
  }

  /**
   * Sets the signer directly.
   *
   * @param signer - A pre-configured Signer instance
   */
  withSigner(signer: Signer): this {
    this._signer = signer;
    return this;
  }

  /**
   * Creates a signer from a key entry.
   *
   * @param keyEntry - The key entry from the keystore
   */
  withKeyEntry(keyEntry: KeyEntry): this {
    this._signer = createSigner(keyEntry);
    return this;
  }

  /**
   * Builds and returns the MultisigClient.
   *
   * @throws Error if required configuration is missing
   */
  build(): MultisigClient {
    if (!this._psmEndpoint) {
      throw new Error('PSM endpoint is required. Call psmEndpoint() first.');
    }
    if (!this._signer) {
      throw new Error('Signer is required. Call withSigner() or withKeyEntry() first.');
    }

    const config: MultisigClientConfig = {
      psmEndpoint: this._psmEndpoint,
      signer: this._signer,
    };

    return new MultisigClient(config);
  }

  /**
   * Builds the client asynchronously.
   *
   * Currently just wraps build(), but allows for future async initialization.
   */
  async buildAsync(): Promise<MultisigClient> {
    return this.build();
  }
}
