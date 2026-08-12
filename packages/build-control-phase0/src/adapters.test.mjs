import test from 'node:test';
import assert from 'node:assert/strict';
import { adapterNames } from './adapters.mjs';

test('adapter registry covers frozen frontend scope', () => assert.deepEqual(adapterNames(), ['cargo', 'swiftpm', 'node-pnpm', 'node-gyp', 'cmake', 'make', 'raw-compiler']));
test('node-gyp remains covered by the frozen frontend registry', () => assert.ok(adapterNames().includes('node-gyp')));
