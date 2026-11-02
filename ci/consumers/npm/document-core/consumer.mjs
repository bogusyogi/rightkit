import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';

const recipeId = 'document-core-root-export-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const surface = [".", "./app/BlockDraftContext", "./app/blockDrafts", "./app/chrome", "./app/deferredMount", "./app/documentPrinting", "./app/dropCoordination", "./app/nativePrintPreview", "./app/resourceCensus", "./app/shell/docBar", "./app/shell/nativeMenu", "./app/shortcutChords", "./app/useCloseFindOnSurfaceChange", "./app/useEditText", "./documents/BackToTopButton", "./documents/DocumentToolbarFind", "./documents/DocumentToolbarShell", "./documents/FullscreenIcon", "./documents/PreviewChrome", "./documents/PreviewMessage", "./documents/base64", "./documents/mdx/mdxRuntimeScript", "./documents/pdf/NativePdfDocument", "./documents/pdf/PdfViewer", "./documents/pdf/macPdfNativeBridge", "./documents/pdf/pdfEditModel", "./documents/pdf/pdfEditor", "./documents/pdf/pdfToMarkdown", "./documents/perfEventCounters", "./documents/preview/usePreviewFrameSrc", "./documents/previewViewport", "./documents/scopedSelectAll", "./documents/searchUtils", "./documents/splitResizeMath", "./documents/toolbarPrimitives", "./documents/txt/cmScroll", "./documents/types", "./documents/useMirroredEditText", "./documents/usePreviewFind", "./documents/useSplitResize", "./documents/web/webUrl", "./package.json", "./platform", "./settings/fonts", "./settings/rk-settings.css", "./settings/sections/EditorSection", "./settings/sections/FileAssociationsSection", "./settings/sections/ThemeSection", "./settings/sections/TypographySection", "./settings/sections/onOff", "./settings/settings", "./settings/themes", "./state/bridgeEvents", "./state/diagnostics", "./state/errors", "./state/motion", "./state/recentUrls", "./state/tauriBridge", "./styles.css"];
const expected = ["BlockDraftProvider", "useBlockDrafts", "useOptionalBlockDrafts"];

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/document-core');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/document-core', rootResolvedUrl: root, entries: { '@rightkit/document-core': root } });
  result.artifact = provenance.artifact;
  const manifest = JSON.parse(readFileSync(path.join(path.dirname(provenance.artifact.modulePath), '..', 'package.json'), 'utf8'));

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  await run('root-export-resolves-and-exposes-reviewed-names', async () => {
    assert.equal(manifest.name, '@rightkit/document-core');
    assert.deepEqual(Object.keys(manifest.exports ?? {}).sort(), surface);
    const mod = await import(provenance.entries['@rightkit/document-core']);
    for (const key of expected) assert.ok(key in mod, `missing root export ${key}`);
    return { checked: expected, exportKeys: surface.length };
  });

  assert.equal(result.scenarios.length, 1);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
