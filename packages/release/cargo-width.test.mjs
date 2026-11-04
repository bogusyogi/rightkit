import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import { cargoWidthArgs, parseCargoWidth, validateCargoWidthPool } from './cargo-width.mjs';

test('Cargo width is strict & appears before Cargo separator', () => {
  assert.deepEqual(cargoWidthArgs(['test', '-p', 'unit', '--', '--test-threads=1'], parseCargoWidth('3')), ['test', '--jobs', '3', '-p', 'unit', '--', '--test-threads=1']);
  for (const value of ['', '0', '-1', '1.0', '01', '4294967296']) assert.throws(() => parseCargoWidth(value));
  for (const args of [['check', '-j4'], ['check', '--jobs=2'], ['check', '--jobs', '2'], ['metadata']]) assert.throws(() => cargoWidthArgs(args, 1));
  assert.deepEqual(cargoWidthArgs(['test', '--', '-j4'], 1), ['test', '--jobs', '1', '--', '-j4']);
});

test('Cargo width needs an available broker pool & cannot exceed it', () => {
  const status = { type: 'LIST', broker: { phase: 'READY', tokens: { size: 8 } } };
  assert.equal(validateCargoWidthPool(8, status), 8);
  assert.throws(() => validateCargoWidthPool(9, status));
  assert.throws(() => validateCargoWidthPool(1, { type: 'LIST', broker: { phase: 'READY' } }));
});

test('width route accepts normal flag ordering & rejects command and app routes', async () => {
  const { cargoWidthRouteProblem } = await import('./cargo-width.mjs');
  assert.equal(cargoWidthRouteProblem(['--units-file', 'units.json', '--to', 'compile', '--cargo-jobs', '2']), null);
  assert.equal(cargoWidthRouteProblem(['--cargo-jobs', '2', '--to', 'compile', '--changed']), null);
  for (const args of [['clippy', '--all', '--cargo-jobs', '2'], ['--all', '--cargo-jobs', '2'], ['--to', 'compile', '--cargo-jobs', '2'], ['--to', 'compile', '--changed', '--route'], ['--to', 'compile', '--changed', '--unit', 'cargo:rightkit-x']]) assert.ok(cargoWidthRouteProblem(args));
});

test('width rejects any selected non-Cargo unit', async () => {
  const { cargoWidthUnitsProblem } = await import('./cargo-width.mjs');
  assert.equal(cargoWidthUnitsProblem([{ selector: 'cargo:rightkit-example' }]), null);
  for (const selector of ['npm:@rightkit/example', 'native:tool', 'custom:adapter']) assert.ok(cargoWidthUnitsProblem([{ selector }]));
});

test('scans must carry the requested width on a known Cargo command', async () => {
  const { cargoWidthScanProblem } = await import('./cargo-width.mjs');
  const cargo = ['rustright', 'cargo'];
  assert.equal(cargoWidthScanProblem({ tool: 'rustright', args: ['cargo', 'check', '--profile', 'release', '--jobs', '2'] }, cargo, 2), null);
  for (const scan of [{ tool: 'pnpm', args: ['test'] }, { tool: 'rustright', args: ['run', 'adapter'] }, { tool: 'rustright', args: ['cargo', 'check', '--jobs', '3'] }, { tool: 'rustright', args: ['cargo', 'check', '--jobs', '2', '-j4'] }]) assert.ok(cargoWidthScanProblem(scan, cargo, 2));
});
