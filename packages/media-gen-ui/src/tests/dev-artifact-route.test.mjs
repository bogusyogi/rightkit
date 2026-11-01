// #170 / #175: the development /api/artifact route (scripts/dev-artifact-route.mjs) serves only media
// from its allowed roots. Allowed roots here are a scratch Content folder (RIGHTKIT_CONTENT_ROOT) and a
// scratch extra root (RIGHTKIT_DEV_ARTIFACT_ROOTS). Nothing machine-specific is allowed and no
// fixtures folder is read without AI_MEDIA_CONSOLE_ALLOW_FIXTURES=1. The route is served over a real
// HTTP server so streaming and range semantics are the ones a browser sees. Scratch files only.
import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const scratch = mkdtempSync(path.join(tmpdir(), 'gr-dev-artifact-route-'));
const contentRoot = path.join(scratch, 'Content');
const extraRoot = path.join(scratch, 'extra');
const outsideRoot = path.join(scratch, 'outside');
mkdirSync(contentRoot, { recursive: true });
mkdirSync(extraRoot, { recursive: true });
mkdirSync(outsideRoot, { recursive: true });

// The route reads its environment at import time, so set it before loading the module.
process.env.RIGHTKIT_CONTENT_ROOT = contentRoot;
process.env.RIGHTKIT_DEV_ARTIFACT_ROOTS = extraRoot;
delete process.env.AI_MEDIA_CONSOLE_ALLOW_FIXTURES;

const routeUrl = pathToFileURL(path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', 'scripts', 'dev-artifact-route.mjs')).href;
const { streamArtifact } = await import(routeUrl);

const PNG_BYTES = Buffer.from('89504e470d0a1a0a0000000d49484452', 'hex');
const inContent = path.join(contentRoot, 'hr', 'still.png');
const inExtra = path.join(extraRoot, 'clip.mp4');
const notMedia = path.join(contentRoot, 'notes.exe');
mkdirSync(path.dirname(inContent), { recursive: true });
writeFileSync(inContent, PNG_BYTES);
writeFileSync(inExtra, Buffer.from('0123456789abcdef'));
writeFileSync(notMedia, 'binary');
writeFileSync(path.join(outsideRoot, 'secret.png'), PNG_BYTES);

const server = http.createServer((req, res) => {
  streamArtifact(req, res);
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const base = `http://127.0.0.1:${server.address().port}/api/artifact`;

test.after(async () => {
  await new Promise((resolve) => server.close(resolve));
  rmSync(scratch, { recursive: true, force: true });
});

const get = (query, headers = {}) => fetch(`${base}${query}`, { headers });
const qp = (p) => `?path=${encodeURIComponent(p)}`;

test('a request without a path is refused with 400', async () => {
  const res = await get('');
  assert.equal(res.status, 400);
});

test('media under the Content root is served with its type and bytes', async () => {
  const res = await get(qp(inContent));
  assert.equal(res.status, 200);
  assert.equal(res.headers.get('content-type'), 'image/png');
  assert.deepEqual(Buffer.from(await res.arrayBuffer()), PNG_BYTES);
});

test('media under an extra root from RIGHTKIT_DEV_ARTIFACT_ROOTS is served', async () => {
  const res = await get(qp(inExtra));
  assert.equal(res.status, 200);
  assert.equal(res.headers.get('content-type'), 'video/mp4');
});

test('a byte range is served as 206 with a content-range header', async () => {
  const res = await get(qp(inExtra), { range: 'bytes=0-3' });
  assert.equal(res.status, 206);
  assert.equal(res.headers.get('content-range'), 'bytes 0-3/16');
  assert.equal(Buffer.from(await res.arrayBuffer()).toString(), '0123');
});

test('a file outside every allowed root is refused with 403, even when it exists', async () => {
  const res = await get(qp(path.join(outsideRoot, 'secret.png')));
  assert.equal(res.status, 403);
});

test('a machine-specific absolute path that is not an allowed root is refused', async () => {
  const res = await get(qp('D:\\Downloads\\x.png'));
  assert.equal(res.status, 403);
});

test('traversal out of an allowed root with .. segments is refused', async () => {
  const res = await get(qp(path.join(contentRoot, '..', 'outside', 'secret.png')));
  assert.equal(res.status, 403);
});

test('the repository fixtures folder is refused without AI_MEDIA_CONSOLE_ALLOW_FIXTURES=1', async () => {
  const repoFixture = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', 'fixtures', 'qa-media', 'clip.mp4');
  const res = await get(qp(repoFixture));
  assert.equal(res.status, 403);
});

test('an allowed root with a missing file returns 404', async () => {
  const res = await get(qp(path.join(contentRoot, 'missing.png')));
  assert.equal(res.status, 404);
});

test('a non-media extension inside an allowed root returns 415', async () => {
  const res = await get(qp(notMedia));
  assert.equal(res.status, 415);
});
