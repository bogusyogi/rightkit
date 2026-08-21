import { randomUUID } from 'node:crypto';
import { mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { readProcessIdentity } from './process-tree.mjs';

export class ThinBrokerAlreadyRunningError extends Error {
  constructor(owner = null) {
    super(`RightKit thin broker is already running${owner?.pid ? ` (pid ${owner.pid})` : ''}`);
    this.name = 'ThinBrokerAlreadyRunningError'; this.code = 'RK_BROKER_ALREADY_RUNNING'; this.owner = owner;
  }
}

async function readOwner(path) {
  try { return JSON.parse(await readFile(path, 'utf8')); }
  catch (error) { if (error.code === 'ENOENT') return null; return { invalid: true }; }
}

function identityToken(value) { return value?.status === 'OK' ? value.identity : null; }

export async function acquireThinBrokerSingleton({ stateRoot, pid = process.pid, readIdentity = readProcessIdentity, kill = process.kill, maxAttempts = 4 } = {}) {
  if (typeof stateRoot !== 'string' || !stateRoot || !Number.isSafeInteger(pid) || pid < 1) throw new TypeError('stateRoot & positive pid are required');
  const lockPath = join(stateRoot, 'broker-lock'); const ownerPath = join(lockPath, 'owner.json');
  await mkdir(stateRoot, { recursive: true });
  const ownIdentity = await Promise.resolve().then(() => readIdentity(pid)).catch(() => null);
  const token = randomUUID();
  for (let attempt = 0; attempt < maxAttempts; attempt += 1) {
    try {
      await mkdir(lockPath);
      await writeFile(ownerPath, `${JSON.stringify({ pid, identity: identityToken(ownIdentity), token, acquiredAt: new Date().toISOString() })}\n`, { encoding: 'utf8', mode: 0o600, flag: 'wx' });
      let released = false;
      return Object.freeze({ path: lockPath, owner: Object.freeze({ pid, identity: identityToken(ownIdentity), token }), release: async () => {
        if (released) return false;
        const owner = await readOwner(ownerPath);
        if (owner?.token !== token) throw new Error('broker singleton ownership changed before release');
        const releasedPath = `${lockPath}.released.${token}`;
        await rename(lockPath, releasedPath); released = true; await rm(releasedPath, { recursive: true, force: true }); return true;
      } });
    } catch (error) {
      if (error.code !== 'EEXIST') {
        if (error.code === 'ENOENT') continue;
        throw error;
      }
    }
    const owner = await readOwner(ownerPath);
    if (await ownerIsLive(owner, { readIdentity, kill })) throw new ThinBrokerAlreadyRunningError(owner);
    const stalePath = `${lockPath}.stale.${randomUUID()}`;
    try { await rename(lockPath, stalePath); await rm(stalePath, { recursive: true, force: true }); }
    catch (error) { if (!['ENOENT', 'EEXIST'].includes(error.code)) throw error; }
  }
  throw Object.assign(new Error('broker singleton acquisition raced too many times'), { code: 'RK_BROKER_LOCK_RACE' });
}

async function ownerIsLive(owner, { readIdentity, kill }) {
  if (!owner || !Number.isSafeInteger(owner.pid) || owner.pid < 1) return false;
  try { kill(owner.pid, 0); } catch (error) { if (error.code === 'ESRCH') return false; return true; }
  if (!owner.identity) return true;
  const observed = await Promise.resolve().then(() => readIdentity(owner.pid)).catch(() => null);
  return observed?.status !== 'OK' || observed.identity === owner.identity;
}
