export type Auth = { pubkey: string; signature: string };

export interface ConfigureRequest {
  account_id: string;
  auth: any;
  initial_state: any;
  storage_type: "Filesystem";
}

export interface ConfigureResponse {
  success: boolean;
  message: string;
  ack_pubkey?: string;
}

export interface StateObject {
  account_id: string;
  state_json: any;
  commitment: string;
  created_at: string;
  updated_at: string;
}

export type DeltaStatus =
  | { status: "candidate"; timestamp: string }
  | { status: "canonical"; timestamp: string }
  | { status: "discarded"; timestamp: string };

export interface DeltaObject {
  account_id: string;
  nonce: number;
  prev_commitment: string;
  new_commitment?: string;
  delta_payload: any;
  ack_sig?: string;
  status?: DeltaStatus;
}

export class WebClient {
  private baseURL: string;
  constructor(baseURL: string) {
    this.baseURL = baseURL.replace(/\/$/, "");
  }

  private headers(auth: Auth): HeadersInit {
    return {
      "content-type": "application/json",
      "x-pubkey": auth.pubkey,
      "x-signature": auth.signature,
    };
  }

  async configure(auth: Auth, req: ConfigureRequest): Promise<ConfigureResponse> {
    const res = await fetch(`${this.baseURL}/configure`, {
      method: "POST",
      headers: this.headers(auth),
      body: JSON.stringify(req),
    });
    const data = (await res.json()) as ConfigureResponse;
    if (!res.ok || !data.success) throw new Error(data.message);
    return data;
  }

  async pushDelta(auth: Auth, delta: DeltaObject): Promise<DeltaObject> {
    const res = await fetch(`${this.baseURL}/delta`, {
      method: "POST",
      headers: this.headers(auth),
      body: JSON.stringify(delta),
    });
    if (!res.ok) {
      const text = await res.text();
      throw new Error(text);
    }
    return (await res.json()) as DeltaObject;
  }

  async getDelta(auth: Auth, account_id: string, nonce: number): Promise<DeltaObject> {
    const url = new URL(`${this.baseURL}/delta`);
    url.searchParams.set("account_id", account_id);
    url.searchParams.set("nonce", String(nonce));
    const res = await fetch(url, { headers: this.headers(auth) });
    if (!res.ok) {
      const text = await res.text();
      throw new Error(text);
    }
    return (await res.json()) as DeltaObject;
  }

  async getDeltaSince(auth: Auth, account_id: string, from_nonce: number): Promise<DeltaObject> {
    const url = new URL(`${this.baseURL}/delta/since`);
    url.searchParams.set("account_id", account_id);
    url.searchParams.set("from_nonce", String(from_nonce));
    const res = await fetch(url, { headers: this.headers(auth) });
    if (!res.ok) {
      const text = await res.text();
      throw new Error(text);
    }
    return (await res.json()) as DeltaObject;
  }

  async getState(auth: Auth, account_id: string): Promise<StateObject> {
    const url = new URL(`${this.baseURL}/state`);
    url.searchParams.set("account_id", account_id);
    const res = await fetch(url, { headers: this.headers(auth) });
    if (!res.ok) {
      const text = await res.text();
      throw new Error(text);
    }
    return (await res.json()) as StateObject;
  }
}
