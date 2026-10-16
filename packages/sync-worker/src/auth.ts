import { decodeBase64Url, toArrayBuffer } from './base64.js';
import { config, MAX_TOKEN_LIFETIME_S, type Env } from './env.js';
import { HttpError, isObject } from './http.js';

export const AUDIENCE = 'rightkit-sync';
export const SCOPE = 'sync';

export interface Principal {
  app: string;
  accountId: string;
  /** Token issue time, unix seconds. */
  iat: number;
}

const ACCOUNT_ID = /^[A-Za-z0-9._:@-]{1,128}$/;
const encoder = new TextEncoder();

async function verifyWith(secret: string, signingInput: string, signature: Uint8Array): Promise<boolean> {
  const key = await crypto.subtle.importKey('raw', encoder.encode(secret), { name: 'HMAC', hash: 'SHA-256' }, false, [
    'verify',
  ]);
  return crypto.subtle.verify('HMAC', key, toArrayBuffer(signature), encoder.encode(signingInput));
}

function unauthorized(code: string, message: string): HttpError {
  return new HttpError(401, code, message);
}

/**
 * Verify the HS256 sync token minted by the account backend (issue #7 of the design).
 * Claims: sub = account id, app, aud = "rightkit-sync", scp = "sync", iat, exp.
 * `app` and `account_id` come only from the token, so a caller can never address another tenant.
 */
export async function authenticate(req: Request, env: Env, nowMs: number): Promise<Principal> {
  const header = req.headers.get('authorization') ?? '';
  const match = /^Bearer ([A-Za-z0-9._-]+)$/.exec(header);
  if (!match) throw unauthorized('missing_token', 'bearer token required');
  const parts = match[1]!.split('.');
  if (parts.length !== 3) throw unauthorized('invalid_token', 'malformed token');
  const [h, p, s] = parts as [string, string, string];

  const headerBytes = decodeBase64Url(h);
  const payloadBytes = decodeBase64Url(p);
  const signature = decodeBase64Url(s);
  if (!headerBytes || !payloadBytes || !signature) throw unauthorized('invalid_token', 'malformed token');

  let head: unknown;
  let claims: unknown;
  try {
    head = JSON.parse(new TextDecoder().decode(headerBytes));
    claims = JSON.parse(new TextDecoder().decode(payloadBytes));
  } catch {
    throw unauthorized('invalid_token', 'malformed token');
  }
  if (!isObject(head) || head.alg !== 'HS256') throw unauthorized('invalid_token', 'unsupported algorithm');
  if (!isObject(claims)) throw unauthorized('invalid_token', 'malformed token');

  const signingInput = `${h}.${p}`;
  const secrets = [env.SYNC_JWT_SECRET, env.SYNC_JWT_SECRET_PREVIOUS].filter(
    (v): v is string => typeof v === 'string' && v.length > 0,
  );
  let valid = false;
  for (const secret of secrets) {
    if (await verifyWith(secret, signingInput, signature)) {
      valid = true;
      break;
    }
  }
  if (!valid) throw unauthorized('invalid_token', 'bad signature');

  const { sub, app, aud, scp, iat, exp } = claims as Record<string, unknown>;
  if (aud !== AUDIENCE || scp !== SCOPE) throw unauthorized('invalid_token', 'wrong audience or scope');
  if (typeof iat !== 'number' || typeof exp !== 'number' || !Number.isFinite(iat) || !Number.isFinite(exp)) {
    throw unauthorized('invalid_token', 'missing iat or exp');
  }
  const nowS = Math.floor(nowMs / 1000);
  if (exp <= nowS) throw unauthorized('session_expired', 'sync session expired');
  if (iat > nowS + 60) throw unauthorized('invalid_token', 'token issued in the future');
  if (exp - iat > MAX_TOKEN_LIFETIME_S) throw unauthorized('invalid_token', 'token lifetime too long');
  if (typeof sub !== 'string' || !ACCOUNT_ID.test(sub)) throw unauthorized('invalid_token', 'bad subject');
  if (typeof app !== 'string' || !app) throw unauthorized('invalid_token', 'bad app');
  if (!config(env).allowedApps.has(app)) throw new HttpError(403, 'app_not_allowed', 'app is not enabled on this worker');
  return { app, accountId: sub, iat };
}
