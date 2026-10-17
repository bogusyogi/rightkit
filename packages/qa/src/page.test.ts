import { afterEach, describe, expect, it } from 'vitest';
import { mkdtemp, readdir, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { ElementOccludedError, FillMismatchError, QaBrowserSession, TimeoutError } from './index.js';
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
    // Only the command may be short: a 30ms connect budget flakes on slow CI runners.
    const client = await CdpClient.connect(server.endpoint); cleanup.push(async () => client.close());
    await expect(client.send('never', {}, undefined, 30)).rejects.toBeInstanceOf(TimeoutError);
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

  it('a navigation discarded by another document committing is sent once more', async () => {
    let navigations = 0;
    const { page, server } = await fixture((message, send) => {
      if (message.method === 'Page.navigate') {
        navigations += 1;
        if (navigations === 1) {
          // The abort answer arrives first; the commit that caused it follows from the renderer.
          send({ id: message.id, result: { errorText: 'net::ERR_ABORTED' } });
          setTimeout(() => send({ method: 'Page.frameNavigated', sessionId: 'session', params: { frame: { id: 'main', loaderId: 'blank', url: 'about:blank' } } }), 20);
        } else {
          send({ method: 'Page.lifecycleEvent', sessionId: 'session', params: { frameId: 'main', loaderId: 'new', name: 'load' } });
          send({ id: message.id, result: { frameId: 'main', loaderId: 'new' } });
        }
      } else pageResponder(message, send);
    });
    await page.goto('https://fixture');
    expect(server.messages.filter((m) => m.method === 'Page.navigate')).toHaveLength(2);
  });

  it('on a page that has navigated, an abort with no other commit is thrown with its URL & not repeated', async () => {
    let navigations = 0;
    const { page, server } = await fixture((message, send) => {
      if (message.method === 'Page.navigate') {
        navigations += 1;
        if (navigations === 1) {
          send({ method: 'Page.lifecycleEvent', sessionId: 'session', params: { frameId: 'main', loaderId: 'new', name: 'load' } });
          send({ id: message.id, result: { frameId: 'main', loaderId: 'new' } });
        } else send({ id: message.id, result: { errorText: 'net::ERR_ABORTED' } });
      } else pageResponder(message, send);
    });
    await page.goto('https://fixture');
    await expect(page.goto('https://fixture/download')).rejects.toThrow('Navigation failed: net::ERR_ABORTED (https://fixture/download)');
    expect(server.messages.filter((m) => m.method === 'Page.navigate')).toHaveLength(2);
  });

  it('the first navigation of a page is repeated after an abort even with no commit seen, then succeeds', async () => {
    let navigations = 0;
    const { page, server } = await fixture((message, send) => {
      if (message.method === 'Page.navigate') {
        navigations += 1;
        if (navigations < 3) send({ id: message.id, result: { errorText: 'net::ERR_ABORTED' } });
        else {
          send({ method: 'Page.lifecycleEvent', sessionId: 'session', params: { frameId: 'main', loaderId: 'new', name: 'load' } });
          send({ id: message.id, result: { frameId: 'main', loaderId: 'new' } });
        }
      } else pageResponder(message, send);
    }, 2000);
    await page.goto('https://fixture');
    expect(server.messages.filter((m) => m.method === 'Page.navigate')).toHaveLength(3);
  });

  it('repeats are bounded: a navigation aborted every time is thrown after four repeats', async () => {
    const { page, server } = await fixture((message, send) => {
      if (message.method === 'Page.navigate') {
        send({ method: 'Page.frameNavigated', sessionId: 'session', params: { frame: { id: 'main', loaderId: 'blank', url: 'about:blank' } } });
        send({ id: message.id, result: { errorText: 'net::ERR_ABORTED' } });
      } else pageResponder(message, send);
    });
    await expect(page.goto('https://fixture')).rejects.toThrow('net::ERR_ABORTED');
    expect(server.messages.filter((m) => m.method === 'Page.navigate')).toHaveLength(5);
  });

  it('newPage waits until the tab\'s own first navigation has committed & its loader is stable', async () => {
    let histories = 0;
    const { server } = await fixture((message, send) => {
      if (message.method === 'Page.getNavigationHistory') {
        histories += 1;
        // Nothing committed for the first three polls, then about:blank.
        send({ id: message.id, result: { currentIndex: 0, entries: [{ url: histories <= 3 ? '' : 'about:blank' }] } });
      } else if (message.method === 'Page.getFrameTree') {
        send({ id: message.id, result: { frameTree: { frame: { id: 'main', loaderId: histories <= 3 ? 'initial' : 'blank' } } } });
      } else pageResponder(message, send);
    });
    // Three polls with nothing committed, then two with the same committed loader.
    expect(server.messages.filter((m) => m.method === 'Page.getNavigationHistory')).toHaveLength(5);
  });

  it('newPage does not wait on a browser that never commits or does not answer the history query', async () => {
    const start = Date.now();
    const { page } = await fixture((message, send) => {
      if (message.method === 'Page.getNavigationHistory') send({ id: message.id, error: { code: -32601, message: 'not found' } });
      else pageResponder(message, send);
    });
    expect(Date.now() - start).toBeLessThan(400);
    await page.close();
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

// The attach and connect tests talk to an in-process fixture over loopback and are not about timing out. They used 500 ms,
// and on a windows-2025 runner one `/json/version` request did not answer in that time while another test file was
// starting real Chrome (the whole test took 2.5 s). They now use the package's own default for attach, connect and
// connectPage, which is the same value. That is the
// product default, not a bound measured on runners; vitest's per-test limit is what bounds a test that hangs.
// Tests that are about timing out keep their own short, explicit values.
const FIXTURE_TIMEOUT_MS = 30_000;

describe('attach to an existing page & page.mouse', () => {
  const pageTargets = [
    { targetId: 'app-page', type: 'page', url: 'app://main/', title: 'Pulse' },
    { targetId: 'app-worker', type: 'service_worker', url: 'app://sw.js', title: '' },
  ];
  /** A debugging endpoint: /json/version points at the same port's browser socket. */
  async function appFixture(options: { targets?: unknown[]; webSocketDebuggerUrl?: string; handler?: typeof pageResponder } = {}) {
    let base = '';
    const targets = options.targets ?? pageTargets;
    const server = await mockCdp((message, send) => {
      if (message.method === 'Target.getTargets') send({ id: message.id, result: { targetInfos: targets } });
      else (options.handler ?? pageResponder)(message, send);
    }, {
      http: (path) => path === '/json/version'
        ? { Browser: 'fixture', webSocketDebuggerUrl: options.webSocketDebuggerUrl ?? `${base}/devtools/browser/app` }
        : undefined,
    });
    base = server.endpoint; cleanup.push(server.close);
    return { server, port: Number(new URL(server.endpoint).port) };
  }
  const methods = (server: { messages: any[] }) => server.messages.map((message) => message.method);
  const mouseEvents = (server: { messages: any[] }, from = 0) => server.messages.slice(from).filter((message) => message.method === 'Input.dispatchMouseEvent').map((message) => message.params);

  it('attaches via /json/version without creating a target & lists page targets only', async () => {
    const { server, port } = await appFixture();
    const browser = await QaBrowserSession.attach({ port, timeout: FIXTURE_TIMEOUT_MS }); cleanup.push(() => browser.close().catch(() => {}));
    expect(await browser.pages()).toEqual([{ targetId: 'app-page', url: 'app://main/', title: 'Pulse' }]);
    expect(server.httpRequests).toEqual(['/json/version']);
    expect(methods(server)).toEqual(['Target.getTargets']);
  });

  it('attachPage(targetId) attaches flattened without creating a target & runs page commands', async () => {
    const { server, port } = await appFixture();
    const browser = await QaBrowserSession.attach({ port, timeout: FIXTURE_TIMEOUT_MS }); cleanup.push(() => browser.close().catch(() => {}));
    const page = await browser.attachPage('app-page');
    expect(page.targetId).toBe('app-page');
    expect(await page.evaluate((n: number) => n, 4)).toBe(true);
    expect(methods(server)).not.toContain('Target.createTarget');
    expect(server.messages.find((message) => message.method === 'Target.attachToTarget').params).toEqual({ targetId: 'app-page', flatten: true });
  });

  it('attachPage(predicate) must match exactly one page; unknown ids attach nothing', async () => {
    const { server, port } = await appFixture({ targets: [...pageTargets, { targetId: 'other', type: 'page', url: 'app://other/', title: 'Other' }] });
    const browser = await QaBrowserSession.attach({ port, timeout: FIXTURE_TIMEOUT_MS }); cleanup.push(() => browser.close().catch(() => {}));
    await expect(browser.attachPage((info) => info.url.startsWith('app://'))).rejects.toThrow('matched 2 pages');
    await expect(browser.attachPage((info) => info.title === 'Missing')).rejects.toThrow('matched 0 pages');
    await expect(browser.attachPage('missing')).rejects.toThrow('No page target');
    expect(await browser.attachPage((info) => info.title === 'Other')).toMatchObject({ targetId: 'other' });
    expect(methods(server)).not.toContain('Target.createTarget');
    expect(server.messages.filter((message) => message.method === 'Target.attachToTarget')).toHaveLength(1);
  });

  it('closing an attached page detaches only & never closes the app page', async () => {
    const { server, port } = await appFixture();
    const browser = await QaBrowserSession.attach({ port, timeout: FIXTURE_TIMEOUT_MS }); cleanup.push(() => browser.close().catch(() => {}));
    const page = await browser.attachPage('app-page');
    await page.close(); await page.close();
    expect(server.messages.filter((message) => message.method.startsWith('Target.')).map((message) => message.method))
      .toEqual(['Target.getTargets', 'Target.attachToTarget', 'Target.detachFromTarget']);
    expect(server.messages.find((message) => message.method === 'Target.detachFromTarget').params).toEqual({ sessionId: 'session' });
    expect(methods(server)).not.toContain('Target.closeTarget');
    await expect(page.evaluate('1')).rejects.toThrow('Page closed');
  });

  it('closing the browser session detaches attached pages & never closes app targets', async () => {
    const { server, port } = await appFixture();
    const browser = await QaBrowserSession.attach({ port, timeout: FIXTURE_TIMEOUT_MS });
    await browser.attachPage('app-page');
    await browser.close();
    expect(methods(server)).toContain('Target.detachFromTarget');
    expect(methods(server)).not.toContain('Target.closeTarget');
    await expect(browser.attachPage('app-page')).rejects.toThrow('session closed');
  });

  it('refuses non-loopback hosts & endpoints unless allowRemoteHost, before any request', async () => {
    await expect(QaBrowserSession.attach({ port: 9333, host: '192.168.1.20' })).rejects.toThrow('non-loopback');
    await expect(QaBrowserSession.attach({ httpEndpoint: 'http://example.com:9333' })).rejects.toThrow('non-loopback');
    const { port } = await appFixture({ webSocketDebuggerUrl: 'ws://192.168.1.20:9222/devtools/browser/x' });
    await expect(QaBrowserSession.attach({ port, timeout: FIXTURE_TIMEOUT_MS })).rejects.toThrow('non-loopback');
  });

  it('validates the port, endpoint shape & discovery payload', async () => {
    await expect(QaBrowserSession.attach({ port: 0 })).rejects.toThrow('Port');
    await expect(QaBrowserSession.attach({ port: 70000 })).rejects.toThrow('Port');
    await expect(QaBrowserSession.attach({})).rejects.toThrow('port or httpEndpoint');
    await expect(QaBrowserSession.attach({ port: 9333, httpEndpoint: 'http://127.0.0.1:9333' })).rejects.toThrow('port or httpEndpoint');
    await expect(QaBrowserSession.attach({ httpEndpoint: 'ws://127.0.0.1:9333' })).rejects.toThrow('http');
    const { port } = await appFixture({ webSocketDebuggerUrl: '' });
    await expect(QaBrowserSession.attach({ port, timeout: FIXTURE_TIMEOUT_MS })).rejects.toThrow('webSocketDebuggerUrl');
  });

  it('page.mouse: move, down, move while held (buttons mask) & up send exact CDP params', async () => {
    const { page, server } = await fixture();
    const start = server.messages.length;
    await page.mouse.move(10, 20);
    await page.mouse.down();
    await page.mouse.move(30, 40);
    await page.mouse.up();
    expect(mouseEvents(server, start)).toEqual([
      { type: 'mouseMoved', x: 10, y: 20, button: 'none', buttons: 0 },
      { type: 'mousePressed', x: 10, y: 20, button: 'left', buttons: 1, clickCount: 1 },
      { type: 'mouseMoved', x: 30, y: 40, button: 'left', buttons: 1 },
      { type: 'mouseReleased', x: 30, y: 40, button: 'left', buttons: 0, clickCount: 1 },
    ]);
    expect(page.mouse.position()).toEqual({ x: 30, y: 40 });
  });

  it('page.mouse.move steps interpolate intermediate moves & end on the target', async () => {
    const { page, server } = await fixture();
    await page.mouse.move(0, 0);
    const start = server.messages.length;
    await page.mouse.move(10, 0, { steps: 2 });
    expect(mouseEvents(server, start).map((event) => [event.x, event.y])).toEqual([[5, 0], [10, 0]]);
  });

  it('page.mouse right & middle buttons use their own mask bits & clickCount', async () => {
    const { page, server } = await fixture();
    const start = server.messages.length;
    await page.mouse.down({ button: 'right', clickCount: 2 });
    await page.mouse.up({ button: 'right' });
    await page.mouse.down({ button: 'middle' });
    await page.mouse.up({ button: 'middle' });
    expect(mouseEvents(server, start).map((event) => [event.type, event.button, event.buttons, event.clickCount])).toEqual([
      ['mousePressed', 'right', 2, 2], ['mouseReleased', 'right', 0, 2],
      ['mousePressed', 'middle', 4, 1], ['mouseReleased', 'middle', 0, 1],
    ]);
  });

  it('page.mouse rejects double press, release without press, bad steps, bad coordinates & buttons without sending', async () => {
    const { page, server } = await fixture();
    await page.mouse.down();
    const start = server.messages.length;
    await expect(page.mouse.down()).rejects.toThrow('already pressed');
    await page.mouse.up();
    await expect(page.mouse.up()).rejects.toThrow('not pressed');
    await expect(page.mouse.move(1, 1, { steps: 0 })).rejects.toThrow('steps');
    await expect(page.mouse.move(Number.NaN, 0)).rejects.toThrow('finite');
    await expect(page.mouse.down({ button: 'back' as any })).rejects.toThrow('Unsupported button');
    expect(mouseEvents(server, start).map((event) => event.type)).toEqual(['mouseReleased']);
  });

  it('click & hover keep their exact event sequence and update the tracked mouse position', async () => {
    const { page, server } = await fixture((message, send) => {
      const declaration = String(message.params?.functionDeclaration ?? '');
      if (message.method === 'Runtime.callFunctionOn' && message.params?.objectId === 'el') {
        send({ id: message.id, result: { result: declaration.includes('getClientRects') ? { value: { x: 10, y: 20, width: 40, height: 20 } } : { value: undefined } } });
      } else if (message.method === 'Runtime.callFunctionOn' && declaration.includes('querySelector')) {
        send({ id: message.id, result: { result: { objectId: 'el' } } });
      } else pageResponder(message, send);
    });
    const start = server.messages.length;
    await page.click('#button');
    expect(mouseEvents(server, start)).toEqual([
      { type: 'mouseMoved', x: 30, y: 30 },
      { type: 'mousePressed', x: 30, y: 30, button: 'left', buttons: 1, clickCount: 1 },
      { type: 'mouseReleased', x: 30, y: 30, button: 'left', buttons: 0, clickCount: 1 },
    ]);
    expect(page.mouse.position()).toEqual({ x: 30, y: 30 });
    const hoverStart = server.messages.length;
    await page.hover('#button');
    expect(mouseEvents(server, hoverStart)).toEqual([{ type: 'mouseMoved', x: 30, y: 30 }]);
  });
});

describe('keyboard table: function keys, US symbols & chords', () => {
  /** Records every CDP command QaKeyboard sends, in order. */
  function recorder() {
    const calls: Array<{ method: string; params: any }> = [];
    const keyboard = new QaKeyboard(async (method, params) => { calls.push({ method, params }); return {}; });
    return { calls, keyboard };
  }
  it('sends exact rawKeyDown & keyUp events for F3 and F5', async () => {
    for (const [key, virtualKey] of [['F3', 114], ['F5', 116]] as const) {
      const { calls, keyboard } = recorder();
      await keyboard.press(key);
      expect(calls).toEqual([
        { method: 'Input.dispatchKeyEvent', params: { type: 'rawKeyDown', key, code: key, windowsVirtualKeyCode: virtualKey, modifiers: 0 } },
        { method: 'Input.dispatchKeyEvent', params: { type: 'keyUp', key, code: key, windowsVirtualKeyCode: virtualKey, modifiers: 0 } },
      ]);
    }
  });
  it('maps F1 through F24 to Windows virtual keys 112 through 135 and rejects the rest', () => {
    for (let n = 1; n <= 24; n++) expect(keyDefinition(`F${n}`, 0)).toMatchObject({ key: `F${n}`, code: `F${n}`, windowsVirtualKeyCode: 111 + n });
    for (const key of ['F0', 'F25', 'F01']) expect(() => keyDefinition(key, 0)).toThrow(`Unsupported key: ${key}`);
  });
  it('sends exact events for ? as Shift+Slash with shift modifier and text', async () => {
    const { calls, keyboard } = recorder();
    await keyboard.press('?');
    expect(calls).toEqual([
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyDown', key: '?', code: 'Slash', windowsVirtualKeyCode: 191, modifiers: 8, text: '?', unmodifiedText: '/' } },
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyUp', key: '?', code: 'Slash', windowsVirtualKeyCode: 191, modifiers: 8 } },
    ]);
  });
  it('sends exact events for backslash', async () => {
    const { calls, keyboard } = recorder();
    await keyboard.press('\\');
    expect(calls).toEqual([
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyDown', key: '\\', code: 'Backslash', windowsVirtualKeyCode: 220, modifiers: 0, text: '\\', unmodifiedText: '\\' } },
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyUp', key: '\\', code: 'Backslash', windowsVirtualKeyCode: 220, modifiers: 0 } },
    ]);
  });
  it('sends exact events for the chord Control+/ (no text while a command modifier is held)', async () => {
    const { calls, keyboard } = recorder();
    await keyboard.press('Control+/');
    expect(calls).toEqual([
      { method: 'Input.dispatchKeyEvent', params: { type: 'rawKeyDown', key: 'Control', code: 'ControlLeft', windowsVirtualKeyCode: 17, modifiers: 2 } },
      { method: 'Input.dispatchKeyEvent', params: { type: 'rawKeyDown', key: '/', code: 'Slash', windowsVirtualKeyCode: 191, modifiers: 2 } },
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyUp', key: '/', code: 'Slash', windowsVirtualKeyCode: 191, modifiers: 2 } },
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyUp', key: 'Control', code: 'ControlLeft', windowsVirtualKeyCode: 17, modifiers: 0 } },
    ]);
  });
  it('covers every printable US symbol, shifted and unshifted, with its base key code', () => {
    const table: Array<[string, string, string, number, number, string]> = [
      ['/', 'Slash', '/', 191, 0, '/'], ['?', 'Slash', '/', 191, 8, '?'],
      ['\\', 'Backslash', '\\', 220, 0, '\\'], ['|', 'Backslash', '\\', 220, 8, '|'],
      ['`', 'Backquote', '`', 192, 0, '`'], ['~', 'Backquote', '`', 192, 8, '~'],
      ['-', 'Minus', '-', 189, 0, '-'], ['_', 'Minus', '-', 189, 8, '_'],
      ['=', 'Equal', '=', 187, 0, '='], ['+', 'Equal', '=', 187, 8, '+'],
      [';', 'Semicolon', ';', 186, 0, ';'], [':', 'Semicolon', ';', 186, 8, ':'],
      ["'", 'Quote', "'", 222, 0, "'"], ['"', 'Quote', "'", 222, 8, '"'],
      [',', 'Comma', ',', 188, 0, ','], ['<', 'Comma', ',', 188, 8, '<'],
      ['.', 'Period', '.', 190, 0, '.'], ['>', 'Period', '.', 190, 8, '>'],
      ['[', 'BracketLeft', '[', 219, 0, '['], ['{', 'BracketLeft', '[', 219, 8, '{'],
      [']', 'BracketRight', ']', 221, 0, ']'], ['}', 'BracketRight', ']', 221, 8, '}'],
      ['1', 'Digit1', '1', 49, 0, '1'], ['!', 'Digit1', '1', 49, 8, '!'],
      ['0', 'Digit0', '0', 48, 0, '0'], [')', 'Digit0', '0', 48, 8, ')'],
    ];
    for (const [key, code, unmodifiedText, windowsVirtualKeyCode, modifiers, text] of table) {
      expect(keyDefinition(key, 0)).toMatchObject({ key: modifiers ? text : unmodifiedText, code, windowsVirtualKeyCode, modifiers: modifiers ? 8 : 0 });
      expect(keyDefinition(key, 0).unmodifiedText).toBe(unmodifiedText);
      expect(keyDefinition(key, 0).text).toBe(text);
    }
    // Shift held as its own modifier selects the shifted character of the same key.
    expect(keyDefinition('/', 8)).toMatchObject({ key: '?', code: 'Slash', text: '?', modifiers: 8 });
    expect(() => keyDefinition('€', 0)).toThrow('Unsupported key: €');
  });
  it('treats a trailing + as the plus key, alone and in a chord', async () => {
    const { calls, keyboard } = recorder();
    await keyboard.press('+');
    expect(calls[0].params).toMatchObject({ type: 'keyDown', key: '+', code: 'Equal', windowsVirtualKeyCode: 187, modifiers: 8, text: '+' });
    const chord = recorder();
    await chord.keyboard.press('Control++');
    expect(chord.calls.map((call) => call.params.key)).toEqual(['Control', '+', '+', 'Control']);
    expect(chord.calls[1].params).toMatchObject({ type: 'rawKeyDown', key: '+', code: 'Equal', modifiers: 10 });
  });
});

describe('browser-level escape hatch', () => {
  async function browserFixture(handler: (message: any, send: (message: any) => void) => void) {
    const server = await mockCdp(handler); cleanup.push(server.close);
    const browser = await QaBrowserSession.connect(server.endpoint, { timeout: FIXTURE_TIMEOUT_MS });
    cleanup.push(() => browser.close().catch(() => {}));
    return { server, browser };
  }
  it('send() forwards browser-scoped commands with no sessionId', async () => {
    const { server, browser } = await browserFixture((message, send) => {
      send({ id: message.id, result: message.method === 'Browser.getVersion' ? { product: 'HeadlessChrome/1.0', protocolVersion: '1.3' } : {} });
    });
    expect(await browser.send('Browser.getVersion')).toEqual({ product: 'HeadlessChrome/1.0', protocolVersion: '1.3' });
    expect(server.messages[0]).toEqual({ id: 1, method: 'Browser.getVersion', params: {} });
    await expect(browser.send('')).rejects.toThrow('CDP method must be a non-empty string');
  });
  it('version() returns Browser.getVersion without a sessionId', async () => {
    const { server, browser } = await browserFixture((message, send) => {
      send({ id: message.id, result: { product: 'HeadlessChrome/1.0', userAgent: 'x' } });
    });
    expect(await browser.version()).toMatchObject({ product: 'HeadlessChrome/1.0' });
    expect(server.messages.at(-1)).toEqual({ id: 1, method: 'Browser.getVersion', params: {} });
  });
  it('commandLine() returns the argv from Browser.getBrowserCommandLine', async () => {
    const { server, browser } = await browserFixture((message, send) => {
      send({ id: message.id, result: message.method === 'Browser.getBrowserCommandLine' ? { arguments: ['chrome', '--headless=new', '--enable-automation'] } : {} });
    });
    expect(await browser.commandLine()).toEqual(['chrome', '--headless=new', '--enable-automation']);
    expect(server.messages.at(-1)).toEqual({ id: 1, method: 'Browser.getBrowserCommandLine', params: {} });
  });
  it('commandLine() rejects with a clear --enable-automation error when the browser refuses', async () => {
    const { browser } = await browserFixture((message, send) => {
      send({ id: message.id, error: { code: -32601, message: "'Browser.getBrowserCommandLine' wasn't found" } });
    });
    await expect(browser.commandLine()).rejects.toThrow(/launched with --enable-automation/);
    await expect(browser.commandLine()).rejects.toThrow(/Browser\.getBrowserCommandLine was refused/);
  });
  it('commandLine() rejects a reply that is not an argument list', async () => {
    const { browser } = await browserFixture((message, send) => {
      send({ id: message.id, result: { arguments: 'chrome --headless' } });
    });
    await expect(browser.commandLine()).rejects.toThrow('did not return an argument list');
  });
  it('escape-hatch methods reject after close without sending', async () => {
    const { server, browser } = await browserFixture((message, send) => send({ id: message.id, result: {} }));
    await browser.close();
    const sent = server.messages.length;
    await expect(browser.send('Browser.getVersion')).rejects.toThrow('Browser session closed');
    await expect(browser.version()).rejects.toThrow('Browser session closed');
    await expect(browser.commandLine()).rejects.toThrow('Browser session closed');
    expect(server.messages.length).toBe(sent);
  });
});

describe('0.4.4: console capture, occlusion, fill, wheel, user agent & failure artifacts', () => {
  const sinceParams = (server: { messages: any[] }, method: string, from = 0) =>
    server.messages.slice(from).filter((message) => message.method === method).map((message) => message.params);

  it('records console calls & uncaught exceptions as typed entries', async () => {
    const { page, server } = await fixture();
    server.emit({ method: 'Runtime.consoleAPICalled', sessionId: 'session', params: {
      type: 'error', args: [{ type: 'string', value: 'boom' }, { type: 'number', value: 4 }, { type: 'object', description: 'Object' }],
      timestamp: 1000, stackTrace: { callFrames: [{ url: 'http://fixture/app.js', lineNumber: 4 }] },
    } });
    server.emit({ method: 'Runtime.exceptionThrown', sessionId: 'session', params: {
      timestamp: 1001, exceptionDetails: { text: 'Uncaught', lineNumber: 9, url: 'http://fixture/app.js', exception: { type: 'object', description: 'Error: late' } },
    } });
    await page.evaluate('1');
    expect(page.consoleEntries()).toEqual([
      { type: 'error', text: 'boom 4 Object', url: 'http://fixture/app.js', line: 5, timestamp: 1000 },
      { type: 'exception', text: 'Error: late', url: 'http://fixture/app.js', line: 10, timestamp: 1001 },
    ]);
  });

  it('keeps the newest 200 entries, returns copies and clears on request', async () => {
    const { page, server } = await fixture();
    for (let i = 0; i < 250; i++) server.emit({ method: 'Runtime.consoleAPICalled', sessionId: 'session', params: { type: 'log', args: [{ type: 'string', value: `m${i}` }], timestamp: i } });
    await page.evaluate('1');
    const entries = page.consoleEntries();
    expect(entries).toHaveLength(200);
    expect(entries[0].text).toBe('m50');
    expect(entries.at(-1)?.text).toBe('m249');
    entries[0].text = 'mutated';
    expect(page.consoleEntries()[0].text).toBe('m50');
    page.clearConsoleEntries();
    expect(page.consoleEntries()).toEqual([]);
  });

  it('click({ checkOccluded }) throws ElementOccludedError naming the cover; default click does no occlusion check', async () => {
    let occluder: string | null = '<div id="toast" class="banner">';
    const { page, server } = await fixture((message, send) => {
      const declaration = String(message.params?.functionDeclaration ?? '');
      if (message.method === 'Runtime.callFunctionOn' && message.params?.objectId === 'el') {
        if (declaration.includes('getClientRects')) send({ id: message.id, result: { result: { value: { x: 10, y: 20, width: 40, height: 20 } } } });
        else if (declaration.includes('elementFromPoint')) send({ id: message.id, result: { result: { value: occluder } } });
        else send({ id: message.id, result: { result: { value: undefined } } });
      } else if (message.method === 'Runtime.callFunctionOn' && declaration.includes('querySelector')) {
        send({ id: message.id, result: { result: { objectId: 'el' } } });
      } else pageResponder(message, send);
    });
    const error = await page.click('#button', { checkOccluded: true }).catch((caught) => caught);
    expect(error).toBeInstanceOf(ElementOccludedError);
    expect(error.message).toBe('Click target is covered by <div id="toast" class="banner"> at (30, 30)');
    expect(sinceParams(server, 'Input.dispatchMouseEvent').map((params) => params.type)).toEqual(['mouseMoved']);

    occluder = null;
    let start = server.messages.length;
    await page.click('#button', { checkOccluded: true });
    expect(sinceParams(server, 'Input.dispatchMouseEvent', start).map((params) => params.type)).toEqual(['mouseMoved', 'mousePressed', 'mouseReleased']);

    occluder = '<div id="toast">';
    start = server.messages.length;
    await page.click('#button');
    expect(server.messages.slice(start).some((message) => String(message.params?.functionDeclaration ?? '').includes('elementFromPoint'))).toBe(false);
  });

  it('fill is insert-only by default; replace selects all first; verify reads back and throws on mismatch', async () => {
    let field = 'old', selected = false, selectCalls = 0, readOverride: string | undefined;
    const { page, server } = await fixture((message, send) => {
      const declaration = String(message.params?.functionDeclaration ?? '');
      if (message.method === 'Input.insertText') {
        field = selected ? message.params.text : field + message.params.text; selected = false;
      } else if (message.method === 'Input.dispatchKeyEvent' && message.params.key === 'Backspace' && message.params.type !== 'keyUp') {
        if (selected) field = ''; selected = false;
      }
      if (message.method === 'Runtime.callFunctionOn' && message.params?.objectId === 'el') {
        if (declaration.includes('.select()')) { selectCalls++; selected = true; send({ id: message.id, result: { result: { value: true } } }); }
        else if (declaration.includes('textContent')) send({ id: message.id, result: { result: { value: readOverride ?? field } } });
        else send({ id: message.id, result: { result: { value: undefined } } });
      } else if (message.method === 'Runtime.callFunctionOn' && declaration.includes('querySelector')) {
        send({ id: message.id, result: { result: { objectId: 'el' } } });
      } else pageResponder(message, send);
    });
    await page.fill('input', 'hello');
    expect(field).toBe('oldhello');
    expect(selectCalls).toBe(0);

    field = 'old';
    await page.fill('input', 'new', { replace: true, verify: true });
    expect(field).toBe('new');
    expect(selectCalls).toBe(1);

    readOverride = 'ab';
    await expect(page.fill('input', 'other', { replace: true, verify: true })).rejects.toBeInstanceOf(FillMismatchError);
    await expect(page.fill('input', 'other', { replace: true, verify: true })).rejects.toThrow('fill verification failed');
    readOverride = undefined;

    const start = server.messages.length;
    await expect(page.fill('input', 'x', { verify: true })).rejects.toThrow('requires replace');
    expect(server.messages.length).toBe(start);

    field = 'abc';
    await page.fill('input', '', { replace: true, verify: true });
    expect(field).toBe('');
  });

  it('page.mouse.wheel sends mouseWheel at the tracked position with exact deltas', async () => {
    const { page, server } = await fixture();
    await page.mouse.move(12, 34);
    const start = server.messages.length;
    await page.mouse.wheel({ deltaY: 120 });
    await page.mouse.wheel({ deltaX: -30 });
    expect(sinceParams(server, 'Input.dispatchMouseEvent', start)).toEqual([
      { type: 'mouseWheel', x: 12, y: 34, deltaX: 0, deltaY: 120 },
      { type: 'mouseWheel', x: 12, y: 34, deltaX: -30, deltaY: 0 },
    ]);
    await expect(page.mouse.wheel({})).rejects.toThrow('non-zero');
    await expect(page.mouse.wheel({ deltaY: Number.NaN })).rejects.toThrow('finite');
    expect(page.mouse.position()).toEqual({ x: 12, y: 34 });
  });

  it('setUserAgent sends Emulation.setUserAgentOverride; resetUserAgent restores the captured original and is a no-op when unset', async () => {
    let current = 'Original/1.0';
    const { page, server } = await fixture((message, send) => {
      const declaration = String(message.params?.functionDeclaration ?? '');
      if (message.method === 'Runtime.callFunctionOn' && declaration.includes('navigator.userAgent')) send({ id: message.id, result: { result: { value: current } } });
      else if (message.method === 'Emulation.setUserAgentOverride') { current = message.params.userAgent; send({ id: message.id, result: {} }); }
      else pageResponder(message, send);
    });
    await page.resetUserAgent();
    expect(sinceParams(server, 'Emulation.setUserAgentOverride')).toEqual([]);
    await page.setUserAgent('QA-Agent/2.0', { platform: 'Win32', acceptLanguage: 'en-US' });
    expect(sinceParams(server, 'Emulation.setUserAgentOverride').at(-1)).toEqual({ userAgent: 'QA-Agent/2.0', platform: 'Win32', acceptLanguage: 'en-US' });
    expect(await page.evaluate(() => navigator.userAgent)).toBe('QA-Agent/2.0');
    await page.resetUserAgent();
    expect(sinceParams(server, 'Emulation.setUserAgentOverride').at(-1)).toEqual({ userAgent: 'Original/1.0' });
    expect(await page.evaluate(() => navigator.userAgent)).toBe('Original/1.0');
    const sent = server.messages.length;
    await page.resetUserAgent();
    expect(server.messages.length).toBe(sent);
    await expect(page.setUserAgent('   ')).rejects.toThrow('non-empty');
  });

  it('waitForFunction timeout writes a PNG & JSON dump into the caller directory, including console entries', async () => {
    const dir = await mkdtemp(path.join(tmpdir(), 'rk-qa-artifacts-'));
    cleanup.push(() => rm(dir, { recursive: true, force: true }));
    const { page, server } = await fixture((message, send) => {
      const declaration = String(message.params?.functionDeclaration ?? '');
      if (message.method === 'Runtime.callFunctionOn' && declaration.includes('window.never')) send({ id: message.id, result: { result: { value: false } } });
      else if (message.method === 'Runtime.callFunctionOn' && declaration.includes('location.href')) send({ id: message.id, result: { result: { value: { url: 'http://fixture/hub', title: 'Hub' } } } });
      else if (message.method === 'Page.captureScreenshot') send({ id: message.id, result: { data: Buffer.from([137, 80, 78, 71]).toString('base64') } });
      else pageResponder(message, send);
    });
    server.emit({ method: 'Runtime.consoleAPICalled', sessionId: 'session', params: { type: 'warning', args: [{ type: 'string', value: 'slow render' }], timestamp: 5 } });
    const error = await page.waitForFunction(() => (window as any).never === true, { timeout: 60, polling: 5, failureArtifacts: { directory: dir, label: 'hub wait' } }).catch((caught) => caught);
    expect(error).toBeInstanceOf(TimeoutError);
    expect(error.artifacts.json.startsWith(dir)).toBe(true);
    const dump = JSON.parse(await readFile(error.artifacts.json, 'utf8'));
    expect(dump).toMatchObject({ label: 'hub-wait', url: 'http://fixture/hub', title: 'Hub', screenshot: path.basename(error.artifacts.screenshot) });
    expect(dump.predicate).toContain('window.never');
    expect(dump.console).toEqual([{ type: 'warning', text: 'slow render', timestamp: 5 }]);
    expect((await readFile(error.artifacts.screenshot)).subarray(0, 4)).toEqual(Buffer.from([137, 80, 78, 71]));
  });

  it('failure artifacts refuse relative paths & RIGHTKIT_RUN_ROOT before any CDP work', async () => {
    const root = await mkdtemp(path.join(tmpdir(), 'rk-qa-run-root-'));
    cleanup.push(() => rm(root, { recursive: true, force: true }));
    const previous = process.env.RIGHTKIT_RUN_ROOT;
    process.env.RIGHTKIT_RUN_ROOT = root;
    try {
      const { page, server } = await fixture();
      const sent = server.messages.length;
      await expect(page.waitForFunction(() => false, { failureArtifacts: { directory: path.join(root, 'evidence') } })).rejects.toThrow('outside RIGHTKIT_RUN_ROOT');
      await expect(page.waitForSelector('#missing', { failureArtifacts: { directory: 'relative/dir' } })).rejects.toThrow('absolute path');
      expect(server.messages.length).toBe(sent);
    } finally {
      if (previous === undefined) delete process.env.RIGHTKIT_RUN_ROOT; else process.env.RIGHTKIT_RUN_ROOT = previous;
    }
  });
});

describe('0.4.5: context re-resolution, page.send, setFocusEmulation, onPoll, scroll option, fill verify window', () => {
  const callsOf = (server: { messages: any[] }, method: string) => server.messages.filter((message) => message.method === method);

  it('evaluate re-resolves the main context after the old one is destroyed mid-call and retries on the new one', async () => {
    let first = true;
    const { page, server } = await fixture((message, send) => {
      if (message.method === 'Runtime.callFunctionOn' && message.params?.executionContextId === 13 && first) {
        first = false;
        // A reload: the replacement context is announced, then the call against the old id fails.
        send({ method: 'Runtime.executionContextCreated', sessionId: 'session', params: { context: { id: 30, auxData: { frameId: 'main', isDefault: true } } } });
        send({ id: message.id, error: { message: 'Cannot find context with specified id' } });
      } else pageResponder(message, send);
    });
    expect(await page.evaluate(() => 4)).toBe(true);
    expect(callsOf(server, 'Runtime.callFunctionOn').map((message) => message.params.executionContextId)).toEqual([13, 30]);
  });

  it('evaluate gives up with the last context error once the caller timeout is spent', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Runtime.callFunctionOn') send({ id: message.id, error: { message: 'Cannot find context with specified id' } });
      else pageResponder(message, send);
    }, 200);
    await expect(page.evaluate(() => 1)).rejects.toThrow('Cannot find context');
  });

  it('page.send is a public page-scoped escape hatch; setFocusEmulation sends the typed command', async () => {
    const { page, server } = await fixture();
    expect(await page.send('Emulation.setFocusEmulationEnabled', { enabled: true })).toEqual({});
    expect(server.messages.at(-1)).toMatchObject({ method: 'Emulation.setFocusEmulationEnabled', params: { enabled: true }, sessionId: 'session' });
    await page.setFocusEmulation(false);
    expect(server.messages.at(-1)).toMatchObject({ method: 'Emulation.setFocusEmulationEnabled', params: { enabled: false }, sessionId: 'session' });
    await expect(page.setFocusEmulation('yes' as any)).rejects.toThrow('boolean');
    await expect(page.send('')).rejects.toThrow('non-empty');
  });

  it('onPoll runs once before each predicate poll; a throw rejects with that error and writes artifacts', async () => {
    const dir = await mkdtemp(path.join(tmpdir(), 'rk-qa-onpoll-'));
    cleanup.push(() => rm(dir, { recursive: true, force: true }));
    const log: string[] = [];
    const { page } = await fixture((message, send) => {
      const declaration = String(message.params?.functionDeclaration ?? '');
      if (message.method === 'Runtime.callFunctionOn' && declaration.includes('window.never')) { log.push('predicate'); send({ id: message.id, result: { result: { value: false } } }); }
      else if (message.method === 'Runtime.callFunctionOn' && declaration.includes('location.href')) send({ id: message.id, result: { result: { value: { url: 'http://fixture/poll', title: 'Poll' } } } });
      else if (message.method === 'Page.captureScreenshot') send({ id: message.id, result: { data: Buffer.from([137, 80, 78, 71]).toString('base64') } });
      else pageResponder(message, send);
    });
    const pollError = new Error('host gave up');
    let polls = 0;
    await expect(page.waitForFunction(() => (window as any).never === true, {
      timeout: 2000, polling: 5,
      onPoll: () => { polls++; log.push('poll'); if (polls === 3) throw pollError; },
      failureArtifacts: { directory: dir, label: 'poll' },
    })).rejects.toBe(pollError);
    expect(log).toEqual(['poll', 'predicate', 'poll', 'predicate', 'poll']);
    const files = (await readdir(dir)).filter((file) => file.endsWith('.json'));
    expect(files).toHaveLength(1);
    expect(JSON.parse(await readFile(path.join(dir, files[0]), 'utf8'))).toMatchObject({ url: 'http://fixture/poll' });
  });

  it('waitForSelector calls onPoll on each poll of its predicate', async () => {
    const { page } = await fixture((message, send) => {
      // The selector predicate is the only one that reads getComputedStyle; keep it false so the wait polls.
      if (message.method === 'Runtime.callFunctionOn' && String(message.params?.functionDeclaration ?? '').includes('getComputedStyle')) send({ id: message.id, result: { result: { value: false } } });
      else pageResponder(message, send);
    });
    let polls = 0;
    await expect(page.waitForSelector('#missing', { timeout: 80, onPoll: () => { polls++; } })).rejects.toThrow('Selector #missing');
    expect(polls).toBeGreaterThan(1);
  });

  it('click & hover take scroll.inline; the default stays centre and an unknown value is refused', async () => {
    const { page, server } = await fixture((message, send) => {
      const declaration = String(message.params?.functionDeclaration ?? '');
      if (message.method === 'Runtime.callFunctionOn' && message.params?.objectId === 'el') {
        send({ id: message.id, result: { result: declaration.includes('getClientRects') ? { value: { x: 10, y: 20, width: 40, height: 20 } } : { value: undefined } } });
      } else if (message.method === 'Runtime.callFunctionOn' && declaration.includes('querySelector')) {
        send({ id: message.id, result: { result: { objectId: 'el' } } });
      } else pageResponder(message, send);
    });
    await page.click('#button');
    await page.click('#button', { scroll: { inline: 'nearest' } });
    await page.hover('#button', { scroll: { inline: 'nearest' } });
    const scrolls = callsOf(server, 'Runtime.callFunctionOn').filter((message) => String(message.params.functionDeclaration).includes('scrollIntoView'));
    expect(scrolls.map((message) => message.params.arguments)).toEqual([[{ value: 'center' }], [{ value: 'nearest' }], [{ value: 'nearest' }]]);
    await expect(page.click('#button', { scroll: { inline: 'left' as any } })).rejects.toThrow('scroll.inline');
  });

  it('fill verify { timeoutMs } polls the readback until it matches; verify true stays a single read', async () => {
    let queue: string[] = [];
    let reads = 0;
    const { page } = await fixture((message, send) => {
      const declaration = String(message.params?.functionDeclaration ?? '');
      if (message.method === 'Runtime.callFunctionOn' && message.params?.objectId === 'el') {
        if (declaration.includes('.select()')) send({ id: message.id, result: { result: { value: true } } });
        else if (declaration.includes('textContent')) { reads++; send({ id: message.id, result: { result: { value: queue.length ? queue.shift() : 'hello' } } }); }
        else send({ id: message.id, result: { result: { value: undefined } } });
      } else if (message.method === 'Runtime.callFunctionOn' && declaration.includes('querySelector')) {
        send({ id: message.id, result: { result: { objectId: 'el' } } });
      } else pageResponder(message, send);
    });
    queue = ['', '']; reads = 0;
    await page.fill('input', 'hello', { replace: true, verify: { timeoutMs: 1000 } });
    expect(reads).toBe(3);

    queue = ['', '', 'hello']; reads = 0;
    await expect(page.fill('input', 'hello', { replace: true, verify: true })).rejects.toBeInstanceOf(FillMismatchError);
    expect(reads).toBe(1);

    queue = []; reads = 0;
    await page.fill('input', 'hello', { replace: true, verify: { timeoutMs: 60 } });
    queue = Array.from({ length: 1000 }, () => 'x'); reads = 0;
    await expect(page.fill('input', 'hello', { replace: true, verify: { timeoutMs: 60 } })).rejects.toBeInstanceOf(FillMismatchError);
    expect(reads).toBeGreaterThan(1);
    await expect(page.fill('input', 'hello', { replace: true, verify: { timeoutMs: 0 } })).rejects.toThrow('Timeout');
  });
});

describe('0.4.6: failure artifacts with extra state, page text, active element and an explicit run-root opt-in', () => {
  /** A page whose failure dump has url, title, page text and an active element; the predicate never holds. */
  async function failingPage(text = 'Hub is idle', active: unknown = { tag: 'input', id: 'search', type: 'text' }) {
    return fixture((message, send) => {
      const declaration = String(message.params?.functionDeclaration ?? '');
      if (message.method === 'Runtime.callFunctionOn' && declaration.includes('window.never')) send({ id: message.id, result: { result: { value: false } } });
      else if (message.method === 'Runtime.callFunctionOn' && declaration.includes('innerText')) send({ id: message.id, result: { result: { value: { url: 'http://fixture/hub', title: 'Hub', text, active } } } });
      else if (message.method === 'Page.captureScreenshot') send({ id: message.id, result: { data: Buffer.from([137, 80, 78, 71]).toString('base64') } });
      else pageResponder(message, send);
    });
  }
  async function scratch() {
    const dir = await mkdtemp(path.join(tmpdir(), 'rk-qa-extra-'));
    cleanup.push(() => rm(dir, { recursive: true, force: true }));
    return dir;
  }
  async function dumpOf(error: any) {
    return JSON.parse(await readFile(error.artifacts.json, 'utf8'));
  }

  it('embeds the extra result, page text and the active element descriptor in the JSON dump', async () => {
    const dir = await scratch();
    const { page } = await failingPage();
    const error = await page.waitForFunction(() => (window as any).never === true, { timeout: 60, polling: 5, failureArtifacts: { directory: dir, extra: () => ({ hubState: 'idle', queued: 3 }) } }).catch((caught) => caught);
    const dump = await dumpOf(error);
    expect(dump.extra).toEqual({ hubState: 'idle', queued: 3 });
    expect(dump.extraError).toBeNull();
    expect(dump.pageText).toEqual({ text: 'Hub is idle', truncated: false, length: 11 });
    expect(dump.activeElement).toEqual({ tag: 'input', id: 'search', type: 'text' });
  });

  it('a throwing or async-throwing extra is recorded as an error string and never masks the timeout', async () => {
    const dir = await scratch();
    const { page } = await failingPage();
    const sync = await page.waitForFunction(() => (window as any).never === true, { timeout: 60, polling: 5, failureArtifacts: { directory: dir, extra: () => { throw new Error('state read failed'); } } }).catch((caught) => caught);
    expect(sync).toBeInstanceOf(TimeoutError);
    expect(sync.message).toContain('timed out');
    expect((await dumpOf(sync)).extraError).toContain('state read failed');
    const async = await page.waitForFunction(() => (window as any).never === true, { timeout: 60, polling: 5, failureArtifacts: { directory: dir, extra: async () => { throw new Error('async read failed'); } } }).catch((caught) => caught);
    expect(async).toBeInstanceOf(TimeoutError);
    expect((await dumpOf(async)).extraError).toContain('async read failed');
  });

  it('an extra that never settles is cut off after its bound and recorded as an error', async () => {
    const dir = await scratch();
    const { page } = await failingPage();
    const started = Date.now();
    const error = await page.waitForFunction(() => (window as any).never === true, { timeout: 60, polling: 5, failureArtifacts: { directory: dir, extra: () => new Promise(() => {}) } }).catch((caught) => caught);
    expect(error).toBeInstanceOf(TimeoutError);
    expect((await dumpOf(error)).extraError).toMatch(/timed out/);
    expect(Date.now() - started).toBeLessThan(5_000);
  });

  it('an unserialisable extra (circular) is recorded as an error string', async () => {
    const dir = await scratch();
    const { page } = await failingPage();
    const circular: any = {}; circular.self = circular;
    const error = await page.waitForFunction(() => (window as any).never === true, { timeout: 60, polling: 5, failureArtifacts: { directory: dir, extra: () => circular } }).catch((caught) => caught);
    expect(error).toBeInstanceOf(TimeoutError);
    expect((await dumpOf(error)).extraError).toMatch(/circular/i);
  });

  it('extra and page text are bounded: a large extra is previewed and truncated, long page text is cut', async () => {
    const dir = await scratch();
    const { page } = await failingPage('p'.repeat(30_000));
    const error = await page.waitForFunction(() => (window as any).never === true, { timeout: 60, polling: 5, failureArtifacts: { directory: dir, extra: () => 'e'.repeat(200_000) } }).catch((caught) => caught);
    const dump = await dumpOf(error);
    expect(dump.extra).toMatchObject({ truncated: true, length: 200_002 });
    expect(dump.extra.preview.length).toBeLessThanOrEqual(64 * 1024);
    expect(dump.pageText.truncated).toBe(true);
    expect(dump.pageText.text.length).toBe(20_000);
    expect(dump.pageText.length).toBe(30_000);
  });

  it('a non-function extra is refused before any CDP work', async () => {
    const dir = await scratch();
    const { page, server } = await failingPage();
    const sent = server.messages.length;
    await expect(page.waitForFunction(() => true, { failureArtifacts: { directory: dir, extra: 'nope' as any } })).rejects.toThrow('extra must be a function');
    expect(server.messages.length).toBe(sent);
  });

  it('allowInsideRunRoot opts in to a directory under RIGHTKIT_RUN_ROOT; the default still refuses', async () => {
    const root = await scratch();
    const previous = process.env.RIGHTKIT_RUN_ROOT;
    process.env.RIGHTKIT_RUN_ROOT = root;
    try {
      const { page } = await failingPage();
      const inside = path.join(root, 'evidence');
      await expect(page.waitForFunction(() => false, { failureArtifacts: { directory: inside } })).rejects.toThrow('outside RIGHTKIT_RUN_ROOT');
      const error = await page.waitForFunction(() => (window as any).never === true, { timeout: 60, polling: 5, failureArtifacts: { directory: inside, allowInsideRunRoot: true } }).catch((caught) => caught);
      expect(error).toBeInstanceOf(TimeoutError);
      expect(error.artifacts.json.startsWith(inside)).toBe(true);
    } finally {
      if (previous === undefined) delete process.env.RIGHTKIT_RUN_ROOT; else process.env.RIGHTKIT_RUN_ROOT = previous;
    }
  });
});

describe('0.4.8: page.send child sessions, labelled evaluate & waits, animation frames, returnPath screenshots, page sockets', () => {
  const callsOf = (server: { messages: any[] }, method: string) => server.messages.filter((message) => message.method === method);

  it('page.send with { sessionId, timeout } addresses a child session; the two-argument and numeric forms keep the page session', async () => {
    const { page, server } = await fixture();
    await page.send('Runtime.evaluate', { expression: '1' }, { sessionId: 'child-1', timeout: 400 });
    await page.send('Runtime.evaluate', { expression: '2' });
    await page.send('Runtime.evaluate', { expression: '3' }, 400);
    expect(callsOf(server, 'Runtime.evaluate').map((message) => [message.params.expression, message.sessionId])).toEqual([['1', 'child-1'], ['2', 'session'], ['3', 'session']]);
    await expect(page.send('Runtime.evaluate', {}, { sessionId: '' })).rejects.toThrow('sessionId must be a non-empty string');
  });

  it('page.send { timeout } bounds only that command', async () => {
    const { page } = await fixture((message, send) => { if (message.method !== 'Hang.never') pageResponder(message, send); });
    await expect(page.send('Hang.never', {}, { timeout: 30 })).rejects.toThrow('Hang.never timed out after 30ms');
    expect(await page.send('Runtime.evaluate', { expression: '1' }, { timeout: 400 })).toEqual({ result: { value: true } });
  });

  it('evaluate(expression, { label }) prefixes its error as "<label>: <description>"; without a label the error is unchanged', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Runtime.evaluate') send({ id: message.id, result: { exceptionDetails: { exception: { description: 'ReferenceError: nope is not defined' } } } });
      else pageResponder(message, send);
    });
    await expect(page.evaluate('nope', { label: 'read flag' })).rejects.toThrow(/^read flag: ReferenceError: nope is not defined$/);
    await expect(page.evaluate('nope')).rejects.toThrow(/^ReferenceError: nope is not defined$/);
    await expect(page.evaluate('nope', { label: '' })).rejects.toThrow('evaluate label must be a non-empty string');
  });

  it('a labelled waitForFunction times out with exactly "Timed out waiting for <label>" and wraps the predicate in Boolean()', async () => {
    const { page, server } = await fixture((message, send) => {
      if (message.method === 'Runtime.callFunctionOn') send({ id: message.id, result: { result: { value: false } } });
      else pageResponder(message, send);
    });
    const error = await page.waitForFunction('window.appReady', { label: 'app ready', timeout: 40 }).catch((caught) => caught);
    expect(error).toBeInstanceOf(TimeoutError);
    expect(error.message).toBe('Timed out waiting for app ready');
    const predicate = callsOf(server, 'Runtime.callFunctionOn').at(-1).params;
    expect(predicate.functionDeclaration).toContain('return Boolean(await (');
    expect(predicate.functionDeclaration).toContain('window.appReady');
    // Labelled waits poll on pollMs (default 250 ms), never through the raf fallback.
    expect(callsOf(server, 'Runtime.callFunctionOn').some((message) => String(message.params.functionDeclaration).includes('requestAnimationFrame'))).toBe(false);
  });

  it('a labelled wait keeps polling through "navigated or closed" and resolves once the predicate holds', async () => {
    let attempts = 0;
    const { page } = await fixture((message, send) => {
      if (message.method !== 'Runtime.callFunctionOn') return pageResponder(message, send);
      attempts++;
      if (attempts === 1) send({ id: message.id, error: { message: 'Inspected target navigated or closed' } });
      else send({ id: message.id, result: { result: { value: true } } });
    });
    await page.waitForFunction('window.ready', { label: 'after reload' });
    expect(attempts).toBe(2);
  });

  it('an unlabelled wait still rethrows "navigated or closed"; pollMs and label mistakes are refused before any CDP work', async () => {
    const { page, server } = await fixture((message, send) => {
      if (message.method === 'Runtime.callFunctionOn') send({ id: message.id, error: { message: 'Inspected target navigated or closed' } });
      else pageResponder(message, send);
    });
    await expect(page.waitForFunction('window.ready', { timeout: 200 })).rejects.toThrow('navigated or closed');
    expect(callsOf(server, 'Runtime.callFunctionOn')).toHaveLength(1);
    const sent = server.messages.length;
    await expect(page.waitForFunction('window.ready', { pollMs: 10 })).rejects.toThrow('pollMs requires label');
    await expect(page.waitForFunction('window.ready', { label: 'a', polling: 10 })).rejects.toThrow('not both');
    await expect(page.waitForFunction('window.ready', { label: '' })).rejects.toThrow('label must be a non-empty string');
    expect(server.messages.length).toBe(sent);
  });

  it('waitForAnimationFrames(frames) makes one callFunctionOn with the frame count; the default is two and zero is refused', async () => {
    const { page, server } = await fixture();
    await page.waitForAnimationFrames(3);
    await page.waitForAnimationFrames();
    const calls = callsOf(server, 'Runtime.callFunctionOn');
    expect(calls.map((message) => message.params.arguments)).toEqual([[{ value: 3 }], [{ value: 2 }]]);
    expect(calls[0].params.functionDeclaration).toContain('requestAnimationFrame');
    const sent = server.messages.length;
    await expect(page.waitForAnimationFrames(0)).rejects.toThrow('frames must be a positive integer');
    expect(server.messages.length).toBe(sent);
  });

  it('screenshot keeps the Buffer contract with fromSurface forced; returnPath writes the file and resolves with its path', async () => {
    const dir = await mkdtemp(path.join(tmpdir(), 'rk-qa-shot-'));
    cleanup.push(() => rm(dir, { recursive: true, force: true }));
    const { page, server } = await fixture((message, send) => {
      if (message.method === 'Page.captureScreenshot') send({ id: message.id, result: { data: Buffer.from('png-bytes').toString('base64') } });
      else pageResponder(message, send);
    });
    const buffer = await page.screenshot();
    expect(buffer.toString()).toBe('png-bytes');
    expect(callsOf(server, 'Page.captureScreenshot').at(-1).params).toEqual({ format: 'png', captureBeyondViewport: false, fromSurface: true });
    const plain = path.join(dir, 'plain.png');
    expect(await page.screenshot({ path: plain })).toBeInstanceOf(Buffer);
    expect(await readFile(plain, 'utf8')).toBe('png-bytes');
    const file = path.join(dir, 'returned.png');
    expect(await page.screenshot({ path: file, returnPath: true })).toBe(file);
    expect(await readFile(file, 'utf8')).toBe('png-bytes');
    expect(callsOf(server, 'Page.captureScreenshot').at(-1).params).toEqual({ format: 'png', captureBeyondViewport: false });
    const sent = server.messages.length;
    await expect(page.screenshot({ returnPath: true })).rejects.toThrow('returnPath requires a non-empty path');
    expect(server.messages.length).toBe(sent);
  });

  /** A page-level socket answers as the page itself: no sessions, so its events and replies carry no sessionId. */
  function directPageResponder(message: any, send: (message: any) => void) {
    if (message.method === 'Runtime.enable') send({ method: 'Runtime.executionContextCreated', params: { context: { id: 13, auxData: { frameId: 'main', isDefault: true } } } });
    const result = message.method === 'Page.getFrameTree' ? { frameTree: { frame: { id: 'main' } } }
      : message.method.startsWith('Runtime.call') || message.method === 'Runtime.evaluate' ? { result: { value: true } } : {};
    send({ id: message.id, result });
  }

  it('connectPage(pageSocket) sends no Target command, runs page commands on the root, and close() closes only the socket', async () => {
    const server = await mockCdp(directPageResponder); cleanup.push(server.close);
    const page = await QaBrowserSession.connectPage(`${server.endpoint}/devtools/page/PAGE-1`, { timeout: FIXTURE_TIMEOUT_MS });
    expect(page.targetId).toBe('PAGE-1');
    expect(await page.evaluate(() => 4)).toBe(true);
    expect(server.messages.map((message) => message.method)).toEqual(['Page.enable', 'Page.getFrameTree', 'Runtime.enable', 'Network.enable', 'Page.setLifecycleEventsEnabled', 'Runtime.callFunctionOn']);
    expect(server.messages.some((message) => 'sessionId' in message)).toBe(false);
    await page.close(); await page.close();
    expect(server.messages.some((message) => message.method.startsWith('Target.'))).toBe(false);
    await expect(page.evaluate('1')).rejects.toThrow('Page closed');
  });

  it('connectPage refuses a non-loopback socket and a malformed or non-ws URL before connecting', async () => {
    await expect(QaBrowserSession.connectPage('ws://192.168.1.20:9222/devtools/page/X')).rejects.toThrow('non-loopback');
    await expect(QaBrowserSession.connectPage('http://127.0.0.1:9222/devtools/page/X')).rejects.toThrow('must be a ws or wss URL');
    await expect(QaBrowserSession.connectPage('not a url')).rejects.toThrow('is not a valid URL');
  });
});
