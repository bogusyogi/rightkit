import assert from 'node:assert/strict';
import { mkdtemp, readdir, readFile, rm, stat, writeFile, mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { extractArchive, inspectArchive, listArchiveMembers, UnsafeArchiveError, validateArchiveMembers } from './remote-tar.mjs';

/** Minimal ustar writer for test fixtures. */
function header({ path, typeflag = '0', size = 0, prefix = '' }) {
  const block = Buffer.alloc(512, 0);
  if (path.length > 100) { block.write(path.slice(0, 155), 345); block.write(path.slice(156), 0); }
  else block.write(path, 0);
  block.write('0000000', 124, 8); // size octal
  block.write(size.toString(8).padStart(11, '0') + '\0', 124, 12);
  block.write(typeflag, 156, 1);
  block.write('ustar\0', 257);
  block.write('00', 263);
  return block;
}

function paxBlock(records) {
  const body = records.map(([key, value]) => {
    const text = ` ${key}=${value}\n`;
    // PAX length includes its decimal prefix and complete record (`length` +
    // leading space through trailing newline). Do not seed with an extra byte:
    // that leaves the parser offset one byte past each record and can hang its
    // length loop on otherwise valid fixtures.
    let length = text.length + String(text.length).length;
    while (`${length}${text}`.length !== length) length += 1;
    return `${length} ${key}=${value}\n`;
  }).join('');
  const padded = Buffer.alloc(Math.ceil(body.length / 512) * 512);
  padded.write(body);
  const paxHeader = header({ path: 'PaxHeader', typeflag: 'x', size: body.length });
  return [paxHeader, padded];
}

function fileBlocks(path, content) {
  const payload = Buffer.from(content, 'utf8');
  const blocks = [header({ path, typeflag: path.endsWith('/') ? '5' : '0', size: payload.byteLength })];
  if (payload.byteLength === 0) return blocks;
  const padded = Buffer.alloc(Math.ceil(payload.byteLength / 512) * 512);
  payload.copy(padded);
  blocks.push(padded);
  return blocks;
}

function buildTar(entries) {
  const chunks = [];
  for (const entry of entries) {
    if (entry.pax) chunks.push(...paxBlock(entry.pax));
    chunks.push(...fileBlocks(entry.path ?? 'member.txt', entry.content ?? ''));
  }
  chunks.push(Buffer.alloc(1024)); // end-of-archive marker
  return Buffer.concat(chunks);
}

function entryWithHeaderOverride(block) {
  // Wrap a raw header (for unsupported typeflags) plus empty payload.
  return [block, Buffer.alloc(512)];
}

test('regular files & directories enumerate, validate, and extract', async () => {
  const archive = buildTar([
    { path: 'src/', content: '' },
    { path: 'src/main.rs', content: 'fn main() {}\n' },
    { path: 'Cargo.toml', content: '[package]\n' },
  ]);
  // Directory entries carry a trailing slash and zero size in this builder.
  const members = listArchiveMembers(archive);
  assert.deepEqual(members.map(({ path }) => path).sort(), ['Cargo.toml', 'src/', 'src/main.rs'].sort());
  assert.equal(validateArchiveMembers(members) > 0, true);
  const root = await mkdtemp(join(tmpdir(), 'remote-tar-'));
  try {
    const written = await extractArchive(archive, root);
    assert.equal(written, 2); // two regular files
    assert.equal(await readFile(join(root, 'src', 'main.rs'), 'utf8'), 'fn main() {}\n');
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('long paths survive via pax path overrides', () => {
  const deepSegment = 'd'.repeat(90);
  const longPath = `${deepSegment}/${deepSegment}/main.rs`;
  const archive = buildTar([{ path: longPath, pax: [['path', longPath]], content: 'x' }]);
  const [{ path }] = listArchiveMembers(archive);
  assert.equal(path, longPath);
  validateArchiveMembers(listArchiveMembers(archive));
});

for (const [label, entries] of Object.entries({
  symlink: [{ rawType: '2' }],
  hardlink: [{ rawType: '1' }],
  fifo: [{ rawType: '6' }],
  charDevice: [{ rawType: '3' }],
  blockDevice: [{ rawType: '4' }],
})) {
  test(`rejects ${label} archive members`, () => {
    const blocks = [header({ path: 'member', typeflag: entries[0].rawType }), Buffer.alloc(512), Buffer.alloc(1024)];
    assert.throws(() => inspectArchive(Buffer.concat(blocks)), (error) => error instanceof UnsafeArchiveError && error.code === 'REMOTE_UNSUPPORTED_MEMBER_TYPE');
  });
}

test('rejects pax linkpath overrides', () => {
  const target = 'elsewhere/Cargo.toml';
  const archive = [...paxBlock([['linkpath', target]]), header({ path: 'link', typeflag: '2' }), Buffer.alloc(512), Buffer.alloc(1024)];
  assert.throws(() => inspectArchive(Buffer.concat(archive)), /linkpath/);
});

for (const [label, path] of Object.entries({
  absolute: '/etc/passwd',
  driveQualified: 'D:/escape.rs',
  parentTraversal: '../escape.rs',
  backslash: 'src\\main.rs',
  alternateStream: 'src/main.rs:hidden',
  trailingDot: 'src/main.',
  trailingSpace: 'src/main ',
  reservedName: 'CON',
  reservedNameWithExtension: 'NUL.txt',
  submoduleConfig: '.gitmodules',
})) {
  test(`rejects ${label}`, () => {
    const archive = buildTar([{ path, content: '' }]);
    assert.throws(() => inspectArchive(archive), UnsafeArchiveError, path);
  });
}

test('rejects Windows case-folding collisions', () => {
  const archive = buildTar([
    { path: 'src/Main.rs', content: '' },
    { path: 'src/main.rs', content: '' },
  ]);
  assert.throws(() => inspectArchive(archive), (error) => error.code === 'REMOTE_ARCHIVE_CASE_COLLISION');
});

test('extraction never escapes the destination for validated archives', async () => {
  const archive = buildTar([
    { path: 'nested/deep/file.txt', content: 'ok' },
  ]);
  const root = await mkdtemp(join(tmpdir(), 'remote-tar-x-'));
  try {
    await extractArchive(archive, join(root, 'replica'));
    assert.equal(await readFile(join(root, 'replica', 'nested', 'deep', 'file.txt'), 'utf8'), 'ok');
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('rejects truncated headers, payloads, and PAX records without looping', () => {
  assert.throws(() => inspectArchive(Buffer.alloc(511)), /whole 512-byte tar blocks/);
  assert.throws(() => inspectArchive(Buffer.concat([header({ path: 'broken', size: 1 })])), /truncated tar member payload/);
  assert.throws(() => inspectArchive(Buffer.concat([header({ path: 'PaxHeader', typeflag: 'x', size: 1 })])), /truncated tar member payload/);
});
