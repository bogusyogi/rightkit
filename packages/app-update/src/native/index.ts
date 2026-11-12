import { isReleaseStatus } from '../index.js';
import type { AutoUpdateClient, AutoUpdateStatus, DownloadAvailableResult, ReleaseStatus, StagedUpdateHandle, UpdateTier } from '../index.js';
import type { BootProbationDecision } from '../probation.js';
/** Safe renderer DTOs for M12-DESIGN v3.1 section 2 + v3.2 (no cached view). */
export type UpdateKind = 'patch' | 'update';
export type UpdateRefusal = 'notLicensed' | 'expired' | 'revoked' | 'tierIneligible' | 'busy' | 'unresolved' | 'disabledNoPublicPatch' | 'offline' | 'none';
export type UpdateState = 'idle' | 'checking' | 'available' | 'downloading' | 'downloaded' | 'installing' | 'failed';
/**
 * Message text for the fixed message codes of the native update host. The keys are exactly the codes
 * `crates/rightkit-tauri-shell/src/update.rs` sends (`UpdateMessage`, camelCase); a key is added here only
 * with the native enum. Native sends the code, the renderer owns the text, and the text names no licence,
 * key, device or account.
 */
export const UPDATE_MESSAGES = Object.freeze({
  none: '',
  unconfigured: 'Updates are not available in this build.',
  busy: 'Another update operation is in progress. Try again in a moment.',
  deferred: 'The update will install when you are not in the middle of something.',
  unresolved: 'The last update did not finish. Try again to complete it.',
  networkUnavailable: 'Could not reach the update server. Check your connection and try again.',
  revoked: 'This update is not available for this installation.',
  expired: 'This update is no longer available.',
  forbidden: 'This window cannot manage updates.',
  unsupported: 'This feature is not available in this app.',
  updateRefused: 'This update cannot be installed on this installation.',
  updateFailed: 'The update could not be downloaded or installed.',
  verifyFailed: 'The update could not be verified.',
  storeFailed: 'The update could not be saved on this device.',
  unknown: 'Something went wrong.',
});
export type UpdateMessageCode = keyof typeof UPDATE_MESSAGES;
export type UpdateErrorCode = 'unconfigured' | 'busy' | 'unresolved' | 'network' | 'store' | 'verify' | 'forbidden' | 'unsupported' | 'unknown';
export interface UpdateCheckView {
  available: boolean; kind?: UpdateKind; version?: string; date?: string; notesSummary?: string;
  tier?: string; eligible: boolean; refusal: UpdateRefusal; handleId?: string; checkedAt: number; fresh: true;
}
export interface UpdateStatusView {
  state: UpdateState; handleId?: string; version?: string; kind?: UpdateKind;
  received?: number; total?: number; message: UpdateMessageCode;
}
export interface UpdateProgress { handleId: string; received: number; total?: number }
export interface UpdateDownloadResult {
  handleId: string; state: 'downloaded' | 'cancelled' | 'failed' | 'refused'; reason?: UpdateRefusal; message: UpdateMessageCode;
}
export interface UpdateInstallResult {
  state: 'installing' | 'refused' | 'failed'; reason?: UpdateRefusal | 'stale' | 'consumed' | 'deferred'; message: UpdateMessageCode;
}
export type Unlisten = () => void;
export interface NativeUpdateTransport {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
  listen<T>(event: string, callback: (event: { payload: T }) => void): Promise<Unlisten>;
}
export interface NativeUpdateClient {
  check(): Promise<UpdateCheckView>;
  status(): Promise<UpdateStatusView>;
  download(handleId: string): Promise<UpdateDownloadResult>;
  install(handleId: string): Promise<UpdateInstallResult>;
  cancel(handleId: string): Promise<{ cancelled: boolean }>;
  /**
   * Tell the native host the app is busy with work an install must not interrupt (`rightkit_update_set_busy`,
   * #1019). While busy, `install` answers `refused` with reason `deferred` and keeps the handle and its staged
   * bytes; install again after `setBusy(false)`. A check and a download are unaffected.
   */
  setBusy(busy: boolean): Promise<void>;
  /** Release status for the native app, read by the shell (`rightkit_update_release_status`) and validated against the release status schema. */
  releaseStatus(): Promise<ReleaseStatus>;
  /**
   * Restart the app after an install (`rightkit_update_relaunch`). The host refuses it unless an install completed,
   * and sequences the exit and the restart so the single-instance guard does not kill the replacement; use it
   * instead of a plugin `relaunch()`.
   */
  relaunch(): Promise<void>;
  onProgress(callback: (progress: UpdateProgress) => void): Promise<Unlisten>;
  /** Also cleans registrations whose asynchronous listen call has not resolved. */
  dispose(): void;
}
export class NativeUpdateClientError extends Error {
  readonly code: UpdateErrorCode;
  readonly messageCode: UpdateMessageCode;
  constructor(code: UpdateErrorCode = 'unknown', messageCode: UpdateMessageCode = 'unknown') {
    super(UPDATE_MESSAGES[messageCode]);
    this.name = 'NativeUpdateClientError'; this.code = code; this.messageCode = messageCode;
  }
}
const kinds = ['patch', 'update'] as const;
const refusals = ['notLicensed', 'expired', 'revoked', 'tierIneligible', 'busy', 'unresolved', 'disabledNoPublicPatch', 'offline', 'none'] as const;
const states = ['idle', 'checking', 'available', 'downloading', 'downloaded', 'installing', 'failed'] as const;
function releaseStatusView(value: unknown): ReleaseStatus {
  if (!isReleaseStatus(value)) return invalid();
  return value;
}
const errorCodes = ['unconfigured', 'busy', 'unresolved', 'network', 'store', 'verify', 'forbidden', 'unsupported', 'unknown'] as const;
type Data = Record<string, unknown>;
function invalid(): never { throw new NativeUpdateClientError(); }
function record(value: unknown): Data {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return invalid();
  return value as Data;
}
function member<T extends string>(value: unknown, values: readonly T[]): T {
  if (typeof value !== 'string' || !values.includes(value as T)) return invalid();
  return value as T;
}
function bool(value: unknown): boolean { return typeof value === 'boolean' ? value : invalid(); }
function numeric(value: unknown): number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : invalid();
}
function text(value: unknown, limit: number): string {
  return typeof value === 'string' && value.length <= limit && !/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/u.test(value) ? value : invalid();
}
function handle(value: unknown): string {
  return typeof value === 'string' && /^[A-Za-z0-9_-]{1,256}$/u.test(value) ? value : invalid();
}
function message(value: unknown): UpdateMessageCode {
  return typeof value === 'string' && Object.hasOwn(UPDATE_MESSAGES, value) ? value as UpdateMessageCode : 'unknown';
}
function releaseFields(source: Data): { kind?: UpdateKind; version?: string } {
  const result: { kind?: UpdateKind; version?: string } = {};
  if (source.kind !== undefined) result.kind = member(source.kind, kinds);
  if (source.version !== undefined) result.version = text(source.version, 256);
  return result;
}
function counts(source: Data): { received?: number; total?: number } {
  const result: { received?: number; total?: number } = {};
  if (source.received !== undefined) result.received = numeric(source.received);
  if (source.total !== undefined) result.total = numeric(source.total);
  if (result.received !== undefined && result.total !== undefined && result.received > result.total) invalid();
  return result;
}
function checkView(value: unknown): UpdateCheckView {
  const source = record(value);
  const result: UpdateCheckView = {
    ...releaseFields(source), available: bool(source.available), eligible: bool(source.eligible),
    refusal: member(source.refusal, refusals), checkedAt: numeric(source.checkedAt), fresh: true,
  };
  if (source.fresh !== true) invalid();
  if ((result.eligible || !result.available) && result.refusal !== 'none') invalid();
  if (source.date !== undefined) result.date = text(source.date, 128);
  if (source.notesSummary !== undefined) result.notesSummary = text(source.notesSummary, 8192);
  if (source.tier !== undefined) result.tier = text(source.tier, 128);
  if (source.handleId !== undefined) {
    if (!result.available || !result.eligible) invalid();
    result.handleId = handle(source.handleId);
  }
  return result;
}
function statusView(value: unknown): UpdateStatusView {
  const source = record(value);
  const result: UpdateStatusView = { ...releaseFields(source), ...counts(source), state: member(source.state, states), message: message(source.message) };
  if (source.handleId !== undefined) result.handleId = handle(source.handleId);
  return result;
}
function progressView(value: unknown): UpdateProgress {
  const source = record(value);
  return { ...counts(source), handleId: handle(source.handleId), received: numeric(source.received) };
}
function downloadResult(value: unknown): UpdateDownloadResult {
  const source = record(value);
  const result: UpdateDownloadResult = { handleId: handle(source.handleId), state: member(source.state, ['downloaded', 'cancelled', 'failed', 'refused']), message: message(source.message) };
  if (source.reason !== undefined) result.reason = member(source.reason, refusals);
  return result;
}
function installResult(value: unknown): UpdateInstallResult {
  const source = record(value);
  const result: UpdateInstallResult = { state: member(source.state, ['installing', 'refused', 'failed']), message: message(source.message) };
  if (source.reason !== undefined) result.reason = member(source.reason, [...refusals, 'stale', 'consumed', 'deferred'] as const);
  return result;
}
function safeError(value: unknown): NativeUpdateClientError {
  // Even Error instances and string rejections lose their native text and cause.
  try {
    const source = record(value);
    const code = member(source.code, errorCodes);
    const defaults: Record<typeof code, UpdateMessageCode> = {
      unconfigured: 'unconfigured', busy: 'busy', unresolved: 'unresolved', network: 'networkUnavailable',
      store: 'storeFailed', verify: 'verifyFailed', forbidden: 'forbidden', unsupported: 'unsupported',
      unknown: 'unknown',
    };
    return new NativeUpdateClientError(code, source.message === undefined ? defaults[code] : message(source.message));
  } catch { return new NativeUpdateClientError(); }
}
/** Native owns authorization, generations and lifecycle. Every call reads native state. */
export function createNativeUpdateClient(transport: NativeUpdateTransport): NativeUpdateClient {
  let disposed = false;
  const registrations = new Set<{ active: boolean; unlisten?: Unlisten }>();
  async function request<T>(command: string, project: (value: unknown) => T, args?: Record<string, unknown>): Promise<T> {
    if (disposed) throw new NativeUpdateClientError('unsupported', 'unsupported');
    let value: unknown;
    try { value = await transport.invoke<unknown>(command, args); }
    catch (error) { throw safeError(error); }
    try { return project(value); } catch { throw new NativeUpdateClientError(); }
  }
  function stop(registration: { active: boolean; unlisten?: Unlisten }): void {
    registration.active = false;
    registrations.delete(registration);
    const unlisten = registration.unlisten;
    registration.unlisten = undefined;
    if (unlisten) { try { unlisten(); } catch { /* Native cleanup text is never exposed. */ } }
  }
  return {
    check: () => request('rightkit_update_check', checkView),
    status: () => request('rightkit_update_status', statusView),
    releaseStatus: () => request('rightkit_update_release_status', releaseStatusView),
    async download(handleId) { return request('rightkit_update_download', downloadResult, { handleId: handle(handleId) }); },
    async install(handleId) { return request('rightkit_update_install', installResult, { handleId: handle(handleId) }); },
    async cancel(handleId) { return request('rightkit_update_cancel', value => ({ cancelled: bool(record(value).cancelled) }), { handleId: handle(handleId) }); },
    async relaunch() { await request('rightkit_update_relaunch', () => undefined); },
    async setBusy(busy) { await request('rightkit_update_set_busy', () => undefined, { busy: bool(busy) }); },
    async onProgress(callback) {
      if (disposed) throw new NativeUpdateClientError('unsupported', 'unsupported');
      const registration: { active: boolean; unlisten?: Unlisten } = { active: true };
      registrations.add(registration);
      try {
        registration.unlisten = await transport.listen<unknown>('rightkit://update-progress', event => {
          if (!registration.active) return;
          let progress: UpdateProgress;
          try { progress = progressView(event.payload); } catch { return; }
          callback(progress);
        });
        if (typeof registration.unlisten !== 'function') throw new NativeUpdateClientError();
        if (!registration.active) stop(registration);
        return () => stop(registration);
      } catch (error) { stop(registration); throw safeError(error); }
    },
    dispose() { disposed = true; for (const registration of registrations) stop(registration); },
  };
}



/** Refusals that are not an entitlement state; each becomes an error status with the native message text. */
const refusalMessages: Partial<Record<UpdateRefusal, UpdateMessageCode>> = {
  expired: 'expired', revoked: 'revoked', busy: 'busy', unresolved: 'unresolved', offline: 'networkUnavailable',
};

function progressPercent(received: number, total: number | undefined): number {
  if (total === undefined || total <= 0) return 0;
  return Math.max(0, Math.min(100, Math.round((received / total) * 100)));
}

/** A native check the renderer may download and install; the native `handleId` never leaves this module. */
interface PendingUpdate {
  readonly handleId: string;
  readonly version: string;
  readonly tier: UpdateTier;
  readonly generation: number;
  prepared: boolean;
}

export interface NativeAutoUpdateOptions {
  /** Metadata-only Pro release version that nudges an ineligible install when the native check finds nothing. */
  proUpdateVersion?: () => Promise<string | null> | string | null;
  /** Runs before the native install starts and records boot probation for `toVersion`. A throw refuses the install. */
  beginBootProbation?: (toVersion: string) => Promise<void> | void;
  /** Runs once the native install accepted the handle (state `installing`), so the install is already under way. */
  relaunch?: () => Promise<void> | void;
  /** Receives every status, including `downloading` progress, as `createAutoUpdateClient` does. */
  onStatus?: (status: AutoUpdateStatus) => void;
  onError?: (error: unknown) => void;
  /**
   * The app's own words for any message code (#1019): `{ deferred: 'Update waits until your export finishes.' }`.
   * A code not listed keeps the default text from `UPDATE_MESSAGES`. Text only; no code is added or removed.
   */
  messages?: Partial<Record<UpdateMessageCode, string>>;
}

/**
 * An `AutoUpdateClient` over the native update host. The native side owns the check, the verified download, the
 * swap and the generation fence; this maps its views onto the shared status vocabulary, so `deriveUpdateOverlay`
 * and `createUpdateConsentRunner` run unchanged. Auto-install policy stays native, so `checkOnce` never installs.
 */
export function createNativeAutoUpdateClient(native: NativeUpdateClient, options: NativeAutoUpdateOptions = {}): AutoUpdateClient {
  let generation = 0;
  let current: PendingUpdate | null = null;
  const pendingByHandle = new WeakMap<StagedUpdateHandle, PendingUpdate>();

  function emit(status: AutoUpdateStatus): AutoUpdateStatus {
    options.onStatus?.(status);
    return status;
  }

  function words(code: UpdateMessageCode): string {
    const own = options.messages?.[code];
    return typeof own === 'string' ? own : UPDATE_MESSAGES[code];
  }

  function fail(error: unknown): AutoUpdateStatus {
    options.onError?.(error);
    return emit({ kind: 'error', message: words(error instanceof NativeUpdateClientError ? error.messageCode : 'unknown') });
  }

  function handleFor(pending: PendingUpdate): StagedUpdateHandle {
    const handle: StagedUpdateHandle = Object.freeze({
      version: pending.version,
      tier: pending.tier,
      generation: pending.generation,
      prepared: pending.prepared,
    });
    pendingByHandle.set(handle, pending);
    return handle;
  }

  async function checkOnce(): Promise<AutoUpdateStatus> {
    const checkGeneration = ++generation;
    current = null;
    emit({ kind: 'checking' });
    try {
      const view = await native.check();
      if (generation !== checkGeneration) return { kind: 'idle' };
      if (!view.available) {
        const proVersion = options.proUpdateVersion ? await options.proUpdateVersion() : null;
        if (generation !== checkGeneration) return { kind: 'idle' };
        return proVersion
          ? emit({ kind: 'available', version: proVersion, tier: 'update', eligible: false, reason: 'pro_required' })
          : emit({ kind: 'up_to_date' });
      }
      if (view.version === undefined || view.kind === undefined) throw new Error('native update check names no version or tier');
      const version = view.version;
      const tier = view.kind;
      if (!view.eligible) {
        if (view.refusal === 'notLicensed' || view.refusal === 'tierIneligible') {
          return emit({ kind: 'available', version, tier, eligible: false, reason: 'pro_required' });
        }
        if (view.refusal === 'disabledNoPublicPatch') {
          return emit({ kind: 'available', version, tier, eligible: false, reason: 'auto_install_off' });
        }
        return emit({ kind: 'error', message: words(refusalMessages[view.refusal] ?? 'unknown') });
      }
      if (view.handleId === undefined) throw new Error('native update check names no handle');
      current = { handleId: view.handleId, version, tier, generation: checkGeneration, prepared: false };
      return emit({ kind: 'available', version, tier, eligible: true });
    } catch (error) {
      if (generation !== checkGeneration) return { kind: 'idle' };
      current = null;
      return fail(error);
    }
  }

  /** Runs the native download for one pending update, reporting its progress while it runs. */
  async function downloadPending(pending: PendingUpdate): Promise<UpdateDownloadResult> {
    let unlisten: Unlisten | undefined;
    try {
      unlisten = await native.onProgress((progress) => {
        if (progress.handleId !== pending.handleId || pending.generation !== generation) return;
        emit({ kind: 'downloading', version: pending.version, tier: pending.tier, pct: progressPercent(progress.received, progress.total) });
      });
      emit({ kind: 'downloading', version: pending.version, tier: pending.tier, pct: 0 });
      return await native.download(pending.handleId);
    } finally {
      try { unlisten?.(); } catch { /* Native cleanup text is never exposed. */ }
    }
  }

  /** Downloads and verifies the current update once; emits no `staged` status. */
  async function prepare(): Promise<{ ok: true; handle: StagedUpdateHandle } | { ok: false; status: AutoUpdateStatus }> {
    const pending = current;
    if (!pending) return { ok: false, status: emit({ kind: 'up_to_date' }) };
    if (!pending.prepared) {
      let result: UpdateDownloadResult;
      try {
        result = await downloadPending(pending);
      } catch (error) {
        if (generation !== pending.generation) return { ok: false, status: { kind: 'idle' } };
        return { ok: false, status: fail(error) };
      }
      if (generation !== pending.generation || current !== pending) return { ok: false, status: { kind: 'idle' } };
      if (result.state === 'cancelled') return { ok: false, status: emit({ kind: 'idle' }) };
      if (result.state !== 'downloaded') {
        return { ok: false, status: emit({ kind: 'error', message: words(result.message) }) };
      }
      pending.prepared = true;
    }
    return { ok: true, handle: handleFor(pending) };
  }

  async function downloadAvailable(): Promise<DownloadAvailableResult> {
    const prepared = await prepare();
    if (!prepared.ok) return prepared;
    const status = emit({ kind: 'staged', version: prepared.handle.version, tier: prepared.handle.tier });
    return { ok: true, handle: prepared.handle, status };
  }

  function discardStaged(staged: StagedUpdateHandle): boolean {
    const pending = pendingByHandle.get(staged);
    if (!pending || current !== pending) return false;
    current = null;
    native.cancel(pending.handleId).then(undefined, (error: unknown) => options.onError?.(error));
    return true;
  }

  async function installStaged(staged: StagedUpdateHandle): Promise<AutoUpdateStatus> {
    const pending = pendingByHandle.get(staged);
    // Claimed synchronously before any await: an overlapping install of the same handle finds nothing current.
    if (!pending || current !== pending) {
      return emit({ kind: 'error', message: 'staged update is no longer current; check for updates again' });
    }
    current = null;
    try {
      if (options.beginBootProbation) await options.beginBootProbation(pending.version);
      const result = await native.install(pending.handleId);
      if (result.state === 'refused' && result.reason === 'deferred') {
        // The host keeps the handle and its staged bytes while the app is busy: the same update installs later.
        if (generation === pending.generation) current = pending;
        return emit({ kind: 'deferred', version: pending.version, tier: pending.tier, reason: words('deferred') });
      }
      if (result.state !== 'installing') return emit({ kind: 'error', message: words(result.message) });
      const ready = emit({ kind: 'ready', version: pending.version, tier: pending.tier });
      if (options.relaunch) await options.relaunch();
      return ready;
    } catch (error) {
      return fail(error);
    }
  }

  async function installAvailable(): Promise<AutoUpdateStatus> {
    const prepared = await prepare();
    if (!prepared.ok) return prepared.status;
    return installStaged(prepared.handle);
  }

  return { checkOnce, downloadAvailable, installStaged, installAvailable, discardStaged };
}

/**
 * Boot probation for the native host. The native host evaluates the sentinel once when it starts, before any
 * renderer runs (a renderer that cannot start cannot record it), and the renderer reads that answer. The commands
 * are the ones `rightkit-tauri-shell` registers: `rightkit_boot_status`, `rightkit_boot_stage`,
 * `rightkit_boot_ok` and `rightkit_boot_begin`.
 */
export interface NativeBootProbation {
  /**
   * The boot evaluation of this start (`rightkit_boot_status`). The native host knows the running version, so
   * `currentVersion` is only checked for shape. A start the host could not evaluate answers `{ kind: 'none' }`.
   */
  evaluateBoot(currentVersion: string): Promise<BootProbationDecision>;
  /**
   * Record how far this boot got (`rightkit_boot_stage`, #448): 1 to 64 letters, digits, `_`, `-` or
   * `.`. Native does nothing when no update is on probation; a failed boot reports the last stage it reached.
   */
  reportStage(stage: string): Promise<void>;
  /** The new version is healthy (`rightkit_boot_ok`): end its probation. */
  acknowledgeHealthy(): Promise<void>;
  /** Put the install of `toVersion` on probation (`rightkit_boot_begin`); call it right before the relaunch. */
  begin(toVersion: string): Promise<void>;
}

function bootDecision(value: unknown): BootProbationDecision {
  const source = record(value);
  // `unknown`: the host could not read the sentinel; it never stops the app from starting and holds no probation.
  if (source.kind === 'none' || source.kind === 'unknown') return { kind: 'none' };
  if (source.kind === 'stale') return { kind: 'stale' };
  if (source.kind === 'probation') {
    return { kind: 'probation', toVersion: text(source.toVersion, 256), bootAttempts: numeric(source.bootAttempts) };
  }
  if (source.kind === 'reinstallRequired') {
    const failedVersion = text(source.failedVersion, 256);
    const fromVersion = text(source.fromVersion, 256);
    const bootAttempts = numeric(source.bootAttempts);
    return { kind: 'reinstall_required', failedVersion, fromVersion, bootAttempts, guidance: 'reinstall' };
  }
  return invalid();
}

export function createNativeBootProbation(transport: NativeUpdateTransport): NativeBootProbation {
  async function call<T>(command: string, project: (value: unknown) => T, args?: Record<string, unknown>): Promise<T> {
    let value: unknown;
    try { value = await transport.invoke<unknown>(command, args); }
    catch (error) { throw safeError(error); }
    try { return project(value); } catch { throw new NativeUpdateClientError(); }
  }
  return {
    async evaluateBoot(currentVersion) {
      text(currentVersion, 256);
      return call('rightkit_boot_status', bootDecision);
    },
    async reportStage(stage) {
      if (typeof stage !== 'string' || !/^[A-Za-z0-9_.-]{1,64}$/u.test(stage)) throw new NativeUpdateClientError();
      await call('rightkit_boot_stage', () => undefined, { stage });
    },
    async acknowledgeHealthy() {
      await call('rightkit_boot_ok', () => undefined);
    },
    async begin(toVersion) {
      await call('rightkit_boot_begin', () => undefined, { toVersion: text(toVersion, 256) });
    },
  };
}
