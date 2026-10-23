import type { UpdateTier } from './contract-vectors.js';
export type { UpdateTier, UpdateHostContractVector } from './contract-vectors.js';
/** Kept on the main entry for compatibility; prefer `@rightkit/app-update/contract-vectors` in tests. */
export { UPDATE_HOST_CONTRACT_VECTORS } from './contract-vectors.js';

export type UpdateHeaders = Record<string, string>;

export type UpdateDownloadEvent =
  | { event: 'Started'; data: { contentLength?: number } }
  | { event: 'Progress'; data: { chunkLength: number } }
  | { event: 'Finished'; data?: unknown }
  | { event: string; data?: unknown };

export type UpdateInstallOptions = { headers?: UpdateHeaders };

export interface RuntimeUpdate {
  version: string;
  /** Exact manifest metadata returned by Tauri's updater check. */
  rawJson?: Record<string, unknown>;
  /** Target/platform key the check resolved, when the adapter exposes it. */
  target?: string;
  downloadAndInstall(
    onEvent?: (event: UpdateDownloadEvent) => void,
    options?: UpdateInstallOptions,
  ): Promise<void>;
  /**
   * Real prepare step (Tauri v2 `Update.download`): downloads and verifies the artifact into the
   * adapter's own cache without installing. Present together with `install` on prepared adapters.
   */
  download?(
    onEvent?: (event: UpdateDownloadEvent) => void,
    options?: UpdateInstallOptions,
  ): Promise<void>;
  /** Installs the payload `download` prepared. Must perform no network access. */
  install?(): Promise<void>;
}

export type CheckForUpdate = (options?: { headers?: UpdateHeaders }) => Promise<RuntimeUpdate | null>;

export type AutoUpdateStatus =
  | { kind: 'idle' }
  | { kind: 'checking' }
  | { kind: 'up_to_date' }
  | {
      kind: 'available';
      version: string;
      tier: UpdateTier | null;
      eligible: boolean;
      reason?: 'auto_install_off' | 'pro_required';
    }
  | { kind: 'deferred'; version: string; tier: UpdateTier | null; reason: string }
  | { kind: 'downloading'; version: string; tier: UpdateTier | null; pct: number }
  | { kind: 'ready'; version: string; tier: UpdateTier | null }
  /** Download step finished: the update is verified and staged, and nothing is installed yet (additive, #625). */
  | { kind: 'staged'; version: string; tier: UpdateTier }
  | { kind: 'error'; message: string };

/**
 * Opaque handle returned by `downloadAvailable()`. It names one verified update; `installStaged()` accepts only
 * a handle this client issued for the update that is still current (a newer check invalidates it).
 */
export interface StagedUpdateHandle {
  readonly version: string;
  readonly tier: UpdateTier;
  /** Target/platform key bound at check time, when known. */
  readonly target?: string;
  /** Manifest digest (sha256/digest) bound at check time, when the manifest carries one. */
  readonly digest?: string;
  /** Manifest signature bound at check time, when the manifest carries one. */
  readonly signature?: string;
  /** Check generation that issued this handle; a newer check fences it. */
  readonly generation: number;
  /**
   * True only when the adapter really downloaded+verified the payload (`download`/`install` pair).
   * False for the legacy combined adapter, which cannot claim a true prefetch: its bytes are
   * fetched by `installStaged()`.
   */
  readonly prepared: boolean;
}

export type DownloadAvailableResult =
  | { ok: true; handle: StagedUpdateHandle; status: AutoUpdateStatus }
  | { ok: false; status: AutoUpdateStatus };

export interface AutoUpdateClientConfig {
  check: CheckForUpdate;
  /** Metadata-only Pro release version used to nudge ineligible Free users. */
  getProUpdateVersion?: () => Promise<string | null> | string | null;
  /** Manifest-check headers; reused for artifacts only with explicit strictInstallHeaders:false. */
  getHeaders?: () => Promise<UpdateHeaders | undefined> | UpdateHeaders | undefined;
  /**
   * Headers used to download/install an eligible artifact. An undefined result clears
   * retained manifest-check headers, as does an absent callback in default strict mode.
   * Apps can provide this callback to make cross-origin policy explicit.
   * It runs after canInstall, immediately before download, and is not cached.
   */
  getInstallHeaders?: (
    tier: UpdateTier,
    context: { checkHeaders?: UpdateHeaders },
  ) => Promise<UpdateHeaders | undefined> | UpdateHeaders | undefined;
  /**
   * Artifact-header isolation. Default true (since the next compatible release): with no
   * `getInstallHeaders`, the artifact download gets NO headers, so manifest-check auth is never
   * sent to a different artifact origin. Set false only as a deprecated compatibility escape to
   * reuse the manifest-check headers; prefer an explicit `getInstallHeaders`.
   */
  strictInstallHeaders?: boolean;
  getTier?: (
    version: string,
    context: { headers?: UpdateHeaders },
  ) => Promise<UpdateTier | null> | UpdateTier | null;
  /**
   * Optional app-side validation of the signed update metadata (for example an
   * extra signature or digest check over `update.rawJson`). Return false or throw
   * to reject the update; it then surfaces as an `error` status and is never
   * offered. Tauri's updater plugin still verifies the artifact signature against
   * the configured pubkey; this hook is additional and defaults to none.
   */
  verifyMetadata?: (
    update: RuntimeUpdate,
    context: { headers?: UpdateHeaders },
  ) => Promise<boolean | void> | boolean | void;
  /**
   * Host relaunch action (for example `@tauri-apps/plugin-process` `relaunch`).
   * Only invoked when `relaunchAfterInstall` is true.
   */
  relaunch?: () => Promise<void> | void;
  /** Relaunch automatically after `ready`. Default false: the host shows its own restart prompt. */
  relaunchAfterInstall?: boolean;
  canInstall?: () => Promise<boolean> | boolean;
  isEligible?: (
    tier: UpdateTier | null,
    context: { headers?: UpdateHeaders },
  ) => Promise<boolean> | boolean;
  shouldAutoInstall?: (
    tier: UpdateTier | null,
    context: { headers?: UpdateHeaders },
  ) => Promise<boolean> | boolean;
  deferReason?: () => string;
  onStatus?: (status: AutoUpdateStatus) => void;
  onError?: (error: unknown) => void;
}

export interface AutoUpdateClient {
  checkOnce(): Promise<AutoUpdateStatus>;
  /**
   * Prepare step. On a prepared adapter (`download`/`install`) this downloads and verifies the
   * artifact now and returns a handle with `prepared: true`. On the legacy combined adapter it
   * stages a handle with `prepared: false` and fetches nothing; it must not be shown as a prefetch.
   */
  downloadAvailable(): Promise<DownloadAvailableResult>;
  /**
   * Install step: consumes the handle exactly once. For a prepared handle it runs only the
   * adapter `install` over the already-downloaded payload (no network, no header resolution).
   */
  installStaged(handle: StagedUpdateHandle): Promise<AutoUpdateStatus>;
  /** Fences a staged handle without installing (cancel/relaunch): later installs of it fail. */
  discardStaged(handle: StagedUpdateHandle): boolean;
  /** Combined compatibility path, implemented as `downloadAvailable` then `installStaged` without a 'staged' emit. */
  installAvailable(): Promise<AutoUpdateStatus>;
}

export interface ReleaseStatus {
  schema: 1;
  appKey: string;
  platform: string | null;
  patchVersion: string | null;
  updateVersion: string | null;
}

export async function fetchReleaseStatus(
  url: string,
  fetchImpl: typeof fetch = fetch,
): Promise<ReleaseStatus> {
  const response = await fetchImpl(url, { cache: 'no-store' });
  if (!response.ok) throw new Error(`release status request failed (${response.status})`);
  const value: unknown = await response.json();
  if (!isReleaseStatus(value)) throw new Error('release status response is invalid');
  return value;
}

export function isReleaseStatus(value: unknown): value is ReleaseStatus {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;
  const record = value as Record<string, unknown>;
  const allowed = new Set(['schema', 'appKey', 'platform', 'patchVersion', 'updateVersion']);
  if (Object.keys(record).some((key) => !allowed.has(key))) return false;
  const nullableString = (input: unknown) => input === null || typeof input === 'string';
  return record.schema === 1
    && typeof record.appKey === 'string'
    && record.appKey.length > 0
    && nullableString(record.platform)
    && nullableString(record.patchVersion)
    && nullableString(record.updateVersion);
}

export function updateTierFromManifest(rawJson: Record<string, unknown> | undefined): UpdateTier {
  const tier = rawJson?.tier;
  if (tier !== 'patch' && tier !== 'update') {
    throw new Error('update manifest tier must be "patch" or "update"');
  }
  return tier;
}

/** A check result held until it is downloaded, installed, or superseded by a newer check generation. */
interface AvailableRecord {
  update: RuntimeUpdate;
  tier: UpdateTier;
  checkHeaders?: UpdateHeaders;
  generation: number;
  target?: string;
  digest?: string;
  signature?: string;
  prepared: boolean;
  preparePromise: Promise<void> | null;
}

const manifestString = (raw: Record<string, unknown> | undefined, ...keys: string[]): string | undefined => {
  for (const key of keys) {
    const value = raw?.[key];
    if (typeof value === 'string' && value) return value;
  }
  return undefined;
};

const hasPreparedAdapter = (update: RuntimeUpdate): boolean =>
  typeof update.download === 'function' && typeof update.install === 'function';

export function createAutoUpdateClient(config: AutoUpdateClientConfig): AutoUpdateClient {
  const shouldAutoInstall = config.shouldAutoInstall ?? (() => false);
  const isEligible = config.isEligible ?? ((tier) => tier !== 'update');
  const deferReason = config.deferReason ?? (() => 'host_not_idle');
  let generation = 0;
  let installInProgress = false;
  let available: AvailableRecord | null = null;
  let ineligibleAvailable: Extract<AutoUpdateStatus, { kind: 'available' }> | null = null;
  const handleByRecord = new WeakMap<object, StagedUpdateHandle>();
  const recordByHandle = new WeakMap<StagedUpdateHandle, AvailableRecord>();

  function emit(status: AutoUpdateStatus): AutoUpdateStatus {
    config.onStatus?.(status);
    return status;
  }

  function emitForGeneration(status: AutoUpdateStatus, expectedGeneration: number): AutoUpdateStatus {
    return generation === expectedGeneration ? emit(status) : status;
  }

  function idleWhenStale(expectedGeneration: number): AutoUpdateStatus | null {
    return generation === expectedGeneration ? null : { kind: 'idle' };
  }

  async function checkOnce(): Promise<AutoUpdateStatus> {
    const checkGeneration = ++generation;
    available = null;
    ineligibleAvailable = null;
    try {
      emit({ kind: 'checking' });
      const headers = await config.getHeaders?.();
      const staleAfterHeaders = idleWhenStale(checkGeneration);
      if (staleAfterHeaders) return staleAfterHeaders;
      const update = await config.check(headers ? { headers } : undefined);
      const staleAfterCheck = idleWhenStale(checkGeneration);
      if (staleAfterCheck) return staleAfterCheck;
      if (!update) {
        const proUpdateVersion = await config.getProUpdateVersion?.();
        const staleAfterStatus = idleWhenStale(checkGeneration);
        if (staleAfterStatus) return staleAfterStatus;
        if (proUpdateVersion) {
          return emit({
            kind: 'available',
            version: proUpdateVersion,
            tier: 'update',
            eligible: false,
            reason: 'pro_required',
          });
        }
        return emit({ kind: 'up_to_date' });
      }

      const tier = config.getTier
        ? await config.getTier(update.version, { headers })
        : updateTierFromManifest(update.rawJson);
      const staleAfterTier = idleWhenStale(checkGeneration);
      if (staleAfterTier) return staleAfterTier;
      if (tier !== 'patch' && tier !== 'update') {
        throw new Error('update manifest tier must be "patch" or "update"');
      }
      if (config.verifyMetadata) {
        const verified = await config.verifyMetadata(update, { headers });
        const staleAfterVerify = idleWhenStale(checkGeneration);
        if (staleAfterVerify) return staleAfterVerify;
        if (verified === false) throw new Error('update metadata verification failed');
      }
      const eligible = await isEligible(tier, { headers });
      const staleAfterEligibility = idleWhenStale(checkGeneration);
      if (staleAfterEligibility) return staleAfterEligibility;
      available = eligible
        ? {
            update,
            tier,
            checkHeaders: headers,
            generation: checkGeneration,
            target: update.target ?? manifestString(update.rawJson, 'target', 'platform'),
            digest: manifestString(update.rawJson, 'digest', 'sha256'),
            signature: manifestString(update.rawJson, 'signature'),
            prepared: false,
            preparePromise: null,
          }
        : null;
      const nextAvailableStatus: Extract<AutoUpdateStatus, { kind: 'available' }> = {
        kind: 'available',
        version: update.version,
        tier,
        eligible,
        reason: !eligible && tier === 'update' ? 'pro_required' : 'auto_install_off',
      };
      ineligibleAvailable = eligible ? null : nextAvailableStatus;
      const availableStatus = emit(nextAvailableStatus);
      if (!eligible) return availableStatus;
      const autoInstall = await shouldAutoInstall(tier, { headers });
      const staleAfterPolicy = idleWhenStale(checkGeneration);
      if (staleAfterPolicy) return staleAfterPolicy;
      if (!autoInstall) return availableStatus;

      const claimed = claimAvailable(checkGeneration);
      return claimed ? install(claimed) : availableStatus;
    } catch (error) {
      if (generation !== checkGeneration) return { kind: 'idle' };
      available = null;
      ineligibleAvailable = null;
      config.onError?.(error);
      return emit({ kind: 'error', message: String(error) });
    }
  }

  /** Returns a handle for the current available update without consuming it; null when nothing is staged-able. */
  function stage(): StagedUpdateHandle | null {
    const current = available;
    if (!current) return null;
    let handle = handleByRecord.get(current);
    if (!handle) {
      handle = Object.freeze({
        version: current.update.version,
        tier: current.tier,
        ...(current.target ? { target: current.target } : {}),
        ...(current.digest ? { digest: current.digest } : {}),
        ...(current.signature ? { signature: current.signature } : {}),
        generation: current.generation,
        prepared: current.prepared,
      });
      handleByRecord.set(current, handle);
      recordByHandle.set(handle, current);
    }
    return handle;
  }

  /** Re-issue the handle after a real prepare so `prepared` reflects the bound payload. */
  function restagePrepared(record: AvailableRecord): StagedUpdateHandle {
    const old = handleByRecord.get(record);
    if (old?.prepared) return old;
    if (old) recordByHandle.delete(old);
    handleByRecord.delete(record);
    const handle = stage();
    if (!handle) throw new Error('staged update is no longer current; check for updates again');
    return handle;
  }

  async function resolveInstallHeaders(record: AvailableRecord): Promise<UpdateHeaders> {
    if (config.getInstallHeaders) return (await config.getInstallHeaders(record.tier, { checkHeaders: record.checkHeaders })) ?? {};
    // Tauri retains check headers when options are omitted. An explicit empty map clears them.
    return (config.strictInstallHeaders === false ? record.checkHeaders : undefined) ?? {};
  }

  function progressEmitter(record: AvailableRecord): (event: UpdateDownloadEvent) => void {
    let total = 0;
    let received = 0;
    return (event) => {
      if (event.event === 'Started' && isStartedEvent(event)) {
        total = event.data.contentLength ?? 0;
        emitForGeneration({ kind: 'downloading', version: record.update.version, tier: record.tier, pct: 0 }, record.generation);
      } else if (event.event === 'Progress' && isProgressEvent(event)) {
        received += event.data.chunkLength;
        const pct = total > 0 ? Math.max(0, Math.min(100, Math.round((received / total) * 100))) : 0;
        emitForGeneration({ kind: 'downloading', version: record.update.version, tier: record.tier, pct }, record.generation);
      }
    };
  }

  /** One real download+verify per record; concurrent prepares share it. */
  function prepare(record: AvailableRecord): Promise<void> {
    if (record.prepared) return Promise.resolve();
    record.preparePromise ??= (async () => {
      const headers = await resolveInstallHeaders(record);
      await record.update.download?.(progressEmitter(record), { headers });
      record.prepared = true;
    })().finally(() => { record.preparePromise = null; });
    return record.preparePromise;
  }

  async function downloadAvailable(): Promise<DownloadAvailableResult> {
    const record = available;
    const handle = stage();
    if (!record || !handle) {
      return { ok: false, status: ineligibleAvailable ? emit(ineligibleAvailable) : emit({ kind: 'up_to_date' }) };
    }
    if (!hasPreparedAdapter(record.update)) {
      const status = emit({ kind: 'staged', version: handle.version, tier: handle.tier });
      return { ok: true, handle, status };
    }
    try {
      await prepare(record);
    } catch (error) {
      if (generation !== record.generation) return { ok: false, status: { kind: 'idle' } };
      config.onError?.(error);
      return { ok: false, status: emit({ kind: 'error', message: String(error) }) };
    }
    if (generation !== record.generation || available !== record) return { ok: false, status: { kind: 'idle' } };
    const preparedHandle = restagePrepared(record);
    const status = emit({ kind: 'staged', version: preparedHandle.version, tier: preparedHandle.tier });
    return { ok: true, handle: preparedHandle, status };
  }

  function discardStaged(handle: StagedUpdateHandle): boolean {
    const record = recordByHandle.get(handle);
    if (!record || available !== record) return false;
    available = null;
    return true;
  }

  async function installStaged(handle: StagedUpdateHandle): Promise<AutoUpdateStatus> {
    // Synchronous claim before any await: overlapping installs of one handle run once.
    const record = recordByHandle.get(handle);
    if (!record || available !== record) {
      return emit({ kind: 'error', message: 'staged update is no longer current; check for updates again' });
    }
    available = null;
    return install(record);
  }

  async function installAvailable(): Promise<AutoUpdateStatus> {
    const handle = stage();
    if (!handle) return ineligibleAvailable ? emit(ineligibleAvailable) : emit({ kind: 'up_to_date' });
    return installStaged(handle);
  }

  function claimAvailable(expectedGeneration?: number) {
    const claimed = available;
    if (!claimed || (expectedGeneration !== undefined && claimed.generation !== expectedGeneration)) {
      return null;
    }
    available = null;
    return claimed;
  }

  async function install(claimed: AvailableRecord): Promise<AutoUpdateStatus> {
    const { update, tier, generation: installGeneration } = claimed;
    // This fence spans records/generations, including preparation, native replacement & relaunch.
    if (installInProgress) {
      if (generation === installGeneration && available === null) available = claimed;
      return emitForGeneration({ kind: 'deferred', version: update.version, tier, reason: 'install_in_progress' }, installGeneration);
    }
    installInProgress = true;
    try {
      const safe = await config.canInstall?.() ?? true;
      if (!safe) {
        const deferredStatus: AutoUpdateStatus = {
          kind: 'deferred',
          version: update.version,
          tier,
          reason: deferReason(),
        };
        if (generation === installGeneration && available === null) available = claimed;
        return emitForGeneration(deferredStatus, installGeneration);
      }
      if (generation !== installGeneration) return { kind: 'idle' };

      if (hasPreparedAdapter(update)) {
        // Prepared payload: install consumes it with zero network. Unprepared records
        // (combined installAvailable/auto-install) prepare first, then install the same payload.
        if (!claimed.prepared) await prepare(claimed);
        if (generation !== installGeneration) return { kind: 'idle' };
        await update.install?.();
      } else {
        const installHeaders = await resolveInstallHeaders(claimed);
        if (generation !== installGeneration) return { kind: 'idle' };
        await update.downloadAndInstall(progressEmitter(claimed), { headers: installHeaders });
      }

      const ready = emitForGeneration({ kind: 'ready', version: update.version, tier }, installGeneration);
      if (config.relaunchAfterInstall && config.relaunch && generation === installGeneration) {
        await config.relaunch();
      }
      return ready;
    } catch (error) {
      const errorStatus: AutoUpdateStatus = { kind: 'error', message: String(error) };
      if (generation !== installGeneration) return errorStatus;
      config.onError?.(error);
      return emit(errorStatus);
    } finally {
      installInProgress = false;
    }
  }

  return { checkOnce, downloadAvailable, installStaged, installAvailable, discardStaged };
}

function isStartedEvent(event: UpdateDownloadEvent): event is { event: 'Started'; data: { contentLength?: number } } {
  return typeof event.data === 'object' && event.data !== null;
}

function isProgressEvent(event: UpdateDownloadEvent): event is { event: 'Progress'; data: { chunkLength: number } } {
  return (
    typeof event.data === 'object'
    && event.data !== null
    && 'chunkLength' in event.data
    && typeof event.data.chunkLength === 'number'
  );
}

export { compareVersions, isNewerVersion, parseVersion, type ParsedVersion } from './version.js';
export {
  resolveChannel,
  validateChannels,
  type UpdateChannelConfig,
  type UpdateChannelMap,
  type UpdateChannelName,
} from './channels.js';
export { createCheckGate, type CheckGate, type CheckGateOptions } from './check-gate.js';
export {
  createPersistedCheckCache,
  type CheckCacheStore,
  type PersistedCheckCache,
  type PersistedCheckCacheOptions,
  type PersistedCheckResult,
} from './check-cache.js';
export {
  createBootProbation,
  previousInstallPath,
  PREVIOUS_SUFFIX,
  type BootProbation,
  type BootFailureInfo,
  type BootProbationConfig,
  type BootProbationDecision,
  type BootProbationRecord,
  type BootProbationStorage,
} from './probation.js';
export {
  UPDATES_PACKAGE_VERSION,
  assertSingleUpdatesVersion,
  registerUpdatesInstance,
} from './single-instance.js';
export {
  createUpdateConsent,
  createUpdateConsentRunner,
  deriveUpdateOverlay,
  type UpdateConsentOperations,
  type UpdateConsentAction,
  type UpdateConsentController,
  type UpdateConsentState,
  type UpdateOverlayModel,
  type UpdateOverlayPhase,
} from './overlay.js';
