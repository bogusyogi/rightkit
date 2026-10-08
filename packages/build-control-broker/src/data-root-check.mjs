import { spawn as nodeSpawn } from 'node:child_process';
import { BUILD_CONTROL_PROTOCOL, SOURCE_RUNTIME_IDENTITY, normalizeRuntimeIdentity } from '@rightkit/build-control-core';

export const EXTERNAL_ROOT_UNREADABLE = 'EXTERNAL_ROOT_UNREADABLE';
const diagnostic = (path, errno) => ({ code: EXTERNAL_ROOT_UNREADABLE, path, errno });
// Run filesystem operations in a disposable process: a removable-volume TCC
// open may wedge even a worker's shared libuv pool. No external module loads.
const probeScript = `
const fs = require('node:fs');
const path = process.argv[1];
let file;
try {
  fs.accessSync(path, fs.constants.R_OK | fs.constants.W_OK | fs.constants.X_OK);
  fs.readdirSync(path);
  file = require('node:path').join(path, '.rightkit-probe-' + process.pid);
  const fd = fs.openSync(file, 'wx', 0o600);
  fs.writeSync(fd, 'probe'); fs.closeSync(fd); fs.unlinkSync(file); file = null;
  process.stdout.write(JSON.stringify({ok:true}));
} catch (error) {
  if (file) { try { fs.unlinkSync(file); } catch {} }
  process.stdout.write(JSON.stringify({ok:false,errno:error.code || 'UNKNOWN'}));
}
`;

export function probeDataRoot(path, { timeoutMs = 3_000, spawn = nodeSpawn, nodePath = process.execPath } = {}) {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1) throw new TypeError('positive probe timeout is required');
  return new Promise((resolve) => {
    let child; let timer; let settled = false; let output = '';
    const finish = (value) => { if (settled) return; settled = true; clearTimeout(timer); resolve(value); };
    const fail = (errno) => finish({ ok: false, diagnostic: diagnostic(path, errno) });
    try { child = spawn(nodePath, ['--input-type=commonjs', '-e', probeScript, path], { cwd: '/', stdio: ['ignore', 'pipe', 'ignore'], windowsHide: true }); }
    catch (error) { fail(error.code ?? 'UNKNOWN'); return; }
    timer = setTimeout(() => {
      // Do not await a wedged child. Reap only this probe, detach its handles.
      child.kill('SIGKILL'); child.stdout?.destroy(); child.unref?.(); fail('timeout');
    }, timeoutMs);
    child.stdout?.on('data', (chunk) => {
      output += chunk;
      if (output.length > 4096) { child.kill('SIGKILL'); fail('INVALID_PROBE'); }
    });
    child.once('error', (error) => fail(error.code ?? 'UNKNOWN'));
    child.once('close', () => {
      try { const result = JSON.parse(output); result.ok === true ? finish({ ok: true }) : fail(result.errno ?? 'UNKNOWN'); }
      catch { fail('INVALID_PROBE'); }
    });
  });
}

/** Socket-facing gate stays available while data is denied or initializing. */
export class DataRootService {
  #path; #runtime; #probe; #create; #log; #service = null; #diagnostic; #checking = null; #timer; #stopping = false; #initializing = null;
  constructor({ path, runtimeIdentity = SOURCE_RUNTIME_IDENTITY, probe = probeDataRoot, createService, log = async () => {}, intervalMs = 30_000 }) {
    this.#path = path; this.#runtime = normalizeRuntimeIdentity(runtimeIdentity); this.#probe = probe;
    this.#create = createService; this.#log = log;
    this.#diagnostic = { code: 'RK_DATA_ROOT_INITIALIZING', path, errno: 'pending' };
    this.#timer = setInterval(() => { void this.check().catch(() => {}); }, intervalMs); this.#timer.unref?.();
  }
  get diagnostic() { return this.#diagnostic; }
  check() {
    if (this.#stopping) return Promise.resolve();
    if (this.#checking) return this.#checking;
    this.#checking = this.#check().finally(() => { this.#checking = null; });
    return this.#checking;
  }
  async #check() {
    let result;
    try { result = await this.#probe(this.#path); }
    catch (error) { result = { ok: false, diagnostic: diagnostic(this.#path, error.code ?? 'UNKNOWN') }; }
    if (this.#stopping) return;
    if (!result.ok) {
      const previous = this.#diagnostic;
      this.#diagnostic = result.diagnostic ?? diagnostic(this.#path, 'UNKNOWN');
      if (previous?.code !== this.#diagnostic.code || previous?.errno !== this.#diagnostic.errno) await this.#log({ type: 'DATA_ROOT_UNAVAILABLE', ...this.#diagnostic });
      return;
    }
    if (this.#diagnostic?.code === EXTERNAL_ROOT_UNREADABLE) await this.#log({ type: 'DATA_ROOT_RESTORED', path: this.#path });
    this.#diagnostic = this.#service ? null : { code: 'RK_DATA_ROOT_INITIALIZING', path: this.#path, errno: 'pending' };
    // Keep STATUS responsive even if subsequent tool verification/state recovery
    // is slow. Only one initializer runs; it starts after a successful probe.
    if (!this.#service && !this.#initializing) {
      this.#initializing = Promise.resolve().then(() => this.#create()).then(async (service) => {
        if (this.#stopping) { await service.shutdown(); return; }
        this.#service = service;
        if (this.#diagnostic?.code === 'RK_DATA_ROOT_INITIALIZING') this.#diagnostic = null;
      }).catch(async (error) => {
        this.#diagnostic = { code: 'RK_DATA_ROOT_INITIALIZATION_FAILED', path: this.#path, errno: error.code ?? 'UNKNOWN', error: error.message };
        await this.#log({ type: 'DATA_ROOT_UNAVAILABLE', ...this.#diagnostic });
      }).finally(() => { this.#initializing = null; });
    }
  }
  async ready() { await this.check(); await this.#initializing; }
  async dispatch(message, emit = () => {}, onDisconnect = null) {
    if (!this.#diagnostic && this.#service) return this.#service.dispatch(message, emit, onDisconnect);
    if (message?.protocol !== BUILD_CONTROL_PROTOCOL) throw new TypeError('RightKit build-control v2 protocol is required');
    const runtime = normalizeRuntimeIdentity(message.runtime);
    if (['version', 'clientDigest', 'brokerDigest'].some((key) => runtime[key] !== this.#runtime[key])) throw new TypeError('RightKit client/broker runtime digest mismatch');
    emit({ type: 'HANDSHAKE', runtime: this.#runtime });
    const detail = this.#diagnostic ?? { code: 'RK_BROKER_STOPPING', path: this.#path };
    const response = message.type === 'STATUS'
      ? { type: 'STATUS', id: message.id, state: 'UNAVAILABLE', runtime: this.#runtime, diagnostic: detail, ...detail }
      : message.type === 'LIST' ? { type: 'LIST', inFlight: [], recent: [], unavailable: true, diagnostic: detail, ...detail }
      : { type: 'RESULT', status: 'REJECTED', exitCode: 125, error: `${detail.code}: ${detail.path} (${detail.errno})`, diagnostic: detail, ...detail };
    emit(response); return response;
  }
  async recordTerminationSignal(signal, metadata) {
    if (!this.#diagnostic && this.#service) return this.#service.recordTerminationSignal(signal, metadata);
    return this.#log({ type: 'BROKER_TERMINATION_SIGNAL', signal, ...metadata });
  }
  async shutdown() {
    if (this.#stopping) return;
    this.#stopping = true; clearInterval(this.#timer);
    await this.#checking;
    await this.#initializing;
    await this.#service?.shutdown();
  }
}
