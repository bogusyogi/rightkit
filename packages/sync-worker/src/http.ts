import { MAX_BODY_BYTES } from './env.js';

export class HttpError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly retryAfterSeconds?: number,
  ) {
    super(message);
  }
}

const BASE_HEADERS = {
  'content-type': 'application/json; charset=utf-8',
  'cache-control': 'no-store',
};

export function json(body: unknown, status = 200, extra: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), { status, headers: { ...BASE_HEADERS, ...extra } });
}

export function errorResponse(e: HttpError): Response {
  const extra: Record<string, string> = {};
  if (e.retryAfterSeconds !== undefined) extra['retry-after'] = String(e.retryAfterSeconds);
  if (e.status === 401) extra['www-authenticate'] = 'Bearer';
  return json({ error: { code: e.code, message: e.message } }, e.status, extra);
}

export async function readJson(req: Request): Promise<unknown> {
  const declared = Number(req.headers.get('content-length') ?? '0');
  if (declared > MAX_BODY_BYTES) throw new HttpError(413, 'body_too_large', 'request body too large');
  const text = await req.text();
  if (text.length > MAX_BODY_BYTES) throw new HttpError(413, 'body_too_large', 'request body too large');
  try {
    return JSON.parse(text);
  } catch {
    throw new HttpError(400, 'bad_json', 'request body is not valid JSON');
  }
}

export function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

/** Content-free structured lifecycle event. Never pass names, ids of records or ciphertext. */
export function event(name: string, fields: Record<string, string | number | boolean>): void {
  console.log(JSON.stringify({ event: name, ...fields }));
}
