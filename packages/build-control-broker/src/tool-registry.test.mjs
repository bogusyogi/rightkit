import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { ImmutableToolRegistry, ToolChangedError } from './tool-registry.mjs';

const hash = (value) => createHash('sha256').update(value).digest('hex');
const tool = (path, contents) => ({ id: 'cargo', path, root: path, sha256: hash(contents) });

test('unchanged tool identity avoids request-time rehash', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-tool-')); try {
    const path = join(root, 'cargo'); const contents = Buffer.from('stable-tool'); await writeFile(path, contents); let reads = 0;
    const registry = new ImmutableToolRegistry({ readFile: async (...args) => { reads += 1; return (await import('node:fs/promises')).readFile(...args); } });
    await registry.register(tool(path, contents)); await registry.verify('cargo'); await registry.verify('cargo');
    assert.equal(reads, 1);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('changed identity rehashes & returns typed TOOL_CHANGED event', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-tool-')); try {
    const path = join(root, 'cargo'); const contents = Buffer.from('stable-tool'); await writeFile(path, contents); const registry = new ImmutableToolRegistry(); await registry.register(tool(path, contents));
    await writeFile(path, 'changed-tool-bytes');
    await assert.rejects(registry.verify('cargo'), (error) => { assert.ok(error instanceof ToolChangedError); assert.equal(error.code, 'TOOL_CHANGED'); assert.equal(error.event.type, 'TOOL_CHANGED'); assert.equal(error.event.reason, 'identity-digest-mismatch'); return true; });
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('startup registration rejects a mismatched expected digest', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-tool-')); try {
    const path = join(root, 'cargo'); await writeFile(path, 'actual');
    await assert.rejects(new ImmutableToolRegistry().register(tool(path, Buffer.from('expected'))), (error) => error.code === 'TOOL_CHANGED' && error.event.reason === 'startup-digest-mismatch');
  } finally { await rm(root, { recursive: true, force: true }); }
});
