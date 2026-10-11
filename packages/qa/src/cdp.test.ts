import { afterEach, describe, expect, it } from 'vitest';
import { CdpClient, connectCdp } from './cdp.js';
import { mockCdp } from '../test/mock-cdp.js';

const cleanup: (() => Promise<unknown>)[] = [];
afterEach(async () => { for (const close of cleanup.splice(0).reverse()) await close(); });

/** Answers every command with an empty result, so a later command doubles as a delivery barrier for earlier events. */
const answer = (message: any, send: (message: any) => void) => send({ id: message.id, result: {} });

describe('connectCdp & CdpClient (0.4.8)', () => {
  it('connects to a browser endpoint and to a page-level webSocketDebuggerUrl, both as root clients', async () => {
    const server = await mockCdp(answer); cleanup.push(server.close);
    const browser = await connectCdp(server.endpoint, { timeout: 500 }); cleanup.push(async () => browser.close());
    const page = await connectCdp(`${server.endpoint}/devtools/page/PAGE`, { timeout: 500 }); cleanup.push(async () => page.close());
    expect(browser).toBeInstanceOf(CdpClient);
    await browser.send('Browser.getVersion');
    await page.send('Runtime.evaluate', { expression: '1' });
    expect(server.messages.map((message) => [message.method, message.sessionId])).toEqual([
      ['Browser.getVersion', undefined],
      ['Runtime.evaluate', undefined],
    ]);
  });

  it('send(method, params, sessionId, timeoutMs) puts the sessionId on the wire and times out on its own bound', async () => {
    const server = await mockCdp((message, send) => { if (message.method !== 'Never.answers') answer(message, send); }); cleanup.push(server.close);
    const client = await connectCdp(server.endpoint, { timeout: 500 }); cleanup.push(async () => client.close());
    await client.send('Runtime.evaluate', { expression: '1' }, 'child-7', 400);
    expect(server.messages.at(-1)).toEqual({ id: expect.any(Number), method: 'Runtime.evaluate', params: { expression: '1' }, sessionId: 'child-7' });
    await expect(client.send('Never.answers', {}, undefined, 30)).rejects.toThrow('Never.answers timed out after 30ms');
  });

  it('on() routes by session and its unsubscribe stops delivery', async () => {
    const server = await mockCdp(answer); cleanup.push(server.close);
    const client = await connectCdp(server.endpoint, { timeout: 500 }); cleanup.push(async () => client.close());
    const root: number[] = [];
    const child: number[] = [];
    const offRoot = client.on('Event.tick', ({ n }) => { root.push(n); });
    const offChild = client.on('Event.tick', ({ n }) => { child.push(n); }, 'child-1');
    server.emit({ method: 'Event.tick', params: { n: 1 } });
    server.emit({ method: 'Event.tick', sessionId: 'child-1', params: { n: 2 } });
    await client.send('barrier');
    offRoot();
    server.emit({ method: 'Event.tick', params: { n: 3 } });
    server.emit({ method: 'Event.tick', sessionId: 'child-1', params: { n: 4 } });
    await client.send('barrier');
    expect(root).toEqual([1]);
    expect(child).toEqual([2, 4]);
    offChild();
  });

  it('onAny fires for the root and every flattened child session, with the sessionId each event arrived on', async () => {
    const server = await mockCdp(answer); cleanup.push(server.close);
    const client = await connectCdp(server.endpoint, { timeout: 500 }); cleanup.push(async () => client.close());
    const seen: Array<[number, string | undefined]> = [];
    const off = client.onAny('Page.loadEventFired', (params, sessionId) => { seen.push([params.n, sessionId]); });
    server.emit({ method: 'Page.loadEventFired', params: { n: 1 } });
    server.emit({ method: 'Page.loadEventFired', sessionId: 'child-1', params: { n: 2 } });
    server.emit({ method: 'Page.loadEventFired', sessionId: 'child-2', params: { n: 3 } });
    server.emit({ method: 'Page.frameNavigated', sessionId: 'child-2', params: { n: 4 } });
    await client.send('barrier');
    expect(seen).toEqual([[1, undefined], [2, 'child-1'], [3, 'child-2']]);
    off();
    server.emit({ method: 'Page.loadEventFired', sessionId: 'child-1', params: { n: 5 } });
    await client.send('barrier');
    expect(seen).toHaveLength(3);
  });

  it('close() needs no await: a second call is harmless, pending commands reject, and later commands reject too', async () => {
    const server = await mockCdp(() => {}); cleanup.push(server.close);
    const client = await connectCdp(server.endpoint, { timeout: 2_000 });
    const pending = client.send('Never.answers');
    const rejected = expect(pending).rejects.toThrow('CDP socket closed');
    client.close();
    client.close();
    await rejected;
    await expect(client.send('Late')).rejects.toThrow('CDP socket closed');
  });
});
