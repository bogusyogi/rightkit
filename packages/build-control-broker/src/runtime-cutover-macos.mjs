import { promisify } from 'node:util';
import { execFile as childExecFile } from 'node:child_process';
import net from 'node:net';
import { readFile, realpath } from 'node:fs/promises';
import { isAbsolute, join, resolve } from 'node:path';
import { BUILD_CONTROL_PROTOCOL } from '@rightkit/build-control-core';
import { installManagedRuntime } from './runtime.mjs';

const execDefault = promisify(childExecFile);
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

/**
 * Install a staged runtime behind a macOS launchd service.  All process and
 * socket seams are injectable so this module can be tested without launchctl.
 */
export async function installManagedRuntimeMacOS(options = {}) {
  const callbacks = createMacOSRuntimeCallbacks(options);
  const installer = options.installManagedRuntime ?? installManagedRuntime;
  return installer({ ...options, ...callbacks });
}

export const installMacOSManagedRuntime = installManagedRuntimeMacOS;

export function createMacOSRuntimeCallbacks(options = {}) {
  const {
    launchdLabel,
    endpoint,
    config,
    execFile = execDefault,
    netModule = net,
    drainProbe,
    processProbe: suppliedProcessProbe,
    timeoutMs = options.healthTimeoutMs ?? 8_000,
    pollMs = 25,
    uid,
    externalRoot,
    plistPath,
    processKill = process.kill,
    nodePath = process.execPath,
    configPath,
  } = options;
  if (typeof launchdLabel !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(launchdLabel)) throw new TypeError('launchdLabel must be an exact launchd label');
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 100) throw new TypeError('timeoutMs must be at least 100');
  if (typeof drainProbe !== 'function') throw new TypeError('drainProbe is required to refuse active thin requests');
  const target = uid == null ? `system/${launchdLabel}` : `gui/${uid}/${launchdLabel}`;
  const domain = uid == null ? 'system' : `gui/${uid}`;
  const socket = endpoint ?? config?.unixSocket;
  if (typeof socket !== 'string' || socket.length === 0) throw new TypeError('endpoint or config.unixSocket is required');

  const serviceStatus = async () => {
    const result = await execFile('launchctl', ['print', target]);
    const text = String(result?.stdout ?? result ?? '');
    const match = text.match(/(?:^|\n)\s*pid\s*=\s*(\d+)/i);
    return match ? { pid: Number(match[1]), raw: text } : { pid: null, raw: text };
  };
  const bootstrap = async () => {
    if (typeof plistPath !== 'string' || !isAbsolute(plistPath)) throw new TypeError('absolute plistPath is required when launchd service is unloaded');
    await execFile('launchctl', ['bootstrap', domain, resolve(plistPath)]);
  };
  const enable = () => execFile('launchctl', ['enable', target]);
  const ensureLoaded = async () => {
    try { return { ...(await serviceStatus()), bootstrapped: false }; }
    catch (initialError) {
      await bootstrap();
      try { return { ...(await serviceStatus()), bootstrapped: true }; }
      catch (bootstrapError) { throw new Error(`launchd service is unavailable after bootstrap: ${bootstrapError.message}`, { cause: initialError }); }
    }
  };
  const kickstart = () => execFile('launchctl', ['kickstart', '-k', target]);
  const ensureDrained = async () => {
    const state = await drainProbe();
    const open = typeof state === 'number' ? state : state?.open ?? state?.count ?? (state === true ? 1 : 0);
    if (open > 0) throw new Error(`legacy broker has ${open} open v1 request(s); cutover refused`);
  };
  const waitForPid = async (previousPid, deadline) => {
    let last;
    while (Date.now() <= deadline) {
      try {
        last = await serviceStatus();
        if (Number.isInteger(last.pid) && last.pid > 0 && last.pid !== previousPid) return last.pid;
      } catch { /* launchd may briefly have no print record during restart */ }
      await sleep(Math.min(pollMs, Math.max(1, deadline - Date.now())));
    }
    throw new Error(`launchd service did not produce a new live PID within ${timeoutMs}ms${last?.pid ? ` (PID ${last.pid})` : ''}`);
  };
  const handshake = async (manifest, deadline, { requirePending = true, endpoint = socket } = {}) => {
    let lastError;
    while (Date.now() <= deadline) try {
      return await new Promise((resolveHandshake, reject) => {
      let buffer = ''; let identity = null; let pending = false;
      const socketHandle = netModule.createConnection(endpoint);
      const timer = setTimeout(() => { socketHandle.destroy(); reject(new Error('launchd runtime HANDSHAKE timed out')); }, Math.max(1, deadline - Date.now()));
          const finish = (error, value) => { clearTimeout(timer); socketHandle.destroy(); error ? reject(error) : resolveHandshake(value); };
          const complete = () => { if (identity && (!requirePending || pending)) finish(null, identity); };
          socketHandle.once('error', (error) => finish(error));
          socketHandle.once('end', () => finish(new Error('launchd runtime is initializing')));
          socketHandle.once('connect', () => socketHandle.write(`${JSON.stringify({ type: 'STATUS', protocol: BUILD_CONTROL_PROTOCOL, runtime: manifest, id: 'runtime-health-probe' })}\n`));
      socketHandle.on('data', (chunk) => {
        buffer += String(chunk);
        for (;;) {
          const line = buffer.indexOf('\n'); if (line < 0) return;
          let event; try { event = JSON.parse(buffer.slice(0, line)); } catch (error) { return finish(error); }
          buffer = buffer.slice(line + 1);
          if (event.type === 'HANDSHAKE') {
            identity = event.runtime;
            if (!identity || identity.schema !== manifest.schema || identity.version !== manifest.version || identity.clientDigest !== manifest.clientDigest || identity.brokerDigest !== manifest.brokerDigest) return finish(new Error('launchd runtime HANDSHAKE identity mismatch'));
            complete(); continue;
          }
          if (event.type === 'STATUS' && event.code === 'EXTERNAL_ROOT_UNREADABLE') return finish(new Error(`EXTERNAL_ROOT_UNREADABLE: ${event.path} (${event.errno})`));
          if (event.type === 'STATUS' && event.id === 'runtime-health-probe' && event.state === 'ACTIVATION_PENDING') { pending = true; complete(); continue; }
          if (event.type === 'RESULT' && requirePending) return finish(new Error(`launchd runtime did not prove activation fence: ${event.code ?? event.status ?? 'rejected'}`));
        }
      });
      });
    } catch (error) {
      lastError = error;
      if (/digest mismatch|timed out|EXTERNAL_ROOT_UNREADABLE/.test(error.message)) throw error;
      await sleep(Math.min(pollMs, Math.max(1, deadline - Date.now())));
    }
    throw new Error(`launchd runtime HANDSHAKE unavailable within ${timeoutMs}ms: ${lastError?.message ?? 'unknown error'}`);
  };
  const probe = suppliedProcessProbe ?? (async ({ pid, runtime }) => {
    const status = await serviceStatus();
    if (status.pid !== Number(pid)) return { alive: false };
    if (typeof runtime !== 'string' || !isAbsolute(runtime)) return { alive: false };
    try {
      processKill(Number(pid), 0);
      const expectedRuntime = await realpath(runtime);
      const expectedScript = join(expectedRuntime, 'build-control-broker', 'src', 'broker.mjs');
      const runtimeCurrentScript = externalRoot ? join(resolve(externalRoot), 'runtime-current', 'build-control-broker', 'src', 'broker.mjs') : null;
      if (typeof configPath !== 'string' || !isAbsolute(configPath) || typeof nodePath !== 'string' || !isAbsolute(nodePath)) return { alive: false };
      const command = String((await execFile('ps', ['-ww', '-p', String(pid), '-o', 'command=']))?.stdout ?? '').trim();
      // ps flattens argv without shell quoting. Splitting on whitespace rejects
      // the internal control root's "Application Support" component. Match the
      // complete, known three-argument command instead of guessing boundaries.
      const script = [expectedScript, runtimeCurrentScript].filter(Boolean).find((candidate) =>
        command === `${nodePath} ${candidate} ${configPath}`);
      if (!script) return { alive: false };
      const resolvedScript = await realpath(script).catch(() => null);
      if (resolvedScript !== await realpath(expectedScript)) return { alive: false };
      return { alive: true, runtimePath: expectedRuntime, scriptPath: expectedScript };
    } catch { return { alive: false }; }
  });
  const legacyProbe = async ({ pid, runtime }) => {
    const state = await probe({ pid, runtime, legacy: true });
    if (!state?.alive) throw new Error('legacy rollback launchd process is not live');
    // v1 brokers do not implement STATUS/HANDSHAKE. A connection proves their
    // launchd-owned listener came back without sending a v2 protocol frame.
    await new Promise((resolveLegacy, reject) => {
      const handle = netModule.createConnection(socket);
      const timer = setTimeout(() => { handle.destroy(); reject(new Error('legacy rollback listener timed out')); }, timeoutMs);
      const finish = (error) => { clearTimeout(timer); handle.destroy(); error ? reject(error) : resolveLegacy(); };
      handle.once('error', finish);
      handle.once('connect', () => finish());
    });
  };

  return {
    stopService: async () => {
      await execFile('launchctl', ['disable', target]);
      await execFile('launchctl', ['bootout', target]).catch(() => {});
      if (await serviceStatus().then(() => true).catch(() => false)) throw new Error('candidate launchd service did not stop before rollback');
    },
    restartService: async ({ manifest, timeoutMs: callTimeoutMs = timeoutMs }) => {
      await ensureDrained();
      await enable();
      const old = await ensureLoaded().catch(() => ({ pid: null }));
      const deadline = Date.now() + callTimeoutMs;
      // Bootstrap already starts a RunAtLoad service. Sending kickstart -k here
      // races activation against termination of that fresh PID, so only kick an
      // already-loaded service. For a new bootstrap, wait for its first PID.
      if (!old.bootstrapped) await kickstart();
      const pid = await waitForPid(old.bootstrapped ? null : old.pid, deadline);
      const runtimeIdentity = await handshake(manifest, deadline);
      return { pid, runtimeIdentity };
    },
    probeService: async ({ manifest, restart, timeoutMs: callTimeoutMs = timeoutMs }) => {
      const deadline = Date.now() + callTimeoutMs;
      const identity = await handshake(manifest, deadline);
      const status = await serviceStatus();
      if (status.pid !== Number(restart?.pid)) throw new Error('launchd service PID changed during probe');
      return { pid: status.pid, runtimeIdentity: identity };
    },
    processProbe: probe,
    rollbackService: async ({ prior, manifest, timeoutMs: callTimeoutMs = timeoutMs }) => {
      if (!prior) throw new Error('cannot rollback launchd runtime without prior selection');
      let priorManifest = manifest;
      let v2Prior = true;
      if (externalRoot && prior) {
        try {
          priorManifest = JSON.parse(await readFile(join(externalRoot, prior, 'runtime-manifest.json'), 'utf8'));
          v2Prior = typeof priorManifest?.schema === 'string' && typeof priorManifest?.version === 'string' && /^[a-f0-9]{64}$/i.test(priorManifest?.clientDigest ?? '') && /^[a-f0-9]{64}$/i.test(priorManifest?.brokerDigest ?? '') && /^[a-f0-9]{40,64}$/i.test(priorManifest?.sourceRevision ?? '') && /^[a-f0-9]{40,64}$/i.test(priorManifest?.sourceTreeDigest ?? '');
        }
        catch { v2Prior = false; }
      }
      const runtime = externalRoot ? resolve(join(externalRoot, prior)) : prior;
      if (!v2Prior) {
        await execFile('launchctl', ['disable', target]);
        if (typeof plistPath === 'string' && isAbsolute(plistPath)) await execFile('launchctl', ['bootout', domain, resolve(plistPath)]).catch(() => {});
        if (await serviceStatus().then(() => true).catch(() => false)) throw new Error('legacy rollback service did not stop');
        const listenerOpen = await new Promise((resolveOpen) => { const handle = netModule.createConnection(socket); const finish = (open) => { handle.destroy(); resolveOpen(open); }; handle.once('connect', () => finish(true)); handle.once('error', () => finish(false)); });
        if (listenerOpen) throw new Error('legacy rollback listener remains open');
        return { pid: null, runtimeIdentity: runtime, legacy: true, stopped: true };
      }
      await enable();
      const old = await ensureLoaded();
      if (!old.bootstrapped) await kickstart();
      const deadline = Date.now() + callTimeoutMs;
      const pid = await waitForPid(old.bootstrapped ? null : old.pid, deadline);
      const restoredConfig = configPath ? JSON.parse((await readFile(configPath, 'utf8')).replace(/^\uFEFF/, '')) : null;
      const identity = await handshake(priorManifest, deadline, { requirePending: false, endpoint: restoredConfig?.unixSocket ?? socket });
      const state = await probe({ pid, runtime, manifest: priorManifest });
      if (!state?.alive) throw new Error('rollback launchd process is not live');
      return { pid, runtimeIdentity: identity };
    },
  };
}

export const createMacOSCutoverCallbacks = createMacOSRuntimeCallbacks;
