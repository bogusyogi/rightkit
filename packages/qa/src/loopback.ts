// Control-plane client for the app's loopback QA endpoint.
//
// Two properties this file exists to guarantee:
//
//  1. NO CALL IS UNBOUNDED. Every fetch carries its own abort deadline, and the
//     action poll's deadline covers the whole queue-and-await cycle. An app that
//     accepts a request and then blocks — a resize that never repaints, a window
//     operation waiting on a message loop that is itself waiting on us — must
//     surface as a timed-out test, not a hung run that a human eventually kills.
//  2. RESULTS ARE CORRELATED, NOT GUESSED. Every action carries a caller-minted
//     request id which is echoed back in the result. Matching on "the most recent
//     result of this kind" attributes one action's outcome to another whenever
//     two actions of the same kind overlap.

import { randomUUID } from 'node:crypto';

export interface LoopbackClientOptions {
  baseUrl: string;
  token: string;
  pollIntervalMs?: number;
  /** Ceiling for a single HTTP round trip. */
  requestTimeoutMs?: number;
  /** Default ceiling for a queued action, measured from queue to result. */
  actionTimeoutMs?: number;
  fetchImpl?: typeof fetch;
  newRequestId?: () => string;
}

export interface ActionResult {
  id: string;
  ok: boolean;
  message?: string;
  requestId?: string;
  [key: string]: unknown;
}

export interface RequestOptions {
  timeoutMs?: number;
  requestId?: string;
}

export interface LoopbackClient {
  request<T = unknown>(method: string, route: string, body?: unknown, options?: RequestOptions): Promise<T>;
  action(kind: string, payload?: Record<string, unknown>, options?: RequestOptions): Promise<ActionResult>;
}

export class LoopbackTimeoutError extends Error {
  readonly requestId: string | undefined;
  readonly timeoutMs: number;

  constructor(what: string, timeoutMs: number, requestId?: string) {
    super(`${what} exceeded ${timeoutMs}ms${requestId ? ` (requestId ${requestId})` : ''}`);
    this.name = 'LoopbackTimeoutError';
    this.timeoutMs = timeoutMs;
    this.requestId = requestId;
  }
}

export const DEFAULT_REQUEST_TIMEOUT_MS = 10_000;
export const DEFAULT_ACTION_TIMEOUT_MS = 30_000;

export function createLoopbackClient(options: LoopbackClientOptions): LoopbackClient {
  const baseUrl = validateLoopbackUrl(options.baseUrl);
  const token = options.token.trim();
  if (!token) throw new Error('loopback bearer token is required');
  const fetchImpl = options.fetchImpl ?? fetch;
  const pollIntervalMs = requirePositive(options.pollIntervalMs ?? 250, 'pollIntervalMs');
  const requestTimeoutMs = requirePositive(options.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS, 'requestTimeoutMs');
  const actionTimeoutMs = requirePositive(options.actionTimeoutMs ?? DEFAULT_ACTION_TIMEOUT_MS, 'actionTimeoutMs');
  const newRequestId = options.newRequestId ?? (() => randomUUID());

  async function request<T = unknown>(
    method: string,
    route: string,
    body?: unknown,
    requestOptions: RequestOptions = {},
  ): Promise<T> {
    const timeoutMs = requirePositive(requestOptions.timeoutMs ?? requestTimeoutMs, 'timeoutMs');
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    let response: Response;
    try {
      response = await fetchImpl(new URL(route, baseUrl), {
        method,
        headers: {
          authorization: `Bearer ${token}`,
          'content-type': 'application/json',
          ...(requestOptions.requestId ? { 'x-rightkit-qa-request-id': requestOptions.requestId } : {}),
        },
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: controller.signal,
      });
    } catch (error) {
      if (controller.signal.aborted) throw new LoopbackTimeoutError(`${method} ${route}`, timeoutMs, requestOptions.requestId);
      throw error;
    } finally {
      clearTimeout(timer);
    }

    const text = await response.text();
    let data: unknown = null;
    try {
      data = text ? JSON.parse(text) : null;
    } catch {
      data = text;
    }
    if (!response.ok) throw new Error(`${method} ${route} failed ${response.status}: ${text}`);
    return data as T;
  }

  async function action(
    kind: string,
    payload: Record<string, unknown> = {},
    actionOptions: RequestOptions = {},
  ): Promise<ActionResult> {
    const requestId = actionOptions.requestId ?? newRequestId();
    const timeoutMs = requirePositive(actionOptions.timeoutMs ?? actionTimeoutMs, 'timeoutMs');
    const deadline = Date.now() + timeoutMs;

    const queued = await request<{ action?: { id?: string } }>(
      'POST',
      '/app-control/actions',
      { kind, payload, requestId },
      { requestId, timeoutMs: remaining(deadline, timeoutMs, requestId, `app-control ${kind}`) },
    );
    const id = queued.action?.id;
    if (!id) throw new Error(`app-control ${kind} did not return an action id (requestId ${requestId})`);

    while (Date.now() < deadline) {
      await delay(Math.min(pollIntervalMs, Math.max(1, deadline - Date.now())));
      let state: { recentResults?: ActionResult[] };
      try {
        state = await request<{ recentResults?: ActionResult[] }>(
          'GET',
          '/app-control/state',
          undefined,
          { requestId, timeoutMs: remaining(deadline, timeoutMs, requestId, `app-control ${kind}`) },
        );
      } catch (error) {
        // A poll cut short by the action's own deadline is an action timeout.
        // Reporting the truncated per-request budget instead would name a number
        // the caller never chose and hide the deadline that actually expired.
        if (error instanceof LoopbackTimeoutError && Date.now() >= deadline) {
          throw new LoopbackTimeoutError(`app-control ${kind}`, timeoutMs, requestId);
        }
        throw error;
      }
      // Prefer the request id; fall back to the action id for apps that predate
      // request-id echo. Never fall back to "the newest result".
      const result = state.recentResults?.find((entry) => entry.requestId === requestId)
        ?? state.recentResults?.find((entry) => entry.id === id && entry.requestId === undefined);
      if (!result) continue;
      if (!result.ok) throw new Error(`app-control ${kind} failed: ${result.message ?? 'unknown error'} (requestId ${requestId})`);
      return { ...result, requestId };
    }
    throw new LoopbackTimeoutError(`app-control ${kind}`, timeoutMs, requestId);
  }

  return { request, action };
}

/**
 * Time left before the action's deadline. Exhaustion reports the ORIGINAL budget,
 * not the zero milliseconds remaining — "exceeded 0ms" tells the reader nothing
 * about what was actually allowed.
 */
function remaining(deadline: number, budgetMs: number, requestId: string, what: string): number {
  const left = deadline - Date.now();
  if (left <= 0) throw new LoopbackTimeoutError(what, budgetMs, requestId);
  return left;
}

function validateLoopbackUrl(value: string): URL {
  const url = new URL(value);
  const host = url.hostname.toLowerCase();
  if (url.protocol !== 'http:' || !['127.0.0.1', 'localhost', '[::1]', '::1'].includes(host)) {
    throw new Error(`QA control endpoint must be an HTTP loopback URL, received ${url.origin}`);
  }
  return url;
}

function requirePositive(value: number, name: string): number {
  if (!Number.isFinite(value) || value <= 0) throw new Error(`${name} must be a positive number of milliseconds`);
  return value;
}

async function delay(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}
