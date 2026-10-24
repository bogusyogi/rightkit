import { validateInstalledArtifact, fixtureEntry } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const recipeId = 'media-ui-player-markup-image-pipeline-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const rootSurface = ['DEFAULT_IMAGE_FONT_ID', 'DEFAULT_MEDIA_PLAYER_LABELS', 'DEFAULT_MEDIA_SPEEDS', 'MAX_EXPORT_CANVAS_PIXELS', 'MAX_IMAGE_ZOOM', 'MIN_IMAGE_ZOOM', 'MediaAdaptersProvider', 'MediaPlayer', 'actualSizeTransform', 'canPan', 'canvasToArrayBuffer', 'clampCrop', 'clampOffset', 'clearImageEditSnapshot', 'cropSourceSize', 'cssFilterForAdjustments', 'cssFontFamilyForImageText', 'defaultCrop', 'defaultImageSaveName', 'editableImageOutputForPath', 'ensureImageOutputPath', 'exportDimensionsFor', 'exportImageEdit', 'fitTransform', 'fitZoom', 'fontCssForImageText', 'getImageEditSnapshot', 'hasAdjustmentEdits', 'hasImageEdits', 'imageMimeFromPath', 'initialImageEdit', 'initialImageTransform', 'isBrowserImagePath', 'isBundledImageFontId', 'mediaClock', 'mediaErrorMessage', 'mimeForOutputExtension', 'normalizeResourceHandle', 'outputScaleForResize', 'resetImageEditStoreForTests', 'rotateTransform', 'sameImageEdit', 'scaleEditToSource', 'setImageEditSnapshot', 'useMediaAdapters', 'validateExportCanvasSize', 'zoomAtPoint'];

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/media-ui');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/media-ui', rootResolvedUrl: root, entries: { '@rightkit/media-ui': root, '@rightkit/media-ui/image': import.meta.resolve('@rightkit/media-ui/image'), '@rightkit/media-ui/styles.css': import.meta.resolve('@rightkit/media-ui/styles.css') } });
  result.artifact = provenance.artifact;
  const manifest = JSON.parse(readFileSync(path.join(path.dirname(provenance.artifact.modulePath), '..', 'package.json'), 'utf8'));
  const React = await import(fixtureEntry(import.meta.url, import.meta.resolve('react')));
  const { renderToStaticMarkup } = await import(fixtureEntry(import.meta.url, import.meta.resolve('react-dom/server')));
  const ui = await import(provenance.entries['@rightkit/media-ui']);
  const image = await import(provenance.entries['@rightkit/media-ui/image']);
  const css = readFileSync(fileURLToPath(provenance.entries['@rightkit/media-ui/styles.css']), 'utf8');
  const h = React.createElement;
  const render = (component, props) => renderToStaticMarkup(h(component, props));
  const plain = html => html.replace(/<svg[^>]*>.*?<\/svg>/g, '<svg/>');
  // The unit formats the pixel count with the host's default locale, so only the digits are compared.
  const tooLarge = pixels => error => error instanceof Error && /^image export is too large \(.+ pixels\)$/.test(error.message) && error.message.replace(/\D/g, '') === String(pixels);

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  await run('export-surface-exact-and-optional-peers-absent', async () => {
    assert.deepEqual(Object.keys(ui).sort(), rootSurface);
    assert.deepEqual(Object.keys(image).sort(), ['ImageEditor', 'ImageViewer']);
    assert.deepEqual(Object.keys(manifest.exports), ['.', './image', './styles.css']);
    assert.deepEqual(Object.keys(manifest.peerDependencies).sort(), ['fabric', 'react', 'react-dom', 'react-image-crop']);
    assert.deepEqual(Object.keys(manifest.peerDependenciesMeta).sort(), ['fabric', 'react-image-crop']);
    assert(manifest.peerDependenciesMeta.fabric.optional && manifest.peerDependenciesMeta['react-image-crop'].optional);
    for (const optional of ['fabric', 'react-image-crop']) assert.throws(() => import.meta.resolve(optional), error => error.code === 'ERR_MODULE_NOT_FOUND', `${optional} is not part of this installation`);
    assert.deepEqual(manifest.sideEffects, ['*.css']);
    for (const selector of ['.rk-media', '.rk-media-controls', '.rk-iv', '.rk-iv-stage']) assert(css.includes(selector), selector);
    return { rootExports: rootSurface.length, imageExports: 2, optionalPeersInstalled: false };
  });

  await run('media-player-server-markup-video-and-audio', async () => {
    const video = plain(render(ui.MediaPlayer, { src: '/media/clip.mp4', kind: 'video', title: 'Clip', info: '320x180 4s' }));
    assert(video.startsWith('<div class="rk-media rk-media-video" data-player-status="loading" tabindex="0" role="group" aria-label="Clip"><video class="rk-media-video" preload="metadata" playsInline="" src="/media/clip.mp4"></video><p role="status" class="rk-media-status">Loading media…</p><div class="rk-media-controls">'));
    for (const fragment of [
      '<button type="button" class="rk-media-btn" aria-label="Play" disabled=""><svg/></button>',
      '<button type="button" class="rk-media-btn" aria-label="Replay from start" disabled=""><svg/></button>',
      '<output class="rk-media-time" aria-label="Playback time">0:00 / 0:00</output>',
      '<input class="rk-media-seek" type="range" aria-label="Seek" min="0" max="1" step="0.1" disabled="" value="0"/>',
      '<button type="button" class="rk-media-btn" aria-label="Mute"><svg/></button>',
      '<input class="rk-media-vol" type="range" aria-label="Volume" min="0" max="1" step="0.05" value="1"/>',
      '<button type="button" class="rk-media-btn rk-media-speed" aria-label="Playback speed 1×">1×</button>',
      '<button type="button" class="rk-media-btn" aria-label="Fullscreen" disabled=""><svg/></button>',
      '<a class="rk-media-btn" href="/media/clip.mp4" target="_blank" rel="noreferrer" aria-label="Open original" title="Open original"><svg/></a></div><div class="rk-media-info">320x180 4s</div></div>',
    ]) assert(video.includes(fragment), fragment);
    const audio = plain(render(ui.MediaPlayer, { src: '/media/a.mp3', kind: 'audio', compact: true, speedControl: 'menu', labels: { play: 'Spela' }, showArtwork: false, showOpenOriginal: false, captionsUrl: 'https://example.invalid/c.vtt' }));
    assert(audio.startsWith('<div class="rk-media rk-media-audio compact" data-player-status="loading" tabindex="0" role="group" aria-label="Media player"><audio preload="metadata" src="/media/a.mp3"><track kind="captions" src="https://example.invalid/c.vtt" label="Transcript" srcLang="und" default=""/></audio>'));
    assert(audio.includes('aria-label="Spela"') && !audio.includes('aria-label="Play"'), 'label overrides replace the defaults');
    assert(audio.includes('<select class="rk-media-speed-menu" aria-label="Playback speed 1×"><option value="0.5">0.5×</option><option value="0.75">0.75×</option><option value="1" selected="">1×</option><option value="1.25">1.25×</option><option value="1.5">1.5×</option><option value="2">2×</option></select>'));
    assert(!audio.includes('Open original') && !audio.includes('Fullscreen'), 'audio has no fullscreen and showOpenOriginal false drops the link');
    const custom = plain(render(ui.MediaPlayer, {
      src: '/media/clip.mp4', kind: 'video', poster: 'https://example.invalid/p.jpg', autoPlay: true, showOpenOriginal: false, speeds: [1, 2], labels: { loading: 'Hold on' }, icons: { play: h('b', null, 'P'), fullscreen: null },
      tracks: [{ src: 'https://example.invalid/en.vtt', srclang: 'en', label: 'English', kind: 'captions', default: true }], onProxy: () => {},
    }));
    assert(custom.includes('<video class="rk-media-video" preload="metadata" autoPlay="" playsInline="" poster="https://example.invalid/p.jpg" src="/media/clip.mp4"><track kind="captions" src="https://example.invalid/en.vtt" label="English" srcLang="en" default=""/></video>'));
    assert(custom.includes('>Hold on</p>') && custom.includes('<button type="button" class="rk-media-btn" aria-label="Play" disabled=""><b>P</b></button>'), 'a custom icon node replaces the built-in glyph');
    assert(custom.includes('<button type="button" class="rk-media-btn" aria-label="Subtitles" aria-pressed="true">CC</button>'), 'a default track shows the subtitles toggle pressed');
    assert(custom.includes('<button type="button" class="rk-media-btn" aria-label="Fullscreen" disabled=""></button>'), 'a null icon renders nothing inside its button');
    assert(!custom.includes('Open original'));
    return { players: 3 };
  });

  await run('adapters-resolve-sources-and-normalise-resource-handles', async () => {
    const withProvider = plain(render(ui.MediaAdaptersProvider, { adapters: { resolveSrc: source => `asset://${source}` }, children: h(ui.MediaPlayer, { src: '/x.mp4', kind: 'video' }) }));
    assert(withProvider.includes('<video class="rk-media-video" preload="metadata" playsInline="" src="asset:///x.mp4"></video>') && withProvider.includes('href="asset:///x.mp4"'), 'a provider adapter resolves the source and the original link');
    const overridden = plain(render(ui.MediaAdaptersProvider, { adapters: { resolveSrc: () => 'provider://wins' }, children: h(ui.MediaPlayer, { src: '/x.mp4', kind: 'video', adapters: { resolveSrc: () => 'prop://wins' } }) }));
    assert(overridden.includes('src="prop://wins"') && !overridden.includes('provider://'), 'a component prop wins over the provider');
    let seen;
    const Probe = ({ override }) => { seen = ui.useMediaAdapters(override); return h('i'); };
    render(Probe, {});
    assert.deepEqual(seen, {}, 'no provider: an empty adapter set');
    const adapters = { notify: () => {} };
    render(ui.MediaAdaptersProvider, { adapters, children: h(Probe, {}) });
    assert.equal(seen, adapters);
    render(ui.MediaAdaptersProvider, { adapters, children: h(Probe, { override: { logError: () => {} } }) });
    assert.deepEqual(Object.keys(seen), ['logError']);
    const released = [];
    const fromFunction = ui.normalizeResourceHandle(() => released.push('fn'));
    fromFunction.release(); fromFunction.update({ dirty: true });
    const fromHandle = ui.normalizeResourceHandle({ release: () => released.push('handle'), update: next => released.push(next) });
    fromHandle.release(); fromHandle.update({ dirty: true });
    const fromNothing = ui.normalizeResourceHandle(undefined);
    assert.equal(fromNothing.release(), undefined);
    assert.equal(fromNothing.update({}), undefined);
    const partial = ui.normalizeResourceHandle({ release: () => released.push('partial') });
    partial.update({ stateExternalized: true }); partial.release();
    assert.deepEqual(released, ['fn', 'handle', { dirty: true }, 'partial']);
    return { adapterPrecedence: ['prop', 'provider', 'empty'] };
  });

  await run('player-labels-clock-and-error-messages', async () => {
    assert.equal(Object.keys(ui.DEFAULT_MEDIA_PLAYER_LABELS).length, 31);
    assert.equal(ui.DEFAULT_MEDIA_PLAYER_LABELS.play, 'Play');
    assert.equal(ui.DEFAULT_MEDIA_PLAYER_LABELS.errorTitle, 'Preview failed');
    assert.deepEqual(ui.DEFAULT_MEDIA_SPEEDS, [0.5, 0.75, 1, 1.25, 1.5, 2]);
    assert.deepEqual([0, 59.9, 75, 3725, Number.NaN, -5, Number.POSITIVE_INFINITY].map(ui.mediaClock), ['0:00', '0:59', '1:15', '1:02:05', '0:00', '0:00', '0:00']);
    const message = (code, labels) => ui.mediaErrorMessage({ error: code === undefined ? null : { code } }, labels);
    assert.equal(message(4), 'Unsupported codec or container for this platform player.');
    assert.equal(message(2), 'Network error while loading media.');
    assert.equal(message(3), 'Could not decode this media file.');
    assert.equal(message(1), 'Could not load this media file.');
    assert.equal(message(undefined), 'Could not load this media file.');
    assert.equal(message(4, { ...ui.DEFAULT_MEDIA_PLAYER_LABELS, errorUnsupported: 'Codec missing' }), 'Codec missing');
    return { labels: Object.keys(ui.DEFAULT_MEDIA_PLAYER_LABELS).length };
  });

  await run('image-edit-model-and-export-geometry', async () => {
    const initial = ui.initialImageEdit();
    assert.deepEqual(initial, { crop: null, rotation: 0, flipX: false, flipY: false, adjustments: { brightness: 100, contrast: 100, saturation: 100, hue: 0, grayscale: 0, sepia: 0, blur: 0 }, text: [], resize: { mode: 'original' } });
    assert.notEqual(ui.initialImageEdit().adjustments, initial.adjustments, 'each edit owns its adjustments');
    assert.equal(ui.hasImageEdits(initial), false);
    assert.equal(ui.hasImageEdits({ ...initial, rotation: 360 }), false, 'a full turn is no edit');
    assert.equal(ui.hasImageEdits({ ...initial, rotation: -90 }), true);
    assert.equal(ui.hasImageEdits({ ...initial, flipX: true }), true);
    assert.equal(ui.hasImageEdits({ ...initial, crop: { x: 0, y: 0, width: 1, height: 1 } }), true);
    assert.equal(ui.hasImageEdits({ ...initial, adjustments: { ...initial.adjustments, sepia: 1 } }), true);
    const layer = { id: 'a', text: '   ', x: 0.5, y: 0.5, width: 50, size: 12, color: '#ffffff', bold: false, angle: 0 };
    assert.equal(ui.hasImageEdits({ ...initial, text: [layer] }), false, 'a blank text layer is no edit');
    assert.equal(ui.hasImageEdits({ ...initial, text: [{ ...layer, text: 'Hi' }] }), true);
    assert.equal(ui.hasAdjustmentEdits(initial.adjustments), false);
    assert.equal(ui.hasAdjustmentEdits({ ...initial.adjustments, blur: 1 }), true);
    assert.equal(ui.sameImageEdit(initial, ui.initialImageEdit()), true);
    assert.equal(ui.sameImageEdit(initial, { ...initial, resize: { mode: 'percent', value: 50 } }), false);
    assert.equal(ui.sameImageEdit({ ...initial, text: [layer] }, { ...initial, text: [{ ...layer }] }), true);
    assert.equal(ui.sameImageEdit({ ...initial, text: [layer] }, { ...initial, text: [{ ...layer, angle: 5 }] }), false);
    assert.equal(ui.cssFilterForAdjustments(initial.adjustments), 'brightness(100%) contrast(100%) saturate(100%) hue-rotate(0deg) grayscale(0%) sepia(0%)');
    assert.equal(ui.cssFilterForAdjustments({ ...initial.adjustments, blur: 2 }).endsWith(' blur(2px)'), true);
    assert.deepEqual(ui.cropSourceSize({ width: 400, height: 200 }, null), { width: 400, height: 200 });
    assert.deepEqual(ui.cropSourceSize({ width: 400, height: 200 }, { x: 0, y: 0, width: 0, height: 0.2 }), { width: 1, height: 1 });
    const source = { width: 400, height: 200 };
    assert.deepEqual([{ mode: 'percent', value: 50 }, { mode: 'width', value: 1000 }, { mode: 'height', value: 1 }, { mode: 'original' }].map(resize => ui.exportDimensionsFor(source, resize)), [{ width: 200, height: 100 }, { width: 1000, height: 500 }, { width: 4, height: 2 }, { width: 400, height: 200 }]);
    assert.equal(ui.outputScaleForResize({ width: 100, height: 100 }, { mode: 'percent', value: 5000 }), 8, 'the scale is bounded to 8');
    assert.equal(ui.outputScaleForResize({ width: 100, height: 100 }, { mode: 'percent', value: 0.1 }), 0.01);
    assert.equal(ui.outputScaleForResize({ width: 0, height: 5 }, { mode: 'percent', value: 50 }), 1);
    assert.equal(ui.MAX_EXPORT_CANVAS_PIXELS, 50_000_000);
    assert.doesNotThrow(() => ui.validateExportCanvasSize({ width: 5000, height: 5000 }));
    assert.throws(() => ui.validateExportCanvasSize({ width: 10_000, height: 10_000 }), tooLarge(100_000_000));
    assert.throws(() => ui.validateExportCanvasSize({ width: Number.NaN, height: 10 }), /image export is too large/);
    const edit = { ...initial, crop: { x: 10, y: 20, width: 100, height: 50 }, text: [{ ...layer, text: 'T', size: 10, width: 50 }] };
    assert.equal(ui.scaleEditToSource(edit, null, source), edit);
    assert.equal(ui.scaleEditToSource(edit, source, source), edit);
    assert.deepEqual(ui.scaleEditToSource(edit, { width: 200, height: 100 }, source), { ...edit, crop: { x: 20, y: 40, width: 200, height: 100 }, text: [{ ...edit.text[0], size: 20, width: 100 }] });
    return { exportSizes: 4 };
  });

  await run('export-pipeline-draws-crop-rotate-flip-filter-text-through-a-fake-canvas', async () => {
    const calls = [];
    const encoded = [];
    const context = new Proxy({
      save: () => calls.push(['save']), restore: () => calls.push(['restore']), translate: (...a) => calls.push(['translate', ...a]), rotate: (...a) => calls.push(['rotate', ...a]), scale: (...a) => calls.push(['scale', ...a]),
      drawImage: (_image, ...a) => calls.push(['drawImage', ...a]), strokeText: (...a) => calls.push(['strokeText', ...a]), fillText: (...a) => calls.push(['fillText', ...a]), measureText: text => ({ width: text.length * 6 }),
    }, { set(target, key, value) { calls.push(['set', key, value]); target[key] = value; return true; } });
    const canvas = { width: 0, height: 0, getContext: type => (type === '2d' ? context : null), toBlob: (callback, mime, quality) => { encoded.push([mime, quality]); callback(new Blob([new Uint8Array([1, 2, 3, 4])], { type: mime })); } };
    let loads = 0;
    let failLoad = false;
    let imageSize = { width: 400, height: 200 };
    const previous = { Image: globalThis.Image, document: globalThis.document };
    globalThis.Image = class { constructor() { this.naturalWidth = imageSize.width; this.naturalHeight = imageSize.height; this.width = imageSize.width; this.height = imageSize.height; } set src(value) { this.source = value; loads += 1; queueMicrotask(() => (failLoad ? this.onerror?.() : this.onload?.())); } get src() { return this.source; } };
    globalThis.document = { createElement: tag => { assert.equal(tag, 'canvas'); return canvas; }, fonts: undefined };
    try {
      const edit = {
        crop: { x: 10, y: 20, width: 200, height: 100 }, rotation: 90, flipX: true, flipY: false,
        adjustments: { brightness: 120, contrast: 100, saturation: 100, hue: 0, grayscale: 0, sepia: 0, blur: 0 },
        text: [{ id: 't1', text: 'Hello world', x: 0.5, y: 0.5, width: 120, size: 20, color: '#ffffff', bold: true, angle: 0 }, { id: 't2', text: '   ', x: 0, y: 0, width: 10, size: 10, color: '#000000', bold: false, angle: 0 }],
        resize: { mode: 'percent', value: 50 },
      };
      const payload = await ui.exportImageEdit('https://example.invalid/a.png', edit, 'png');
      assert.deepEqual([payload.extension, payload.mime, payload.buffer.byteLength], ['png', 'image/png', 4]);
      assert.deepEqual(new Uint8Array(payload.buffer), new Uint8Array([1, 2, 3, 4]));
      assert.deepEqual([canvas.width, canvas.height], [50, 100], 'crop 200x100, rotated a quarter turn, halved');
      assert.deepEqual(encoded, [['image/png', 0.92]]);
      assert.equal(loads, 1);
      assert.deepEqual(calls[0], ['save']);
      assert.deepEqual(calls[1], ['set', 'filter', 'brightness(120%) contrast(100%) saturate(100%) hue-rotate(0deg) grayscale(0%) sepia(0%)']);
      assert.deepEqual(calls[2], ['translate', 25, 50]);
      assert.deepEqual(calls[3], ['rotate', Math.PI / 2]);
      assert.deepEqual(calls[4], ['scale', -0.5, 0.5], 'flipX mirrors the horizontal scale');
      assert.deepEqual(calls[5], ['drawImage', 10, 20, 200, 100, -100, -50, 200, 100]);
      const text = calls.slice(6);
      assert.deepEqual(text.find(call => call[0] === 'translate'), ['translate', 90, 30], 'normalised text position maps into the cropped source space');
      assert.deepEqual(text.filter(call => call[0] === 'fillText'), [['fillText', 'Hello world', 0, 0]], 'a blank layer draws nothing and a short line is not wrapped');
      assert.deepEqual(text.find(call => call[0] === 'set' && call[1] === 'font'), ['set', 'font', '700 20px system-ui, sans-serif']);
      assert.deepEqual(text.find(call => call[0] === 'set' && call[1] === 'filter'), ['set', 'filter', 'none'], 'filters do not apply to text');
      assert.equal(calls.filter(call => call[0] === 'save').length, calls.filter(call => call[0] === 'restore').length, 'every save is restored');

      calls.length = 0;
      const wide = await ui.exportImageEdit('https://example.invalid/a.png', { ...ui.initialImageEdit(), text: [{ id: 'w', text: 'one two three four five', x: 0.5, y: 0.5, width: 60, size: 20, color: '#ff0000', bold: false, angle: 15 }] }, 'jpeg', [{ id: 'brand', name: 'Brand', stack: 'Brand Sans, sans-serif' }], null, { defaultFontId: 'brand' });
      assert.deepEqual([wide.extension, wide.mime], ['jpeg', 'image/jpeg']);
      assert.deepEqual([canvas.width, canvas.height], [400, 200]);
      assert.deepEqual(encoded.at(-1), ['image/jpeg', 0.92]);
      assert(calls.filter(call => call[0] === 'fillText').length > 1, 'long text wraps within the layer width');
      assert.deepEqual(calls.find(call => call[0] === 'set' && call[1] === 'font'), ['set', 'font', '20px Brand Sans, sans-serif'], 'the default font id selects a bundled font');

      calls.length = 0;
      await ui.exportImageEdit('https://example.invalid/a.png', { ...ui.initialImageEdit(), crop: { x: 10, y: 20, width: 100, height: 50 } }, 'webp', [], { width: 200, height: 100 });
      assert.deepEqual(calls.find(call => call[0] === 'drawImage'), ['drawImage', 20, 40, 200, 100, -100, -50, 200, 100], 'a recipe authored on a display variant is rescaled to the original');

      canvas.getContext = () => null;
      await assert.rejects(ui.exportImageEdit('https://example.invalid/a.png', ui.initialImageEdit(), 'png'), /could not create image export canvas/);
      canvas.getContext = type => (type === '2d' ? context : null);
      canvas.toBlob = callback => callback(null);
      await assert.rejects(ui.exportImageEdit('https://example.invalid/a.png', ui.initialImageEdit(), 'png'), /could not export image canvas/);
      canvas.toBlob = (callback, mime) => callback(new Blob([new Uint8Array([1])], { type: mime }));
      imageSize = { width: 10_000, height: 10_000 };
      await assert.rejects(ui.exportImageEdit('https://example.invalid/big.png', ui.initialImageEdit(), 'png'), tooLarge(100_000_000), 'the pixel budget is checked before a canvas is allocated');
      imageSize = { width: 400, height: 200 };
      failLoad = true;
      await assert.rejects(ui.exportImageEdit('https://example.invalid/a.png', ui.initialImageEdit(), 'png'), /could not load image for export/);
    } finally {
      globalThis.Image = previous.Image;
      if (previous.document === undefined) delete globalThis.document; else globalThis.document = previous.document;
    }
    return { canvas: [50, 100], encodings: encoded.length };
  });

  await run('image-save-names-types-and-canvas-bytes', async () => {
    assert.deepEqual(['a.PNG', 'a.jpg', 'a.jpeg', 'a.webp', 'a.gif', 'a.bmp', 'a.svg', 'a.ico', 'a.AVIF', 'a.tiff', 'a'].map(ui.isBrowserImagePath), [true, true, true, true, true, true, true, true, true, false, false]);
    assert.deepEqual(['/a.png', '/a.JPG', '/a.jpeg', '/a.webp', '/a.gif', '/a.bmp'].map(ui.editableImageOutputForPath), ['png', 'jpeg', 'jpeg', 'webp', null, null]);
    assert.deepEqual(['/a.ico', '/a.svg', '/a.jpg', '/a.xyz'].map(ui.imageMimeFromPath), ['image/x-icon', 'image/svg+xml', 'image/jpeg', null]);
    assert.deepEqual(['png', 'jpeg', 'webp'].map(ui.mimeForOutputExtension), ['image/png', 'image/jpeg', 'image/webp']);
    assert.deepEqual(['/p/Photo.JPG', 'C:\\p\\scan.bmp', '/p/b.webp', '/p/noext'].map(ui.defaultImageSaveName), ['Photo-edited.jpg', 'scan-edited.png', 'b-edited.webp', 'noext-edited.png']);
    assert.deepEqual([['/p/a.bmp', 'jpeg'], ['/p/a.bmp', 'webp'], ['/p/a.png', 'webp']].map(([file, ext]) => ui.ensureImageOutputPath(file, ext)), ['/p/a.bmp.jpg', '/p/a.bmp.webp', '/p/a.png']);
    const seen = [];
    const buffer = await ui.canvasToArrayBuffer({ toBlob: (callback, mime, quality) => { seen.push([mime, quality]); callback(new Blob([new Uint8Array([9, 8, 7])])); } }, 'image/webp', 0.5);
    assert.deepEqual([...new Uint8Array(buffer)], [9, 8, 7]);
    assert.deepEqual(seen, [['image/webp', 0.5]]);
    await assert.rejects(ui.canvasToArrayBuffer({ toBlob: callback => callback(null) }, 'image/png'), /could not export image canvas/);
    return { extensionsChecked: 11 };
  });

  await run('viewer-transform-crop-and-font-helpers', async () => {
    const t = ui.initialImageTransform();
    assert.deepEqual(t, { zoom: 1, offsetX: 0, offsetY: 0, rotation: 0, flipX: false, flipY: false, mode: 'fit' });
    assert.deepEqual([ui.MIN_IMAGE_ZOOM, ui.MAX_IMAGE_ZOOM], [0.1, 8]);
    const viewport = { width: 200, height: 100 };
    const photo = { width: 400, height: 200 };
    assert.equal(ui.fitZoom(viewport, { width: 400, height: 400 }, 0), 0.25);
    assert.equal(ui.fitZoom({ width: 1000, height: 1000 }, { width: 400, height: 400 }, 0), 1, 'fit never upscales');
    assert.equal(ui.fitZoom(viewport, { width: 400, height: 100 }, 90), 0.25, 'a quarter turn swaps the image axes');
    assert.equal(ui.fitZoom({ width: 0, height: 100 }, { width: 4, height: 4 }, 0), 1);
    assert.deepEqual(ui.zoomAtPoint({ ...t }, viewport, photo, 2, { x: 150, y: 50 }), { ...t, zoom: 2, offsetX: -50, mode: 'free' });
    assert.equal(ui.zoomAtPoint(t, viewport, photo, 100, { x: 0, y: 0 }).zoom, 8, 'zoom is bounded');
    assert.deepEqual(ui.rotateTransform(t, 90, viewport, photo), { ...t, zoom: 0.25, rotation: 90 });
    assert.equal(ui.rotateTransform(t, -90, viewport, photo).rotation, 270);
    assert.deepEqual(ui.actualSizeTransform({ ...t, zoom: 3, offsetX: 99 }, viewport, photo), { ...t, mode: 'actual' });
    assert.deepEqual(ui.fitTransform({ ...t, zoom: 3, offsetX: 99, mode: 'free' }, viewport, photo), { ...t, zoom: 0.5, mode: 'fit' });
    assert.deepEqual(ui.clampOffset({ ...t, zoom: 1, offsetX: 500, offsetY: -500 }, viewport, photo), { ...t, offsetX: 100, offsetY: -50 });
    assert.equal(ui.canPan({ ...t, zoom: 1 }, viewport, photo), true);
    assert.equal(ui.canPan({ ...t, zoom: 0.1 }, viewport, photo), false);
    assert.deepEqual(ui.defaultCrop({ width: 1000, height: 500 }), { x: 90, y: 45, width: 820, height: 410 });
    assert.deepEqual(ui.defaultCrop({ width: 1000, height: 500 }, 1), { x: 295, y: 45, width: 410, height: 410 });
    assert.deepEqual(ui.clampCrop({ x: -5, y: 490, width: 2000, height: 30.4 }, { width: 1000, height: 500 }), { x: 0, y: 470, width: 1000, height: 30 });
    const fonts = [{ id: 'default', name: 'Default', stack: 'Inter, sans-serif' }, { id: 'mono', name: 'Mono', stack: 'Menlo, monospace' }];
    assert.equal(ui.DEFAULT_IMAGE_FONT_ID, 'default');
    assert.equal(ui.cssFontFamilyForImageText(fonts, undefined), 'Inter, sans-serif');
    assert.equal(ui.cssFontFamilyForImageText(fonts, 'mono'), 'Menlo, monospace');
    assert.equal(ui.cssFontFamilyForImageText([], 'My "Font"\\x'), '"My Fontx", system-ui, sans-serif', 'a system font name is sanitised before it reaches CSS');
    assert.equal(ui.cssFontFamilyForImageText([], ''), 'system-ui, sans-serif');
    assert.equal(ui.cssFontFamilyForImageText([], '', { fallbackStack: 'Fallback, serif' }), 'Fallback, serif');
    assert.equal(ui.fontCssForImageText([], 'Arial', 3, true, { systemFallbackStack: 'serif' }), '700 8px "Arial", serif');
    assert.equal(ui.fontCssForImageText(fonts, 'mono', 24.4, false), '24px Menlo, monospace');
    assert.equal(ui.isBundledImageFontId(fonts, 'mono'), true);
    assert.equal(ui.isBundledImageFontId(fonts, 'Arial'), false);
    return { fitZoomDownscale: ui.fitZoom(viewport, { width: 400, height: 400 }, 0), defaultCrop: ui.defaultCrop({ width: 1000, height: 500 }) };
  });

  await run('edit-snapshot-store-copies-history', async () => {
    ui.resetImageEditStoreForTests();
    assert.equal(ui.getImageEditSnapshot('/pics/a.png'), undefined);
    const edit = { ...ui.initialImageEdit(), rotation: 90 };
    const undo = [ui.initialImageEdit()];
    ui.setImageEditSnapshot('/pics/a.png', { edit, undo, redo: [] });
    undo.push(edit);
    const stored = ui.getImageEditSnapshot('/pics/a.png');
    assert.equal(stored.undo.length, 1, 'the store keeps its own copy of the history');
    assert.equal(stored.edit, edit);
    stored.undo.length = 0;
    assert.equal(ui.getImageEditSnapshot('/pics/a.png').undo.length, 1, 'readers receive copies');
    ui.clearImageEditSnapshot('/pics/a.png');
    assert.equal(ui.getImageEditSnapshot('/pics/a.png'), undefined);
    ui.setImageEditSnapshot('/pics/b.png', { edit, undo: [], redo: [] });
    ui.resetImageEditStoreForTests();
    assert.equal(ui.getImageEditSnapshot('/pics/b.png'), undefined);
    return { paths: 2 };
  });

  await run('image-viewer-and-editor-server-markup-without-optional-peers', async () => {
    const props = { path: '/pics/a.png', src: 'https://example.invalid/a.png' };
    const viewer = plain(render(image.ImageViewer, props));
    assert(viewer.startsWith('<link rel="preload" as="image" href="https://example.invalid/a.png"/><div class="rk-iv"><div class="rk-iv-toolbar rk-iv-panels"><div class="rk-iv-bar"><div class="rk-iv-capsule" role="toolbar" aria-label="Image tools">'));
    const labels = html => [...html.matchAll(/aria-label="([^"]+)"/g)].map(match => match[1]);
    assert.deepEqual(labels(viewer), ['Image tools', 'Fit to window', 'Actual size', 'Crop', 'Adjust', 'Add text', 'Export size', 'Undo last edit', 'Redo edit', 'Reset edits and view', 'Zoom out', 'Zoom percent', 'Zoom in', 'Rotate left', 'Rotate right', 'Flip horizontal', 'Flip vertical', 'a.png']);
    assert(viewer.includes('<select aria-label="Zoom percent" title="Zoom 100%" disabled="" class="rk-iv-input rk-iv-zoom"><option value="25">25%</option>'));
    assert(viewer.includes('data-rk-image-stage="true" role="group" aria-roledescription="image viewer" aria-label="a.png" tabindex="0" class="rk-iv-stage"'), 'the stage is named after the file');
    assert(viewer.includes('<img data-rk-image-printable="true" src="https://example.invalid/a.png" alt="" class="rk-iv-img" style="filter:brightness(100%) contrast(100%) saturate(100%) hue-rotate(0deg) grayscale(0%) sepia(0%)" draggable="false"/>'));
    assert.equal(plain(render(image.ImageEditor, props)), viewer, 'ImageEditor is the same component under its editing name');
    const readOnly = plain(render(image.ImageViewer, { ...props, readOnly: true, displayName: 'Holiday.png', toolbarLabel: 'Viewer tools' }));
    assert.deepEqual(labels(readOnly), ['Viewer tools', 'Fit to window', 'Actual size', 'Zoom out', 'Zoom percent', 'Zoom in', 'Holiday.png'], 'view-only mode keeps zoom and fit and hides every edit control');
    const swap = render(image.ImageViewer, { ...props, displaySrc: 'https://example.invalid/a-small.png' });
    assert(swap.startsWith('<link rel="preload" as="image" href="https://example.invalid/a-small.png"/>') && swap.includes('src="https://example.invalid/a-small.png"'), 'the display variant is what loads first');
    return { editorControls: labels(viewer).length - 2, viewOnlyControls: labels(readOnly).length - 2 };
  });

  assert.equal(result.scenarios.length, 10);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
