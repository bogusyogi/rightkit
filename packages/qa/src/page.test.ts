import { afterEach, describe, expect, it } from 'vitest';
import { QaBrowserSession, TimeoutError } from './index.js';
import { CdpClient } from './cdp.js';
import { keyDefinition, QaKeyboard } from './keyboard.js';
import { mockCdp, pageResponder } from '../test/mock-cdp.js';
const cleanup: (() => Promise<unknown>)[] = [];
afterEach(async () => { for (const close of cleanup.splice(0).reverse()) await close(); });

async function fixture(handler = pageResponder, timeout = 500) {
  const server = await mockCdp(handler); cleanup.push(server.close);
  const browser = await QaBrowserSession.connect(server.endpoint, { timeout });
  cleanup.push(() => browser.close().catch(() => {}));
  const page = await browser.newPage();
  return { server, browser, page };
}

describe('raw CDP', () => {
  it('routes concurrent out-of-order responses & subscriptions by session', async () => {
    let first: any;
    const server = await mockCdp((message, send) => {
      if (!first) first = message;
      else {
        send({ id: message.id, result: { value: 'second' } }); send({ id: first.id, result: { value: 'first' } });
        send({ method: 'event', sessionId: 's2', params: { n: 2 } }); send({ method: 'event', sessionId: 's1', params: { n: 1 } });
      }
    }); cleanup.push(server.close);
    const client = await CdpClient.connect(server.endpoint); cleanup.push(async () => client.close());
    const events: number[] = []; const other: number[] = [];
    // Events arrive as separate socket frames after the responses; wait for delivery instead of assuming it.
    let delivered!: () => void; const seen = new Promise<void>((resolve) => { delivered = resolve; });
    const off = client.on('event', ({ n }) => { events.push(n); delivered(); }, 's1');
    const offOther = client.on('event', ({ n }) => other.push(n), 's2');
    expect(await Promise.all([client.send('one', {}, 's1'), client.send('two', {}, 's2')])).toEqual([{ value: 'first' }, { value: 'second' }]);
    await seen; // the s2 event was sent before the s1 event on the same ordered socket, so both have been routed
    expect(events).toEqual([1]); expect(other).toEqual([2]); off(); offOther();
  });
  it('times out commands, ignores late replies & rejects pending calls on disconnect', async () => {
    const server = await mockCdp(() => {}); cleanup.push(server.close);
    const client = await CdpClient.connect(server.endpoint, { timeout: 30 }); cleanup.push(async () => client.close());
    await expect(client.send('never')).rejects.toBeInstanceOf(TimeoutError);
    server.emit({ id: server.messages[0].id, result: { late: true } });
    const pending = client.send('pending'); const rejected = expect(pending).rejects.toThrow(/CDP socket (closed|error)/);
    server.disconnect(); await rejected;
  });
  it('throws protocol errors with their message', async () => {
    const server = await mockCdp((message, send) => send({ id: message.id, error: { message: 'bad command', code: -1 } })); cleanup.push(server.close);
    const client = await CdpClient.connect(server.endpoint); cleanup.push(async () => client.close());
    await expect(client.send('bad')).rejects.toThrow('bad command');
  });
  it('creates flattened targets, selects main default context & closes owned targets', async () => {
    const { page, server, browser } = await fixture();
    expect(await page.evaluate((n) => n, 4)).toBe(true);
    expect(server.messages.find((m) => m.method === 'Runtime.callFunctionOn').params).toMatchObject({ executionContextId: 13, returnByValue: true, awaitPromise: true, arguments: [{ value: 4 }] });
    expect(server.messages.find((m) => m.method === 'Target.attachToTarget').params.flatten).toBe(true);
    await browser.close(); await browser.close();
    expect(server.messages.filter((m) => m.method === 'Target.closeTarget')).toHaveLength(1);
  });
  it('throws JavaScript exceptionDetails', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Runtime.callFunctionOn') send({ id: message.id, result: { exceptionDetails: { exception: { description: 'Error: fixture boom' } } } });
      else pageResponder(message, send);
    });
    await expect(page.evaluate(() => 1)).rejects.toThrow('fixture boom');
  });
  it('tracks destroyed contexts & waits for replacement main context', async () => {
    let update: ((message: any) => void) | undefined;
    const { page, server } = await fixture((message, send) => { update = send; pageResponder(message, send); });
    update!({ method: 'Runtime.executionContextsCleared', sessionId: 'session', params: {} });
    await new Promise((r) => setTimeout(r, 10));
    const pending = page.evaluate(() => 1);
    update!({ method: 'Runtime.executionContextCreated', sessionId: 'session', params: { context: { id: 20, auxData: { frameId: 'main', isDefault: true } } } });
    await pending;
    expect(server.messages.filter((m) => m.method === 'Runtime.callFunctionOn').at(-1).params.executionContextId).toBe(20);
  });
  it('correlates navigation lifecycle to main frame & new loader', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Page.navigate') {
        for (const [frameId, loaderId] of [['child', 'new'], ['main', 'old'], ['main', 'new']]) send({ method: 'Page.lifecycleEvent', sessionId: 'session', params: { frameId, loaderId, name: 'load' } });
        send({ id: message.id, result: { frameId: 'main', loaderId: 'new' } });
      } else pageResponder(message, send);
    });
    await page.goto('https://fixture');
  });
  it('navigation & predicate deadlines throw typed named timeouts', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Page.navigate') send({ id: message.id, result: { loaderId: 'new' } });
      else if (message.method === 'Runtime.callFunctionOn') send({ id: message.id, result: { result: { value: false } } });
      else pageResponder(message, send);
    });
    await expect(page.goto('https://fixture', { timeout: 35 })).rejects.toBeInstanceOf(TimeoutError);
    await expect(page.waitForSelector('#missing', { timeout: 35 })).rejects.toThrow('Selector #missing');
    await expect(page.waitForFunction(() => false, { timeout: 35, polling: 5 })).rejects.toThrow('Predicate');
  });
  it('rejects waits promptly when page closes', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Runtime.callFunctionOn') send({ id: message.id, result: { result: { value: false } } });
      else pageResponder(message, send);
    });
    const waiting = page.waitForFunction(() => false); const rejected = expect(waiting).rejects.toThrow('Page closed');
    await page.close(); await rejected;
  });
  it('dispatches real key commands & validates keys', async () => {
    const { page, server } = await fixture();
    await page.keyboard.press('Control+A'); await page.keyboard.press('Enter');
    const keys = server.messages.filter((m) => m.method === 'Input.dispatchKeyEvent').map((m) => m.params);
    expect(keys[1]).toMatchObject({ key: 'a', code: 'KeyA', modifiers: 2, type: 'rawKeyDown' });
    expect(keys[4]).toMatchObject({ key: 'Enter', text: '\r', type: 'keyDown' });
    await expect(page.keyboard.press('Unknown')).rejects.toThrow('Unsupported key');
  });
  it('maps every required special key, letters, digits & modifiers', () => {
    for (const key of ['Escape', 'Enter', 'Tab', 'ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Backspace', 'Delete', 'Home', 'End', 'PageUp', 'PageDown', 'Space', 'Shift', 'Control', 'Alt', 'Meta', 'a', 'Z', '0', '9']) {
      expect(keyDefinition(key, 0).windowsVirtualKeyCode).toBeGreaterThan(0);
    }
    expect(keyDefinition('a', 8).text).toBe('A'); expect(keyDefinition('1', 8).text).toBe('!');
    expect(keyDefinition('a', 8).unmodifiedText).toBe('a');
  });

  it('debug stays opt-in & logs method names without source or input', async () => {
    const server = await mockCdp(pageResponder); cleanup.push(server.close);
    const logs: string[] = [];
    const browser = await QaBrowserSession.connect(server.endpoint, { debug: message => logs.push(message) });
    cleanup.push(() => browser.close());
    const page = await browser.newPage();
    await page.evaluate(() => 'private-source', 'private-argument');
    expect(logs).toContain('CDP Runtime.callFunctionOn');
    expect(logs.join('\n')).not.toMatch(/private-source|private-argument/);
  });

  it('isolates throwing event listeners & removes subscriptions', async () => {
    const server = await mockCdp((message, send) => {
      send({ method: 'event', params: {} }); send({ id: message.id, result: {} });
    }); cleanup.push(server.close);
    const client = await CdpClient.connect(server.endpoint); cleanup.push(async () => client.close());
    let observed = 0;
    client.on('event', () => { throw new Error('observer'); });
    const off = client.on('event', () => observed++);
    await client.send('first'); off(); await client.send('second');
    expect(observed).toBe(1);
  });

  it('rejects invalid deadlines, wait modes & polling before issuing work', async () => {
    const { page } = await fixture();
    await expect(page.goto('fixture', { timeout: -1 })).rejects.toThrow('Timeout');
    await expect(page.goto('fixture', { waitUntil: 'unknown' as any })).rejects.toThrow('waitUntil');
    await expect(page.waitForFunction(() => true, { polling: 0 })).rejects.toThrow('Polling');
    await expect(page.waitForSelector('x', { visible: true, hidden: true })).rejects.toThrow('visible');
    await expect(page.screenshot({ fullPage: true as any })).rejects.toThrow('viewport PNG');
  });

  it('cleans up a target whose initialization fails', async () => {
    const server = await mockCdp((message, send) => {
      if (message.method === 'Network.enable') send({ id: message.id, error: { message: 'initialize failed' } });
      else pageResponder(message, send);
    }); cleanup.push(server.close);
    const browser = await QaBrowserSession.connect(server.endpoint); cleanup.push(() => browser.close());
    await expect(browser.newPage()).rejects.toThrow('initialize failed');
    expect(server.messages.filter(message => message.method === 'Target.closeTarget')).toHaveLength(1);
  });

  it('closes in-flight newPage targets & shares close completion', async () => {
    let created!: () => void;
    const creating = new Promise<void>(resolve => created = resolve);
    const server = await mockCdp((message, send) => {
      if (message.method === 'Target.createTarget') {
        created(); setTimeout(() => pageResponder(message, send), 20);
      } else pageResponder(message, send);
    }); cleanup.push(server.close);
    const browser = await QaBrowserSession.connect(server.endpoint); cleanup.push(() => browser.close());
    const pending = browser.newPage(); const rejection = expect(pending).rejects.toThrow('session closed');
    await creating;
    const closing = browser.close(); expect(browser.close()).toBe(closing);
    await rejection; await closing;
    expect(server.messages.filter(message => message.method === 'Target.closeTarget')).toHaveLength(1);
    await expect(browser.newPage()).rejects.toThrow('session closed');
  });

  it('rejects pending page commands & waits on target detach', async () => {
    const { page, server } = await fixture((message, send) => {
      if (message.method !== 'Runtime.callFunctionOn') pageResponder(message, send);
    });
    const pending = page.evaluate(() => 1); const rejection = expect(pending).rejects.toThrow('Page closed');
    server.emit({ method: 'Target.detachedFromTarget', params: { sessionId: 'session' } });
    await rejection;
    await expect(page.evaluate('1')).rejects.toThrow('Page closed');
  });

  it('shares concurrent page close completion & clears user subscriptions', async () => {
    const { page, server } = await fixture();
    let observed = 0;
    page.on('custom', () => observed++);
    const closing = page.close(); expect(page.close()).toBe(closing); await closing;
    server.emit({ method: 'custom', sessionId: 'session', params: {} });
    await new Promise(resolve => setTimeout(resolve, 10));
    expect(observed).toBe(0);
    expect(server.messages.filter(message => message.method === 'Target.closeTarget')).toHaveLength(1);
    expect(() => page.on('custom', () => {})).toThrow('Page closed');
  });

  it('keeps main-frame context through iframe creation/destruction & frame swaps', async () => {
    const { page, server } = await fixture();
    server.emit({ method: 'Page.frameNavigated', sessionId: 'session', params: { frame: { id: 'new-main' } } });
    server.emit({ method: 'Runtime.executionContextCreated', sessionId: 'session', params: { context: { id: 20, auxData: { frameId: 'new-main', isDefault: true } } } });
    server.emit({ method: 'Runtime.executionContextCreated', sessionId: 'session', params: { context: { id: 21, auxData: { frameId: 'child', isDefault: true } } } });
    server.emit({ method: 'Runtime.executionContextDestroyed', sessionId: 'session', params: { executionContextId: 21 } });
    // Flush preceding events over the same transport before evaluating.
    await page.send('Page.getFrameTree');
    await page.evaluate('1');
    expect(server.messages.at(-1).params.contextId).toBe(20);
  });

  it('navigation ignores unrelated loaders, handles reload & propagates navigation errors', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Page.reload') {
        send({ method: 'Page.lifecycleEvent', sessionId: 'session', params: { frameId: 'main', loaderId: 'old', name: 'load' } });
        send({ method: 'Page.lifecycleEvent', sessionId: 'session', params: { frameId: 'main', loaderId: 'reload', name: 'init' } });
        send({ id: message.id, result: {} });
        setTimeout(() => send({ method: 'Page.lifecycleEvent', sessionId: 'session', params: { frameId: 'main', loaderId: 'reload', name: 'DOMContentLoaded' } }), 10);
      } else if (message.method === 'Page.navigate') send({ id: message.id, result: { errorText: 'net::ERR_FAILED' } });
      else pageResponder(message, send);
    });
    await page.reload({ waitUntil: 'domcontentloaded' });
    await expect(page.goto('fixture')).rejects.toThrow('net::ERR_FAILED');
  });

  it('networkidle0 observes 500ms with zero requests & resets on brief bursts', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Page.navigate') {
        send({ id: message.id, result: { loaderId: 'new' } });
        setTimeout(() => {
          send({ method: 'Network.requestWillBeSent', sessionId: 'session', params: { requestId: 'burst' } });
          send({ method: 'Network.loadingFinished', sessionId: 'session', params: { requestId: 'burst' } });
        }, 100);
      } else pageResponder(message, send);
    }, 1500);
    const start = Date.now();
    await page.goto('fixture', { waitUntil: 'networkidle0' });
    expect(Date.now() - start).toBeGreaterThanOrEqual(590);
  });

  it('forgets abandoned old-document requests while retaining new-loader requests', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Page.navigate') {
        const event = (method: string, params: any) => send({ method, params, sessionId: 'session' });
        event('Network.requestWillBeSent', { requestId: 'favicon', frameId: 'main', loaderId: 'old' });
        event('Network.requestWillBeSent', { requestId: 'document', frameId: 'main', loaderId: 'new' });
        event('Page.frameNavigated', { frame: { id: 'main', loaderId: 'new' } });
        send({ id: message.id, result: { loaderId: 'new' } });
        setTimeout(() => event('Network.loadingFinished', { requestId: 'document' }), 100);
      } else pageResponder(message, send);
    }, 1500);
    const start = Date.now();
    await page.goto('fixture', { waitUntil: 'networkidle0' });
    expect(Date.now() - start).toBeGreaterThanOrEqual(590);
  });

  it('releases modifiers after dispatch failures without replacing original error', async () => {
    const calls: any[] = [];
    const keyboard = new QaKeyboard(async (_method, params) => {
      calls.push(params);
      if (params.key === 'a' || params.key === 'Shift' && params.type === 'keyUp') throw new Error('input failed');
      return {};
    });
    await expect(keyboard.press('Control+Shift+a')).rejects.toThrow('input failed');
    expect(calls.at(-1)).toMatchObject({ type: 'keyUp', key: 'Control', modifiers: 0 });
  });
});
