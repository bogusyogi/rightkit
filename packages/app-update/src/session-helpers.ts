// Small policy helpers for a host that wires `createUpdateSession`: a QA status fixture, the tier eligibility
// rule, the auto-install setting read and the guarded background start. No browser or Tauri import.
import type { AutoUpdateStatus } from './index.js';
import type { UpdateTier } from './contract-vectors.js';
import type { UpdateSession } from './session.js';

/** Options of {@link qaUpdateStatus}. */
export interface QaUpdateStatusOptions {
  /**
   * Whether fixtures are allowed at all, for example `import.meta.env.DEV`. The host passes it so a
   * production build can never show a fixture. Default false.
   */
  enabled: boolean;
  /** The query string to read, for example `window.location.search`. Empty or absent reads as no fixture. */
  search: string | undefined;
  /** Query parameter that must equal `flagValue` for any fixture to apply. Default `qa`. */
  flagParam?: string;
  /** Value of `flagParam` that turns fixtures on. Default `1`. */
  flagValue?: string;
  /** Query parameter naming the fixture: `available`, `downloading`, `ready` or `error`. Default `qa-update`. */
  statusParam?: string;
  /** The version the fixtures name. Default `9.9.9`. */
  version?: string;
  /** The tier the fixtures name. Default `patch`. */
  tier?: UpdateTier;
  /** The percentage of the `downloading` fixture. Default 64. */
  pct?: number;
  /** The message of the `error` fixture. Default `The update could not be installed.` */
  errorMessage?: string;
}

/**
 * A fixed update status chosen by a query parameter, for QA screenshots of the update surfaces: the
 * `initialStatus` of `createUpdateSession`. Answers `{ kind: 'idle' }` unless `enabled` is true, the flag
 * parameter matches and the status parameter names one of `available`, `downloading`, `ready`, `error`.
 * The host passes `enabled`, the search string and, if its parameters differ, their names.
 */
export function qaUpdateStatus(options: QaUpdateStatusOptions): AutoUpdateStatus {
  const idle: AutoUpdateStatus = { kind: 'idle' };
  if (!options.enabled || !options.search) return idle;
  let params: URLSearchParams;
  try {
    params = new URLSearchParams(options.search);
  } catch {
    return idle;
  }
  if (params.get(options.flagParam ?? 'qa') !== (options.flagValue ?? '1')) return idle;
  const version = options.version ?? '9.9.9';
  const tier = options.tier ?? 'patch';
  switch (params.get(options.statusParam ?? 'qa-update')) {
    case 'available':
      return { kind: 'available', version, tier, eligible: true };
    case 'downloading':
      return { kind: 'downloading', version, tier, pct: options.pct ?? 64 };
    case 'ready':
      return { kind: 'ready', version, tier };
    case 'error':
      return { kind: 'error', message: options.errorMessage ?? 'The update could not be installed.' };
    default:
      return idle;
  }
}

/**
 * The usual `isEligible(tier)` rule of `createAutoUpdateClient`: no tier is never eligible, the `patch` tier
 * always is, and any other tier needs the licence. The host passes `hasLicense`, which says whether the
 * install holds the paid licence the feature tier requires.
 */
export async function isUpdateTierEligible(
  tier: UpdateTier | null,
  hasLicense: () => Promise<boolean> | boolean,
): Promise<boolean> {
  if (tier === null) return false;
  if (tier === 'patch') return true;
  return (await hasLicense()) === true;
}

/**
 * The `autoInstall` option of `createUpdateSession` over a settings read. The host passes `readSettings`
 * (its settings command) and `select`, which picks the auto-install flag from what it returned. Only a
 * literal `true` installs; a throw is left to the session, which reads it as false.
 */
export function autoInstallFromSettings<Settings>(
  readSettings: () => Promise<Settings> | Settings,
  select: (settings: Settings) => unknown,
): () => Promise<boolean> {
  return async () => select(await readSettings()) === true;
}

/**
 * Starts the native background update listener of `session` when `isNative()` is true, otherwise does
 * nothing. The host passes the environment check, for example a Tauri detection, so a browser preview or a
 * server render never calls the native commands.
 */
export function startBackgroundUpdateChecks(session: Pick<UpdateSession, 'startBackground'>, isNative: () => boolean): void {
  if (isNative()) session.startBackground();
}
