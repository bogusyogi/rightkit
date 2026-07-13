export type UpdateTier = 'patch' | 'update';

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
  getHeaders?: () => Promise<UpdateHeaders | undefined> | UpdateHeaders | undefined;
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
  let available:
    | { update: RuntimeUpdate; tier: UpdateTier | null; headers?: UpdateHeaders; eligible: boolean }
    | null = null;

  function emit(status: AutoUpdateStatus): AutoUpdateStatus {
    config.onStatus?.(status);
    return status;
  }

  async function checkOnce(): Promise<AutoUpdateStatus> {
    try {
      available = null;
      emit({ kind: 'checking' });
      const headers = await config.getHeaders?.();
      const update = await config.check(headers ? { headers } : undefined);
      if (!update) {
        const proUpdateVersion = await config.getProUpdateVersion?.();
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
      if (tier !== 'patch' && tier !== 'update') {
        throw new Error('update manifest tier must be "patch" or "update"');
      }
      const eligible = await isEligible(tier, { headers });
      available = { update, tier, headers, eligible };
      const availableStatus = emit({
        kind: 'available',
        version: update.version,
        tier,
        eligible,
        reason: !eligible && tier === 'update' ? 'pro_required' : 'auto_install_off',
      });
      if (!eligible || !(await shouldAutoInstall(tier, { headers }))) return availableStatus;

      return install(update, tier, headers);
    } catch (error) {
      config.onError?.(error);
      return emit({ kind: 'error', message: String(error) });
    }
  }

  async function installAvailable(): Promise<AutoUpdateStatus> {
    if (!available) return emit({ kind: 'up_to_date' });
    if (!available.eligible) {
      return emit({
        kind: 'available',
        version: available.update.version,
        tier: available.tier,
        eligible: false,
        reason: available.tier === 'update' ? 'pro_required' : undefined,
      });
    }
    return install(available.update, available.tier, available.headers);
  }

  async function install(
    update: RuntimeUpdate,
    tier: UpdateTier | null,
    headers?: UpdateHeaders,
  ): Promise<AutoUpdateStatus> {
    try {
      const safe = await config.canInstall?.() ?? true;
      if (!safe) {
        return emit({
          kind: 'deferred',
          version: update.version,
          tier,
          reason: deferReason(),
        });
      }

      let total = 0;
      let received = 0;
      let readyStatus: AutoUpdateStatus | null = null;
      await update.downloadAndInstall((event) => {
        if (event.event === 'Started' && isStartedEvent(event)) {
          total = event.data.contentLength ?? 0;
          emit({ kind: 'downloading', version: update.version, tier, pct: 0 });
        } else if (event.event === 'Progress' && isProgressEvent(event)) {
          received += event.data.chunkLength;
          const pct = total > 0 ? Math.max(0, Math.min(100, Math.round((received / total) * 100))) : 0;
          emit({ kind: 'downloading', version: update.version, tier, pct });
        } else if (event.event === 'Finished') {
          readyStatus = emit({ kind: 'ready', version: update.version, tier });
        }
      }, headers ? { headers } : undefined);

      return readyStatus ?? emit({ kind: 'ready', version: update.version, tier });
    } catch (error) {
      config.onError?.(error);
      return emit({ kind: 'error', message: String(error) });
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
