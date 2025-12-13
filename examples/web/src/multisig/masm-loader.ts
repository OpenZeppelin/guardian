/**
 * Loads MASM files from the public directory.
 */

const MASM_BASE_URL = '/masm';

/**
 * Fetches MASM code from a file in the public/masm directory.
 */
export async function loadMasmFile(filename: string): Promise<string> {
  const url = `${MASM_BASE_URL}/${filename}`;
  const response = await fetch(url);

  if (!response.ok) {
    throw new Error(`Failed to load MASM file ${filename}: ${response.statusText}`);
  }

  return response.text();
}

export async function loadMultisigMasm(): Promise<string> {
  return loadMasmFile('multisig.masm');
}

export async function loadPsmMasm(): Promise<string> {
  return loadMasmFile('psm.masm');
}
