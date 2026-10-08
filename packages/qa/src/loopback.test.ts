import { describe, expect, it } from 'vitest';
import { LoopbackTimeoutError, createLoopbackClient } from './loopback.js';

interface MockRequest {
  url: URL;
  method: string;
  headers: Headers;
  body: string | undefined;
  signal: AbortSignal | undefined;
}

type MockHandler = (request: MockRequest) => Response | Promise<Response>;

function mockFetch(handler: MockHandler): typeof fetch {
  return async (input, init = {}) => handler({
    url: new URL(input instanceof Request ? input.url : String(input)),
    method: init.method ?? 'GET',
    headers: new Headers(init.headers),
    body: typeof init.body === 'string' ? init.body : undefined,
    signal: init.signal,
  });
}

function json(body: unknown): Response {
  return new Response(JSON.stringify(body), { headers: { 'content-type': 'application/json' } });
}

function hangingFetch(_input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  return new Promise((_resolve, reject) => {
    const abort = () => reject(new DOMException('The operation was aborted', 'AbortError'));
    if (init?.signal?.aborted) abort();
    else init?.signal?.addEventListener('abort', abort, { once: true });
  });
}

describe('bearer-authenticated loopback client', () => {
  it('queues an action and polls until its matching result completes', async () => {
    let polls = 0;
    const client = createLoopbackClient({
      baseUrl: 'http://127.0.0.1',
      token: 'secret-token',
      pollIntervalMs: 1,
      fetchImpl: mockFetch(({ url, headers }) => {
        expect(headers.get('authorization')).toBe('Bearer secret-token');
        if (url.pathname === '/app-control/actions') return json({ action: { id: 'action-1' } });
        polls += 1;
        return json({ recentResults: polls < 2 ? [] : [{ id: 'action-1', ok: true, message: 'done' }] });
      }),
    });
    await expect(client.action('refresh', {}, { timeoutMs: 1_000 })).resolves.toMatchObject({ id: 'action-1', ok: true });
    expect(polls).toBe(2);
  });

  it('rejects non-loopback control endpoints', () => {
    expect(() => createLoopbackClient({ baseUrl: 'https://example.com', token: 'secret' })).toThrow(/loopback/i);
  });

  it('aborts a request the app never answers instead of hanging the run', async () => {
    const client = createLoopbackClient({ baseUrl: 'http://127.0.0.1', token: 'secret-token', requestTimeoutMs: 60, fetchImpl: hangingFetch });

    const started = Date.now();
    await expect(client.request('GET', '/app-control/state')).rejects.toBeInstanceOf(LoopbackTimeoutError);
    expect(Date.now() - started).toBeLessThan(5_000);
  });

  it('bounds the whole action, not just each poll', async () => {
    const client = createLoopbackClient({
      baseUrl: 'http://127.0.0.1',
      token: 'secret-token',
      pollIntervalMs: 5,
      fetchImpl: mockFetch(({ url }) => url.pathname === '/app-control/actions'
        ? json({ action: { id: 'action-1' } })
        : json({ recentResults: [] })),
    });
    await expect(client.action('resize', {}, { timeoutMs: 80 })).rejects.toThrow(/app-control resize exceeded 80ms/);
  });

  it('sends a request id and matches the result that echoes it', async () => {
    const seen: string[] = [];
    const client = createLoopbackClient({
      baseUrl: 'http://127.0.0.1',
      token: 'secret-token',
      pollIntervalMs: 1,
      newRequestId: () => 'req-ours',
      fetchImpl: mockFetch(({ url, headers }) => {
        if (url.pathname === '/app-control/actions') {
          const header = headers.get('x-rightkit-qa-request-id');
          if (header) seen.push(header);
          return json({ action: { id: 'shared-id' } });
        }
        return json({ recentResults: [
          { id: 'shared-id', ok: false, message: 'the OTHER resize failed', requestId: 'req-other' },
          { id: 'shared-id', ok: true, message: 'ours succeeded', requestId: 'req-ours' },
        ] });
      }),
    });
    await expect(client.action('resize', { width: 1024 }, { timeoutMs: 1_000 }))
      .resolves.toMatchObject({ ok: true, message: 'ours succeeded', requestId: 'req-ours' });
    expect(seen).toEqual(['req-ours']);
  });

  it('does not attribute another action\'s failure to this one', async () => {
    const client = createLoopbackClient({
      baseUrl: 'http://127.0.0.1',
      token: 'secret-token',
      pollIntervalMs: 2,
      newRequestId: () => 'mine',
      fetchImpl: mockFetch(({ url }) => url.pathname === '/app-control/actions'
        ? json({ action: { id: 'action-9' } })
        : json({ recentResults: [{ id: 'action-9', ok: false, message: 'unrelated', requestId: 'someone-else' }] })),
    });
    await expect(client.action('open', {}, { timeoutMs: 60 })).rejects.toThrow(/app-control open exceeded 60ms.*requestId mine/);
  });

  it('refuses a non-positive timeout rather than treating it as unbounded', () => {
    expect(() => createLoopbackClient({ baseUrl: 'http://127.0.0.1:1', token: 't', requestTimeoutMs: 0 }))
      .toThrow(/requestTimeoutMs must be a positive number/);
  });
});
