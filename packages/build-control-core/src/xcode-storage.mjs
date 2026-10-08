import { lstat, readdir, readFile, writeFile, mkdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { STORAGE_DEFAULTS, inside, noSymlinkPath, moveToVolumeTrash, ownerAlive, measureTree, withStorageLock } from './storage.mjs';

const exec = promisify(execFile);
const versionOf = value => typeof value === 'string' ? value.match(/^(\d+\.\d+(?:\.\d+)?)(?:\s|$)/)?.[1] : undefined;
const validHistory = value => value && typeof value === 'object' && !Array.isArray(value) && Object.entries(value).every(([key, time]) => versionOf(key) === key && Number.isFinite(time) && time >= 0);
const versionCompare = (a, b) => { const aa = a.split('.').map(Number), bb = b.split('.').map(Number); for (let i = 0; i < Math.max(aa.length, bb.length); i++) if ((aa[i] ?? 0) !== (bb[i] ?? 0)) return (bb[i] ?? 0) - (aa[i] ?? 0); return 0; };
export function connectedDeviceVersions(snapshot) {
  if (!snapshot || !Array.isArray(snapshot.result?.devices)) throw new Error('device discovery incomplete; pruning refused');
  const versions = new Set();
  for (const device of snapshot.result.devices) {
    const props = device.properties ?? {}; const connection = props.connection ?? props.connectionProperties ?? device.connectionProperties ?? {};
    const state = props.state ?? {}; const details = props.device ?? props.deviceProperties ?? device.deviceProperties ?? props;
    const connected = connection.tunnelState === 'connected' || connection.transportType === 'wired' || state.state === 'connected' || state === 'connected';
    if (connected) { const version = versionOf(details.osVersionNumber ?? details.osVersion ?? props.osVersionNumber ?? props.osVersion); if (!version) throw new Error('connected device OS version unavailable; pruning refused'); versions.add(version); }
  }
  return [...versions];
}
export async function discoverDeviceVersions({ run = exec } = {}) {
  const { stdout } = await run('xcrun', ['devicectl', 'list', 'devices', '--json-output', '-'], { timeout: 30_000, maxBuffer: 4 * 1024 ** 2 });
  return connectedDeviceVersions(JSON.parse(stdout));
}
export async function xcodeManagedArgs({ managedRoot, runRoot }) {
  const root = await noSymlinkPath(resolve(runRoot));
  if (!inside(root, join(managedRoot, 'runs'))) throw new Error('DerivedData requires a managed run root');
  const meta = JSON.parse(await readFile(join(root, '.rightkit-run.json'), 'utf8'));
  if (meta.schemaVersion !== 1 || !ownerAlive(meta)) throw new Error('DerivedData requires a live managed run');
  const derivedData = join(root, 'DerivedData'); await mkdir(derivedData, { recursive: true });
  return ['-derivedDataPath', derivedData, '-resultBundlePath', join(root, 'evidence', `xcode-${Date.now()}.xcresult`)];
}
export async function deviceSupportPlan({ directory, connectedVersions, recent = {}, now = Date.now(), keep = STORAGE_DEFAULTS.deviceKeep, recentMs = STORAGE_DEFAULTS.deviceRecentMs }) {
  if (!validHistory(recent)) throw new Error('invalid device use history');
  await noSymlinkPath(directory);
  if (!Array.isArray(connectedVersions) || connectedVersions.some(v => !versionOf(v)) || !Number.isSafeInteger(keep) || keep < 0) throw new Error('valid connected device discovery required');
  const folders = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) if (entry.isDirectory()) { const version = versionOf(entry.name); if (version) folders.push({ path: join(directory, entry.name), version, bytes: await measureTree(join(directory, entry.name)) }); }
  const newest = [...new Set(folders.map(row => row.version))].sort(versionCompare).slice(0, keep);
  const remove = [], retain = [];
  for (const row of folders) {
    let reason;
    if (connectedVersions.includes(row.version)) reason = 'connected-device';
    else if (Number.isFinite(recent[row.version]) && now - recent[row.version] < recentMs) reason = 'recent-device';
    else if (newest.includes(row.version)) reason = 'newest-two';
    if (reason) retain.push({ ...row, reason }); else remove.push({ ...row, reason: 'unused-device-version' });
  }
  return { remove, retain };
}
export async function pruneDeviceSupport({ managedRoot, directory, connectedVersions, apply = false, now = Date.now(), move = moveToVolumeTrash, ...options }) {
  const stateRoot = join(managedRoot, 'xcode');
  const execute = async () => {
    let recent = {}; try { recent = JSON.parse(await readFile(join(stateRoot, 'devices.json'), 'utf8')); } catch (e) { if (e.code !== 'ENOENT') throw new Error('device history unreadable; pruning refused', { cause: e }); }
    if (!validHistory(recent)) throw new Error('invalid device use history; pruning refused');
    for (const version of connectedVersions) recent[version] = now;
    const plan = await deviceSupportPlan({ directory, connectedVersions, recent, now, ...options });
    const moves = [];
    for (const row of plan.remove) moves.push(await move(row.path, { apply }));
    if (apply) await writeFile(join(stateRoot, 'devices.json'), JSON.stringify(recent));
    return { ...plan, moves };
  };
  return apply ? withStorageLock(stateRoot, execute) : execute();
}
/** Record connected device versions when preparing a managed Xcode invocation. */
export async function recordDeviceUse({ managedRoot, versions, now = Date.now() }) {
  if (!Array.isArray(versions) || versions.some(v => !versionOf(v))) throw new Error('valid device versions required');
  return withStorageLock(join(managedRoot, 'xcode'), async () => {
    const file = join(managedRoot, 'xcode', 'devices.json'); let recent = {};
    try { recent = JSON.parse(await readFile(file, 'utf8')); } catch (error) { if (error.code !== 'ENOENT') throw error; }
    if (!validHistory(recent)) throw new Error('invalid device use history');
    for (const version of versions) recent[version] = now;
    await writeFile(file, JSON.stringify(recent)); return recent;
  });
}
