import type { UpdateTier } from './contract-vectors.js';
export type { UpdateTier, UpdateHostContractVector } from './contract-vectors.js';
/** Kept on the main entry for compatibility; prefer `@rightkit/updates/contract-vectors` in tests. */
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
  downloadAndInstall(
    onEvent?: (event: UpdateDownloadEvent) => void,
    options?: UpdateInstallOptions,
  ): Promise<void>;
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
  | { kind: 'error'; message: string };

export interface AutoUpdateClientConfig {
  check: CheckForUpdate;
  /** Metadata-only Pro release version used to nudge ineligible Free users. */
  getProUpdateVersion?: () => Promise<string | null> | string | null;
  /** Manifest-check headers; also the legacy install fallback when no separate callback is set. */
  getHeaders?: () => Promise<UpdateHeaders | undefined> | UpdateHeaders | undefined;
  /**
   * Headers used to download/install an eligible artifact. When absent, the
   * manifest-check headers are reused for backward compatibility. New apps
   * should always provide this callback to make cross-origin policy explicit.
   * It runs after canInstall, immediately before download, and is not cached.
   */
  getInstallHeaders?: (
    tier: UpdateTier,
    context: { checkHeaders?: UpdateHeaders },
  ) => Promise<UpdateHeaders | undefined> | UpdateHeaders | undefined;
  getTier?: (
    version: string,
    context: { headers?: UpdateHeaders },
  ) => Promise<UpdateTier | null> | UpdateTier | null;
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

function isReleaseStatus(value: unknown): value is ReleaseStatus {
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

export function createAutoUpdateClient(config: AutoUpdateClientConfig): AutoUpdateClient {
  const shouldAutoInstall = config.shouldAutoInstall ?? (() => false);
  const isEligible = config.isEligible ?? ((tier) => tier !== 'update');
  const deferReason = config.deferReason ?? (() => 'host_not_idle');
  let generation = 0;
  let available:
    | {
        update: RuntimeUpdate;
        tier: UpdateTier;
        checkHeaders?: UpdateHeaders;
        generation: number;
      }
    | null = null;
  let ineligibleAvailable: Extract<AutoUpdateStatus, { kind: 'available' }> | null = null;

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
      const eligible = await isEligible(tier, { headers });
      const staleAfterEligibility = idleWhenStale(checkGeneration);
      if (staleAfterEligibility) return staleAfterEligibility;
      available = eligible
        ? { update, tier, checkHeaders: headers, generation: checkGeneration }
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

  async function installAvailable(): Promise<AutoUpdateStatus> {
    const claimed = claimAvailable();
    if (claimed) return install(claimed);
    return ineligibleAvailable ? emit(ineligibleAvailable) : emit({ kind: 'up_to_date' });
  }

  function claimAvailable(expectedGeneration?: number) {
    const claimed = available;
    if (!claimed || (expectedGeneration !== undefined && claimed.generation !== expectedGeneration)) {
      return null;
    }
    available = null;
    return claimed;
  }

  async function install(claimed: NonNullable<typeof available>): Promise<AutoUpdateStatus> {
    const { update, tier, checkHeaders, generation: installGeneration } = claimed;
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

      const installHeaders = config.getInstallHeaders
        ? await config.getInstallHeaders(tier, { checkHeaders })
        : checkHeaders;
      if (generation !== installGeneration) return { kind: 'idle' };
      let total = 0;
      let received = 0;
      await update.downloadAndInstall((event) => {
        if (event.event === 'Started' && isStartedEvent(event)) {
          total = event.data.contentLength ?? 0;
          emitForGeneration({ kind: 'downloading', version: update.version, tier, pct: 0 }, installGeneration);
        } else if (event.event === 'Progress' && isProgressEvent(event)) {
          received += event.data.chunkLength;
          const pct = total > 0 ? Math.max(0, Math.min(100, Math.round((received / total) * 100))) : 0;
          emitForGeneration({ kind: 'downloading', version: update.version, tier, pct }, installGeneration);
        }
      }, installHeaders ? { headers: installHeaders } : undefined);

      return emitForGeneration({ kind: 'ready', version: update.version, tier }, installGeneration);
    } catch (error) {
      const errorStatus: AutoUpdateStatus = { kind: 'error', message: String(error) };
      if (generation !== installGeneration) return errorStatus;
      config.onError?.(error);
      return emit(errorStatus);
    }
  }

  return { checkOnce, installAvailable };
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
  createBootProbation,
  type BootProbation,
  type BootProbationConfig,
  type BootProbationDecision,
  type BootProbationRecord,
  type BootProbationStorage,
} from './probation.js';
