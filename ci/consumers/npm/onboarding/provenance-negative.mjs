import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { gzipSync } from 'node:zlib';
import { validateInstalledArtifact } from './provenance.mjs';

// Optional source-only counterexample runner. Owner executes separately from qualification.
const base = process.env.RIGHTKIT_CONSUMER_NEGATIVE_ROOT;
assert(base && path.isAbsolute(base), 'explicit external scratch root required');
mkdirSync(base, { recursive: true });
const scratch = mkdtempSync(path.join(base, 'onboarding-provenance-negative-'));
function tar(files) {
  const blocks = [];
  for (const [relative, text] of Object.entries(files)) {
    const bytes = Buffer.from(text), header = Buffer.alloc(512);
    header.write(`package/${relative}`);
    for (const [start, width, value] of [[100, 8, 420], [108, 8, 0], [116, 8, 0], [124, 12, bytes.length], [136, 12, 0]]) header.write(value.toString(8).padStart(width - 1, '0') + '\0', start);
    header.fill(32, 148, 156); header[156] = 48; header.write('ustar\0', 257); header.write('00', 263);
    const sum = header.reduce((total, value) => total + value, 0);
    header.write(sum.toString(8).padStart(6, '0') + '\0 ', 148);
    blocks.push(header, bytes, Buffer.alloc((512 - bytes.length % 512) % 512));
  }
  return gzipSync(Buffer.concat([...blocks, Buffer.alloc(1024)]));
}
const marker = path.join(scratch, 'import-executed');
const source = `import {writeFileSync} from 'node:fs';writeFileSync(${JSON.stringify(marker)},'bad');export const poisoned=true;`;
const files = { 'package.json': JSON.stringify({ name: '@rightkit/onboarding', version: '0.6.3', type: 'module' }), 'dist/index.js': source };
const archive = path.join(scratch, 'fixture.tgz'), bytes = tar(files);
writeFileSync(archive, bytes);
const sha256 = createHash('sha256').update(bytes).digest('hex');
let cases = 0;
try {
  for (const mode of ['ancestor-fallback', 'installed-byte-mismatch']) {
    const fixture = path.join(scratch, mode, 'consumer'); mkdirSync(fixture, { recursive: true });
    const packageRoot = path.join(mode === 'ancestor-fallback' ? path.dirname(fixture) : fixture, 'node_modules', '@rightkit', 'onboarding');
    mkdirSync(path.join(packageRoot, 'dist'), { recursive: true });
    for (const [relative, content] of Object.entries(files)) writeFileSync(path.join(packageRoot, relative), content);
    if (mode === 'installed-byte-mismatch') writeFileSync(path.join(packageRoot, 'dist/index.js'), source + '\n// changed installed bytes');
    const url = pathToFileURL(path.join(packageRoot, 'dist/index.js')).href;
    let rejected = false;
    try {
      const checked = validateInstalledArtifact({ fixtureUrl: pathToFileURL(path.join(fixture, 'consumer.mjs')).href, archive, sha256, version: '0.6.3', resolution: 'packed', packageName: '@rightkit/onboarding', rootResolvedUrl: url, entries: { '@rightkit/onboarding': url } });
      await import(checked.entries['@rightkit/onboarding']);
    } catch (error) {
      assert.match(String(error.message), mode === 'ancestor-fallback' ? /fixture installation/ : /archive file mismatch/);
      rejected = true;
    }
    assert(rejected && !existsSync(marker), `${mode} must reject before executing target export`);
    cases++;
  }
  process.stdout.write(`${JSON.stringify({ recipeId: 'onboarding-provenance-negative', ok: true, rejectedBeforeImport: cases })}\n`);
} finally { rmSync(scratch, { recursive: true, force: true }); }
