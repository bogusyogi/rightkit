import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { CdpClient, TimeoutError, validateTimeout, type CdpParams, type QaConnectOptions } from './cdp.js';
import { QaKeyboard } from './keyboard.js';
import { QaMouse } from './mouse.js';
export { TimeoutError, type QaConnectOptions } from './cdp.js';
export { QaKeyboard } from './keyboard.js';
export { QaMouse, type MouseButton, type MouseButtonOptions, type MouseMoveOptions, type MousePosition, type MouseWheelOptions } from './mouse.js';

export interface NavigationOptions { waitUntil?: 'load' | 'domcontentloaded' | 'networkidle0'; timeout?: number }
export interface ViewportOptions { width: number; height: number; deviceScaleFactor?: number; mobile?: boolean }
/** Where a timed-out wait writes its screenshot and JSON state dump. */
export interface FailureArtifactOptions {
  /** Absolute directory, created if missing. Must lie outside RIGHTKIT_RUN_ROOT, which disposeQaWorkspace deletes. */
  directory: string;
  /** Short name used in the file names and the dump. Default 'wait'. */
  label?: string;
}
export interface WaitOptions { timeout?: number; polling?: 'raf' | number; failureArtifacts?: FailureArtifactOptions }
export interface SelectorWaitOptions { visible?: boolean; hidden?: boolean; timeout?: number; failureArtifacts?: FailureArtifactOptions }
export interface ClickOptions {
  button?: 'left' | 'right' | 'middle'; clickCount?: number; delay?: number;
  /** Default false. When true, after the pointer moves and before pressing, checks that the element at the click point is the target or its descendant; otherwise throws ElementOccludedError. */
  checkOccluded?: boolean;
}
export interface FillOptions {
  /** Default false: insert at the caret, exactly as type() does. True selects the whole field first, so the value replaces its content; an empty value then deletes the selection. */
  replace?: boolean;
  /** Default false. Reads the value back after insertion and throws FillMismatchError on a difference. Requires replace. */
  verify?: boolean;
}
/** One console call or uncaught exception of a page. `line` is 1-based. */
export interface ConsoleEntry { type: string; text: string; url?: string; line?: number; timestamp: number }
export interface UserAgentOptions { platform?: string; acceptLanguage?: string }
export interface TypeOptions { delay?: number }
export interface ScreenshotOptions { path?: string; type?: 'png'; captureBeyondViewport?: false; fullPage?: false }
export interface BoundingBox { x: number; y: number; width: number; height: number }
/** One page target listed by the browser. */
export interface QaPageInfo { targetId: string; url: string; title: string }
export interface QaAttachOptions extends QaConnectOptions {
  /** Debugging port on `host`; the browser socket is read from http://<host>:<port>/json/version. Give this or httpEndpoint. */
  port?: number;
  /** Full http or https origin of the debugging endpoint, e.g. http://127.0.0.1:9333. Give this or port. */
  httpEndpoint?: string;
  /** Default 127.0.0.1. */
  host?: string;
  /** Default false. A debugging port is full control of the app; allow a non-loopback host only for a machine you control. */
  allowRemoteHost?: boolean;
}
type PageFunction<Args extends unknown[], Result> = (...args: Args) => Result;
/** Newest console entries kept per page. */
const CONSOLE_LIMIT = 200;
/** Thrown by click({ checkOccluded: true }) when another element is on top of the target at the click point. */
export class ElementOccludedError extends Error {
  readonly point: { x: number; y: number };
  /** Short description of the element on top, e.g. <div id="toast">. Text content is never included. */
  readonly occluder: string;
  constructor(occluder: string, point: { x: number; y: number }) {
    super(`Click target is covered by ${occluder} at (${point.x}, ${point.y})`);
    this.name = 'ElementOccludedError';
    this.occluder = occluder;
    this.point = point;
  }
}
/** Thrown by fill({ verify: true }) when the field does not hold the requested value. The values are not included. */
export class FillMismatchError extends Error {
  readonly expectedLength: number;
  readonly actualLength: number;
  constructor(expectedLength: number, actualLength: number) {
    super(`fill verification failed: field holds ${actualLength} characters, expected ${expectedLength}`);
    this.name = 'FillMismatchError';
    this.expectedLength = expectedLength;
    this.actualLength = actualLength;
  }
}
/** Text of one console argument, from its CDP RemoteObject. */
function remoteText(object: CdpParams): string {
  if ('value' in object) return typeof object.value === 'string' ? object.value : JSON.stringify(object.value) ?? String(object.value);
  return String(object.unserializableValue ?? object.description ?? object.type ?? '');
}
/** url and 1-based line, present only when the event carries them. */
function locationOf(url: unknown, zeroBasedLine: unknown): Pick<ConsoleEntry, 'url' | 'line'> {
  const location: Pick<ConsoleEntry, 'url' | 'line'> = {};
  if (typeof url === 'string' && url) location.url = url;
  if (typeof zeroBasedLine === 'number' && zeroBasedLine >= 0) location.line = zeroBasedLine + 1;
  return location;
}
function consoleEntry(event: CdpParams): ConsoleEntry {
  const frame = event.stackTrace?.callFrames?.[0] as CdpParams | undefined;
  return {
    type: String(event.type),
    text: ((event.args ?? []) as CdpParams[]).map(remoteText).join(' '),
    ...locationOf(frame?.url, frame?.lineNumber),
    timestamp: typeof event.timestamp === 'number' ? event.timestamp : Date.now(),
  };
}
function exceptionEntry(event: CdpParams): ConsoleEntry {
  const details: CdpParams = event.exceptionDetails ?? {};
  return {
    type: 'exception',
    text: String(details.exception?.description ?? details.exception?.value ?? details.text ?? 'Uncaught exception'),
    ...locationOf(details.url, details.lineNumber),
    timestamp: typeof event.timestamp === 'number' ? event.timestamp : Date.now(),
  };
}
/**
 * Validates a failureArtifacts option before any CDP work: absolute, and never inside
 * RIGHTKIT_RUN_ROOT (the workspace home that disposeQaWorkspace deletes).
 */
function failureTarget(options: FailureArtifactOptions | undefined): { directory: string; label: string } | undefined {
  if (!options) return undefined;
  const { directory, label } = options;
  if (typeof directory !== 'string' || !path.isAbsolute(directory)) throw new Error('failureArtifacts.directory must be an absolute path');
  if (label !== undefined && typeof label !== 'string') throw new Error('failureArtifacts.label must be a string');
  const resolved = path.resolve(directory);
  const runRoot = process.env.RIGHTKIT_RUN_ROOT;
  if (runRoot) {
    const relative = path.relative(path.resolve(runRoot), resolved);
    const inside = relative === '' || (!path.isAbsolute(relative) && relative !== '..' && !relative.startsWith(`..${path.sep}`));
    if (inside) throw new Error('failureArtifacts.directory must be outside RIGHTKIT_RUN_ROOT: disposeQaWorkspace deletes that tree');
  }
  return { directory: resolved, label: (label ?? 'wait').replace(/[^A-Za-z0-9._-]+/g, '-').slice(0, 60) || 'wait' };
}
let failureSequence = 0;
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

const LOOPBACK_NAMES = new Set(['localhost', '::1']);
/** Bare host name (IPv6 without brackets) for a URL hostname or a host option. */
function bareHost(host: string): string { return host.replace(/^\[(.*)\]$/, '$1'); }
function isLoopback(host: string): boolean {
  const bare = bareHost(host);
  return LOOPBACK_NAMES.has(bare) || /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(bare);
}
function assertDebugHost(host: string, allowRemoteHost: boolean | undefined, what: string) {
  if (allowRemoteHost || isLoopback(host)) return;
  throw new Error(`Refusing non-loopback ${what} ${host}: a debugging port controls the whole app. Pass allowRemoteHost: true only for a machine you control.`);
}
function debugOrigin(options: QaAttachOptions): URL {
  const given = [options.port !== undefined, options.httpEndpoint !== undefined].filter(Boolean).length;
  if (given !== 1) throw new Error('Pass exactly one of port or httpEndpoint');
  if (options.httpEndpoint !== undefined) {
    let url: URL;
    try { url = new URL(options.httpEndpoint); } catch { throw new Error('httpEndpoint must be an http or https URL'); }
    if (url.protocol !== 'http:' && url.protocol !== 'https:') throw new Error('httpEndpoint must be an http or https URL');
    return url;
  }
  const port = options.port!;
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('Port must be an integer from 1 to 65535');
  const host = options.host ?? '127.0.0.1';
  const authority = host.includes(':') ? `[${bareHost(host)}]` : host;
  try { return new URL(`http://${authority}:${port}`); } catch { throw new Error('Debugging host is not a valid host name'); }
}
async function readVersion(origin: URL, timeout: number): Promise<CdpParams> {
  const url = new URL('/json/version', origin);
  let response: Response;
  try {
    // Redirects are refused so the request cannot be bounced to another host.
    response = await fetch(url, { redirect: 'error', signal: AbortSignal.timeout(timeout) });
  } catch (error) {
    if (error instanceof Error && (error.name === 'TimeoutError' || error.name === 'AbortError')) throw new TimeoutError(`Debugging endpoint ${origin.origin} timed out after ${timeout}ms`);
    throw new Error(`Could not reach debugging endpoint ${origin.origin}`);
  }
  if (!response.ok) throw new Error(`Debugging endpoint ${url.pathname} answered ${response.status}`);
  try { return await response.json() as CdpParams; } catch { throw new Error(`Debugging endpoint ${url.pathname} did not return JSON`); }
}

/** Owns only targets it creates; launcher owns browser process & profile cleanup. */
export class QaBrowserSession {
  private openPages = new Set<QaPage>();
  private closed = false;
  private closing?: Promise<void>;
  private opening = new Set<Promise<QaPage>>();
  private constructor(private client: CdpClient) {}
  static async connect(wsEndpoint: string, options: QaConnectOptions = {}): Promise<QaBrowserSession> {
    return new QaBrowserSession(await CdpClient.connect(wsEndpoint, options));
  }
  /**
   * Attach to a browser that is already running with a debugging port (for example an installed
   * WebView2 app). Reads the browser socket from /json/version. Never creates or closes targets:
   * use pages() and attachPage() to reach the app's own pages.
   */
  static async attach(options: QaAttachOptions = {}): Promise<QaBrowserSession> {
    const timeout = validateTimeout(options.timeout ?? 30_000);
    const origin = debugOrigin(options);
    assertDebugHost(origin.hostname, options.allowRemoteHost, 'debugging host');
    const version = await readVersion(origin, timeout);
    const socketUrl = version.webSocketDebuggerUrl;
    if (typeof socketUrl !== 'string' || !socketUrl) throw new Error('/json/version has no webSocketDebuggerUrl');
    let socket: URL;
    try { socket = new URL(socketUrl); } catch { throw new Error('webSocketDebuggerUrl is not a valid URL'); }
    if (socket.protocol !== 'ws:' && socket.protocol !== 'wss:') throw new Error('webSocketDebuggerUrl must be a ws or wss URL');
    assertDebugHost(socket.hostname, options.allowRemoteHost, 'browser socket host');
    return new QaBrowserSession(await CdpClient.connect(socketUrl, { timeout, debug: options.debug }));
  }
  /** Page targets (type "page") the browser lists now. Read only. */
  async pages(): Promise<QaPageInfo[]> {
    if (this.closed) throw new Error('Browser session closed');
    const { targetInfos } = await this.client.send('Target.getTargets');
    return (targetInfos as CdpParams[])
      .filter((info) => info.type === 'page')
      .map((info) => ({ targetId: info.targetId, url: info.url, title: info.title }));
  }
  /**
   * Attach to an existing page by target id, or by a predicate that must match exactly one page.
   * The returned QaPage detaches on close; it never closes the app's target.
   */
  attachPage(target: string | ((info: QaPageInfo) => boolean)): Promise<QaPage> {
    if (this.closed) return Promise.reject(new Error('Browser session closed'));
    const pending = this.attachExisting(target);
    this.opening.add(pending);
    void pending.then(() => this.opening.delete(pending), () => this.opening.delete(pending));
    return pending;
  }
  private async attachExisting(target: string | ((info: QaPageInfo) => boolean)): Promise<QaPage> {
    const pages = await this.pages();
    let info: QaPageInfo | undefined;
    if (typeof target === 'string') {
      info = pages.find((page) => page.targetId === target);
      if (!info) throw new Error(`No page target with id ${target}`);
    } else {
      const matches = pages.filter(target);
      if (matches.length !== 1) throw new Error(`Predicate matched ${matches.length} pages; it must match exactly one`);
      info = matches[0];
    }
    const { sessionId } = await this.client.send('Target.attachToTarget', { targetId: info.targetId, flatten: true });
    const page = new QaPage(this.client, info.targetId, sessionId, () => this.openPages.delete(page), false);
    this.openPages.add(page);
    try {
      await page.initialize();
      if (this.closed) throw new Error('Browser session closed');
      return page;
    } catch (error) {
      await page.close().catch(() => {}); // detach only
      throw error;
    }
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
      const page = new QaPage(this.client, targetId, sessionId, () => this.openPages.delete(page));
      this.openPages.add(page);
      await page.initialize();
      if (this.closed) throw new Error('Browser session closed');
      return page;
    } catch (error) {
      await this.client.send('Target.closeTarget', { targetId }).catch(() => {});
      for (const page of this.openPages) if (page.targetId === targetId) page.markClosed();
      throw error;
    }
  }
  /**
   * UNSUPPORTED escape hatch: sends one browser-scoped CDP command (no sessionId) and resolves
   * with its raw result. The protocol is Chrome's, not this package's: method names, params and
   * results may change or disappear with a browser version. Prefer the typed QaPage API.
   */
  send(method: string, params: CdpParams = {}): Promise<CdpParams> {
    if (this.closed) return Promise.reject(new Error('Browser session closed'));
    if (typeof method !== 'string' || !method) return Promise.reject(new Error('CDP method must be a non-empty string'));
    return this.client.send(method, params);
  }
  /** Browser.getVersion (unsupported surface, see send). Read only. */
  version(): Promise<CdpParams> {
    return this.send('Browser.getVersion');
  }
  /**
   * Browser argv via Browser.getBrowserCommandLine (unsupported surface, see send). The browser
   * answers only when it was launched with --enable-automation; launchChromeForTesting passes no
   * such flag by default, so pass { args: ['--enable-automation'] } to use this. A refusal rejects
   * with an error that names the requirement; the browser is not changed.
   */
  async commandLine(): Promise<string[]> {
    let reply: CdpParams;
    try {
      reply = await this.send('Browser.getBrowserCommandLine');
    } catch (error) {
      // CdpClient prefixes errors the browser itself returned with "CDP: "; timeouts and closed sockets pass through.
      if (error instanceof Error && error.message.startsWith('CDP: ')) {
        throw new Error(`Browser.getBrowserCommandLine was refused (${error.message.slice(5)}). It requires the browser to be launched with --enable-automation, e.g. launchChromeForTesting({ args: ['--enable-automation'] }).`, { cause: error });
      }
      throw error;
    }
    const argv = reply.arguments;
    if (!Array.isArray(argv) || !argv.every((arg) => typeof arg === 'string')) throw new Error('Browser.getBrowserCommandLine did not return an argument list');
    return argv;
  }
  close(): Promise<void> {
    if (this.closing) return this.closing;
    this.closed = true;
    this.closing = (async () => {
      try {
        await Promise.allSettled([...this.opening]);
        const results = await Promise.allSettled([...this.openPages].map((page) => page.close()));
        const failed = results.find((result) => result.status === 'rejected');
        if (failed?.status === 'rejected') throw failed.reason;
      } finally { this.client.close(); }
    })();
    return this.closing;
  }
}

export class QaPage {
  readonly keyboard: QaKeyboard;
  readonly mouse: QaMouse;
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
  private consoleBuffer: ConsoleEntry[] = [];
  private userAgentBaseline?: string;
  private userAgentOverridden = false;
  private get objectGroup() { return `qa-elements-${this.generation}`; }
  /** @internal Use QaBrowserSession.newPage() or attachPage(). `owned` pages are closed on close(); attached ones only detach. */
  constructor(private client: CdpClient, readonly targetId: string, private sessionId: string, private onClosed: () => void, private owned = true) {
    this.keyboard = new QaKeyboard((method, params) => this.send(method, params));
    this.mouse = new QaMouse((method, params) => this.send(method, params));
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
      this.on('Runtime.consoleAPICalled', (event) => this.recordConsole(consoleEntry(event))),
      this.on('Runtime.exceptionThrown', (event) => this.recordConsole(exceptionEntry(event))),
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
  /** Owned pages (newPage) close their target. Attached pages (attachPage) only detach; the app's page stays open. */
  close(): Promise<void> {
    if (this.closing) return this.closing;
    if (this.closed) return Promise.resolve();
    this.closing = (async () => {
      if (this.owned) {
        try { await this.client.send('Target.closeTarget', { targetId: this.targetId }); }
        finally { this.markClosed(); }
        return;
      }
      try { await this.client.send('Target.detachFromTarget', { sessionId: this.sessionId }); }
      catch { /* already detached: the local page is closed either way */ }
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
    const artifacts = failureTarget(options.failureArtifacts);
    const timeout = options.timeout ?? this.client.timeout;
    try {
      await this.bounded(`Predicate ${source}`, timeout, async (remaining, stopped) => {
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
    } catch (error) {
      if (artifacts && error instanceof TimeoutError) await this.attachFailureArtifacts(error, artifacts, source, timeout);
      throw error;
    }
  }
  async waitForSelector(selector: string, options: SelectorWaitOptions = {}): Promise<QaElement | null> {
    if (options.visible && options.hidden) throw new Error('visible & hidden cannot both be true');
    const artifacts = failureTarget(options.failureArtifacts);
    const timeout = options.timeout ?? this.client.timeout;
    try {
      return await this.bounded(`Selector ${selector}`, timeout, async remaining => {
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
      if (error instanceof TimeoutError) {
        const named = new TimeoutError(`Selector ${selector} timed out after ${timeout}ms`);
        if (artifacts) await this.attachFailureArtifacts(named, artifacts, `waitForSelector ${JSON.stringify(selector)} visible=${!!options.visible} hidden=${!!options.hidden}`, timeout);
        throw named;
      }
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
  /** Recent console calls and uncaught exceptions of this page, oldest first. Bounded to the newest 200. Returns copies. */
  consoleEntries(): ConsoleEntry[] { return this.consoleBuffer.map((entry) => ({ ...entry })); }
  /** Empties the console buffer. */
  clearConsoleEntries(): void { this.consoleBuffer.length = 0; }
  private recordConsole(entry: ConsoleEntry) {
    this.consoleBuffer.push(entry);
    if (this.consoleBuffer.length > CONSOLE_LIMIT) this.consoleBuffer.splice(0, this.consoleBuffer.length - CONSOLE_LIMIT);
  }
  /**
   * Overrides the user agent (Emulation.setUserAgentOverride) for this page, including later navigations.
   * Each call replaces the whole override, so platform & acceptLanguage are sent only when given.
   */
  async setUserAgent(userAgent: string, options: UserAgentOptions = {}): Promise<void> {
    if (typeof userAgent !== 'string' || !userAgent.trim()) throw new Error('User agent must be a non-empty string');
    // The first override records the page's own navigator.userAgent so resetUserAgent can restore it.
    if (!this.userAgentOverridden) this.userAgentBaseline = await this.evaluate(() => navigator.userAgent);
    await this.send('Emulation.setUserAgentOverride', {
      userAgent,
      ...(options.platform !== undefined ? { platform: options.platform } : {}),
      ...(options.acceptLanguage !== undefined ? { acceptLanguage: options.acceptLanguage } : {}),
    });
    this.userAgentOverridden = true;
  }
  /** Restores the navigator.userAgent captured before the first setUserAgent. A no-op when this page never overrode it. */
  async resetUserAgent(): Promise<void> {
    if (!this.userAgentOverridden || this.userAgentBaseline === undefined) return;
    await this.send('Emulation.setUserAgentOverride', { userAgent: this.userAgentBaseline });
    this.userAgentOverridden = false;
    this.userAgentBaseline = undefined;
  }
  /**
   * Types a value into a field. Default: insert at the caret, exactly as type() does. With replace the
   * whole content is selected first and replaced. With verify (requires replace) the value is read back
   * once after insertion and FillMismatchError is thrown on a difference.
   */
  async fill(target: string | QaElement, value: string, options: FillOptions = {}): Promise<void> {
    if (typeof value !== 'string') throw new Error('fill value must be a string');
    if (options.verify && !options.replace) throw new Error('fill verify requires replace: true; an insert-only fill has no fixed expected value');
    await this.withElement(target, async (handle) => {
      if (!options.replace) { await this.typeElement(handle, value); return; }
      await handle.focus();
      await this.selectAll(handle);
      if (value === '') await this.keyboard.press('Backspace');
      else await this.send('Input.insertText', { text: value });
      if (options.verify) {
        const actual = await handle.evaluate((element) => (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement ? element.value : element.textContent) ?? '');
        if (actual !== value) throw new FillMismatchError(value.length, actual.length);
      }
    });
  }
  private async selectAll(handle: QaElement): Promise<void> {
    const selected = await handle.evaluate((element) => {
      if (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement) { element.select(); return true; }
      if (element.isContentEditable) {
        const range = element.ownerDocument.createRange();
        range.selectNodeContents(element);
        const selection = element.ownerDocument.getSelection();
        selection?.removeAllRanges();
        selection?.addRange(range);
        return true;
      }
      return false;
    });
    if (!selected) throw new Error('fill target is not an editable input, textarea or contenteditable element');
  }
  /** Throws ElementOccludedError naming the element on top unless the target itself or a composed descendant is hit. */
  private async assertUnoccluded(handle: QaElement, point: { x: number; y: number }): Promise<void> {
    const occluder = await handle.evaluate((element, x: number, y: number) => {
      let hit: Element | null = element.ownerDocument.elementFromPoint(x, y);
      // The document reports a shadow host, not the element inside its open shadow root: descend.
      while (hit?.shadowRoot) {
        const inner: Element | null = hit.shadowRoot.elementFromPoint(x, y);
        if (!inner || inner === hit) break;
        hit = inner;
      }
      // Composed ancestry: a shadow root's host counts as its parent, so shadow content inside the target passes.
      for (let node: Node | null = hit; node; node = node.parentNode ?? (node as ShadowRoot).host ?? null) {
        if (node === element) return null;
      }
      if (!hit) return '(no element at the click point)';
      const id = hit.id ? ` id="${hit.id}"` : '';
      const className = typeof hit.className === 'string' ? hit.className.trim().slice(0, 80) : '';
      return `<${hit.tagName.toLowerCase()}${id}${className ? ` class="${className}"` : ''}>`;
    }, point.x, point.y);
    if (occluder !== null) throw new ElementOccludedError(occluder, point);
  }
  /** Writes a PNG and a JSON state dump for a timed-out wait into the caller directory. */
  private async captureFailure(directory: string, label: string, predicate: string, timeout: number): Promise<{ json: string; screenshot?: string }> {
    await mkdir(directory, { recursive: true });
    const stem = `${label}-${Date.now()}-${++failureSequence}`;
    const json = path.join(directory, `${stem}.json`);
    const png = path.join(directory, `${stem}.png`);
    let screenshot: string | undefined;
    let screenshotError: string | undefined;
    try { await writeFile(png, await this.screenshot()); screenshot = png; }
    catch (error) { screenshotError = error instanceof Error ? error.message : String(error); }
    let where: { url?: string; title?: string } = {};
    try {
      where = resultValue(await this.run(() => ({ url: location.href, title: document.title }), [], true, 2_000)) ?? {};
    } catch { /* page gone or stuck: the dump records no url or title */ }
    const dump = {
      label,
      predicate,
      timeoutMs: timeout,
      timestamp: new Date().toISOString(),
      url: where.url ?? null,
      title: where.title ?? null,
      screenshot: screenshot ? path.basename(screenshot) : null,
      screenshotError: screenshotError ?? null,
      console: this.consoleEntries(),
    };
    await writeFile(json, JSON.stringify(dump, null, 2));
    return screenshot ? { json, screenshot } : { json };
  }
  /** Attaches artifact paths to a timeout error. A write failure is appended to its message; the timeout still throws. */
  private async attachFailureArtifacts(error: TimeoutError, target: { directory: string; label: string }, predicate: string, timeout: number): Promise<void> {
    try { error.artifacts = await this.captureFailure(target.directory, target.label, predicate, timeout); }
    catch (captureError) {
      error.message += ` (failure artifacts were not written: ${captureError instanceof Error ? captureError.message : String(captureError)})`;
    }
  }

  async click(target: string | QaElement, options: ClickOptions = {}): Promise<void> {
    this.mouse.assertReleased();
    await this.withElement(target, async (handle) => {
      const point = await this.point(handle), button = options.button ?? 'left', count = options.clickCount ?? 1;
      await this.send('Input.dispatchMouseEvent', { type: 'mouseMoved', ...point });
      this.mouse.track(point.x, point.y);
      if (options.checkOccluded) await this.assertUnoccluded(handle, point);
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
    this.mouse.assertReleased();
    await this.withElement(target, async (handle) => {
      const point = await this.point(handle);
      await this.send('Input.dispatchMouseEvent', { type: 'mouseMoved', ...point });
      this.mouse.track(point.x, point.y);
    });
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
