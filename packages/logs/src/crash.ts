import type { LogsInvoke } from './index.js';

/** Tauri command the app registers; it deserializes a `WindowErrorReport` and calls
 * `rightkit_crash::record_window_error`. */
export const RIGHTKIT_CRASH_WINDOW_ERROR_COMMAND = 'rightkit_crash_window_error';

export interface WindowCrashPayload {
  kind: 'error' | 'unhandledrejection';
  message: string;
  stack: string;
  /** File name and position only: no origin, query or fragment. */
  source: string | null;
}

/** The slice of `window` this module needs. Injected so tests and non-DOM hosts work. */
export interface WindowCrashTarget {
  addEventListener(type: 'error' | 'unhandledrejection', listener: (event: any) => void): void;
  removeEventListener(type: 'error' | 'unhandledrejection', listener: (event: any) => void): void;
}

export interface WindowCrashConfig {
  invoke: LogsInvoke;
  /** Default: `globalThis`. */
  target?: WindowCrashTarget;
  /** Reports allowed per `windowMs`. Default 5. */
  maxPerWindow?: number;
  /** Rate window. Default 60000. */
  windowMs?: number;
  /** Reports allowed for the page lifetime. Default 25. */
  maxTotal?: number;
  maxMessageChars?: number;
  maxStackChars?: number;
  /** Known secrets of the page (tokens, keys) removed before sending. */
  redactValues?: readonly string[];
  now?: () => number;
}

// Markers match the Rust scrubber (`rightkit_logs::scrub_text` /
// `redact_url_text`) so JS and Rust outputs agree on the shared parity vectors.
const SCRUBBERS: Array<[RegExp, string]> = [
  [/(\b(?:Bearer|Basic)\s+)[A-Za-z0-9._~+/=-]{8,}/gi, '$1[redacted:credential]'],
  [/\b(?:sk|pk|rk|ghp|gho|xox[abp])[-_][A-Za-z0-9_-]{12,}/g, '[redacted:credential]'],
  [/\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]*/g, '[redacted:credential]'],
  [/[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/g, '[redacted:email]'],
  [/([?&#](?:token|key|api_key|apikey|secret|password|code|auth|sig|access_token|refresh_token|id_token|client_secret|state)=)[^&\s)'"]*/gi, '$1[redacted]'],
  [/\b[0-9a-f]{32,}\b/gi, '[redacted:credential]'],
  [/(\/(?:Users|home)\/)[^\\/\s:)]+/g, '$1<user>'],
  [/([A-Za-z]:\\Users\\)[^\\/\s:)]+/g, '$1<user>'],
];

function redactCrashUrls(text: string): string {
  return text.replace(
    /([A-Za-z][A-Za-z0-9+.-]*:\/\/)([^\s"'<>`)\]},]+)/g,
    (_match, scheme: string, token: string) => {
      const beforeFragment = token.split('#')[0] ?? token;
      const queryAt = beforeFragment.indexOf('?');
      const head = queryAt >= 0 ? beforeFragment.slice(0, queryAt) : beforeFragment;
      const slashAt = head.indexOf('/');
      const authority = slashAt >= 0 ? head.slice(0, slashAt) : head;
      const path = slashAt >= 0 ? head.slice(slashAt) : '';
      const hostPort = authority.split('@').at(-1) ?? authority;
      return `${scheme}${hostPort}${path}${queryAt >= 0 ? '?[redacted]' : ''}`;
    },
  );
}

/** Removes URL query/fragment, known secrets and secret-shaped text. Pure. */
export function scrubWindowCrashText(text: string, redactValues: readonly string[] = []): string {
  let out = String(text ?? '');
  for (const secret of [...redactValues].filter(Boolean).sort((a, b) => b.length - a.length)) {
    out = out.split(secret).join('[redacted]');
  }
  out = redactCrashUrls(out);
  for (const [pattern, replacement] of SCRUBBERS) out = out.replace(pattern, replacement);
  return out;
}

function sourceName(raw: unknown, line: unknown, col: unknown, redact: readonly string[]): string | null {
  if (typeof raw !== 'string' || raw === '') return null;
  if (raw.length > 65536) return '[oversized source]';
  const clean = scrubWindowCrashText(raw, redact).split(/[?#]/)[0] ?? '';
  const file = clean.slice(clean.lastIndexOf('/') + 1) || clean;
  const pos = typeof line === 'number' ? `:${line}${typeof col === 'number' ? `:${col}` : ''}` : '';
  return `${file}${pos}`;
}

function read(value: unknown, key: string): unknown {
  try { return value == null ? undefined : (value as Record<string, unknown>)[key]; }
  catch { return undefined; }
}

function own(value: object, key: string): unknown {
  try {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    return descriptor && 'value' in descriptor ? descriptor.value : undefined;
  } catch { return undefined; }
}

/** Admit intact strings before scrubbing; never expose a truncated raw secret prefix. */
function describe(reason: unknown, redact: readonly string[]): { message: string; stack: string } {
  let remainingChars = 65536;
  let remainingNodes = 32;
  const seen = new WeakSet<object>();
  function text(value: string): string {
    if (value.length > remainingChars) return '[oversized value]';
    remainingChars -= value.length;
    // Scrub before JSON escaping as well as before final output truncation.
    return scrubWindowCrashText(value, redact);
  }
  function format(value: unknown, depth: number): string {
    if (remainingNodes-- <= 0) return '"[node limit]"';
    if (typeof value === 'string') return JSON.stringify(text(value));
    if (value === null) return 'null';
    if (typeof value === 'number' || typeof value === 'boolean' || value === undefined) return String(value);
    if (typeof value === 'bigint') {
      return value >= -9007199254740991n && value <= 9007199254740991n ? `${value}n` : '"[large bigint]"';
    }
    if (typeof value !== 'object') return typeof value === 'symbol' ? '"[symbol]"' : '"[function]"';
    if (seen.has(value)) return '"[circular]"';
    if (depth >= 3) return '"[depth limit]"';
    seen.add(value);
    const fields: string[] = [];
    let examined = 0;
    try {
      // Stop enumeration at width bound; never invoke accessors, toJSON, or toString.
      for (const key in value) {
        if (examined++ >= 8) { fields.push('"[width limit]":true'); break; }
        const descriptor = Object.getOwnPropertyDescriptor(value, key);
        if (!descriptor) continue;
        if (remainingNodes <= 0) { fields.push('"[node limit]":true'); break; }
        const label = JSON.stringify(text(key));
        fields.push(`${label}:${'value' in descriptor ? format(descriptor.value, depth + 1) : '"[accessor]"'}`);
      }
    } catch { fields.push('"[unavailable]":true'); }
    return `{${fields.join(',')}}`;
  }
  try {
    if (typeof reason === 'string') return { message: text(reason), stack: '' };
    let error = false;
    try { error = reason instanceof Error; } catch { /* revoked proxy */ }
    if (error) {
      const name = read(reason, 'name');
      const message = read(reason, 'message');
      const stack = read(reason, 'stack');
      return {
        message: `${typeof name === 'string' ? text(name) : 'Error'}: ${typeof message === 'string' ? text(message) : '[unavailable]'}`,
        stack: typeof stack === 'string' ? text(stack) : '',
      };
    }
    if (reason && typeof reason === 'object') {
      const message = own(reason, 'message');
      if (typeof message === 'string') {
        const code = own(reason, 'code');
        const stack = own(reason, 'stack');
        return {
          message: `${typeof code === 'string' ? `${text(code)}: ` : ''}${text(message)}`,
          stack: typeof stack === 'string' ? text(stack) : '',
        };
      }
    }
    return { message: format(reason, 0), stack: '' };
  } catch { return { message: '[unavailable reason]', stack: '' }; }
}

/**
 * Opt-in: capture `window` `error` and `unhandledrejection`, scrub, rate-limit and send to the
 * app's crash command. Nothing is installed until this is called. Returns an uninstall function.
 * Failures to send are swallowed: crash capture must never raise a new error.
 */
export function installWindowCrashCapture(config: WindowCrashConfig): () => void {
  const target = config.target ?? (globalThis as unknown as WindowCrashTarget);
  const maxPerWindow = Math.max(1, config.maxPerWindow ?? 5);
  const windowMs = Math.max(1, config.windowMs ?? 60_000);
  const maxTotal = Math.max(1, config.maxTotal ?? 25);
  const maxMessage = Math.max(1, config.maxMessageChars ?? 1_000);
  const maxStack = Math.max(1, config.maxStackChars ?? 8_000);
  const now = config.now ?? Date.now;
  const redact = config.redactValues ?? [];
  let windowStart = -Infinity;
  let inWindow = 0;
  let total = 0;

  function allow(): boolean {
    if (total >= maxTotal) return false;
    const t = now();
    if (t - windowStart >= windowMs) {
      windowStart = t;
      inWindow = 0;
    }
    if (inWindow >= maxPerWindow) return false;
    inWindow += 1;
    total += 1;
    return true;
  }

  function send(kind: WindowCrashPayload['kind'], message: string, stack: string, source: string | null): void {
    try {
      if (!allow()) return;
      const payload: WindowCrashPayload = {
        kind,
        message: scrubWindowCrashText(message, redact).slice(0, maxMessage),
        stack: scrubWindowCrashText(stack, redact).slice(0, maxStack),
        source: source === null ? null : scrubWindowCrashText(source, redact).slice(0, 120),
      };
      void Promise.resolve(config.invoke(RIGHTKIT_CRASH_WINDOW_ERROR_COMMAND, { report: payload })).catch(() => undefined);
    } catch {
      // never raise from the crash path
    }
  }

  const onError = (event: any): void => {
    try {
      const err = read(event, 'error');
      const d = describe(err !== undefined && err !== null ? err : read(event, 'message') ?? 'error', redact);
      send('error', d.message, d.stack, sourceName(read(event, 'filename'), read(event, 'lineno'), read(event, 'colno'), redact));
    } catch { /* never raise from malformed event or source data */ }
  };
  const onRejection = (event: any): void => {
    const d = describe(read(event, 'reason'), redact);
    send('unhandledrejection', d.message, d.stack, null);
  };

  target.addEventListener('error', onError);
  target.addEventListener('unhandledrejection', onRejection);
  return () => {
    target.removeEventListener('error', onError);
    target.removeEventListener('unhandledrejection', onRejection);
  };
}
