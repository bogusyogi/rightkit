/** Structured error data from the engine (`error.details`), or null when the engine sent none. */
export type EngineErrorDetails = Record<string, unknown>;

/** An engine failure. `message` reads "code: message"; `code`, `detail` and `details` are the parts. */
export class EngineError extends Error {
  code: string | null;
  detail: string;
  details: EngineErrorDetails | null;
  constructor(code: string | null, detail: string, details: EngineErrorDetails | null = null) {
    super(code ? `${code}: ${detail}` : detail);
    this.name = 'EngineError';
    this.code = code;
    this.detail = detail;
    this.details = details;
  }
}

const UNKNOWN_ERROR = 'Unknown error';

/** A plain object (not an array, not null) from the engine's `details`, or null. */
function detailsOf(value: unknown): EngineErrorDetails | null {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as EngineErrorDetails) : null;
}

function jsonOf(value: unknown): string {
  try {
    return JSON.stringify(value) ?? UNKNOWN_ERROR;
  } catch {
    return UNKNOWN_ERROR;
  }
}

/**
 * Turn whatever a rejected command or engine call carries into a readable Error.
 * Its message is never empty and never "[object Object]": "code: message" for an engine error,
 * the message alone, the JSON of an unrecognised object, or "Unknown error".
 */
export function toError(error: unknown): Error {
  if (error == null) return new Error(UNKNOWN_ERROR);
  if (error instanceof EngineError) return error;
  if (error instanceof Error) {
    const code = (error as { code?: unknown }).code;
    if (typeof code === 'string' && error.message.startsWith(`${code}: `)) return new EngineError(code, error.message.slice(code.length + 2), detailsOf((error as { details?: unknown }).details));
    return error.message ? error : new Error(UNKNOWN_ERROR);
  }
  if (typeof error === 'string') {
    const text = error.trim();
    if (!text) return new Error(UNKNOWN_ERROR);
    const m = /^([a-z][a-z0-9_]*):\s*([\s\S]*)$/.exec(text);
    return m ? new EngineError(m[1], m[2].trim() || UNKNOWN_ERROR) : new Error(text);
  }
  if (typeof error === 'object') {
    // Tauri rejects with the engine's {code, message, details?} object; details is absent on older engines.
    const e = error as { code?: unknown; message?: unknown; details?: unknown; error?: unknown };
    const message = typeof e.message === 'string' ? e.message.trim() : '';
    // A wrapper such as {error: {code, message}} reads as the error it wraps.
    if (!message && e.error != null) return toError(e.error);
    const code = typeof e.code === 'string' && e.code.trim() ? e.code.trim() : null;
    if (message) return new EngineError(code, message, detailsOf(e.details));
    if (code) return new EngineError(code, UNKNOWN_ERROR, detailsOf(e.details));
    return new Error(jsonOf(error));
  }
  return new Error(String(error).trim() || UNKNOWN_ERROR);
}

export function errorCode(error: unknown): string | null {
  const e = toError(error);
  return e instanceof EngineError ? e.code : null;
}

/** The engine's structured `details` for an error, or null. Never parse the message for these. */
export function errorDetails(error: unknown): EngineErrorDetails | null {
  const e = toError(error);
  return e instanceof EngineError ? e.details : null;
}

/** Readable text with the code prefix: "code: message". Use it when the code is shown or branched on. */
export function errorMessage(error: unknown): string {
  return toError(error).message;
}

/** Readable text without the engine's code prefix, for a view that already shows the code itself. */
export function errorText(error: unknown): string {
  const e = toError(error);
  return e instanceof EngineError ? e.detail : e.message.replace(/^[a-z][a-z0-9_]*:\s*/, '');
}
