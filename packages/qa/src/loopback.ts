export interface LoopbackClientOptions {
  baseUrl: string;
  token: string;
  pollIntervalMs?: number;
  fetchImpl?: typeof fetch;
}

export interface ActionResult {
  id: string;
  ok: boolean;
  message?: string;
  [key: string]: unknown;
}

export interface LoopbackClient {
  request<T = unknown>(method: string, route: string, body?: unknown): Promise<T>;
  action(
    kind: string,
    payload?: Record<string, unknown>,
    options?: { timeoutMs?: number },
  ): Promise<ActionResult>;
}

export function createLoopbackClient(options: LoopbackClientOptions): LoopbackClient {
  const baseUrl = validateLoopbackUrl(options.baseUrl);
  const token = options.token.trim();
  if (!token) throw new Error('loopback bearer token is required');
  const fetchImpl = options.fetchImpl ?? fetch;
  const pollIntervalMs = options.pollIntervalMs ?? 250;

  async function request<T = unknown>(method: string, route: string, body?: unknown): Promise<T> {
    const response = await fetchImpl(new URL(route, baseUrl), {
      method,
      headers: {
        authorization: `Bearer ${token}`,
        'content-type': 'application/json',
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await response.text();
    let data: unknown = null;
    try {
      data = text ? JSON.parse(text) : null;
    } catch {
      data = text;
    }
    if (!response.ok) {
      throw new Error(`${method} ${route} failed ${response.status}: ${text}`);
    }
    return data as T;
  }

  async function action(
    kind: string,
    payload: Record<string, unknown> = {},
    actionOptions: { timeoutMs?: number } = {},
  ): Promise<ActionResult> {
    const queued = await request<{ action?: { id?: string } }>('POST', '/app-control/actions', { kind, payload });
    const id = queued.action?.id;
    if (!id) throw new Error(`app-control ${kind} did not return an action id`);
    const deadline = Date.now() + (actionOptions.timeoutMs ?? 30_000);
    while (Date.now() < deadline) {
      await delay(pollIntervalMs);
      const state = await request<{ recentResults?: ActionResult[] }>('GET', '/app-control/state');
      const result = state.recentResults?.find((entry) => entry.id === id);
      if (!result) continue;
      if (!result.ok) throw new Error(`app-control ${kind} failed: ${result.message ?? 'unknown error'}`);
      return result;
    }
    throw new Error(`app-control ${kind} timed out`);
  }

  return { request, action };
}

function validateLoopbackUrl(value: string): URL {
  const url = new URL(value);
  const host = url.hostname.toLowerCase();
  if (url.protocol !== 'http:' || !['127.0.0.1', 'localhost', '[::1]'].includes(host)) {
    throw new Error(`QA control endpoint must be an HTTP loopback URL, received ${url.origin}`);
  }
  return url;
}

async function delay(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}
