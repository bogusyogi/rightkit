import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const recipeId = 'doc-render-markdown-sanitize-consumer';
const result = {
  schemaVersion: 1, recipeId, ok: false, nativeHostQualified: false,
  host: { platform: process.platform, arch: process.arch, node: process.version },
  artifact: null, scenarios: [], errors: [],
};
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
// Fixture failures report fixed scenario names only: never serialize an untrusted host error.
let stage = 'artifact-provenance';
try {
  const archive = required('RIGHTKIT_CONSUMER_ARTIFACT');
  assert(archive.startsWith('/') || /^[A-Za-z]:[\\/]/.test(archive), 'artifact must be absolute');
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/doc-render');
  const stylesUrl = import.meta.resolve('@rightkit/doc-render/styles.css');
  const shikiUrl = import.meta.resolve('@rightkit/doc-render/shiki.css');
  const provenance = validateInstalledArtifact({
    fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/doc-render', rootResolvedUrl: root,
    entries: { '@rightkit/doc-render': root, '@rightkit/doc-render/styles.css': stylesUrl, '@rightkit/doc-render/shiki.css': shikiUrl },
  });
  result.artifact = provenance.artifact;
  const docRender = await import(provenance.entries['@rightkit/doc-render']);
  const run = async (name, body) => { stage = name; const evidence = await body(); result.scenarios.push({ name, ok: true, evidence }); };

  await run('root-exports-public-surface', async () => {
    for (const name of ['MarkdownRenderer', 'HtmlRenderer', 'CodeBlock', 'renderMarkdown', 'renderHtml', 'renderMarkdownToHtml', 'parseMarkdown', 'sanitizeHtml', 'safeLinkHref', 'isSafeUri', 'buildBlocks', 'formatCitation']) {
      assert.equal(typeof docRender[name], 'function', name);
    }
    return { functionExports: 12 };
  });

  await run('raw-html-never-emitted-as-script', async () => {
    delete globalThis.__rightkitFixturePwned;
    const html = docRender.renderMarkdownToHtml('# Fixture\n\n<script>globalThis.__rightkitFixturePwned = true</script>\n\nSafe text.');
    assert.equal(/<script/i.test(html), false);
    assert.match(html, /<h1[\s>]/);
    assert.ok(html.includes('Safe text.'));
    assert.equal(globalThis.__rightkitFixturePwned, undefined);
    return { scriptElementEmitted: false, headingEmitted: true, executed: false };
  });

  await run('link-policy-allows-only-reviewed-schemes', async () => {
    assert.equal(docRender.safeLinkHref('javascript:alert(1)'), undefined);
    assert.equal(docRender.safeLinkHref(' https://example.com/a '), 'https://example.com/a');
    assert.equal(docRender.safeLinkHref('mailto:support@example.com'), 'mailto:support@example.com');
    assert.equal(docRender.isSafeUri('javascript:alert(1)'), false);
    return { javascriptScheme: 'refused', httpsScheme: 'allowed', mailtoScheme: 'allowed' };
  });

  await run('parse-tree-deterministic-and-cache-clearable', async () => {
    const first = docRender.parseMarkdown('# A\n\nBody');
    const second = docRender.parseMarkdown('# A\n\nBody');
    assert.ok(Array.isArray(first) && first.length > 0);
    assert.deepEqual(second, first);
    docRender.clearMarkdownParseCache();
    assert.equal(docRender.getMarkdownParseCacheEntryCount(), 0);
    assert.equal(docRender.getMarkdownParseCacheBytes(), 0);
    return { blocks: first.length, cacheClearedToZero: true };
  });

  await run('stylesheet-subpaths-bound-to-archive', async () => {
    const styles = readFileSync(fileURLToPath(provenance.entries['@rightkit/doc-render/styles.css']));
    const shiki = readFileSync(fileURLToPath(provenance.entries['@rightkit/doc-render/shiki.css']));
    assert.ok(styles.length > 0 && shiki.length > 0);
    return { stylesBytes: styles.length, shikiBytes: shiki.length };
  });

  stage = 'qualification-complete';
  result.ok = true;
} catch {
  result.errors.push({ stage, code: 'consumer-assertion-failed' });
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
