import { validateInstalledArtifact, fixtureEntry } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const recipeId = 'pdf-ui-viewer-markup-headless-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const surface = ['MAX_PASSWORD_ATTEMPTS', 'MAX_REGEX_BOUND', 'MAX_REGEX_PATTERN_LENGTH', 'MAX_REGEX_REPEATS', 'MAX_REGEX_TEXT', 'PdfPasswordCancelledError', 'PdfSearchContext', 'PdfTruncationNotice', 'PdfViewer', 'REGEX_DEADLINE_MS', 'ZOOM_MAX', 'ZOOM_MIN', 'acquirePdfRenderKeepalive', 'attachPasswordHandler', 'boundedCanvasDpr', 'clampScale', 'configurePdfJs', 'createLoadTimer', 'createPdfLinkService', 'createRegexSearcher', 'ensurePdfWorker', 'findLiteralMatches', 'findRegexMatches', 'highlightSpan', 'isSafeExternalUrl', 'pdfDocumentInit', 'pdfTextContentSource', 'pdfWasmUrl', 'regexPatternError', 'renderPageLimit', 'renderPdfTextLayer', 'thumbnailWindow', 'usePdfSearch', 'viewportRotation', 'withTimeout'];
const wait = ms => new Promise(resolve => setTimeout(resolve, ms));
const until = async (predicate, what) => { for (let i = 0; i < 400; i += 1) { if (predicate()) return; await wait(5); } assert.fail(`timed out waiting for ${what}`); };

// Minimal DOM covering the calls highlightSpan makes; the recipe has no browser and no DOM library.
class FakeText { constructor(data) { this.data = data; } get textContent() { return this.data; } }
class FakeFragment { constructor() { this.children = []; } appendChild(node) { this.children.push(node); return node; } }
class FakeMark {
  constructor(tag) { this.tag = tag; this.style = { cssText: '' }; this.children = []; }
  get textContent() { return this.children.map(child => child.data).join(''); }
  set textContent(value) { this.children = [new FakeText(value)]; }
}
class FakeSpan {
  constructor(text) { this.children = [new FakeText(text)]; }
  get textContent() { return this.children.map(child => child.textContent).join(''); }
  get childElementCount() { return this.children.filter(child => child instanceof FakeMark).length; }
  replaceChildren(...nodes) { this.children = nodes.flatMap(node => (node instanceof FakeFragment ? node.children : [node])); }
}
const marks = span => span.children.filter(child => child instanceof FakeMark).map(mark => ({ text: mark.textContent, css: mark.style.cssText }));

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/pdf-ui');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/pdf-ui', rootResolvedUrl: root, entries: { '@rightkit/pdf-ui': root, '@rightkit/pdf-ui/styles.css': import.meta.resolve('@rightkit/pdf-ui/styles.css') } });
  result.artifact = provenance.artifact;
  const manifest = JSON.parse(readFileSync(path.join(path.dirname(provenance.artifact.modulePath), '..', 'package.json'), 'utf8'));
  const React = await import(fixtureEntry(import.meta.url, import.meta.resolve('react')));
  const { renderToStaticMarkup } = await import(fixtureEntry(import.meta.url, import.meta.resolve('react-dom/server')));
  const pdfjs = await import(fixtureEntry(import.meta.url, import.meta.resolve('pdfjs-dist')));
  const ui = await import(provenance.entries['@rightkit/pdf-ui']);
  const css = readFileSync(fileURLToPath(provenance.entries['@rightkit/pdf-ui/styles.css']), 'utf8');
  const h = React.createElement;

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  await run('export-surface-exact-and-pinned-pdfjs-dependency', async () => {
    assert.deepEqual(Object.keys(ui).sort(), surface);
    assert.deepEqual(Object.keys(manifest.exports), ['.', './styles.css']);
    assert.deepEqual(Object.keys(manifest.dependencies), ['pdfjs-dist']);
    assert.deepEqual(Object.keys(manifest.peerDependencies), ['react']);
    assert.deepEqual(manifest.sideEffects, ['*.css']);
    assert.equal(pdfjs.version, '6.4.299', 'the viewer and this recipe share the pinned pdf.js');
    for (const selector of ['.rk-pdf-viewer', '.rk-pdf-toolbar', '.rk-pdf-scroll', '.rk-pdf-page', '.rk-pdf-notice']) assert(css.includes(selector), selector);
    assert(css.includes('var(--rk-pdf-ink,#1d1d1f)') && css.includes('var(--rk-pdf-bg,#f2f2f2)'), 'surface colours read CSS variables with literal fallbacks');
    return { exports: surface.length, pdfjs: pdfjs.version };
  });

  await run('viewer-server-markup-default-and-options', async () => {
    const source = { kind: 'url', url: 'https://example.invalid/doc.pdf' };
    const toolbar = '<div class="rk-pdf-toolbar"><button type="button" disabled="" aria-label="Previous page">‹</button><form><input aria-label="Page number" value="1"/><span>/ —</span></form><button type="button" disabled="" aria-label="Next page">›</button><button type="button" aria-label="Zoom out">−</button><form><input aria-label="Zoom percentage" value="100"/>%</form><button type="button" aria-label="Zoom in">+</button><button type="button" aria-label="Fit width">Fit</button><button type="button" aria-label="Rotate">↻</button><button type="button" aria-pressed="false">Outline</button><button type="button" aria-pressed="false">Pages</button>';
    const find = '<form class="rk-pdf-find"><input aria-label="Find in PDF" placeholder="Find" value=""/><button type="submit">Find</button><button type="button" disabled="">Next</button>';
    const body = '<div class="rk-pdf-body"><div class="rk-pdf-scroll" role="region" aria-label="PDF pages" tabindex="0"><div class="rk-pdf-pages"></div></div></div>';
    const viewer = props => renderToStaticMarkup(h(ui.PdfViewer, { source, ...props }));
    assert.equal(viewer({}), `<div class="rk-pdf-viewer " data-loading="true">${toolbar}${find}</form></div>${body}</div>`);
    assert.equal(viewer({ findBar: false }), `<div class="rk-pdf-viewer " data-loading="true">${toolbar}</div>${body}</div>`);
    assert.equal(viewer({ hideToolbar: true, className: 'host', toolbar: h('b', null, 'x') }), `<div class="rk-pdf-viewer host" data-loading="true">${body}</div>`);
    assert.equal(viewer({ regexFind: true }), `<div class="rk-pdf-viewer " data-loading="true">${toolbar}${find}<label><input type="checkbox"/> Regex</label></form></div>${body}</div>`);
    const outline = viewer({ initialView: { page: 2, scale: 1.5, fit: null, rotation: 90, panel: 'outline' }, maxRenderPages: 3 });
    assert(outline.includes('<input aria-label="Page number" value="2"/>') && outline.includes('<input aria-label="Zoom percentage" value="150"/>'));
    assert(outline.includes('<button type="button" aria-pressed="true">Outline</button>') && outline.includes('<aside class="rk-pdf-outline" aria-label="PDF outline"><ul></ul></aside>'));
    assert(viewer({ initialView: { panel: 'thumbs' } }).includes('<button type="button" aria-pressed="true">Pages</button>'));
    assert.equal(viewer({ documentOptions: { wasmUrl: '/pdfjs/', cMapUrl: '/cmaps/' } }), viewer({}), 'getDocument options do not change the markup');
    assert.equal(viewer({ maxRenderPages: 3, truncationNotice: 'Capped' }), viewer({ maxRenderPages: 3 }), 'no notice before pages are known');
    assert.equal(renderToStaticMarkup(h(ui.PdfViewer, { source: { kind: 'bytes', data: new Uint8Array([37, 80, 68, 70]) } })), viewer({}), 'the source kind does not change the markup');
    assert.equal(renderToStaticMarkup(h(ui.PdfViewer, { source: { kind: 'loader', load: async () => new Uint8Array() } })), viewer({}));
    return { variants: ['default', 'findBar=false', 'hideToolbar', 'regexFind', 'outline panel', 'thumbs panel', 'bytes source', 'loader source'] };
  });

  await run('search-context-hook-and-truncation-notice', async () => {
    const Probe = () => { try { ui.usePdfSearch(); return h('i', null, 'inside'); } catch (error) { return h('i', null, error.message); } };
    assert.equal(renderToStaticMarkup(h(Probe)), '<i>usePdfSearch must be used inside PdfViewer</i>');
    let keys;
    const Inside = () => { keys = Object.keys(ui.usePdfSearch()); return h('i', null, 'inside'); };
    const html = renderToStaticMarkup(h(ui.PdfViewer, { source: { kind: 'url', url: 'https://example.invalid/doc.pdf' }, toolbar: h(Inside), findBar: false }));
    assert.deepEqual(keys, ['find', 'next', 'prev', 'result', 'viewState']);
    assert(html.includes('<i>inside</i>'));
    const notice = props => renderToStaticMarkup(h(ui.PdfTruncationNotice, props));
    assert.equal(notice({ numPages: 10, shown: 3, notice: undefined }), '');
    assert.equal(notice({ numPages: 3, shown: 3, notice: 'hidden' }), '');
    assert.equal(notice({ numPages: 0, shown: 0, notice: 'hidden' }), '');
    assert.equal(notice({ numPages: 10, shown: 3, notice: 'Capped' }), '<p class="rk-pdf-notice" role="status">Capped</p>');
    assert.equal(notice({ numPages: 10, shown: 3, notice: ({ shown, total }) => `Showing first ${shown} of ${total} pages` }), '<p class="rk-pdf-notice" role="status">Showing first 3 of 10 pages</p>');
    assert.equal(typeof ui.PdfSearchContext.Provider, 'object');
    return { searchKeys: keys };
  });

  await run('document-init-and-pdfjs-configuration', async () => {
    const bytes = new Uint8Array([37, 80, 68, 70]);
    assert.deepEqual(ui.pdfDocumentInit('https://example.invalid/a.pdf'), { url: 'https://example.invalid/a.pdf' });
    assert.deepEqual(ui.pdfDocumentInit(bytes), { data: bytes });
    assert.deepEqual(ui.pdfDocumentInit(bytes, {}), { data: bytes });
    const options = { wasmUrl: '/pdfjs/', cMapUrl: '/cmaps/', cMapPacked: true, standardFontDataUrl: '/fonts/' };
    assert.deepEqual(ui.pdfDocumentInit(bytes, options), { data: bytes, ...options });
    assert.deepEqual(ui.pdfDocumentInit('/a.pdf', options), { url: '/a.pdf', ...options });
    assert.deepEqual(ui.pdfDocumentInit(bytes, { url: '/other.pdf', data: new Uint8Array([1]), range: {} }), { data: bytes }, 'options can never replace the source or the transport');
    assert.equal(ui.pdfWasmUrl(), '/pdfjs/');
    ui.configurePdfJs({ wasmUrl: '/assets/pdfjs/' });
    assert.equal(ui.pdfWasmUrl(), '/assets/pdfjs/');
    const defaultWorker = pdfjs.GlobalWorkerOptions.workerSrc;
    pdfjs.GlobalWorkerOptions.workerSrc = '';
    ui.configurePdfJs({ wasmUrl: '/assets/pdfjs/' });
    assert.equal(pdfjs.GlobalWorkerOptions.workerSrc, '', 'configuring wasm alone does not touch the worker');
    ui.ensurePdfWorker();
    assert.match(pdfjs.GlobalWorkerOptions.workerSrc, /^file:\/\/.*pdfjs-dist\/build\/pdf\.worker\.min\.mjs$/, 'without configuration the worker resolves through new URL(..., import.meta.url)');
    ui.configurePdfJs({ workerSrc: 'https://example.invalid/pdf.worker.min.mjs' });
    assert.equal(pdfjs.GlobalWorkerOptions.workerSrc, 'https://example.invalid/pdf.worker.min.mjs');
    ui.ensurePdfWorker();
    assert.equal(pdfjs.GlobalWorkerOptions.workerSrc, 'https://example.invalid/pdf.worker.min.mjs', 'ensurePdfWorker keeps a configured worker');
    assert.equal(ui.pdfWasmUrl(), '/assets/pdfjs/', 'a later partial configuration keeps earlier values');
    return { workerConfigured: true, wasmConfigured: true };
  });

  await run('password-flow-and-load-timer', async () => {
    const task = () => ({ onPassword: null, destroyed: 0, destroy() { this.destroyed += 1; } });
    const answers = [];
    let t = task();
    let flow = ui.attachPasswordHandler(t, () => async (reason, attempt) => { answers.push([reason, attempt]); return attempt === 1 ? 'wrong' : 'right'; });
    t.onPassword(value => answers.push(['update', value]), 1);
    await until(() => answers.length === 2, 'the first password answer');
    t.onPassword(value => answers.push(['update', value]), 2);
    await until(() => answers.length === 4, 'the second password answer');
    assert.deepEqual(answers, [['need', 1], ['update', 'wrong'], ['incorrect', 2], ['update', 'right']]);
    assert.equal(flow.failure, null);
    assert.equal(t.destroyed, 0);

    t = task();
    flow = ui.attachPasswordHandler(t, () => async () => null);
    t.onPassword(() => assert.fail('declining must not answer'), 1);
    await until(() => flow.failure !== null, 'the declined password');
    assert(flow.failure instanceof ui.PdfPasswordCancelledError && flow.failure.reason === 'need' && flow.failure.name === 'PdfPasswordCancelledError' && flow.failure.message === 'PDF password required');
    assert.equal(t.destroyed, 1);

    t = task();
    flow = ui.attachPasswordHandler(t, () => undefined);
    t.onPassword(() => {}, 2);
    assert.equal(flow.failure.reason, 'incorrect');
    assert.equal(flow.failure.message, 'PDF password incorrect');
    assert.equal(t.destroyed, 1, 'without a handler the load ends at once');

    t = task();
    const boom = new Error('handler failed');
    flow = ui.attachPasswordHandler(t, () => async () => { throw boom; });
    t.onPassword(() => {}, 1);
    await until(() => flow.failure !== null, 'the throwing handler');
    assert.equal(flow.failure, boom);

    t = task();
    let asked = 0;
    flow = ui.attachPasswordHandler(t, () => () => { asked += 1; return 'again'; });
    for (let i = 0; i < ui.MAX_PASSWORD_ATTEMPTS + 1; i += 1) t.onPassword(() => {}, 2);
    await until(() => flow.failure !== null, 'the attempt cap');
    assert.equal(ui.MAX_PASSWORD_ATTEMPTS, 10);
    assert.equal(asked, ui.MAX_PASSWORD_ATTEMPTS);
    assert.equal(flow.failure.message, 'PDF password attempts exhausted');

    const timerCalls = [];
    t = task();
    ui.attachPasswordHandler(t, () => async () => 'x', { pause: () => timerCalls.push('pause'), resume: () => timerCalls.push('resume') });
    t.onPassword(() => timerCalls.push('update'), 1);
    await until(() => timerCalls.length === 3, 'timer pause and resume');
    assert.deepEqual(timerCalls, ['pause', 'resume', 'update'], 'the load clock stops while the user is asked');

    const expired = ui.createLoadTimer(10, 'load timed out');
    await assert.rejects(expired.promise, /load timed out/);
    const paused = ui.createLoadTimer(30, 'resumed timer fired');
    let pausedSettled = false;
    paused.promise.catch(() => { pausedSettled = true; });
    paused.pause();
    await wait(90);
    assert.equal(pausedSettled, false, 'a paused clock never fires');
    paused.resume();
    await assert.rejects(paused.promise, /resumed timer fired/);
    const cleared = ui.createLoadTimer(10, 'cleared timer fired');
    let clearedSettled = false;
    cleared.promise.catch(() => { clearedSettled = true; });
    cleared.clear();
    await wait(60);
    assert.equal(clearedSettled, false, 'a cleared clock never fires');
    return { attemptCap: ui.MAX_PASSWORD_ATTEMPTS, failureReasons: ['need', 'incorrect'] };
  });

  await run('find-literal-and-bounded-regex-search', async () => {
    const spans = (text, query, options) => ui.findLiteralMatches(text, query, options).map(m => text.slice(m.start, m.end));
    assert.deepEqual(ui.findLiteralMatches('one two one', 'one'), [{ start: 0, end: 3 }, { start: 8, end: 11 }]);
    assert.deepEqual(spans('Alpha alpha ALPHA', 'alpha'), ['Alpha', 'alpha', 'ALPHA']);
    assert.deepEqual(spans('Alpha alpha ALPHA', 'alpha', { matchCase: true }), ['alpha']);
    assert.deepEqual(spans('cat concatenate cat.', 'cat', { wholeWord: true }), ['cat', 'cat']);
    assert.deepEqual(spans('a  b\tc\nd', 'a b c d'), ['a  b\tc\nd'], 'whitespace runs match one space');
    assert.deepEqual(spans('inter- national', 'international'), []);
    assert.deepEqual(spans('inter- national', 'international', { joinHyphenated: true }), ['inter- national']);
    assert.deepEqual(ui.findLiteralMatches('anything', '   '), []);
    assert.deepEqual(spans('ΟΔΟΣ οδος', 'οδοσ'), ['ΟΔΟΣ', 'οδος'], 'final sigma folds with the medial form');
    assert.deepEqual(ui.findRegexMatches('ab12 cd345', '[a-z]\\d+'), [{ start: 1, end: 4 }, { start: 6, end: 10 }]);
    assert.deepEqual(ui.findRegexMatches('ab12 cd345', '[a-z]+\\d+'), [], 'two unbounded repeats are refused by the strict policy');
    assert.deepEqual(ui.findRegexMatches('Ab ab', 'ab', { matchCase: true }), [{ start: 3, end: 5 }]);
    assert.deepEqual(ui.findRegexMatches('cat concat', 'cat', { wholeWord: true }), [{ start: 0, end: 3 }]);
    assert.deepEqual(ui.findRegexMatches('abc', 'x*'), [], 'zero-length matches are skipped');
    assert.deepEqual(ui.findRegexMatches('aaaa', '(a+)+$'), [], 'a refused pattern matches nothing');
    assert.deepEqual(ui.findRegexMatches('abc', ''), []);
    assert.equal(ui.regexPatternError('a+b'), null);
    assert.equal(ui.regexPatternError('x'.repeat(201)), 'pattern is longer than 200 characters');
    assert.equal(ui.regexPatternError('(a)\\1'), 'backreferences are not allowed');
    assert.equal(ui.regexPatternError('(a+)+'), 'a repeat on a group that repeats or alternates is not allowed');
    assert.equal(ui.regexPatternError('(?:a|b)*'), 'a repeat on a group that repeats or alternates is not allowed');
    assert.equal(ui.regexPatternError('a*a*b'), 'more than 1 unbounded repeat is not allowed');
    assert.equal(ui.regexPatternError('a{2000}'), 'repeat bounds above 1000 are not allowed');
    assert.equal(ui.regexPatternError('a*a*b', 'basic'), null, 'the basic policy only bounds length and syntax');
    assert.equal(ui.regexPatternError('[a-z]+\\d+'), 'more than 1 unbounded repeat is not allowed');
    assert.equal(ui.regexPatternError('['), 'invalid regular expression');
    assert.deepEqual([ui.MAX_REGEX_PATTERN_LENGTH, ui.MAX_REGEX_REPEATS, ui.MAX_REGEX_BOUND, ui.MAX_REGEX_TEXT, ui.REGEX_DEADLINE_MS], [200, 1, 1000, 100000, 250]);
    assert.deepEqual(ui.findRegexMatches(`${'x'.repeat(ui.MAX_REGEX_TEXT)}needle`, 'needle'), [], 'only the first MAX_REGEX_TEXT units are searched');
    assert.equal(typeof Worker, 'undefined', 'plain Node has no Worker, so the searcher takes its synchronous path');
    const searcher = ui.createRegexSearcher(50);
    assert.deepEqual(await searcher.search('ab12 cd345', '[a-z]\\d+'), { matches: [{ start: 1, end: 4 }, { start: 6, end: 10 }], timedOut: false });
    assert.deepEqual(await searcher.search('abc', ''), { matches: [], timedOut: false });
    assert.deepEqual(await searcher.search('aaaa', '(a+)+$'), { matches: [], timedOut: false });
    searcher.dispose();
    return { policies: ['strict', 'basic'], searcher: 'synchronous fallback' };
  });

  await run('zoom-dpr-rotation-page-limit-and-thumbnail-window-math', async () => {
    assert.deepEqual([ui.ZOOM_MIN, ui.ZOOM_MAX], [0.25, 5]);
    assert.deepEqual([0.01, 1, 9].map(ui.clampScale), [0.25, 1, 5]);
    assert.equal(ui.boundedCanvasDpr({ width: 100, height: 100 }, 2), 2);
    assert.equal(ui.boundedCanvasDpr({ width: 4000, height: 4000 }, 2), 1, 'the canvas never exceeds 16 million pixels');
    assert.equal(ui.boundedCanvasDpr({ width: 100, height: 100 }, Number.NaN), 1);
    assert.equal(ui.boundedCanvasDpr({ width: 100, height: 100 }, -3), 1);
    assert.equal(ui.boundedCanvasDpr({ width: 100, height: 100 }, 0.01), 0.1);
    assert.deepEqual([[0, 0], [90, 90], [270, 180], [undefined, 450], [-90, 0], [Number.NaN, 90]].map(([page, user]) => ui.viewportRotation(page, user)), [0, 180, 90, 90, 270, 90]);
    assert.deepEqual([[10, undefined], [10, 3], [10, 3.9], [10, 99], [10, -2], [10, Number.NaN], [0, 5], [Number.NaN, 5]].map(([pages, max]) => ui.renderPageLimit(pages, max)), [10, 3, 3, 10, 0, 10, 0, 0]);
    assert.deepEqual(ui.thumbnailWindow({ numPages: 100, scrollTop: 0, viewportHeight: 300, itemHeight: 100, overscan: 2 }), { start: 1, end: 5, before: 0, after: 9500 });
    assert.deepEqual(ui.thumbnailWindow({ numPages: 100, scrollTop: 5000, viewportHeight: 300, itemHeight: 100, overscan: 2 }), { start: 49, end: 55, before: 4800, after: 4500 });
    assert.deepEqual(ui.thumbnailWindow({ numPages: 100, scrollTop: 99999, viewportHeight: 300, itemHeight: 100, overscan: 0 }), { start: 98, end: 100, before: 9700, after: 0 });
    assert.deepEqual(ui.thumbnailWindow({ numPages: 0, scrollTop: 0, viewportHeight: 300, itemHeight: 100, overscan: 2 }), { start: 1, end: 0, before: 0, after: 0 });
    assert.equal(await ui.withTimeout(Promise.resolve('done'), 1000, 'late'), 'done');
    await assert.rejects(ui.withTimeout(new Promise(() => {}), 10, 'stuck pdf.js call'), /stuck pdf\.js call/);
    await assert.rejects(ui.withTimeout(Promise.reject(new Error('inner')), 1000, 'late'), /inner/);
    return { zoomRange: [ui.ZOOM_MIN, ui.ZOOM_MAX] };
  });

  await run('link-service-routes-only-safe-urls-and-named-actions', async () => {
    for (const url of ['https://example.invalid/', 'HTTP://example.invalid/', 'mailto:person@example.invalid']) assert.equal(ui.isSafeExternalUrl(url), true, url);
    for (const url of ['javascript:alert(1)', 'file:///etc/passwd', 'data:text/html,x', 'ftp://example.invalid/', '//example.invalid/', '']) assert.equal(ui.isSafeExternalUrl(url), false, url);
    const log = [];
    const page = { current: 3 };
    const service = ui.createPdfLinkService({
      goToDest: dest => log.push(['dest', dest]), goToPage: n => log.push(['page', n]), currentPage: () => page.current, pagesCount: () => 9, openExternal: url => { log.push(['open', url]); },
    });
    assert.equal(service.externalLinkEnabled, true);
    assert.equal(service.isInPresentationMode, false);
    assert.equal(service.getDestinationHash({}), '#');
    assert.equal(service.getAnchorUrl('#x'), '#');
    const anchor = { listeners: new Map(), addEventListener(type, listener) { this.listeners.set(type, listener); } };
    service.addLinkAttributes(anchor, 'https://example.invalid/page', true);
    assert.deepEqual([anchor.href, anchor.title, anchor.rel], ['#', 'https://example.invalid/page', 'noopener noreferrer'], 'the document-supplied URL never becomes the href');
    assert.deepEqual([...anchor.listeners.keys()], ['click', 'auxclick']);
    let prevented = 0;
    anchor.listeners.get('click')({ preventDefault: () => { prevented += 1; } });
    anchor.listeners.get('auxclick')({ preventDefault: () => { prevented += 1; } });
    assert.equal(prevented, 2);
    const hostile = { listeners: new Map(), addEventListener(type, listener) { this.listeners.set(type, listener); } };
    service.addLinkAttributes(hostile, 'javascript:alert(1)');
    hostile.listeners.get('click')({ preventDefault() {} });
    assert.deepEqual(log, [['open', 'https://example.invalid/page'], ['open', 'https://example.invalid/page']], 'an unsafe URL is never handed to the host');
    log.length = 0;
    service.goToDestination('chapter-1'); service.goToDestination([null, { name: 'Fit' }]); service.goToPage(5);
    for (const action of ['NextPage', 'PrevPage', 'FirstPage', 'LastPage', 'Print', 'GoBack']) service.executeNamedAction(action);
    assert.deepEqual(log, [['dest', 'chapter-1'], ['dest', [null, { name: 'Fit' }]], ['page', 5], ['page', 4], ['page', 2], ['page', 1], ['page', 9]]);
    assert.equal(service.executeSetOCGState({}), undefined);
    assert.equal(await service.getAttachmentContent('id'), null);
    const inert = ui.createPdfLinkService({ goToDest() {}, goToPage() {}, currentPage: () => 1, pagesCount: () => 1 });
    const quiet = { listeners: new Map(), addEventListener(type, listener) { this.listeners.set(type, listener); } };
    inert.addLinkAttributes(quiet, 'https://example.invalid/');
    assert.doesNotThrow(() => quiet.listeners.get('click')({ preventDefault() {} }), 'external links are inert without openExternal');
    return { actions: ['NextPage', 'PrevPage', 'FirstPage', 'LastPage'], unsafeUrlsRefused: 6 };
  });

  await run('text-layer-source-and-find-highlighting-on-fake-dom', async () => {
    const calls = [];
    const stream = { kind: 'stream' };
    const page = { getTextContent: async () => { calls.push('getTextContent'); return { items: [], styles: {} }; }, streamTextContent: () => { calls.push('streamTextContent'); return stream; } };
    assert.deepEqual(await ui.pdfTextContentSource(page), { items: [], styles: {} });
    assert.equal(await ui.pdfTextContentSource(page, 'streamTextContent'), stream);
    assert.deepEqual(calls, ['getTextContent', 'streamTextContent']);
    const previous = globalThis.document;
    globalThis.document = { createTextNode: data => new FakeText(data), createDocumentFragment: () => new FakeFragment(), createElement: tag => new FakeMark(tag) };
    try {
      const mark = background => `background:${background};color:inherit;border-radius:2px`;
      let span = new FakeSpan('one two one');
      ui.highlightSpan(span, 'one');
      assert.deepEqual(marks(span), [{ text: 'one', css: mark('rgba(255,200,0,.40)') }, { text: 'one', css: mark('rgba(255,200,0,.40)') }]);
      assert.equal(span.textContent, 'one two one');
      ui.highlightSpan(span, 'one', { activeOccurrence: 1, markBg: '#111', markActiveBg: '#222' });
      assert.deepEqual(marks(span).map(m => m.css), [mark('#111'), mark('#222')], 're-running with new options is idempotent');
      ui.highlightSpan(span, 'absent');
      assert.equal(span.childElementCount, 0, 'a span with no match is flattened back to plain text');
      assert.equal(span.textContent, 'one two one');
      span = new FakeSpan('Alpha alphabet alpha');
      ui.highlightSpan(span, 'alpha', { wholeWord: true, matchCase: true });
      assert.deepEqual(marks(span).map(m => m.text), ['alpha'], 'whole-word and match-case: only the standalone lower-case word');
      ui.highlightSpan(span, 'alpha', { wholeWord: true });
      assert.deepEqual(marks(span).map(m => m.text), ['Alpha', 'alpha']);
      span = new FakeSpan('ab12 cd345');
      ui.highlightSpan(span, '[a-z]\\d+', { regex: true });
      assert.deepEqual(marks(span).map(m => m.text), ['b12', 'd345']);
      span = new FakeSpan('x');
      ui.highlightSpan(span, 'x', { markBg: 'red; background-image: url(https://example.invalid/x)', markActiveBg: 'expression(1)', activeOccurrence: 0 });
      assert.deepEqual(marks(span).map(m => m.css), [mark('rgba(255,140,0,.65)')], 'a colour that could add a declaration or fetch a resource is ignored');
    } finally { if (previous === undefined) delete globalThis.document; else globalThis.document = previous; }
    return { sources: ['getTextContent', 'streamTextContent'] };
  });

  await run('render-keepalive-wraps-animation-frames-only-while-held', async () => {
    const originalRequest = callback => { void callback; return 7; };
    const originalCancel = () => {};
    const fakeWindow = { requestAnimationFrame: originalRequest, cancelAnimationFrame: originalCancel, setTimeout: (fn, ms) => setTimeout(fn, ms), clearTimeout: id => clearTimeout(id) };
    const previous = globalThis.window;
    globalThis.window = fakeWindow;
    try {
      const release = ui.acquirePdfRenderKeepalive();
      assert.notEqual(fakeWindow.requestAnimationFrame, originalRequest, 'the frame request is wrapped while held');
      let fired = null;
      fakeWindow.requestAnimationFrame(timestamp => { fired = timestamp; });
      assert.equal(fired, null);
      await until(() => fired !== null, 'the 100 ms fallback timer');
      assert.equal(typeof fired, 'number', 'a paused frame is replaced by the timer');
      const second = ui.acquirePdfRenderKeepalive();
      release(); release();
      assert.notEqual(fakeWindow.requestAnimationFrame, originalRequest, 'still wrapped while another holder remains');
      second();
      assert.equal(fakeWindow.requestAnimationFrame, originalRequest, 'the original frame request returns when the last holder releases');
      assert.equal(fakeWindow.cancelAnimationFrame, originalCancel);
    } finally { if (previous === undefined) delete globalThis.window; else globalThis.window = previous; }
    return { fallbackMs: 100 };
  });

  assert.equal(result.scenarios.length, 10);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
