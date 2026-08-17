import { promisify } from 'node:util';
import { execFile as childExecFile } from 'node:child_process';
import net from 'node:net';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { isAbsolute, join, resolve } from 'node:path';
import { BUILD_CONTROL_PROTOCOL } from '@rightkit/build-control-core';
import { describeWindowsJobAddon, installManagedRuntime } from './runtime.mjs';

const execDefault = promisify(childExecFile);
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

/**
 * Install a staged runtime behind a Windows scheduled task. Mirrors
 * runtime-cutover-macos.mjs's service-control contract exactly; the JSON
 * handshake below is intentionally re-implemented rather than imported from
 * the macOS module — it is protocol logic, not OS logic, but duplicating it
 * keeps this file addable without touching the tested macOS path.
 */
export async function installManagedRuntimeWindows(options = {}) {
  const callbacks = createWindowsRuntimeCallbacks(options);
  const installer = options.installManagedRuntime ?? installManagedRuntime;
  const windowsJobAddon = options.windowsJobAddon ?? await describeWindowsJobAddon(options.sourcePackagesRoot);
  return installer({ ...options, ...callbacks, platform: 'win32', windowsJobAddon });
}

/** One scheduled task, `/RestartOnFailure`, ONSTART + logon triggers so a
 * reboot & a crash both bring the broker back — the launchd `KeepAlive` +
 * `RunAtLoad` equivalent. schtasks.exe's own flags cannot express restart
 * policy; only the Task Scheduler XML schema can. */
export function renderTaskDefinition({ taskName, nodePath, brokerScript, configPath, author = 'RightKit' }) {
  const escape = (value) => value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  return `<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Author>${escape(author)}</Author>
    <Description>RightKit managed build broker (v2)</Description>
  </RegistrationInfo>
  <Triggers>
    <BootTrigger>
      <Enabled>true</Enabled>
    </BootTrigger>
    <LogonTrigger>
      <Enabled>true</Enabled>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <!-- S4U: runs as this user without an interactive desktop session, so a
           console-subsystem binary like node.exe never flashes a window —
           InteractiveToken would, because it runs inside the visible session. -->
      <LogonType>S4U</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowHardTerminate>true</AllowHardTerminate>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Hidden>true</Hidden>
    <Priority>7</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>999</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>${escape(nodePath)}</Command>
      <Arguments>"${escape(brokerScript)}" "${escape(configPath)}"</Arguments>
    </Exec>
  </Actions>
</Task>
`;
}

export async function ensureWindowsTaskRegistered({ taskName, taskXmlPath, execFile = execDefault }) {
  const query = await execFile('schtasks', ['/Query', '/TN', taskName]).then(() => true).catch(() => false);
  if (query) return;
  if (typeof taskXmlPath !== 'string' || !isAbsolute(taskXmlPath)) throw new TypeError('absolute taskXmlPath is required when the scheduled task is unregistered');
  await execFile('schtasks', ['/Create', '/TN', taskName, '/XML', taskXmlPath, '/F']);
}

export function createWindowsRuntimeCallbacks(options = {}) {
  const {
    taskName,
    endpoint,
    config,
    execFile = execDefault,
    netModule = net,
    drainProbe,
    processProbe: suppliedProcessProbe,
    timeoutMs = options.healthTimeoutMs ?? 8_000,
    pollMs = 25,
    externalRoot,
    nodePath = process.execPath,
    configPath,
  } = options;
  if (typeof taskName !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(taskName)) throw new TypeError('taskName must be an exact scheduled-task name');
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 100) throw new TypeError('timeoutMs must be at least 100');
  if (typeof drainProbe !== 'function') throw new TypeError('drainProbe is required to refuse open legacy v1 requests');
  const pipe = endpoint ?? config?.windowsPipe;
  if (typeof pipe !== 'string' || pipe.length === 0) throw new TypeError('endpoint or config.windowsPipe is required');

  /** Task Scheduler reports task state (running/not), not a PID directly; the
   * live PID is cross-checked the same way the mac probe does — by matching
   * the exact node+script+config command line, not trusting a bare number. */
  const livePid = async () => {
    const { stdout } = await execFile('powershell', ['-NoProfile', '-NonInteractive', '-Command',
      `Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*${configPath.replace(/'/g, "''")}*' } | Select-Object -ExpandProperty ProcessId`]);
    const pid = Number(String(stdout).trim().split(/\r?\n/)[0]);
    return Number.isSafeInteger(pid) && pid > 0 ? pid : null;
  };
  const ensureRegistered = async ({ taskXmlPath }) => ensureWindowsTaskRegistered({ taskName, taskXmlPath, execFile });
  const ensureDrained = async () => {
    const state = await drainProbe();
    const open = typeof state === 'number' ? state : state?.open ?? state?.count ?? (state === true ? 1 : 0);
    if (open > 0) throw new Error(`legacy broker has ${open} open v1 request(s); cutover refused`);
  };
  const waitForPid = async (previousPid, deadline) => {
    let last = null;
    while (Date.now() <= deadline) {
      last = await livePid().catch(() => null);
      if (Number.isInteger(last) && last > 0 && last !== previousPid) return last;
      await sleep(Math.min(pollMs, Math.max(1, deadline - Date.now())));
    }
    throw new Error(`scheduled task did not produce a new live PID within ${timeoutMs}ms${last ? ` (PID ${last})` : ''}`);
  };
  const handshake = async (manifest, deadline, { requirePending = true } = {}) => {
    let lastError;
    while (Date.now() <= deadline) {
      try {
        return await new Promise((resolveHandshake, reject) => {
          let buffer = ''; let identity = null; let pending = false;
          const socketHandle = netModule.createConnection(pipe);
          const timer = setTimeout(() => { socketHandle.destroy(); reject(new Error('windows runtime HANDSHAKE timed out')); }, Math.max(1, deadline - Date.now()));
          const finish = (error, value) => { clearTimeout(timer); socketHandle.destroy(); error ? reject(error) : resolveHandshake(value); };
          const complete = () => { if (identity && (!requirePending || pending)) finish(null, identity); };
          socketHandle.once('error', (error) => finish(error));
          socketHandle.once('connect', () => socketHandle.write(`${JSON.stringify({ type: 'STATUS', protocol: BUILD_CONTROL_PROTOCOL, runtime: manifest, id: 'runtime-health-probe' })}\n`));
          socketHandle.on('data', (chunk) => {
            buffer += String(chunk);
            for (;;) {
              const line = buffer.indexOf('\n'); if (line < 0) return;
              let event; try { event = JSON.parse(buffer.slice(0, line)); } catch (error) { return finish(error); }
              buffer = buffer.slice(line + 1);
              if (event.type === 'HANDSHAKE') {
                identity = event.runtime;
                if (!identity || identity.schema !== manifest.schema || identity.version !== manifest.version || identity.clientDigest !== manifest.clientDigest || identity.brokerDigest !== manifest.brokerDigest) return finish(new Error('windows runtime HANDSHAKE identity mismatch'));
                complete(); continue;
              }
              if (event.type === 'STATUS' && event.id === 'runtime-health-probe' && event.state === 'ACTIVATION_PENDING') { pending = true; complete(); continue; }
              if (event.type === 'RESULT' && requirePending) return finish(new Error(`windows runtime did not prove activation fence: ${event.code ?? event.status ?? 'rejected'}`));
            }
          });
        });
      } catch (error) {
        lastError = error;
        if (/digest mismatch|timed out/.test(error.message)) throw error;
        await sleep(Math.min(pollMs, Math.max(1, deadline - Date.now())));
      }
    }
    throw new Error(`windows runtime HANDSHAKE unavailable within ${timeoutMs}ms: ${lastError?.message ?? 'unknown error'}`);
  };
  const probe = suppliedProcessProbe ?? (async ({ pid, runtime }) => {
    if (typeof runtime !== 'string' || !isAbsolute(runtime)) return { alive: false };
    try {
      const live = await livePid();
      if (live !== Number(pid)) return { alive: false };
      const expectedRuntime = resolve(runtime);
      const expectedScript = join(expectedRuntime, 'build-control-broker', 'src', 'broker.mjs');
      const runtimeCurrentScript = externalRoot ? join(resolve(externalRoot), 'runtime-current', 'build-control-broker', 'src', 'broker.mjs') : null;
      const { stdout } = await execFile('powershell', ['-NoProfile', '-NonInteractive', '-Command',
        `Get-CimInstance Win32_Process -Filter "ProcessId=${Number(pid)}" | Select-Object -ExpandProperty CommandLine`]);
      const commandLine = String(stdout).trim();
      // node.exe "<script>" "<config>" — matches the exact two-argument shape
      // the mac probe enforces, tolerant of the quoting Windows' CIM adds.
      const scriptMatches = commandLine.includes(expectedScript) || (runtimeCurrentScript && commandLine.includes(runtimeCurrentScript));
      if (!scriptMatches || !commandLine.includes(configPath)) return { alive: false };
      return { alive: true, runtimePath: expectedRuntime, scriptPath: expectedScript };
    } catch { return { alive: false }; }
  });
  const legacyProbe = async ({ pid, runtime }) => {
    const state = await probe({ pid, runtime, legacy: true });
    if (!state?.alive) throw new Error('legacy rollback scheduled-task process is not live');
    await new Promise((resolveLegacy, reject) => {
      const handle = netModule.createConnection(pipe);
      const timer = setTimeout(() => { handle.destroy(); reject(new Error('legacy rollback listener timed out')); }, timeoutMs);
      const finish = (error) => { clearTimeout(timer); handle.destroy(); error ? reject(error) : resolveLegacy(); };
      handle.once('error', finish);
      handle.once('connect', () => finish());
    });
  };

  return {
    restartService: async ({ manifest, taskXmlPath, timeoutMs: callTimeoutMs = timeoutMs }) => {
      await ensureDrained();
      await ensureRegistered({ taskXmlPath });
      const oldPid = await livePid().catch(() => null);
      await execFile('schtasks', ['/End', '/TN', taskName]).catch(() => {});
      await execFile('schtasks', ['/Run', '/TN', taskName]);
      const deadline = Date.now() + callTimeoutMs;
      const pid = await waitForPid(oldPid, deadline);
      const runtimeIdentity = await handshake(manifest, deadline);
      return { pid, runtimeIdentity };
    },
    probeService: async ({ manifest, restart, timeoutMs: callTimeoutMs = timeoutMs }) => {
      const deadline = Date.now() + callTimeoutMs;
      const identity = await handshake(manifest, deadline);
      const live = await livePid();
      if (live !== Number(restart?.pid)) throw new Error('scheduled-task broker PID changed during probe');
      return { pid: live, runtimeIdentity: identity };
    },
    processProbe: probe,
    rollbackService: async ({ prior, manifest, timeoutMs: callTimeoutMs = timeoutMs }) => {
      if (!prior) {
        const registered = await execFile('schtasks', ['/Query', '/TN', taskName]).then(() => true).catch(() => false);
        if (!registered) return { pid: null, runtimeIdentity: null };
        await execFile('schtasks', ['/End', '/TN', taskName]);
        return { pid: null, runtimeIdentity: null };
      }
      let priorManifest = manifest; let v2Prior = true;
      if (externalRoot && prior) {
        try {
          priorManifest = JSON.parse(await readFile(join(externalRoot, prior, 'runtime-manifest.json'), 'utf8'));
          v2Prior = typeof priorManifest?.schema === 'string' && typeof priorManifest?.version === 'string' && /^[a-f0-9]{64}$/i.test(priorManifest?.clientDigest ?? '') && /^[a-f0-9]{64}$/i.test(priorManifest?.brokerDigest ?? '') && /^[a-f0-9]{40,64}$/i.test(priorManifest?.sourceRevision ?? '') && /^[a-f0-9]{40,64}$/i.test(priorManifest?.sourceTreeDigest ?? '');
        } catch { v2Prior = false; }
      }
      const oldPid = await livePid().catch(() => null);
      await execFile('schtasks', ['/End', '/TN', taskName]).catch(() => {});
      await execFile('schtasks', ['/Run', '/TN', taskName]);
      const deadline = Date.now() + callTimeoutMs;
      const pid = await waitForPid(oldPid, deadline);
      if (v2Prior) { const identity = await handshake(priorManifest, deadline, { requirePending: false }); return { pid, runtimeIdentity: identity }; }
      await legacyProbe({ pid, runtime: prior });
      return { pid, runtimeIdentity: null };
    },
    ensureRegistered,
  };
}

/** Renders & writes the Task Scheduler XML to a throwaway temp file for the
 * `/Create /XML` call, then removes it — schtasks reads it once at register
 * time and keeps its own copy internally. */
export async function withTaskDefinitionFile(definition, run) {
  const dir = await mkdtemp(join(tmpdir(), 'rk-task-'));
  const file = join(dir, 'task.xml');
  try {
    const bytes = Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(definition, 'utf16le')]);
    await writeFile(file, bytes);
    return await run(file);
  }
  finally { await rm(dir, { recursive: true, force: true }); }
}
