/**
 * MASM file loading utilities.
 *
 * Provides functions to load Miden Assembly files for account components.
 */

// Default base URL for MASM files (can be overridden)
let masmBaseUrl = '/masm';

/**
 * Sets the base URL for loading MASM files.
 * Useful when MASM files are hosted at a different location.
 *
 * @param baseUrl - The base URL (e.g., '/masm' or 'https://cdn.example.com/masm')
 */
export function setMasmBaseUrl(baseUrl: string): void {
  masmBaseUrl = baseUrl;
}

/**
 * Gets the current MASM base URL.
 */
export function getMasmBaseUrl(): string {
  return masmBaseUrl;
}

/**
 * Fetches MASM code from a file.
 *
 * @param filename - The filename to load (e.g., 'multisig.masm')
 * @returns The MASM source code
 */
export async function loadMasmFile(filename: string): Promise<string> {
  const url = `${masmBaseUrl}/${filename}`;
  const response = await fetch(url);

  if (!response.ok) {
    throw new Error(`Failed to load MASM file ${filename}: ${response.statusText}`);
  }

  return response.text();
}

/**
 * Loads the multisig authentication component MASM.
 */
export async function loadMultisigMasm(): Promise<string> {
  return loadMasmFile('multisig.masm');
}

/**
 * Loads the PSM component MASM.
 */
export async function loadPsmMasm(): Promise<string> {
  return loadMasmFile('psm.masm');
}

// =============================================================================
// Embedded MASM (for bundling)
// =============================================================================

// These can be used when MASM files are bundled with the SDK
// rather than fetched at runtime.

let embeddedMultisigMasm: string | null = null;
let embeddedPsmMasm: string | null = null;

/**
 * Sets the embedded multisig MASM code.
 * Use this to bundle MASM with the SDK instead of fetching.
 */
export function setEmbeddedMultisigMasm(masm: string): void {
  embeddedMultisigMasm = masm;
}

/**
 * Sets the embedded PSM MASM code.
 * Use this to bundle MASM with the SDK instead of fetching.
 */
export function setEmbeddedPsmMasm(masm: string): void {
  embeddedPsmMasm = masm;
}

/**
 * Gets the multisig MASM code, preferring embedded if available.
 */
export async function getMultisigMasm(): Promise<string> {
  if (embeddedMultisigMasm) {
    return embeddedMultisigMasm;
  }
  return loadMultisigMasm();
}

/**
 * Gets the PSM MASM code, preferring embedded if available.
 */
export async function getPsmMasm(): Promise<string> {
  if (embeddedPsmMasm) {
    return embeddedPsmMasm;
  }
  return loadPsmMasm();
}
