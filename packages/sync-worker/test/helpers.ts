import { env, SELF } from 'cloudflare:test';

const enc = new TextEncoder();

function b64url(bytes: Uint8Array): string {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

export interface TokenOptions {
  sub?: string;
  app?: string;
  aud?: string;
  scp?: string;
  /** Offset from now in seconds. */
  iatOffset?: number;
  lifetime?: number;
  secret?: string;
  alg?: string;
}

export async function mint(o: TokenOptions = {}): Promise<string> {
  const nowS = Math.floor(Date.now() / 1000);
  const iat = nowS + (o.iatOffset ?? 0);
  const header = { alg: o.alg ?? 'HS256', typ: 'JWT' };
  const claims = {
    sub: o.sub ?? 'acct-1',
    app: o.app ?? 'heardright',
    aud: o.aud ?? 'rightkit-sync',
    scp: o.scp ?? 'sync',
    iat,
    exp: iat + (o.lifetime ?? 3600),
  };
  const input = `${b64url(enc.encode(JSON.stringify(header)))}.${b64url(enc.encode(JSON.stringify(claims)))}`;
  const key = await crypto.subtle.importKey('raw', enc.encode(o.secret ?? env.SYNC_JWT_SECRET), { name: 'HMAC', hash: 'SHA-256' }, false, [
    'sign',
  ]);
  const sig = new Uint8Array(await crypto.subtle.sign('HMAC', key, enc.encode(input)));
  return `${input}.${b64url(sig)}`;
}

export function b64(bytes: Uint8Array | string): string {
  const data = typeof bytes === 'string' ? enc.encode(bytes) : bytes;
  let s = '';
  for (const b of data) s += String.fromCharCode(b);
  return btoa(s);
}

export interface Api {
  token: string;
  call(method: string, path: string, body?: unknown, headers?: Record<string, string>): Promise<{ status: number; body: any; headers: Headers }>;
  prepare(zone?: string): Promise<void>;
  save(zone: string, policy: string, records: unknown[]): Promise<any>;
  changes(zone: string, since?: string | number, limit?: number): Promise<{ status: number; body: any }>;
}

export async function client(options: TokenOptions = {}): Promise<Api> {
  const token = await mint(options);
  const call: Api['call'] = async (method, path, body, headers = {}) => {
    const res = await SELF.fetch(`https://sync.test${path}`, {
      method,
      headers: { authorization: `Bearer ${token}`, ...(body !== undefined ? { 'content-type': 'application/json' } : {}), ...headers },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await res.text();
    return { status: res.status, body: text ? JSON.parse(text) : null, headers: res.headers };
  };
  return {
    token,
    call,
    async prepare(zone = 'transcripts') {
      const r = await call('POST', `/v1/sync/zones/${zone}`);
      if (r.status !== 200) throw new Error(`prepare failed: ${r.status}`);
    },
    async save(zone, policy, records) {
      const r = await call('POST', `/v1/sync/zones/${zone}/save`, { policy, records });
      if (r.status !== 200) throw new Error(`save failed: ${r.status} ${JSON.stringify(r.body)}`);
      return r.body;
    },
    async changes(zone, since, limit) {
      const q = new URLSearchParams();
      if (since !== undefined) q.set('since', String(since));
      if (limit !== undefined) q.set('limit', String(limit));
      const r = await call('GET', `/v1/sync/zones/${zone}/changes?${q}`);
      return { status: r.status, body: r.body };
    },
  };
}

export function rec(name: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return { name, type: 'Transcript', updated_at: 1_000, enc: b64(`cipher:${name}`), ...extra };
}
