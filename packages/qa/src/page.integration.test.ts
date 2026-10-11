import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { createServer, type Server } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { launchChromeForTesting, type ChromeForTestingBrowser } from '@rightkit/qa-chrome';
import { ElementOccludedError, FillMismatchError, QaBrowserSession, type QaPage, TimeoutError } from './index.js';

// Deliberately no skip: this integration lane requires installed Chrome for Testing.
describe('QaPage with real Chrome for Testing', () => {
  let chrome: ChromeForTestingBrowser;
  let browser: QaBrowserSession;
  let page: QaPage;
  let server: Server;
  let url: string;
  let output: string;
  beforeAll(async () => {
    const html = await readFile(new URL('../test/fixtures/page.html', import.meta.url));
    output = await mkdtemp(path.join(tmpdir(), 'rk-page-test-'));
    server = createServer((_request, response) => {
      response.writeHead(200, { 'Content-Type': 'text/html' }); response.end(html);
    });
    await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
    url = `http://127.0.0.1:${(server.address() as { port: number }).port}`;
    chrome = await launchChromeForTesting();
    browser = await QaBrowserSession.connect(chrome.wsEndpoint, { timeout: 5000 });
  }, 30_000);
  beforeEach(async () => {
    page = await browser.newPage();
    await page.setViewport({ width: 640, height: 480, deviceScaleFactor: 2 });
    await page.evaluateOnNewDocument((value: string) => { (window as any).beforeScripts = value; }, 'initialized');
    await page.goto(url);
  });
  afterEach(async () => { await page?.close(); });
  afterAll(async () => {
    try { await browser?.close(); }
    finally {
      try {
        if (chrome) {
          const report = await chrome.close();
          expect(report.exited).toBe(true); expect(report.profileRemoved).toBe(true);
          expect(existsSync(chrome.profileDir)).toBe(false);
        }
      } finally {
        if (server) { server.closeAllConnections(); await new Promise<void>(resolve => server.close(() => resolve())); }
        if (output) await rm(output, { recursive: true, force: true });
      }
    }
  }, 20_000);

  it('navigates, reloads & observes DOMContentLoaded, load, network idle & same-document navigation', async () => {
    const events: string[] = [];
    const off = page.on('Page.lifecycleEvent', event => events.push(event.name));
    await page.goto(`${url}/second`, { waitUntil: 'domcontentloaded' });
    expect(await page.evaluate('location.pathname')).toBe('/second');
    await page.reload({ waitUntil: 'load' });
    const start = Date.now();
    await page.goto(`${url}/idle`, { waitUntil: 'networkidle0' });
    expect(Date.now() - start).toBeGreaterThanOrEqual(500);
    await page.goto(`${url}/idle#hash`);
    expect(await page.evaluate('location.hash')).toBe('#hash');
    expect(events).toContain('DOMContentLoaded'); expect(events).toContain('load'); off();
  });

  it('sets viewport, DPR & mobile metrics; init scripts precede page scripts', async () => {
    expect(await page.evaluate(() => [innerWidth, innerHeight, devicePixelRatio, (window as any).initSeen])).toEqual([640, 480, 2, 'initialized']);
    await page.evaluateOnNewDocument('window.fromSource = 42');
    await page.reload();
    expect(await page.evaluate('window.fromSource')).toBe(42);
    await page.setViewport({ width: 360, height: 640, deviceScaleFactor: 3, mobile: true });
    expect(await page.evaluate(() => [innerWidth, innerHeight, devicePixelRatio])).toEqual([360, 640, 3]);
  });

  it('evaluates JSON, async functions, strings, element callbacks & exceptions in main default context', async () => {
    expect(await page.evaluate(async (n: number) => ({ result: await Promise.resolve(n * 2), child: (window as any).childOnly ?? false }), 4)).toEqual({ result: 8, child: false });
    expect(await page.evaluate('document.title')).toBe('QaPage fixture');
    expect(await page.$eval('h1', (element, suffix: string) => element.textContent + suffix, '!')).toBe('QaPage fixture!');
    expect(await page.$$eval('.item', (elements, separator: string) => elements.map(element => element.textContent).join(separator), '/')).toBe('one/two');
    expect(await page.evaluate({ result(n: number) { return n + 1; } }.result, 2)).toBe(3);
    await expect(page.evaluate(() => { throw new Error('fixture boom'); })).rejects.toThrow('fixture boom');
    await expect(page.evaluate('throw "string boom"')).rejects.toThrow('string boom');
    await expect(page.$eval('#missing', element => element.id)).rejects.toThrow('#missing');
    await expect(page.evaluate(() => new Promise(() => {}))).rejects.toBeInstanceOf(TimeoutError);
  }, 10_000);

  it('waits for present, visible, hidden & missing selectors; predicates poll by RAF & interval', async () => {
    await (await page.waitForSelector('#hidden'))!.dispose();
    expect(await page.waitForSelector('#hidden', { hidden: true })).toBeNull();
    expect(await page.waitForSelector('#missing', { hidden: true })).toBeNull();
    await page.evaluate(() => { setTimeout(() => { document.querySelector<HTMLElement>('#later')!.hidden = false; }, 40); });
    await (await page.waitForSelector('#later', { visible: true }))!.dispose();
    await page.waitForFunction((text: string) => document.querySelector('#later')?.textContent === text, { polling: 'raf' }, 'Later');
    await page.evaluate(() => { setTimeout(() => { (window as any).ready = true; }, 40); });
    await page.waitForFunction('window.ready', { polling: 10 });
    await expect(page.waitForSelector('#missing', { timeout: 60 })).rejects.toThrow(/Selector #missing/);
    await expect(page.waitForFunction(() => false, { timeout: 60 })).rejects.toThrow(/Predicate.*false/);
  });

  it('creates, evaluates, measures & disposes handles; navigation invalidates references', async () => {
    expect(await page.$('#missing')).toBeNull();
    const handles = await page.$$('.item');
    expect(handles).toHaveLength(2);
    expect(await handles[1].evaluate(element => element.textContent)).toBe('two');
    expect(await handles[0].boundingBox()).toEqual(await handles[0].evaluate(element => {
      const { x, y, width, height } = element.getBoundingClientRect(); return { x, y, width, height };
    }));
    await handles[0].dispose(); await handles[0].dispose();
    await expect(handles[0].evaluate(element => element.id)).rejects.toThrow('disposed');
    const hidden = (await page.$('#hidden'))!;
    expect(await hidden.boundingBox()).toBeNull(); await hidden.dispose();
    await page.reload();
    await expect(handles[1].evaluate(element => element.id)).rejects.toThrow('navigation');
    expect(await page.evaluate('document.title')).toBe('QaPage fixture');
  });

  it('clicks centres after scrolling, supports handles, double/right/middle clicks & trusted events', async () => {
    await page.click('#click', { clickCount: 2, delay: 5 });
    expect(await page.$eval('#click', element => element.textContent)).toBe('Clicked');
    const far = (await page.$('#far'))!;
    await page.click(far);
    expect(await far.evaluate(element => element.textContent)).toBe('Far clicked');
    expect(await page.evaluate('scrollY')).toBeGreaterThan(0);
    await far.click({ button: 'right' }); await far.click({ button: 'middle' }); await far.dispose();
    const events = await page.evaluate('window.events');
    expect(events).toEqual(expect.arrayContaining([
      expect.objectContaining({ type: 'dblclick', id: 'click', trusted: true }),
      expect.objectContaining({ type: 'click', id: 'far', trusted: true }),
      expect.objectContaining({ type: 'contextmenu', id: 'far', trusted: true }),
    ]));
  });

  it('types Unicode, presses editing/navigation keys & modifiers, hovers & focuses through page/handles', async () => {
    await page.type('#text', 'hello', { delay: 2 });
    await page.keyboard.press('Backspace'); await page.keyboard.press('End'); await page.keyboard.press('Space');
    await page.keyboard.press('Shift+a'); await page.keyboard.press('1');
    expect(await page.$eval('#text', element => (element as HTMLInputElement).value)).toBe('hell A1');
    await page.keyboard.press('Home'); await page.keyboard.press('Delete');
    expect(await page.$eval('#text', element => (element as HTMLInputElement).value)).toBe('ell A1');
    await page.keyboard.press('Escape');
    await (await page.waitForSelector('#hidden', { visible: true }))!.dispose();
    const text = (await page.$('#text'))!;
    await text.type('🙂', { delay: 2 }); await text.focus();
    expect(await text.evaluate(element => element === document.activeElement)).toBe(true);
    await page.keyboard.press('Tab');
    expect(await page.evaluate('document.activeElement.id')).toBe('next');
    await page.focus('#text');
    await page.keyboard.press('Shift+ArrowLeft');
    await page.keyboard.press('Enter');
    const hover = (await page.$('#hover'))!;
    await page.hover('#hover'); await hover.hover();
    expect(await hover.evaluate(element => element.textContent)).toBe('Hovered');
    const events = await page.evaluate('window.events');
    expect(events).toEqual(expect.arrayContaining([
      expect.objectContaining({ type: 'input', id: 'text', trusted: true }),
      expect.objectContaining({ type: 'keydown', id: 'text', key: 'Escape', trusted: true }),
      expect.objectContaining({ type: 'mouseover', id: 'hover', trusted: true }),
    ]));
    expect(await text.evaluate(element => (element as HTMLInputElement).value)).toContain('🙂');
    await text.dispose(); await hover.dispose();
  });

  it('attaches to the running page by target id, moves & holds the mouse, and detaches without closing it', async () => {
    const attached = await QaBrowserSession.attach({ port: chrome.port, timeout: 5000 });
    try {
      expect((await attached.pages()).map(info => info.targetId)).toContain(page.targetId);
      const view = await attached.attachPage(page.targetId);
      expect(await view.evaluate('location.href')).toContain(url);
      await page.evaluate(() => {
        const log: string[] = [];
        (window as any).pointer = log;
        for (const type of ['mousedown', 'mousemove', 'mouseup']) document.addEventListener(type, (event: MouseEvent) => log.push(`${type}:${event.buttons}`));
      });
      const centre = await page.$eval('#hover', element => {
        const { x, y, width, height } = element.getBoundingClientRect(); return { x: x + width / 2, y: y + height / 2 };
      });
      await view.mouse.move(centre.x, centre.y, { steps: 3 });
      expect(await page.$eval('#hover', element => element.textContent)).toBe('Hovered');
      await view.mouse.down();
      await view.mouse.move(centre.x + 4, centre.y);
      await view.mouse.up();
      const log = (await page.evaluate('window.pointer')) as string[];
      expect(log.filter(entry => entry !== 'mousemove:0')).toEqual(['mousedown:1', 'mousemove:1', 'mouseup:0']);
      await view.close();
      expect(await page.evaluate('document.title')).toBe('QaPage fixture');
      expect((await attached.pages()).map(info => info.targetId)).toContain(page.targetId);
    } finally { await attached.close(); }
    expect(await page.evaluate('1 + 1')).toBe(2);
  });

  it('selects single/multiple values & dispatches input/change with empty selection support', async () => {
    expect(await page.select('#select', 'two', 'one')).toEqual(['one']);
    expect(await page.select('#select', 'two')).toEqual(['two']);
    expect(await page.$eval('#select', element => (element as HTMLSelectElement).value)).toBe('two');
    expect(await page.select('#select', 'missing')).toEqual([]);
    expect(await page.$eval('#select', element => (element as HTMLSelectElement).selectedIndex)).toBe(-1);
    expect(await page.select('#multi', 'one', 'two')).toEqual(['one', 'two']);
    expect(await page.select('#multi')).toEqual([]);
    const events = await page.evaluate('window.events');
    expect(events).toEqual(expect.arrayContaining([
      expect.objectContaining({ type: 'input', id: 'select' }), expect.objectContaining({ type: 'change', id: 'multi' }),
    ]));
    await expect(page.select('#text', 'one')).rejects.toThrow('not a select');
  });

  it('presses F3, F5, ? and backslash; the page receives the matching key and code', async () => {
    for (const key of ['F3', 'F5', '?', '\\']) await page.keyboard.press(key);
    const events = await page.evaluate('window.events');
    expect(events).toEqual(expect.arrayContaining([
      expect.objectContaining({ type: 'keydown', key: 'F3', code: 'F3', trusted: true }),
      expect.objectContaining({ type: 'keydown', key: 'F5', code: 'F5', trusted: true }),
      expect.objectContaining({ type: 'keydown', key: '?', code: 'Slash', trusted: true }),
      expect.objectContaining({ type: 'keydown', key: '\\', code: 'Backslash', trusted: true }),
    ]));
  });

  it('reads the browser version through the escape hatch; commandLine() needs --enable-automation', async () => {
    const version = await browser.version();
    expect(String(version.product)).toMatch(/Chrome/);
    expect(await browser.send('Browser.getVersion')).toMatchObject({ product: version.product });
    await expect(browser.commandLine()).rejects.toThrow('launched with --enable-automation');
    const automated = await launchChromeForTesting({ args: ['--enable-automation'] });
    const session = await QaBrowserSession.connect(automated.wsEndpoint, { timeout: 5000 });
    try {
      const argv = await session.commandLine();
      expect(argv).toEqual(expect.arrayContaining(['--enable-automation', '--headless=new']));
      expect(argv.some(arg => arg.startsWith('--user-data-dir='))).toBe(true);
    } finally {
      await session.close();
      expect((await automated.close()).exited).toBe(true);
    }
  }, 30_000);

  it('captures viewport PNG at expected DPR size & writes identical bytes', async () => {
    const file = path.join(output, 'viewport.png');
    const png = await page.screenshot({ path: file, type: 'png', captureBeyondViewport: false, fullPage: false });
    expect(Buffer.isBuffer(png)).toBe(true);
    expect(png.subarray(0, 8)).toEqual(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
    expect([png.readUInt32BE(16), png.readUInt32BE(20)]).toEqual([1280, 960]);
    expect(await readFile(file)).toEqual(png);
  });

  it('0.4.4 console: records console.error calls and uncaught exceptions from the real page', async () => {
    await page.evaluate(() => console.error('fixture-error', 7));
    await page.evaluate(() => { setTimeout(() => { throw new Error('late-boom'); }, 0); });
    await new Promise(resolve => setTimeout(resolve, 100));
    const entries = page.consoleEntries();
    expect(entries).toEqual(expect.arrayContaining([expect.objectContaining({ type: 'error', text: 'fixture-error 7' })]));
    expect(entries).toEqual(expect.arrayContaining([expect.objectContaining({ type: 'exception', text: expect.stringContaining('late-boom') })]));
  });

  it('0.4.4 click({ checkOccluded }): names a cover over the target; passes when the hit is the target or a descendant', async () => {
    await page.evaluate(() => {
      const cover = document.createElement('div');
      cover.id = 'cover';
      cover.style.cssText = 'position:fixed;left:0;top:0;width:100vw;height:100vh;z-index:10';
      document.body.append(cover);
    });
    await expect(page.click('#click', { checkOccluded: true })).rejects.toBeInstanceOf(ElementOccludedError);
    await expect(page.click('#click', { checkOccluded: true })).rejects.toThrow(/covered by <div id="cover"/);
    expect(await page.$eval('#click', element => element.textContent)).toBe('Click');
    await page.evaluate(() => {
      document.querySelector('#cover')!.remove();
      document.querySelector('#click')!.innerHTML = '<span id="inner" style="display:block;width:100%;height:100%">Click</span>';
    });
    await page.click('#click', { checkOccluded: true });
    expect(await page.$eval('#click', element => element.textContent)).toBe('Clicked');
  });

  it('0.4.4 click({ checkOccluded }) passes when the hit is inside a shadow root hosted by the target', async () => {
    await page.evaluate(() => {
      const button = document.querySelector('#click')!;
      button.innerHTML = '<span id="host" style="display:block;width:100%;height:100%"></span>';
      document.querySelector('#host')!.attachShadow({ mode: 'open' }).innerHTML = '<i id="deep" style="display:block;width:100%;height:100%">deep</i>';
    });
    await page.click('#click', { checkOccluded: true });
    expect(await page.$eval('#click', element => element.textContent)).toBe('Clicked');
  });

  it('0.4.4 fill: insert-only default, replace+verify, empty clears, and non-editable targets are refused', async () => {
    const value = () => page.$eval('#text', element => (element as HTMLInputElement).value);
    await page.fill('#text', 'first');
    expect(await value()).toBe('first');
    await page.fill('#text', 'second');
    expect(await value()).toBe('firstsecond');
    await page.fill('#text', 'third', { replace: true, verify: true });
    expect(await value()).toBe('third');
    await page.fill('#text', '', { replace: true, verify: true });
    expect(await value()).toBe('');
    await expect(page.fill('#hover', 'x', { replace: true })).rejects.toThrow('not an editable');
    await expect(page.fill('#text', 'x', { verify: true })).rejects.toThrow('requires replace');
  });

  it('0.4.4 mouse.wheel scrolls the element under the tracked pointer', async () => {
    await page.evaluate(() => {
      const box = document.createElement('div');
      box.id = 'scroller';
      box.style.cssText = 'height:100px;overflow:auto;width:300px';
      box.innerHTML = '<div style="height:2000px"></div>';
      document.querySelector('main')!.append(box);
    });
    const centre = await page.$eval('#scroller', element => {
      element.scrollIntoView({ block: 'center', behavior: 'instant' });
      const { x, y, width, height } = element.getBoundingClientRect();
      return { x: x + width / 2, y: y + height / 2 };
    });
    await page.mouse.move(centre.x, centre.y);
    await page.mouse.wheel({ deltaY: 200 });
    await page.waitForFunction("document.querySelector('#scroller').scrollTop > 0", { timeout: 2000 });
    expect(await page.$eval('#scroller', element => element.scrollTop)).toBeGreaterThan(0);
  });

  it('0.4.4 user agent: setUserAgent changes navigator; resetUserAgent restores the default', async () => {
    const originalAgent = await page.evaluate(() => navigator.userAgent);
    const originalPlatform = await page.evaluate(() => navigator.platform);
    await page.setUserAgent('RightKit-QA/9.9 (Test)', { platform: 'Win32' });
    expect(await page.evaluate(() => navigator.userAgent)).toBe('RightKit-QA/9.9 (Test)');
    expect(await page.evaluate(() => navigator.platform)).toBe('Win32');
    await page.resetUserAgent();
    expect(await page.evaluate(() => navigator.userAgent)).toBe(originalAgent);
    expect(await page.evaluate(() => navigator.platform)).toBe(originalPlatform);
  });

  it('0.4.4 failure artifacts: a waitForFunction timeout writes a real PNG and JSON state dump', async () => {
    await page.evaluate(() => console.error('before-timeout'));
    const error = await page.waitForFunction('window.neverSet === true', { timeout: 300, failureArtifacts: { directory: output, label: 'never' } }).catch(caught => caught);
    expect(error).toBeInstanceOf(TimeoutError);
    const dump = JSON.parse(await readFile(error.artifacts.json, 'utf8'));
    expect(dump.url).toContain(url);
    expect(dump.title).toBe('QaPage fixture');
    expect(dump.predicate).toContain('neverSet');
    expect(dump.console).toEqual(expect.arrayContaining([expect.objectContaining({ type: 'error', text: 'before-timeout' })]));
    expect((await readFile(error.artifacts.screenshot)).subarray(0, 8)).toEqual(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
  });

  it('0.4.5 waitForFunction survives location.reload(): the poll retries on the new main context', async () => {
    await page.evaluateOnNewDocument('window.afterReload = true');
    const waiting = page.waitForFunction('window.afterReload === true', { timeout: 5000, polling: 10 });
    await page.reload();
    await waiting;
    expect(await page.evaluate('window.afterReload')).toBe(true);
  });

  it('0.4.5 evaluate works on the new context after a reload', async () => {
    await page.reload();
    expect(await page.evaluate('2 + 2')).toBe(4);
  });

  it('0.4.5 onPoll runs on each poll; a throw rejects the wait with that error', async () => {
    let polls = 0;
    await expect(page.waitForFunction('false', { timeout: 300, polling: 20, onPoll: () => { polls++; } })).rejects.toThrow(/Predicate/);
    expect(polls).toBeGreaterThan(3);
    const gaveUp = new Error('host gave up');
    await expect(page.waitForFunction('false', { timeout: 2000, polling: 10, onPoll: () => { throw gaveUp; } })).rejects.toBe(gaveUp);
  });

  it('0.4.5 fill verify { timeoutMs } waits out a field that is cleared and restored; verify true does not', async () => {
    await page.evaluate(() => {
      for (const id of ['#text', '#next']) {
        const input = document.querySelector(id) as HTMLInputElement;
        input.addEventListener('input', () => {
          const typed = input.value;
          input.value = '';
          setTimeout(() => { input.value = typed; }, 80);
        });
      }
    });
    await page.fill('#text', 'delayed', { replace: true, verify: { timeoutMs: 2000 } });
    expect(await page.$eval('#text', element => (element as HTMLInputElement).value)).toBe('delayed');
    await expect(page.fill('#next', 'instant', { replace: true, verify: true })).rejects.toBeInstanceOf(FillMismatchError);
    await new Promise(resolve => setTimeout(resolve, 120));
  });

  it('0.4.5 scroll.inline nearest puts the target at the edge; the default centres it', async () => {
    await page.evaluate(() => {
      const box = document.createElement('div');
      box.id = 'wide';
      box.style.cssText = 'width:200px;overflow-x:auto;white-space:nowrap';
      box.innerHTML = '<span style="display:inline-block;width:1000px;height:20px"></span><span id="target" style="display:inline-block;width:20px;height:20px;background:#f00"></span><span style="display:inline-block;width:2000px;height:20px"></span>';
      document.querySelector('main')!.append(box);
    });
    const scrollLeft = () => page.$eval('#wide', element => (element as HTMLElement).scrollLeft);
    await page.click('#target');
    const centred = await scrollLeft();
    await page.$eval('#wide', element => { (element as HTMLElement).scrollLeft = 0; });
    await page.click('#target', { scroll: { inline: 'nearest' } });
    const nearest = await scrollLeft();
    expect(centred).toBeGreaterThan(nearest);
    expect(nearest).toBeGreaterThan(0);
  });

  it('0.4.5 page.send runs a raw page command; setFocusEmulation toggles the emulation', async () => {
    const reply = await page.send('Runtime.evaluate', { expression: '21 * 2', returnByValue: true });
    expect(reply.result.value).toBe(42);
    // The retry in evaluate keys on this message; Chrome must still send it for a missing context.
    await expect(page.send('Runtime.evaluate', { expression: '1', contextId: 999999 })).rejects.toThrow('Cannot find context with specified id');
    await page.setFocusEmulation(true);
    await page.setFocusEmulation(false);
    await expect(page.setFocusEmulation(1 as any)).rejects.toThrow('boolean');
  });

  it('0.4.6 failure dump: real page text, the focused element descriptor (no value) and the extra callback', async () => {
    await page.type('#text', 'secret-value');
    await page.focus('#text');
    const error = await page.waitForFunction('window.neverSet === true', { timeout: 300, failureArtifacts: { directory: output, label: 'state', extra: () => ({ step: 'typed' }) } }).catch(caught => caught);
    expect(error).toBeInstanceOf(TimeoutError);
    const dump = JSON.parse(await readFile(error.artifacts.json, 'utf8'));
    expect(dump.pageText.text).toContain('QaPage fixture');
    expect(dump.pageText.truncated).toBe(false);
    expect(dump.activeElement).toMatchObject({ tag: 'input', id: 'text' });
    expect(JSON.stringify(dump)).not.toContain('secret-value');
    expect(dump.extra).toEqual({ step: 'typed' });
    expect(dump.extraError).toBeNull();
  });
});
