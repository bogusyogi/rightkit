import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, realpathSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { gunzipSync } from 'node:zlib';

const within = (root, target) => {
  const relative = path.relative(root, target);
  return !path.isAbsolute(relative) && relative !== '..' && !relative.startsWith(`..${path.sep}`);
};
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
export function fixtureEntry(fixtureUrl, resolvedUrl) {
  const root = realpathSync(path.dirname(fileURLToPath(fixtureUrl)));
  const entry = realpathSync(fileURLToPath(resolvedUrl));
  assert(within(root, entry) && entry.split(path.sep).includes('node_modules'), 'dependency export must belong to fixture installation');
  return pathToFileURL(entry).href;
}
const field = bytes => bytes.toString('utf8').replace(/\0.*$/s, '');
function archiveFiles(bytes) {
  const tar = gunzipSync(bytes, { maxOutputLength: 128 * 1024 * 1024 });
  const files = new Map();
  let offset = 0, pending = {}, global = {}, ended = false;
  while (offset + 512 <= tar.length) {
    const header = tar.subarray(offset, offset + 512);
    if (header.every(value => value === 0)) { ended = true; break; }
    const number = bytes => { const text = field(bytes).trim(); assert(/^[0-7]+$/.test(text)); return parseInt(text, 8); };
    const checksum = number(header.subarray(148, 156));
    assert.equal(header.reduce((sum, byte, index) => sum + (index >= 148 && index < 156 ? 32 : byte), 0), checksum, 'invalid tar checksum');
    const size = number(header.subarray(124, 136));
    assert(Number.isSafeInteger(size) && size >= 0 && offset + 512 + size <= tar.length, 'invalid tar size');
    const data = tar.subarray(offset + 512, offset + 512 + size);
    const type = String.fromCharCode(header[156]);
    const prefix = field(header.subarray(345, 500));
    const rawName = `${prefix ? `${prefix}/` : ''}${field(header.subarray(0, 100))}`;
    offset += 512 + Math.ceil(size / 512) * 512;
    if (type === 'x' || type === 'g') {
      const values = {};
      for (let cursor = 0; cursor < data.length;) {
        const space = data.indexOf(32, cursor);
        assert(space > cursor);
        const lengthText = data.subarray(cursor, space).toString();
        assert(/^[0-9]+$/.test(lengthText));
        const length = Number(lengthText), end = cursor + length;
        assert(length > space - cursor + 2 && end <= data.length && data[end - 1] === 10);
        const record = data.subarray(space + 1, end - 1).toString('utf8'), equal = record.indexOf('=');
        assert(equal > 0);
        values[record.slice(0, equal)] = record.slice(equal + 1);
        cursor = end;
      }
      assert(!('size' in values) && !('linkpath' in values), 'unsupported pax size/link override');
      if (type === 'g') global = { ...global, ...values }; else pending = values;
      continue;
    }
    const name = pending.path ?? global.path ?? rawName;
    pending = {};
    assert(name.startsWith('package/') && !name.includes('\\'), 'unexpected archive path');
    const relative = name.slice(8);
    assert(relative.split('/').every(part => part !== '..' && part !== '.'), 'unsafe archive path');
    if (type === '5') continue;
    assert(type === '0' || type === '\0', 'archive links/special entries are forbidden');
    assert(relative && !files.has(relative), 'duplicate archive file');
    files.set(relative, data);
  }
  assert(ended && files.has('package.json'), 'incomplete npm archive');
  return files;
}

export function validateInstalledArtifact({ fixtureUrl, archive, sha256, version, resolution, packageName, rootResolvedUrl, entries }) {
  assert(path.isAbsolute(archive), 'artifact must be absolute');
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert(['packed', 'fetched'].includes(resolution));
  const archiveBytes = readFileSync(archive);
  assert.equal(hash(archiveBytes), sha256, 'archive digest mismatch');
  const fixtureRoot = realpathSync(path.dirname(fileURLToPath(fixtureUrl)));
  const rootModule = realpathSync(fileURLToPath(rootResolvedUrl));
  assert(within(fixtureRoot, rootModule) && rootModule.split(path.sep).includes('node_modules'), 'resolved export must belong to fixture installation');
  const packageRoot = realpathSync(path.resolve(path.dirname(rootModule), '..'));
  assert(within(fixtureRoot, packageRoot), 'package root escapes fixture');
  const files = archiveFiles(archiveBytes);
  const manifest = JSON.parse(files.get('package.json').toString('utf8'));
  assert.equal(manifest.name, packageName);
  assert.equal(manifest.version, version);
  for (const [relative, bytes] of files) {
    const installed = realpathSync(path.resolve(packageRoot, ...relative.split('/')));
    assert(within(packageRoot, installed), 'installed file escapes package root');
    assert.equal(hash(readFileSync(installed)), hash(bytes), `installed archive file mismatch: ${relative}`);
  }
  const checkedEntries = {};
  for (const [specifier, resolvedUrl] of Object.entries(entries)) {
    const modulePath = realpathSync(fileURLToPath(resolvedUrl));
    assert(within(packageRoot, modulePath), 'selected export escapes installed package');
    const relative = path.relative(packageRoot, modulePath).split(path.sep).join('/');
    assert(files.has(relative), 'selected export absent from exact archive');
    checkedEntries[specifier] = pathToFileURL(modulePath).href;
  }
  return { entries: checkedEntries, artifact: { path: archive, sha256, version, resolution, modulePath: rootModule, installationBinding: 'all-archive-files-sha256', archiveFileCount: files.size } };
}
