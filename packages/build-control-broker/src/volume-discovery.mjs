import { execFile as nodeExecFile } from 'node:child_process';
import { statfsSync, realpathSync } from 'node:fs';
import { promisify } from 'node:util';

const execFile = promisify(nodeExecFile);

function capacityFromStats(stats) {
  const blocks = Number(stats?.blocks);
  const blockSize = Number(stats?.bsize ?? stats?.frsize);
  if (!Number.isSafeInteger(blocks) || blocks < 1 || !Number.isSafeInteger(blockSize) || blockSize < 1) {
    throw new Error('managed volume capacity is unavailable');
  }
  const capacity = blocks * blockSize;
  if (!Number.isSafeInteger(capacity) || capacity < 1) throw new Error('managed volume capacity is out of range');
  return capacity;
}

function plistValue(xml, key) {
  return String(xml ?? '').match(new RegExp(`<key>${key}</key>\\s*<string>([^<]+)</string>`))?.[1] ?? null;
}

async function discoverVolumeId(path, platform, run, stats = null) {
  if (platform === 'darwin') {
    try {
      const result = await run('diskutil', ['info', '-plist', path]);
      return plistValue(result?.stdout, 'VolumeUUID') ?? plistValue(result?.stdout, 'APFSVolumeUUID');
    } catch { return null; }
  }
  if (platform === 'win32') {
    try {
      const escaped = String(path).replaceAll("'", "''");
      const result = await run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', `(Get-Volume -FilePath '${escaped}').UniqueId`], { windowsHide: true });
      return String(result?.stdout ?? '').trim().split(/\r?\n/).find(Boolean) ?? null;
    } catch { return null; }
  }
  const fallback = stats?.fsid ?? stats?.f_fsid;
  return fallback === undefined || fallback === null ? null : String(fallback);
}

/** Resolve currently mounted volume identity & capacity from a path.
 * Callers must compare the returned identity with config before mutating it. */
export async function discoverManagedVolume(path, { platform = process.platform, statfs = statfsSync, realpath = realpathSync, run = execFile, volumeId = undefined } = {}) {
  if (typeof path !== 'string' || !path) throw new TypeError('managed volume path is required');
  const canonicalPath = await Promise.resolve(realpath(path));
  const stats = await Promise.resolve(statfs(canonicalPath));
  const capacityBytes = capacityFromStats(stats);
  const id = volumeId === undefined ? await discoverVolumeId(canonicalPath, platform, run, stats) : volumeId;
  if (id !== null && (typeof id !== 'string' || !id.trim())) throw new Error('managed volume identity is invalid');
  return Object.freeze({ path: canonicalPath, volumeId: id ? String(id).trim() : null, capacityBytes, mounted: true, writable: true });
}

export { capacityFromStats };
