import { createServer } from 'node:http';
import type { Server } from 'node:http';
import { afterEach, describe, expect, it } from 'vitest';
import { LoopbackTimeoutError, createLoopbackClient } from './loopback.js';

const servers: Server[] = [];

afterEach(async () => {
  await Promise.all(servers.splice(0).map((server) => new Promise<void>((resolve) => server.close(() => resolve()))));
});

async function listen(handler: Parameters<typeof createServer>[1]): Promise<string> {
  const server = createServer(handler);
  servers.push(server);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('missing test server port');
  return `http://127.0.0.1:${address.port}`;
}

describe('bearer-authenticated loopback client', () => {
  it('queues an action and polls until its matching result completes', async () => {
    let polls = 0;
    const baseUrl = await listen((request, response) => {
      expect(request.headers.authorization).toBe('Bearer secret-token');
      response.setHeader('content-type', 'application/json');
      if (request.url === '/app-control/actions') {
        response.end(JSON.stringify({ action: { id: 'action-1' } }));
        return;
      }
      polls += 1;
      response.end(JSON.stringify({
        recentResults: polls < 2 ? [] : [{ id: 'action-1', ok: true, message: 'done' }],
      }));
    });

    const client = createLoopbackClient({ baseUrl, token: 'secret-token', pollIntervalMs: 1 });
    await expect(client.action('refresh', {}, { timeoutMs: 1_000 })).resolves.toMatchObject({ id: 'action-1', ok: true });
    expect(polls).toBe(2);
  });

  it('rejects non-loopback control endpoints', () => {
    expect(() => createLoopbackClient({ baseUrl: 'https://example.com', token: 'secret' })).toThrow(/loopback/i);
  });

  it('aborts a request the app never answers instead of hanging the run', async () => {
    const baseUrl = await listen(() => {
      // Accept the connection and never respond: the resize-RPC deadlock shape.
    });
    const client = createLoopbackClient({ baseUrl, token: 'secret-token', requestTimeoutMs: 60 });

    const started = Date.now();
    await expect(client.request('GET', '/app-control/state')).rejects.toBeInstanceOf(LoopbackTimeoutError);
    expect(Date.now() - started).toBeLessThan(5_000);
  });

  it('bounds the whole action, not just each poll', async () => {
    const baseUrl = await listen((request, response) => {
      response.setHeader('content-type', 'application/json');
      if (request.url === '/app-control/actions') {
        response.end(JSON.stringify({ action: { id: 'action-1' } }));
        return;
      }
      response.end(JSON.stringify({ recentResults: [] }));
    });

    const client = createLoopbackClient({ baseUrl, token: 'secret-token', pollIntervalMs: 5 });
    await expect(client.action('resize', {}, { timeoutMs: 80 })).rejects.toThrow(/app-control resize exceeded 80ms/);
  });

  it('sends a request id and matches the result that echoes it', async () => {
    const seen: string[] = [];
    const baseUrl = await listen((request, response) => {
      response.setHeader('content-type', 'application/json');
      if (request.url === '/app-control/actions') {
        const header = request.headers['x-rightkit-qa-request-id'];
        if (typeof header === 'string') seen.push(header);
        response.end(JSON.stringify({ action: { id: 'shared-id' } }));
        return;
      }
      // Two overlapping resizes: only the echoed request id disambiguates them.
      response.end(JSON.stringify({
        recentResults: [
          { id: 'shared-id', ok: false, message: 'the OTHER resize failed', requestId: 'req-other' },
          { id: 'shared-id', ok: true, message: 'ours succeeded', requestId: 'req-ours' },
        ],
      }));
    });

    const client = createLoopbackClient({ baseUrl, token: 'secret-token', pollIntervalMs: 1, newRequestId: () => 'req-ours' });
    await expect(client.action('resize', { width: 1024 }, { timeoutMs: 1_000 }))
      .resolves.toMatchObject({ ok: true, message: 'ours succeeded', requestId: 'req-ours' });
    expect(seen).toEqual(['req-ours']);
  });

  it('does not attribute another action\'s failure to this one', async () => {
    const baseUrl = await listen((request, response) => {
      response.setHeader('content-type', 'application/json');
      if (request.url === '/app-control/actions') {
        response.end(JSON.stringify({ action: { id: 'action-9' } }));
        return;
      }
      response.end(JSON.stringify({ recentResults: [{ id: 'action-9', ok: false, message: 'unrelated', requestId: 'someone-else' }] }));
    });

    const client = createLoopbackClient({ baseUrl, token: 'secret-token', pollIntervalMs: 2, newRequestId: () => 'mine' });
    await expect(client.action('open', {}, { timeoutMs: 60 })).rejects.toThrow(/app-control open exceeded 60ms.*requestId mine/);
  });

  it('refuses a non-positive timeout rather than treating it as unbounded', () => {
    expect(() => createLoopbackClient({ baseUrl: 'http://127.0.0.1:1', token: 't', requestTimeoutMs: 0 }))
      .toThrow(/requestTimeoutMs must be a positive number/);
  });
});
