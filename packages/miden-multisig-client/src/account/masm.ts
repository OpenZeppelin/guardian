export class MasmLoader {
  private baseUrl: string;
  private embeddedMultisigMasm: string | null = null;
  private embeddedPsmMasm: string | null = null;

  constructor(baseUrl = '/masm') {
    this.baseUrl = baseUrl;
  }

  setBaseUrl(baseUrl: string): void {
    this.baseUrl = baseUrl;
  }

  getBaseUrl(): string {
    return this.baseUrl;
  }

  setEmbeddedMultisig(masm: string): void {
    this.embeddedMultisigMasm = masm;
  }

  setEmbeddedPsm(masm: string): void {
    this.embeddedPsmMasm = masm;
  }

  async load(filename: string): Promise<string> {
    const url = `${this.baseUrl}/${filename}`;
    const response = await fetch(url);
    if (!response.ok) {
      throw new Error(`Failed to load MASM file ${filename}: ${response.statusText}`);
    }
    return response.text();
  }

  async loadMultisig(): Promise<string> {
    return this.load('multisig.masm');
  }

  async loadPsm(): Promise<string> {
    return this.load('psm.masm');
  }

  async getMultisigMasm(): Promise<string> {
    if (this.embeddedMultisigMasm) {
      return this.embeddedMultisigMasm;
    }
    return this.loadMultisig();
  }

  async getPsmMasm(): Promise<string> {
    if (this.embeddedPsmMasm) {
      return this.embeddedPsmMasm;
    }
    return this.loadPsm();
  }
}

const defaultMasmLoader = new MasmLoader();

export function setMasmBaseUrl(baseUrl: string): void {
  defaultMasmLoader.setBaseUrl(baseUrl);
}

export function getMasmBaseUrl(): string {
  return defaultMasmLoader.getBaseUrl();
}

export async function loadMasmFile(filename: string): Promise<string> {
  return defaultMasmLoader.load(filename);
}

export async function loadMultisigMasm(): Promise<string> {
  return defaultMasmLoader.loadMultisig();
}

export async function loadPsmMasm(): Promise<string> {
  return defaultMasmLoader.loadPsm();
}

export function setEmbeddedMultisigMasm(masm: string): void {
  defaultMasmLoader.setEmbeddedMultisig(masm);
}

export function setEmbeddedPsmMasm(masm: string): void {
  defaultMasmLoader.setEmbeddedPsm(masm);
}

export async function getMultisigMasm(): Promise<string> {
  return defaultMasmLoader.getMultisigMasm();
}

export async function getPsmMasm(): Promise<string> {
  return defaultMasmLoader.getPsmMasm();
}

export const masmLoader = defaultMasmLoader;
