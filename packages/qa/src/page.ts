import { writeFile } from 'node:fs/promises';
import { CdpClient, TimeoutError, validateTimeout, type CdpParams, type QaConnectOptions } from './cdp.js';
import { QaKeyboard } from './keyboard.js';
export { TimeoutError, type QaConnectOptions } from './cdp.js';
export { QaKeyboard } from './keyboard.js';

export interface NavigationOptions { waitUntil?: 'load' | 'domcontentloaded' | 'networkidle0'; timeout?: number }
export interface ViewportOptions { width: number; height: number; deviceScaleFactor?: number; mobile?: boolean }
export interface WaitOptions { timeout?: number; polling?: 'raf' | number }
export interface SelectorWaitOptions { visible?: boolean; hidden?: boolean; timeout?: number }
export interface ClickOptions { button?: 'left' | 'right' | 'middle'; clickCount?: number; delay?: number }
export interface TypeOptions { delay?: number }
export interface ScreenshotOptions { path?: string; type?: 'png'; captureBeyondViewport?: false; fullPage?: false }
export interface BoundingBox { x: number; y: number; width: number; height: number }
type PageFunction<Args extends unknown[], Result> = (...args: Args) => Result;
const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
function functionSource(fn: Function): string {
  const source = fn.toString();
  if (source.includes('[native code]')) throw new Error('Cannot evaluate native function');
  // Parse only (never execute) to distinguish methods from async arrow functions.
  try { new Function(`return (${source}\n)`); }
  catch {
    const declaration = source.startsWith('async ') ? source.replace(/^async\s+/, 'async function ') : `function ${source}`;
    new Function(`return (${declaration}\n)`);
    return declaration;
  }
  return source;
}
function argument(value: unknown): CdpParams {
  if (value === undefined) return {};
  if (typeof value === 'number' && (!Number.isFinite(value) || Object.is(value, -0))) return { unserializableValue: Object.is(value, -0) ? '-0' : String(value) };
  if (typeof value === 'bigint') return { unserializableValue: `${value}n` };
  return { value };
}
function resultValue(result: CdpParams): any {
  if (result.exceptionDetails) {
    throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.exception?.value ?? result.exceptionDetails.text ?? 'JavaScript evaluation failed');
  }
  return result.result?.value;
}

/** Owns only targets it creates; launcher owns browser process & profile cleanup. */
export class QaBrowserSession {
  private pages = new Set<QaPage>();
  private closed = false;
  private closing?: Promise<void>;
  private opening = new Set<Promise<QaPage>>();
  private constructor(private client: CdpClient) {}
  static async connect(wsEndpoint: string, options: QaConnectOptions = {}): Promise<QaBrowserSession> {
    return new QaBrowserSession(await CdpClient.connect(wsEndpoint, options));
  }
  newPage(): Promise<QaPage> {
    if (this.closed) return Promise.reject(new Error('Browser session closed'));
    const pending = this.createPage();
    this.opening.add(pending);
    void pending.then(() => this.opening.delete(pending), () => this.opening.delete(pending));
    return pending;
  }
  private async createPage(): Promise<QaPage> {
    const { targetId } = await this.client.send('Target.createTarget', { url: 'about:blank' });
    try {
      const { sessionId } = await this.client.send('Target.attachToTarget', { targetId, flatten: true });
      const page = new QaPage(this.client, targetId, sessionId, () => this.pages.delete(page));
      this.pages.add(page);
      await page.initialize();
      if (this.closed) throw new Error('Browser session closed');
      return page;
    } catch (error) {
      await this.client.send('Target.closeTarget', { targetId }).catch(() => {});
      for (const page of this.pages) if (page.targetId === targetId) page.markClosed();
      throw error;
    }
  }
  close(): Promise<void> {
    if (this.closing) return this.closing;
    this.closed = true;
    this.closing = (async () => {
      try {
        await Promise.allSettled([...this.opening]);
        const results = await Promise.allSettled([...this.pages].map((page) => page.close()));
        const failed = results.find((result) => result.status === 'rejected');
        if (failed?.status === 'rejected') throw failed.reason;
      } finally { this.client.close(); }
    })();
    return this.closing;
  }
}

export class QaPage {
  readonly keyboard: QaKeyboard;
  private frameId?: string;
  private contexts = new Map<number, CdpParams>();
  private contextId?: number;
  private handles = new Set<QaElement>();
  private unsubs: (() => void)[] = [];
  private subscriptions = new Set<() => void>();
  private closed?: Error;
  private closing?: Promise<void>;
  private closedListeners = new Set<(error: Error) => void>();
  private requests = new Map<string, { frameId?: string; loaderId?: string }>();
  private frameParents = new Map<string, string>();
  private lastNetworkActivity = 0;
  private generation = 0;
  private navigating = false;
  private get objectGroup() { return `qa-elements-${this.generation}`; }
  /** @internal Use QaBrowserSession.newPage(). */
  constructor(private client: CdpClient, readonly targetId: string, private sessionId: string, private onClosed: () => void) {
    this.keyboard = new QaKeyboard((method, params) => this.send(method, params));
    this.unsubs.push(
      this.on('Runtime.executionContextCreated', ({ context }) => { this.contexts.set(context.id, context); this.chooseContext(); }),
      this.on('Runtime.executionContextDestroyed', ({ executionContextId }) => {
        this.contexts.delete(executionContextId);
        if (executionContextId === this.contextId) { this.invalidateHandles(); this.chooseContext(); }
      }),
      this.on('Runtime.executionContextsCleared', () => { this.contexts.clear(); this.contextId = undefined; this.invalidateHandles(); }),
      this.on('Page.frameNavigated', ({ frame }) => {
        if (frame.parentId) this.frameParents.set(frame.id, frame.parentId);
        else { this.frameId = frame.id; this.frameParents.clear(); this.invalidateHandles(); this.chooseContext(); }
        // Chrome can abandon old-document favicon/fetch requests without a terminal event.
        // New document requests have already started by frameNavigated; preserve that loader.
        for (const [id, request] of this.requests) {
          if ((!frame.parentId || request.frameId === frame.id) && request.loaderId !== frame.loaderId) this.requests.delete(id);
        }
      }),
      this.on('Page.frameAttached', ({ frameId, parentFrameId }) => this.frameParents.set(frameId, parentFrameId)),
      this.on('Page.frameDetached', ({ frameId, reason }) => {
        if (reason === 'swap') return;
        const removed = new Set<string>([frameId]);
        for (const id of removed) for (const [child, parent] of this.frameParents) if (parent === id) removed.add(child);
        for (const id of removed) this.frameParents.delete(id);
        for (const [id, request] of this.requests) if (request.frameId && removed.has(request.frameId)) this.requests.delete(id);
      }),
      this.on('Network.requestWillBeSent', ({ requestId, frameId, loaderId }) => { this.requests.set(requestId, { frameId, loaderId }); this.lastNetworkActivity = Date.now(); }),
      this.on('Network.loadingFinished', ({ requestId }) => { this.requests.delete(requestId); this.lastNetworkActivity = Date.now(); }),
      this.on('Network.loadingFailed', ({ requestId }) => { this.requests.delete(requestId); this.lastNetworkActivity = Date.now(); }),
      client.on('Disconnected', ({ error }) => this.markClosed(error)),
      client.on('Target.detachedFromTarget', ({ sessionId }) => { if (sessionId === this.sessionId) this.markClosed(); }),
      this.on('Inspector.detached', () => this.markClosed()),
    );
  }
  private chooseContext() {
    this.contextId = [...this.contexts.values()].reverse().find((context) => context.auxData?.isDefault && context.auxData.frameId === this.frameId)?.id;
  }
  /** Subscribe to this page's raw CDP events; returned function unsubscribes. */
  on(method: string, listener: (params: CdpParams) => void): () => void {
    if (this.closed) throw this.closed;
    const off = this.client.on(method, listener, this.sessionId);
    const unsubscribe = () => { off(); this.subscriptions.delete(unsubscribe); };
    this.subscriptions.add(unsubscribe);
    return unsubscribe;
  }
  /** @internal */
  async initialize() {
    await this.send('Page.enable');
    const { frameTree } = await this.send('Page.getFrameTree');
    this.frameId = frameTree.frame.id; this.chooseContext();
    await this.send('Runtime.enable');
    await this.send('Network.enable');
    await this.send('Page.setLifecycleEventsEnabled', { enabled: true });
  }
  /** @internal */
  send(method: string, params: CdpParams = {}, timeout?: number): Promise<CdpParams> {
    if (this.closed) return Promise.reject(this.closed);
    return this.client.send(method, params, this.sessionId, timeout);
  }
  private invalidateHandles() {
    const objectGroup = this.objectGroup;
    this.generation++;
    for (const handle of this.handles) handle.invalidate();
    this.handles.clear();
    // Navigation destroys the context; explicitly release any surviving remote references.
    if (!this.closed) void this.send('Runtime.releaseObjectGroup', { objectGroup }).catch(() => {});
  }
  /** @internal */
  markClosed(error = new Error('Page closed')) {
    if (this.closed) return;
    this.closed = error; this.invalidateHandles();
    this.client.rejectSession(this.sessionId, error);
    for (const unsubscribe of this.unsubs.splice(0)) unsubscribe();
    for (const unsubscribe of this.subscriptions) unsubscribe();
    for (const listener of this.closedListeners) listener(error);
    this.closedListeners.clear(); this.onClosed();
  }
  close(): Promise<void> {
    if (this.closing) return this.closing;
    if (this.closed) return Promise.resolve();
    this.closing = (async () => {
      try { await this.client.send('Target.closeTarget', { targetId: this.targetId }); }
      finally { this.markClosed(); }
    })();
    return this.closing;
  }
  private bounded<T>(label: string, timeout: number, work: (remaining: () => number, stopped: () => boolean) => Promise<T>): Promise<T> {
    if (this.closed) return Promise.reject(this.closed);
    validateTimeout(timeout);
    const deadline = Date.now() + timeout;
    let stopped = false;
    return new Promise<T>((resolve, reject) => {
      const finish = (error?: unknown, value?: T) => {
        if (stopped) return;
        stopped = true; clearTimeout(timer); this.closedListeners.delete(onClose);
        if (error) reject(error); else resolve(value!);
      };
      const onClose = (error: Error) => finish(error);
      this.closedListeners.add(onClose);
      const timer = setTimeout(() => finish(new TimeoutError(`${label} timed out after ${timeout}ms`)), timeout);
      work(() => Math.max(1, deadline - Date.now()), () => stopped).then((value) => finish(undefined, value), (error) => finish(error));
    });
  }
  private async mainContext(timeout = this.client.timeout): Promise<number> {
    return this.bounded('Main execution context', timeout, async (_remaining, stopped) => {
      while (this.contextId === undefined) { if (stopped()) throw new Error('Context wait cancelled'); await sleep(10); }
      return this.contextId;
    });
  }
  private async navigate(method: string, params: CdpParams, options: NavigationOptions): Promise<void> {
    if (this.navigating) throw new Error('Navigation already in progress');
    const waitUntil = options.waitUntil ?? 'load';
    if (!['load', 'domcontentloaded', 'networkidle0'].includes(waitUntil)) throw new Error(`Unsupported waitUntil: ${waitUntil}`);
    const timeout = validateTimeout(options.timeout ?? this.client.timeout);
    this.navigating = true;
    const events: CdpParams[] = [];
    let sameDocument = false;
    let loaderId: string | undefined;
    const off = this.on('Page.lifecycleEvent', (event) => events.push(event));
    const offSame = this.on('Page.navigatedWithinDocument', ({ frameId }) => { if (frameId === this.frameId) sameDocument = true; });
    try {
      await this.bounded(`Navigation (${waitUntil})`, timeout, async (remaining, stopped) => {
        const response = await this.send(method, params, remaining());
        if (response.errorText) throw new Error(`Navigation failed: ${response.errorText}`);
        loaderId = response.loaderId;
        if (method === 'Page.navigate' && !loaderId) sameDocument = true;
        let idleSince: number | undefined;
        while (!stopped()) {
          const name = waitUntil === 'domcontentloaded' ? 'DOMContentLoaded' : 'load';
          // Reload's loader is learned from init, never a previous document's load event.
          if (!loaderId && !sameDocument) loaderId = events.find((event) => event.frameId === this.frameId && event.name === 'init')?.loaderId;
          const ready = sameDocument || events.some((event) => event.frameId === this.frameId && event.loaderId === loaderId && event.name === name);
          if (ready && waitUntil !== 'networkidle0') return;
          const committed = sameDocument || !!loaderId;
          if (waitUntil === 'networkidle0' && committed && !this.requests.size) {
            idleSince = Math.max(idleSince ?? Date.now(), this.lastNetworkActivity);
            if (Date.now() - idleSince >= 500) return;
          } else idleSince = undefined;
          await sleep(Math.min(20, remaining()));
        }
      });
    } finally { off(); offSame(); this.navigating = false; }
  }
  goto(url: string, options: NavigationOptions = {}): Promise<void> { return this.navigate('Page.navigate', { url }, options); }
  reload(options: NavigationOptions = {}): Promise<void> { return this.navigate('Page.reload', {}, options); }
  async setViewport(options: ViewportOptions): Promise<void> {
    await this.send('Emulation.setDeviceMetricsOverride', { ...options, deviceScaleFactor: options.deviceScaleFactor ?? 1, mobile: options.mobile ?? false });
  }
  async evaluateOnNewDocument<Args extends unknown[]>(fn: PageFunction<Args, unknown> | string, ...args: Args): Promise<void> {
    const source = typeof fn === 'string' ? fn : `(${functionSource(fn)})(...${JSON.stringify(args)})`;
    await this.send('Page.addScriptToEvaluateOnNewDocument', { source });
  }
  private async run(fn: Function | string, args: unknown[], returnByValue: boolean, timeout = this.client.timeout): Promise<CdpParams> {
    const deadline = Date.now() + timeout;
    const contextId = await this.mainContext(timeout);
    timeout = Math.max(1, deadline - Date.now());
    const common = { returnByValue, awaitPromise: true, objectGroup: this.objectGroup, timeout };
    const result = typeof fn === 'string'
      ? await this.send('Runtime.evaluate', { expression: fn, contextId, ...common }, timeout)
      : await this.send('Runtime.callFunctionOn', { functionDeclaration: functionSource(fn), executionContextId: contextId, arguments: args.map(argument), ...common }, timeout);
    resultValue(result); return result;
  }
  async evaluate<Args extends unknown[], Result>(fn: PageFunction<Args, Result> | string, ...args: Args): Promise<Awaited<Result>> {
    return resultValue(await this.run(fn, args, true));
  }
  /** @internal */
  async callElement<Args extends unknown[], Result>(handle: QaElement, fn: (element: HTMLElement, ...args: Args) => Result, args: Args): Promise<Awaited<Result>> {
    handle.assertUsable(this);
    const result = await this.send('Runtime.callFunctionOn', {
      objectId: handle.objectId, functionDeclaration: `function(...args) { return (${functionSource(fn)})(this, ...args); }`,
      arguments: args.map(argument), returnByValue: true, awaitPromise: true,
    });
    return resultValue(result);
  }
  private handle(objectId: string): QaElement {
    const handle = new QaElement(this, objectId, () => this.handles.delete(handle));
    this.handles.add(handle); return handle;
  }
  async $(selector: string): Promise<QaElement | null> {
    return this.lookup(selector);
  }
  private async lookup(selector: string, timeout = this.client.timeout): Promise<QaElement | null> {
    const generation = this.generation;
    const { result } = await this.run((selector: string) => document.querySelector(selector), [selector], false, timeout);
    if (generation !== this.generation) throw new Error('Element lookup interrupted by navigation');
    return result.objectId ? this.handle(result.objectId) : null;
  }
  async $$(selector: string): Promise<QaElement[]> {
    const generation = this.generation;
    const { result } = await this.run((selector: string) => Array.from(document.querySelectorAll(selector)), [selector], false);
    if (!result.objectId) return [];
    try {
      const { result: properties } = await this.send('Runtime.getProperties', { objectId: result.objectId, ownProperties: true });
      if (generation !== this.generation) throw new Error('Element lookup interrupted by navigation');
      return properties.filter((property: CdpParams) => /^\d+$/.test(property.name) && property.value?.objectId).map((property: CdpParams) => this.handle(property.value.objectId));
    } finally { await this.send('Runtime.releaseObject', { objectId: result.objectId }).catch(() => {}); }
  }
  async $eval<Args extends unknown[], Result>(selector: string, fn: (element: HTMLElement, ...args: Args) => Result, ...args: Args): Promise<Awaited<Result>> {
    const handle = await this.requireElement(selector);
    try { return await handle.evaluate(fn, ...args); } finally { await handle.dispose(); }
  }
  async $$eval<Args extends unknown[], Result>(selector: string, fn: (elements: HTMLElement[], ...args: Args) => Result, ...args: Args): Promise<Awaited<Result>> {
    const source = `function(selector, ...args) { return (${functionSource(fn)})(Array.from(document.querySelectorAll(selector)), ...args); }`;
    return resultValue(await this.runSourceFunction(source, [selector, ...args]));
  }
  private async runSourceFunction(source: string, args: unknown[], timeout = this.client.timeout) {
    const deadline = Date.now() + timeout;
    const executionContextId = await this.mainContext(timeout);
    timeout = Math.max(1, deadline - Date.now());
    const result = await this.send('Runtime.callFunctionOn', { functionDeclaration: source, executionContextId, arguments: args.map(argument), returnByValue: true, awaitPromise: true, timeout }, timeout);
    resultValue(result); return result;
  }
  async waitForFunction<Args extends unknown[]>(fn: PageFunction<Args, unknown> | string, options: WaitOptions = {}, ...args: Args): Promise<void> {
    const polling = options.polling ?? 'raf';
    if (typeof polling === 'number' && (!(polling > 0) || !Number.isFinite(polling))) throw new Error('Polling interval must be positive');
    const source = typeof fn === 'string' ? `function() { return (${fn}); }` : functionSource(fn);
    await this.bounded(`Predicate ${source}`, options.timeout ?? this.client.timeout, async (remaining, stopped) => {
      while (!stopped()) {
        try {
          if (resultValue(await this.runSourceFunction(source, args, remaining()))) return;
          if (polling === 'raf') {
            // A browser animation frame, bounded fallback for background targets.
            await this.runSourceFunction('function() { return new Promise(resolve => { const timer = setTimeout(resolve, 100); requestAnimationFrame(() => { clearTimeout(timer); resolve(); }); }); }', [], remaining());
          } else await sleep(Math.min(polling, remaining()));
        } catch (error) {
          if (error instanceof TimeoutError) { await sleep(remaining()); continue; }
          if (!/context.*(destroyed|not found)|Cannot find context/i.test(String(error))) throw error;
          await sleep(Math.min(10, remaining()));
        }
      }
    });
  }
  async waitForSelector(selector: string, options: SelectorWaitOptions = {}): Promise<QaElement | null> {
    if (options.visible && options.hidden) throw new Error('visible & hidden cannot both be true');
    try {
      return await this.bounded(`Selector ${selector}`, options.timeout ?? this.client.timeout, async remaining => {
        await this.waitForFunction((selector: string, visible: boolean, hidden: boolean) => {
          const element = document.querySelector(selector);
          if (!element) return hidden;
          const style = getComputedStyle(element), box = element.getBoundingClientRect();
          const shown = style.visibility !== 'hidden' && style.visibility !== 'collapse' && box.width > 0 && box.height > 0;
          return hidden ? !shown : visible ? shown : true;
        }, { timeout: remaining() }, selector, !!options.visible, !!options.hidden);
        return options.hidden ? null : this.lookup(selector, remaining());
      });
    } catch (error) {
      if (error instanceof TimeoutError) throw new TimeoutError(`Selector ${selector} timed out after ${options.timeout ?? this.client.timeout}ms`);
      throw error;
    }
  }
  private async requireElement(selector: string): Promise<QaElement> {
    const handle = await this.$(selector);
    if (!handle) throw new Error(`No element matches selector: ${selector}`);
    return handle;
  }
  private async withElement<T>(target: string | QaElement, work: (handle: QaElement) => Promise<T>): Promise<T> {
    const handle = typeof target === 'string' ? await this.requireElement(target) : target;
    handle.assertUsable(this);
    try { return await work(handle); } finally { if (typeof target === 'string') await handle.dispose(); }
  }
  private async point(handle: QaElement): Promise<{ x: number; y: number }> {
    await handle.evaluate((element) => { if (!element.isConnected) throw new Error('Element detached'); element.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' }); });
    const box = await handle.boundingBox();
    if (!box || !box.width || !box.height) throw new Error('Element has no clickable box');
    return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
  }
  async click(target: string | QaElement, options: ClickOptions = {}): Promise<void> {
    await this.withElement(target, async (handle) => {
      const point = await this.point(handle), button = options.button ?? 'left', count = options.clickCount ?? 1;
      await this.send('Input.dispatchMouseEvent', { type: 'mouseMoved', ...point });
      for (let clickCount = 1; clickCount <= count; clickCount++) {
        await this.send('Input.dispatchMouseEvent', { type: 'mousePressed', ...point, button, buttons: button === 'left' ? 1 : button === 'right' ? 2 : 4, clickCount });
        if (options.delay) await sleep(options.delay);
        await this.send('Input.dispatchMouseEvent', { type: 'mouseReleased', ...point, button, buttons: 0, clickCount });
      }
    });
  }
  async type(selector: string, text: string, options: TypeOptions = {}): Promise<void> {
    await this.withElement(selector, (handle) => this.typeElement(handle, text, options));
  }
  /** @internal */
  async typeElement(handle: QaElement, text: string, options: TypeOptions = {}): Promise<void> {
    await handle.focus();
    if (!options.delay) { await this.send('Input.insertText', { text }); return; }
    for (const char of text) { await this.send('Input.insertText', { text: char }); await sleep(options.delay); }
  }
  async hover(target: string | QaElement): Promise<void> {
    await this.withElement(target, async (handle) => { await this.send('Input.dispatchMouseEvent', { type: 'mouseMoved', ...await this.point(handle) }); });
  }
  async focus(target: string | QaElement): Promise<void> { await this.withElement(target, (handle) => handle.focus()); }
  async select(selector: string, ...values: string[]): Promise<string[]> {
    return this.$eval(selector, (element, values) => {
      if (!(element instanceof HTMLSelectElement)) throw new Error('Element is not a select');
      element.selectedIndex = -1;
      const selected: string[] = [];
      for (const option of Array.from(element.options)) {
        if (!element.multiple && selected.length) break;
        option.selected = values.includes(option.value);
        if (option.selected) selected.push(option.value);
      }
      element.dispatchEvent(new Event('input', { bubbles: true })); element.dispatchEvent(new Event('change', { bubbles: true }));
      return selected;
    }, values);
  }
  async screenshot(options: ScreenshotOptions = {}): Promise<Buffer> {
    if (options.type && options.type !== 'png' || options.fullPage || options.captureBeyondViewport) throw new Error('Only viewport PNG screenshots are supported');
    const { data } = await this.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false, fromSurface: true });
    const buffer = Buffer.from(data, 'base64');
    if (options.path) await writeFile(options.path, buffer);
    return buffer;
  }
}

/** Remote DOM reference; dispose explicitly or let navigation release its object group. */
export class QaElement {
  private disposed = false;
  /** @internal */
  constructor(private page: QaPage, readonly objectId: string, private onDisposed: () => void) {}
  /** @internal */
  assertUsable(page: QaPage) {
    if (this.disposed) throw new Error('Element handle disposed or released by navigation');
    if (page !== this.page) throw new Error('Element belongs to another page');
  }
  /** @internal */
  invalidate() { this.disposed = true; this.onDisposed(); }
  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.invalidate(); await this.page.send('Runtime.releaseObject', { objectId: this.objectId });
  }
  evaluate<Args extends unknown[], Result>(fn: (element: HTMLElement, ...args: Args) => Result, ...args: Args): Promise<Awaited<Result>> {
    return this.page.callElement(this, fn, args);
  }
  click(options: ClickOptions = {}): Promise<void> { return this.page.click(this, options); }
  type(text: string, options: TypeOptions = {}): Promise<void> { return this.page.typeElement(this, text, options); }
  hover(): Promise<void> { return this.page.hover(this); }
  async focus(): Promise<void> { await this.evaluate((element) => element.focus()); }
  boundingBox(): Promise<BoundingBox | null> {
    return this.evaluate((element) => {
      if (!element.isConnected || !element.getClientRects().length || getComputedStyle(element).visibility === 'hidden') return null;
      const { x, y, width, height } = element.getBoundingClientRect(); return { x, y, width, height };
    });
  }
}
