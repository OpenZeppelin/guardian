import { connect, type ClientHttp2Session } from 'node:http2';

/**
 * Routes gRPC-web calls over HTTP/2.
 *
 * The Miden RPC and prover endpoints sit behind an AWS ALB whose gRPC target
 * group only accepts HTTP/2. Node's built-in fetch is HTTP/1.1 only, so the
 * ALB answers 464 with no headers at all, which the SDK's gRPC-web client
 * reports as "missing content-type header in gRPC response". Browsers never
 * hit this because they negotiate h2 through ALPN.
 */

const sessions = new Map<string, ClientHttp2Session>();

function sessionFor(origin: string): ClientHttp2Session {
  const existing = sessions.get(origin);
  if (existing && !existing.closed && !existing.destroyed) {
    return existing;
  }
  const session = connect(origin);
  session.on('close', () => sessions.delete(origin));
  session.on('error', () => sessions.delete(origin));
  sessions.set(origin, session);
  return session;
}

export function closeH2Sessions(): void {
  for (const session of sessions.values()) {
    session.close();
  }
  sessions.clear();
}

async function h2Request(url: URL, init: RequestInit, body: Uint8Array): Promise<Response> {
  const session = sessionFor(url.origin);
  const headers: Record<string, string> = {};
  new Headers(init.headers).forEach((value, key) => {
    headers[key] = value;
  });

  return new Promise<Response>((resolve, reject) => {
    const request = session.request({
      ':method': init.method ?? 'POST',
      ':path': `${url.pathname}${url.search}`,
      ...headers,
    });
    request.setTimeout(120_000, () => request.destroy(new Error('gRPC request timed out')));

    const chunks: Buffer[] = [];
    let status = 0;
    const responseHeaders = new Headers();

    request.on('response', (incoming) => {
      for (const [key, value] of Object.entries(incoming)) {
        if (key.startsWith(':') || value === undefined) continue;
        responseHeaders.set(key, Array.isArray(value) ? value.join(', ') : String(value));
      }
      status = Number(incoming[':status'] ?? 0);
    });
    request.on('data', (chunk: Buffer) => chunks.push(chunk));
    request.on('end', () => {
      resolve(
        new Response(new Uint8Array(Buffer.concat(chunks)), { status, headers: responseHeaders }),
      );
    });
    request.on('error', reject);

    if (body.byteLength > 0) request.write(body);
    request.end();
  });
}

export function installH2GrpcFetch(): void {
  const nativeFetch = globalThis.fetch.bind(globalThis);

  globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const request = new Request(input as RequestInfo, init);
    const contentType = request.headers.get('content-type') ?? '';
    const url = new URL(request.url);

    if (!contentType.startsWith('application/grpc-web') || url.protocol !== 'https:') {
      return nativeFetch(input as RequestInfo, init);
    }

    const body = new Uint8Array(await request.arrayBuffer());
    const headers: Record<string, string> = {};
    request.headers.forEach((value, key) => {
      headers[key] = value;
    });
    return h2Request(url, { method: request.method, headers }, body);
  };
}
