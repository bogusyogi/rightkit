import { createServer } from 'node:http';
import { afterEach, describe, expect, it } from 'vitest';
import { createLoopbackClient } from './loopback.js';

const servers: Array<ReturnType<typeof createServer>> = [];

afterEach(async () => {
  await Promise.all(servers.splice(0).map((server) => new Promise<void>((resolve) => server.close(() => resolve()))));
});

describe('bearer-authenticated loopback client', () => {
  it('queues an action and polls until its matching result completes', async () => {
    let polls = 0;
    const server = createServer((request, response) => {
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
    servers.push(server);
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('missing test server port');

    const client = createLoopbackClient({
      baseUrl: `http://127.0.0.1:${address.port}`,
      token: 'secret-token',
      pollIntervalMs: 1,
    });
    await expect(client.action('refresh', {}, { timeoutMs: 1_000 })).resolves.toMatchObject({
      id: 'action-1',
      ok: true,
    });
    expect(polls).toBe(2);
  });

  it('rejects non-loopback control endpoints', () => {
    expect(() => createLoopbackClient({ baseUrl: 'https://example.com', token: 'secret' }))
      .toThrow(/loopback/i);
  });
});
