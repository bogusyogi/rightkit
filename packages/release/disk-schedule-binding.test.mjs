// #776: the schedule binding is read with plutil (formatting-independent) and a failed
// disk run self-check stops before any scratch creation, scan, sweep or purge.
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { readScheduleBinding } from './disk-stable-root.mjs';
import { runDiskCommand } from './disk-command.mjs';
const scratch = os.tmpdir();

test('native plist parser reads normalized whitespace & decoded XML paths', { skip: process.platform !== 'darwin' }, () => {
  const base = mkdtempSync(path.join(scratch, 'parser-oracle-'));
  const file = path.join(base, 'fixture.plist');
  writeFileSync(file, `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>ProgramArguments</key>
  <array>
    <string>/fixture/A&amp;B/node</string>
    <string>/fixture/A&amp;B/current/cli/right-release.mjs</string>
    <string>disk</string><string>run</string>
  </array>
</dict></plist>`);
  assert.deepEqual(readScheduleBinding({ platform: 'darwin', plistFile: file }), {
    node: '/fixture/A&B/node', cli: '/fixture/A&B/current/cli/right-release.mjs', source: file,
  });
  writeFileSync(file, '<plist><dict><key>ProgramArguments</key><string>wrong type</string></dict></plist>');
  assert.deepEqual(readScheduleBinding({ platform: 'darwin', plistFile: file }), { node: null, cli: null, source: file });
});

test('failed selfCheck logs & returns before scratch creation, scans, sweep or purge', async () => {
  const base = mkdtempSync(path.join(scratch, 'failclosed-oracle-'));
  const heavy = path.join(base, 'heavy');
  const shouldNotExist = path.join(base, 'scratch-must-stay-absent');
  const sentinel = path.join(base, 'sentinel');
  writeFileSync(sentinel, 'preserve');
  const policy = { schema: 1, staleDays: 7, floors: { mac: { path: base, label: 'fixture', warnFreeBytes: 1000 } }, heavyVolume: { mac: heavy }, holding: { mac: [{ prefix: base, path: path.join(base, 'hold') }] }, scratchRoots: { mac: [shouldNotExist] }, workspaceRoots: { mac: [] }, writers: [] };
  const policyFile = path.join(base, 'policy.json');
  writeFileSync(policyFile, JSON.stringify(policy));
  const unexpected = () => { throw new Error('selfCheck failure reached disk scan'); };
  const selfCheck = { ok: false, registered: true, problems: ['fixture broken binding'], version: 'fixture' };
  const result = await runDiskCommand(['run', '--policy', policyFile], { platform: 'darwin', stdout: () => {}, deps: { selfCheck: () => selfCheck, openPaths: [], check: { size: unexpected, space: unexpected }, plan: { size: unexpected } } });
  assert.deepEqual(result, { exitCode: 2, selfCheck });
  assert.equal(existsSync(shouldNotExist), false);
  assert.equal(readFileSync(sentinel, 'utf8'), 'preserve');
  const events = readFileSync(path.join(heavy, 'disk-hygiene.jsonl'), 'utf8').trim().split('\n').map(JSON.parse);
  assert.equal(events.length, 1);
  assert.equal(events[0].event, 'disk.run.selfcheck.fail');
});
