import { createHash } from 'node:crypto';
import { execFile as execFileCb } from 'node:child_process';
import { mkdtemp, mkdir, readdir, rm, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { promisify } from 'node:util';
import { assertReport, gate } from './schema.mjs';

const execFile = promisify(execFileCb);
const run = async (command, args = [], timeout = 15_000) => {
  try { const { stdout, stderr } = await execFile(command, args, { encoding: 'utf8', timeout }); return { ok: true, stdout: stdout.trim(), stderr: stderr.trim() }; }
  catch (error) { return { ok: false, code: error.code ?? null, stdout: String(error.stdout ?? '').trim(), stderr: String(error.stderr ?? error.message).trim() }; }
};
const hash = (value) => createHash('sha256').update(value).digest('hex');
const GiB = 1024 ** 3;

function classifyTopLevel(name) {
  if (/cache|\.pnpm-store|cargo-home|npm-cache|swift-module/i.test(name)) return 'shared-cache';
  if (/target|build|install-stage|native-probes/i.test(name)) return 'target-or-stage';
  if (/backup|archive|readback|extract/i.test(name)) return 'backup-or-archive';
  if (/\.dmg$|\.app$|installer/i.test(name)) return 'installer';
  if (/\.mp4$|\.mov$|\.png$|\.jpg$|\.jpeg$/i.test(name)) return 'media';
  if (/claude|dispatch|tools|screenright|download/i.test(name)) return 'repository-or-user-data';
  return 'unmanaged-user-data';
}

async function topLevelInventory(root) {
  const entries = await readdir(root, { withFileTypes: true }).catch(() => null);
  if (!entries) return { complete: false, entries: [], diagnostic: `cannot enumerate ${root}` };
  const measured = [];
  for (const entry of entries) {
    const item = path.join(root, entry.name);
    // One path at a time prevents a capacity probe from creating an I/O storm.
    const result = await run('du', ['-skx', item], 30_000);
    const kib = Number(result.stdout.split(/\s+/)[0]);
    measured.push({
      path: item, type: entry.isDirectory() ? 'directory' : entry.isFile() ? 'file' : 'other',
      class: classifyTopLevel(entry.name), bytes: result.ok && Number.isFinite(kib) ? kib * 1024 : null,
      measured: result.ok && Number.isFinite(kib), diagnostic: result.ok ? undefined : result.stderr
    });
  }
  return { complete: measured.every((item) => item.measured), entries: measured };
}

async function nativeModuleInventory(workspace) {
  const found = await run('rg', ['--files', workspace, '-g', '*.node', '-g', '*.dylib', '-g', '*.so', '-g', '*.bundle'], 120_000);
  const paths = found.ok && found.stdout ? found.stdout.split('\n').filter(Boolean) : [];
  return { command: `rg --files ${workspace} -g *.node -g *.dylib -g *.so -g *.bundle`, status: found.ok ? 'PASS' : 'BLOCKED_EXTERNAL', count: paths.length, representativePaths: paths.slice(0, 20), diagnostic: found.ok ? undefined : found.stderr };
}

export async function machineInventory() {
  const [sw, hw, volumes, identities] = await Promise.all([
    run('sw_vers'), run('sysctl', ['-n', 'hw.model']), run('diskutil', ['list', '-plist']), run('security', ['find-identity', '-v', '-p', 'codesigning'])
  ]);
  return {
    schema: 'rightkit.build-control-phase0.macos.inventory/v1', generatedAt: new Date().toISOString(),
    os: sw.stdout, hardwareModel: hw.stdout, diskutilPlist: volumes.stdout,
    signing: { developerIdIdentityPresent: /Developer ID Application:/.test(identities.stdout), commandOk: identities.ok, diagnostic: identities.stderr || undefined },
    entitlementRequest: {
      entitlement: 'com.apple.developer.endpoint-security.client',
      state: 'BLOCKED_EXTERNAL',
      browserAttempt: { surface: 'Apple Developer account', result: 'AUTHENTICATION_REQUIRED', submitted: false, dispositionRead: false },
      requestPath: 'Apple Developer account holder submits Endpoint Security entitlement request in Certificates, Identifiers & Profiles. Current in-app browser reached Apple sign-in, but no authenticated account session was available; existing request state was not read & no request was submitted.',
      dependency: 'Authenticated Apple Developer account access plus Apple Developer Program entitlement approval for team 6KLGD3LLKF'
    }
  };
}

export async function capacityModel(workspace = process.cwd(), externalRoot = '/Volumes/D') {
  const df = await run('df', ['-kP']);
  const rows = df.stdout.split('\n').slice(1).map((line) => line.trim().split(/\s+/)).filter((r) => r.length >= 6).map((r) => ({ filesystem: r[0], totalBytes: Number(r[1]) * 1024, usedBytes: Number(r[2]) * 1024, freeBytes: Number(r[3]) * 1024, mount: r.slice(5).join(' ') }));
  const d = rows.find((row) => row.mount === externalRoot);
  const protectedClasses = ['toolchains', 'package-inputs', 'registry', 'receipts', 'policy-state', 'stage', 'target', 'temp', 'shared-cache'];
  const inventory = d ? await topLevelInventory(d.mount) : { complete: false, entries: [], diagnostic: 'external D volume is not mounted' };
  const classifiedBytes = inventory.complete ? inventory.entries.reduce((sum, item) => sum + item.bytes, 0) : null;
  const nativeModules = await nativeModuleInventory(workspace);
  const observedWorkingSets = inventory.entries.filter((item) => ['shared-cache', 'target-or-stage'].includes(item.class) && item.measured).map((item) => ({ class: item.class, path: item.path, bytes: item.bytes, source: 'current top-level occupancy; not a clean/warm build measurement' }));
  const canSolve = Boolean(d && inventory.complete && classifiedBytes !== null);
  return {
    schema: 'rightkit.build-control-phase0.macos.capacity/v1', generatedAt: new Date().toISOString(),
    status: 'BLOCKED_EXTERNAL', protectedClasses, volumes: rows, externalD: d ?? null,
    classifiedInventory: inventory,
    workingSets: { observed: observedWorkingSets, complete: false, missing: ['clean build peak per workspace/class/toolchain', 'warm build working set per workspace/class/toolchain', 'P50/P95/P99 growth', 'stage size'] },
    reservationPeaks: { observedRequests: 0, maximumActiveReservationsBytes: 0, complete: false, detail: 'No broker request or reservation trace exists on this machine; zero is an observed count, not a capacity allowance.' },
    nativeModules,
    capacityEquation: {
      formula: 'Cmax = T - P - A - R - F - X', totalBytes: d?.totalBytes ?? null,
      protectedNonCacheBytes: classifiedBytes, artifactBudgetBytes: null, recoveryBudgetBytes: null, reserveFloorBytes: null, transitionHeadroomBytes: null,
      cacheMaximumBytes: null, measured: false,
      unsolvedTerms: ['A', 'R', 'F', 'X'],
      note: canSolve ? 'P is a complete top-level inventory; Cmax remains unsolved because no measured artifact, recovery, free-floor, or transition reservation exists.' : 'Inventory is incomplete; Cmax is intentionally unsolved.'
    },
    detail: d ? `D capacity ${d.totalBytes} bytes, free ${d.freeBytes} bytes. Inventory is ${inventory.complete ? 'complete' : 'incomplete'}; no aspirational cache maximum is emitted.` : 'external D volume is not mounted'
  };
}

export async function cloneProbe(root = '/Volumes/D') {
  const parent = await stat(root).catch(() => null);
  if (!parent?.isDirectory()) return { schema: 'rightkit.build-control-phase0.macos.clone/v1', status: 'BLOCKED_EXTERNAL', root, detail: 'external D volume is not mounted' };
  const temp = await mkdtemp(path.join(root, '.rightkit-phase0-clone-'));
  try {
    const source = path.join(temp, 'source.bin'); const clone = path.join(temp, 'clone.bin');
    await writeFile(source, Buffer.alloc(1024 * 1024, 0x5a));
    const copy = await run('cp', ['-c', source, clone]);
    const [a, b] = await Promise.all([stat(source), stat(clone).catch(() => null)]);
    return { schema: 'rightkit.build-control-phase0.macos.clone/v1', status: copy.ok && b ? 'PASS' : 'FAIL', root, command: { ok: copy.ok, stderr: copy.stderr }, source: { size: a.size, blocks: a.blocks }, destination: b && { size: b.size, blocks: b.blocks }, note: 'cp -c success is clonefile capability evidence; block counts alone do not prove shared extents.' };
  } finally { await rm(temp, { recursive: true, force: true }); }
}

export async function internalWriteProbe() {
  const before = await run('fs_usage', ['-w', '-f', 'filesystem', '-t', '1']);
  // fs_usage needs privilege on most hosts. This is intentionally a capability probe, never a policy bypass.
  return { schema: 'rightkit.build-control-phase0.macos.internal-write/v1', status: before.ok ? 'PENDING_USER_APPROVAL' : 'BLOCKED_EXTERNAL', command: 'fs_usage -w -f filesystem -t 1', detail: before.ok ? 'trace facility available; workload correlation must run under a future isolated runner UID' : 'filesystem tracing requires elevated host authorization', diagnostic: before.stderr };
}

export async function runnerUidIsolationProbe() {
  const [id, dscl, sudo] = await Promise.all([run('id', ['-u']), run('dscl', ['.', '-list', '/Users', 'UniqueID']), run('sudo', ['-n', 'true'])]);
  const accounts = dscl.stdout.split('\n').map((line) => line.trim().split(/\s+/)).filter((row) => row.length === 2).map(([name, uid]) => ({ name, uid: Number(uid) }));
  const runnerCandidates = accounts.filter(({ name }) => /^_rightkit_runner|^rightkit-runner/i.test(name));
  return { schema: 'rightkit.build-control-phase0.macos.runner-uid/v1', status: 'BLOCKED_EXTERNAL', currentUid: id.stdout, accountDirectoryReadable: dscl.ok, runnerCandidates, adminProbe: { command: 'sudo -n true', ok: sudo.ok, exitCode: sudo.code, diagnostic: sudo.stderr || undefined }, attemptedCommands: ['id -u', 'dscl . -list /Users UniqueID', 'sudo -n true'], detail: runnerCandidates.length ? 'Runner-like accounts found, but non-login, ACL, privilege-drop, teardown, process-survival, wipe & generation-reuse traces are not yet available.' : 'No pre-provisioned locked non-login runner UID pool exists. sudo -n cannot create isolated accounts; no account or session mutation was attempted.' };
}

export async function endpointSecuritySpike() {
  return {
    schema: 'rightkit.build-control-phase0.macos.endpoint-security/v1',
    status: 'BLOCKED_EXTERNAL',
    mode: 'NOT_ACTIVATED',
    authExecDefault: 'DENY',
    authorizationPath: 'bounded in-memory lookup only',
    installation: { signedUniversalHost: 'NOT_TESTED', applicationsActivation: 'NOT_TESTED', interactiveApproval: 'NOT_TESTED', fullDiskAccess: 'NOT_TESTED', headlessProvisioningHandoff: 'NOT_TESTED' },
    stress: { deadlineMisses: 'NOT_MEASURED', implicitAllows: 'NOT_MEASURED', sequenceGaps: 'NOT_MEASURED' },
    entitlement: { state: 'UNKNOWN_NOT_AUTHENTICATED', browserAttempt: 'AUTHENTICATION_REQUIRED', submitted: false, dispositionRead: false },
    dependency: 'Verified Apple-granted com.apple.developer.endpoint-security.client entitlement plus isolated interactive system-extension test context',
    detail: 'No extension has been built, signed, installed, activated, denied, or allowed by this Phase-0 harness.'
  };
}

export async function baseline() {
  const [cargo, rustc, node, git] = await Promise.all([run('cargo', ['--version']), run('rustc', ['--version']), run('node', ['--version']), run('git', ['--version'])]);
  return { schema: 'rightkit.build-control-phase0.macos.baseline/v1', generatedAt: new Date().toISOString(), routes: { cargo, rustc, node, git }, status: [cargo, rustc, node, git].every((x) => x.ok) ? 'PASS' : 'FAIL' };
}

export async function provenanceProbe(workspace = process.cwd()) {
  const [head, branch, dirty] = await Promise.all([run('git', ['-C', workspace, 'rev-parse', 'HEAD']), run('git', ['-C', workspace, 'branch', '--show-current']), run('git', ['-C', workspace, 'status', '--porcelain'])]);
  const payload = { workspace: path.resolve(workspace), head: head.stdout || null, branch: branch.stdout || null, dirty: dirty.stdout.split('\n').filter(Boolean).length };
  return { schema: 'rightkit.build-control-phase0.macos.provenance/v1', ...payload, digest: hash(JSON.stringify(payload)), status: head.ok ? 'PASS' : 'FAIL' };
}

export async function report(workspace = process.cwd(), externalRoot = '/Volumes/D') {
  // Clone probe creates a temporary root under external storage, so it must finish
  // before inventory enumerates that storage.
  const clone = await cloneProbe(externalRoot);
  const [inventory, capacity, baselineResult, provenance, writes, runner, endpointSecurity] = await Promise.all([machineInventory(), capacityModel(workspace, externalRoot), baseline(), provenanceProbe(workspace), internalWriteProbe(), runnerUidIsolationProbe(), endpointSecuritySpike()]);
  const gates = [
    gate('apple-entitlement', 'BLOCKED_EXTERNAL', inventory.entitlementRequest.dependency, ['machine-inventory-macos.json']),
    gate('system-extension-activation', endpointSecurity.status, endpointSecurity.dependency, ['endpoint-security-spike.json']),
    gate('auth-exec', endpointSecurity.status, endpointSecurity.detail, ['endpoint-security-spike.json']),
    gate('capacity-floor', capacity.status, capacity.detail, ['capacity-model-macos.json']),
    gate('apfs-clone', clone.status, clone.detail ?? clone.note, ['apfs-clone-reflink.json']),
    gate('baseline', baselineResult.status, 'current execution-route baseline', ['baseline.json']),
    gate('runner-uid-isolation', runner.status, runner.detail, ['runner-uid-isolation.json']),
    gate('internal-write', writes.status, writes.detail, ['internal-write.json'])
  ];
  return assertReport({ schema: 'rightkit.build-control-phase0.macos.report/v1', version: 1, generatedAt: new Date().toISOString(), status: gates.some((x) => x.status === 'FAIL') ? 'FAIL' : gates.some((x) => x.status !== 'PASS') ? 'BLOCKED_EXTERNAL' : 'PASS', machine: { hardwareModel: inventory.hardwareModel, os: inventory.os }, gates, evidence: [inventory, capacity, clone, baselineResult, provenance, writes, runner, endpointSecurity] });
}

export async function writeEvidence(directory, workspace, externalRoot) {
  const value = await report(workspace, externalRoot); await mkdir(directory, { recursive: true });
  const names = ['machine-inventory-macos.json', 'capacity-model-macos.json', 'apfs-clone-reflink.json', 'baseline.json', 'provenance.json', 'internal-write.json', 'runner-uid-isolation.json', 'endpoint-security-spike.json'];
  await Promise.all(value.evidence.map((item, index) => writeFile(path.join(directory, names[index]), `${JSON.stringify(item, null, 2)}\n`)));
  await writeFile(path.join(directory, 'report.json'), `${JSON.stringify(value, null, 2)}\n`);
  return value;
}
