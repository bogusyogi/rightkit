import test from 'node:test';
import assert from 'node:assert/strict';
import { EMPTY_IMPORT_FORM, isAbsolutePath, licenceEntries, validateImportForm } from '../lib/asset-import.ts';

const form = (patch = {}) => ({ ...EMPTY_IMPORT_FORM, path: '/Users/me/Pictures/photo.png', origin: 'supplied', ...patch });

test('a complete supplied form builds the engine request with unrecorded facts left unknown', () => {
  const { errors, request } = validateImportForm(form());
  assert.deepEqual(errors, {});
  assert.deepEqual(request, { path: '/Users/me/Pictures/photo.png', origin: 'supplied', evidence: 'unknown', rights: { status: 'unknown' } });
});

test('cleared rights without licence evidence are refused in the form, as the engine refuses them', () => {
  const { errors, request } = validateImportForm(form({ rightsStatus: 'cleared', licenceEvidence: '   \n\n' }));
  assert.ok(errors.licence, 'licence error expected');
  assert.equal(request, null);
});

test('cleared rights take licence evidence one per non-empty line, trimmed', () => {
  const { errors, request } = validateImportForm(form({ rightsStatus: 'cleared', licenceEvidence: ' receipt 4417 \n\nhttps://example.com/licence\r\n' }));
  assert.deepEqual(errors, {});
  assert.deepEqual(request.rights, { status: 'cleared', licence_evidence: ['receipt 4417', 'https://example.com/licence'] });
});

test('attribution is sent only when it has text', () => {
  assert.equal(validateImportForm(form({ attribution: '   ' })).request.rights.attribution, undefined);
  assert.equal(validateImportForm(form({ attribution: ' Photo by A. Example ' })).request.rights.attribution, 'Photo by A. Example');
});

test('origin is required, and a missing path is named', () => {
  const noOrigin = validateImportForm(form({ origin: '' }));
  assert.ok(noOrigin.errors.origin);
  assert.equal(noOrigin.request, null);
  const noPath = validateImportForm(form({ path: '  ' }));
  assert.ok(noPath.errors.path);
  assert.equal(noPath.request, null);
});

test('a relative path is refused, while POSIX, drive and UNC paths are accepted', () => {
  assert.ok(validateImportForm(form({ path: 'photo.png' })).errors.path);
  assert.equal(isAbsolutePath('/Users/me/a.png'), true);
  assert.equal(isAbsolutePath('C:\\Pictures\\a.png'), true);
  assert.equal(isAbsolutePath('C:/Pictures/a.png'), true);
  assert.equal(isAbsolutePath('\\\\server\\share\\a.png'), true);
  assert.equal(isAbsolutePath('photo.png'), false);
  assert.deepEqual(validateImportForm(form({ path: 'C:\\Pictures\\a.png' })).errors, {});
});

test('scope is all three ids or none, and each id must match the engine pattern', () => {
  const partial = validateImportForm(form({ venture: 'orthic', channel: 'cutright-how-to' }));
  assert.ok(partial.errors.scope);
  assert.equal(partial.request, null);
  const badId = validateImportForm(form({ venture: 'orthic', channel: 'bad id', account: 'main' }));
  assert.ok(badId.errors.scope);
  const leadingDash = validateImportForm(form({ venture: '-orthic', channel: 'c', account: 'main' }));
  assert.ok(leadingDash.errors.scope);
  const full = validateImportForm(form({ venture: ' orthic ', channel: 'cutright-how-to', account: 'main' }));
  assert.deepEqual(full.errors, {});
  assert.deepEqual(full.request.scope, { venture: 'orthic', channel: 'cutright-how-to', account: 'main' });
});

test('no scope fields means no scope in the request', () => {
  assert.equal('scope' in validateImportForm(form()).request, false);
});

test('licence entries drop blank lines and trim each line', () => {
  assert.deepEqual(licenceEntries('\n  a \n\n b\r\n'), ['a', 'b']);
});
