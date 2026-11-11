import type { Invoke } from './client.js';

/**
 * A `fetch`-compatible function over the native fetch bridge (`FetchBridge` in `rightkit-app-update`, #448).
 * The native host compiles the allowlist (exact https hosts, optional path prefixes); the webview carries no
 * policy, so it needs no `connect-src` for those hosts and no `tauri-plugin-http`. Redirects, address vetting,
 * size limits and the forbidden headers (`cookie`, `host`, `content-length`, ...) are native.
 */

/** The command the app registers for the native fetch bridge. */
export const RIGHTKIT_FETCH_COMMAND = 'rightkit_app_update_fetch';

export type NativeFetchErrorCode = 'refused' | 'invalid' | 'timeout' | 'tooLarge' | 'network' | 'unknown';

const fetchCodes: readonly NativeFetchErrorCode[] = ['refused', 'invalid', 'timeout', 'tooLarge', 'network', 'unknown'];

/** Fixed text for each native code (the same words as `FetchError` in `rightkit-app-update`). */
export const NATIVE_FETCH_MESSAGES = Object.freeze({
  refused: 'The request is not allowed for this app.',
  invalid: 'The request is malformed.',
  timeout: 'The request timed out.',
  tooLarge: 'The request or response is too large.',
  network: 'The server could not be reached.',
  unknown: 'The request failed.',
} satisfies Record<NativeFetchErrorCode, string>);

/** A native fetch failure. A `TypeError`, as `fetch` rejects with; the text is fixed and names no URL or header. */
export class NativeFetchError extends TypeError {
  readonly code: NativeFetchErrorCode;
  constructor(code: NativeFetchErrorCode = 'unknown') {
    super(NATIVE_FETCH_MESSAGES[code]);
    this.name = 'NativeFetchError';
    this.code = code;
  }
}

interface NativeFetchRequest {
  url: string;
  method: string;
  headers: [string, string][];
  bodyBase64?: string;
  timeoutMs?: number;
}

interface NativeFetchResponse {
  status: number;
  headers: [string, string][];
  bodyBase64: string;
  url: string;
}

export interface NativeFetchOptions {
  invoke: Invoke;
  /** The command name; default `RIGHTKIT_FETCH_COMMAND`. */
  command?: string;
  /** Per-request timeout sent to the host (the host clamps it to its policy). */
  timeoutMs?: number;
}

function toBase64(bytes: Uint8Array): string {
  let binary = '';
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(binary);
}

function fromBase64(text: string): Uint8Array<ArrayBuffer> {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

function nativeError(value: unknown): NativeFetchError {
  const code = value !== null && typeof value === 'object' ? (value as { code?: unknown }).code : undefined;
  return new NativeFetchError(typeof code === 'string' && (fetchCodes as readonly string[]).includes(code) ? (code as NativeFetchErrorCode) : 'unknown');
}

function pairs(value: unknown): [string, string][] {
  if (!Array.isArray(value)) throw new NativeFetchError();
  return value.map((entry) => {
    if (!Array.isArray(entry) || entry.length !== 2 || typeof entry[0] !== 'string' || typeof entry[1] !== 'string') throw new NativeFetchError();
    return [entry[0], entry[1]] as [string, string];
  });
}

function response(value: unknown): { status: number; headers: [string, string][]; body: Uint8Array<ArrayBuffer>; url: string } {
  if (value === null || typeof value !== 'object') throw new NativeFetchError();
  const v = value as Partial<NativeFetchResponse>;
  if (typeof v.status !== 'number' || !Number.isInteger(v.status) || v.status < 200 || v.status > 599) throw new NativeFetchError();
  if (typeof v.bodyBase64 !== 'string' || typeof v.url !== 'string') throw new NativeFetchError();
  try {
    return { status: v.status, headers: pairs(v.headers), body: fromBase64(v.bodyBase64), url: v.url };
  } catch { throw new NativeFetchError(); }
}

const noBody = (status: number): boolean => status === 204 || status === 205 || status === 304;

/**
 * Build a `fetch` that runs through the native bridge. It takes what `fetch` takes (a string, a `URL` or a
 * `Request`, plus `init`) and answers a real `Response` (`url` is the final URL after native redirects). `signal`
 * is honoured before and after the native call (the native request itself is not cancelled). Only the `https`
 * URLs the native policy lists are reachable; anything else rejects with `NativeFetchError` code `refused`.
 */
export function createNativeFetch(options: NativeFetchOptions): typeof fetch {
  const command = options.command ?? RIGHTKIT_FETCH_COMMAND;
  return async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const request = new Request(input, init);
    if (request.signal.aborted) throw request.signal.reason ?? new DOMException('The operation was aborted.', 'AbortError');
    const body = request.body === null ? null : new Uint8Array(await request.arrayBuffer());
    const headers: [string, string][] = [];
    request.headers.forEach((value, name) => headers.push([name, value]));
    const payload: NativeFetchRequest = { url: request.url, method: request.method, headers };
    if (body !== null && body.length > 0) payload.bodyBase64 = toBase64(body);
    if (options.timeoutMs !== undefined) payload.timeoutMs = options.timeoutMs;
    let raw: unknown;
    try { raw = await options.invoke<unknown>(command, { request: payload }); } catch (error) { throw nativeError(error); }
    if (request.signal.aborted) throw request.signal.reason ?? new DOMException('The operation was aborted.', 'AbortError');
    const answer = response(raw);
    const result = new Response(noBody(answer.status) ? null : answer.body, { status: answer.status, headers: answer.headers });
    Object.defineProperty(result, 'url', { value: answer.url });
    return result;
  };
}
