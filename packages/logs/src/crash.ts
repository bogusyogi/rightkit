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

function sourceName(raw: unknown, line: unknown, col: unknown): string | null {
  if (typeof raw !== 'string' || raw === '') return null;
  const clean = raw.split(/[?#]/)[0] ?? '';
  const file = clean.slice(clean.lastIndexOf('/') + 1) || clean;
  const pos = typeof line === 'number' ? `:${line}${typeof col === 'number' ? `:${col}` : ''}` : '';
  return `${file}${pos}`.slice(0, 120);
}

function describe(reason: unknown): { message: string; stack: string } {
  if (reason instanceof Error) return { message: `${reason.name}: ${reason.message}`, stack: reason.stack ?? '' };
  if (typeof reason === 'string') return { message: reason, stack: '' };
  const obj = reason as { message?: unknown; stack?: unknown } | null;
  if (obj && typeof obj.message === 'string') {
    return { message: obj.message, stack: typeof obj.stack === 'string' ? obj.stack : '' };
  }
  return { message: 'non-error rejection', stack: '' };
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
    if (!allow()) return;
    const payload: WindowCrashPayload = {
      kind,
      message: scrubWindowCrashText(message, redact).slice(0, maxMessage),
      stack: scrubWindowCrashText(stack, redact).slice(0, maxStack),
      source: source === null ? null : scrubWindowCrashText(source, redact),
    };
    try {
      void Promise.resolve(config.invoke(RIGHTKIT_CRASH_WINDOW_ERROR_COMMAND, { report: payload })).catch(() => undefined);
    } catch {
      // never raise from the crash path
    }
  }

  const onError = (event: any): void => {
    const err = event?.error;
    const d = err !== undefined && err !== null ? describe(err) : { message: String(event?.message ?? 'error'), stack: '' };
    send('error', d.message, d.stack, sourceName(event?.filename, event?.lineno, event?.colno));
  };
  const onRejection = (event: any): void => {
    const d = describe(event?.reason);
    send('unhandledrejection', d.message, d.stack, null);
  };

  target.addEventListener('error', onError);
  target.addEventListener('unhandledrejection', onRejection);
  return () => {
    target.removeEventListener('error', onError);
    target.removeEventListener('unhandledrejection', onRejection);
  };
}
