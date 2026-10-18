/**
 * @deprecated Retained `.previous` app copies are removed (D01). Kept only so hosts can recognize
 * and clean a stale copy an older RightKit left; new code never creates or restores one.
 */
export const PREVIOUS_SUFFIX = '.previous';

/**
 * @deprecated See `PREVIOUS_SUFFIX`. Returns `<installPath>.previous` for stale-copy cleanup only.
 */
export function previousInstallPath(installPath: string): string {
  if (typeof installPath !== 'string' || installPath.trim() === '') {
    throw new Error('rollback: installPath must be a non-empty path');
  }
  const trimmed = installPath.replace(/[\\/]+$/, '');
  if (trimmed === '') throw new Error('rollback: installPath must not be a filesystem root');
  if (trimmed.endsWith(PREVIOUS_SUFFIX)) throw new Error('rollback: installPath must be the fixed install path, not a .previous copy');
  return `${trimmed}${PREVIOUS_SUFFIX}`;
}

export interface BootProbationRecord {
  fromVersion: string;
  toVersion: string;
  /** Boots of `toVersion` seen without a healthy acknowledgement. */
  bootAttempts: number;
  /** The fixed install path this update replaced in place (sentinel context only). */
  installPath?: string;
  /** @deprecated Legacy records may carry it; it is ignored and never restored from. */
  previousPath?: string;
}

/** Host-supplied persistence (file, plugin-store, localStorage). The package never touches disk. */
export interface BootProbationStorage {
  read(): Promise<BootProbationRecord | null> | BootProbationRecord | null;
  write(record: BootProbationRecord): Promise<void> | void;
  clear(): Promise<void> | void;
}

/** Typed boot-failure sentinel info: no binary payload is retained or restored. */
export interface BootFailureInfo {
  fromVersion: string;
  toVersion: string;
  installPath?: string;
  bootAttempts: number;
  guidance: 'reinstall';
}

export type BootProbationDecision =
  | { kind: 'none' }
  /** The installed version is on probation; acknowledge once the app is healthy. */
  | { kind: 'probation'; toVersion: string; bootAttempts: number }
  /**
   * The new version never acknowledged within the allowed boots. No `.previous` copy exists to
   * restore: the host surfaces reinstall guidance for `failedVersion` (last good: `fromVersion`).
   */
  | {
      kind: 'reinstall_required';
      failedVersion: string;
      fromVersion: string;
      bootAttempts: number;
      installPath?: string;
      guidance: 'reinstall';
    }
  /** A record existed for a different version (superseded); it was cleared. */
  | { kind: 'stale' };

export interface BootProbationConfig {
  storage: BootProbationStorage;
  /** Unacknowledged boots tolerated before boot failure is declared. Default 2. */
  maxBootAttempts?: number;
  /** Called once when boot failure is declared. The host shows reinstall guidance; nothing is restored. */
  onBootFailure?: (info: BootFailureInfo) => Promise<void> | void;
  /** @deprecated Alias of `onBootFailure` (no rollback/restore is performed). */
  onRollback?: (info: BootFailureInfo) => Promise<void> | void;
}

export interface BootProbation {
  /**
   * Record an install that is about to take effect. Call right before relaunch. Only the typed
   * sentinel (versions, attempts, optional fixed `installPath`) is persisted, never a payload path.
   */
  begin(fromVersion: string, toVersion: string, options?: { installPath?: string }): Promise<void>;
  /** Call once at every app start with the running version. */
  evaluateBoot(currentVersion: string): Promise<BootProbationDecision>;
  /** Call when the new version has proven healthy. Clears the probation record. */
  acknowledgeHealthy(): Promise<void>;
}

/**
 * Boot-probation state machine for runtime updates. Pure policy: it counts boots and decides;
 * the host owns persistence and reinstall guidance. Opt-in; the update client behaves
 * identically without it.
 */
export function createBootProbation(config: BootProbationConfig): BootProbation {
  const maxBootAttempts = config.maxBootAttempts ?? 2;
  if (!Number.isInteger(maxBootAttempts) || maxBootAttempts < 1) {
    throw new Error('boot probation: maxBootAttempts must be a positive integer');
  }
  const sentinel = (record: BootProbationRecord): BootProbationRecord => ({
    fromVersion: record.fromVersion,
    toVersion: record.toVersion,
    bootAttempts: record.bootAttempts,
    ...(record.installPath ? { installPath: record.installPath } : {}),
  });
  return {
    async begin(fromVersion, toVersion, options) {
      // Same fixed-path validation the old rollback path enforced; the path is sentinel context only.
      if (options?.installPath !== undefined) previousInstallPath(options.installPath);
      await config.storage.write(sentinel({
        fromVersion,
        toVersion,
        bootAttempts: 0,
        ...(options?.installPath === undefined ? {} : { installPath: options.installPath }),
      }));
    },
    async evaluateBoot(currentVersion) {
      const record = await config.storage.read();
      if (!record) return { kind: 'none' };
      if (record.toVersion !== currentVersion) {
        await config.storage.clear();
        return { kind: 'stale' };
      }
      const bootAttempts = record.bootAttempts + 1;
      if (bootAttempts > maxBootAttempts) {
        await config.storage.clear();
        const info: BootFailureInfo = {
          fromVersion: record.fromVersion,
          toVersion: record.toVersion,
          ...(record.installPath ? { installPath: record.installPath } : {}),
          bootAttempts,
          guidance: 'reinstall',
        };
        await config.onBootFailure?.(info);
        await config.onRollback?.(info);
        return {
          kind: 'reinstall_required',
          failedVersion: record.toVersion,
          fromVersion: record.fromVersion,
          bootAttempts,
          ...(record.installPath ? { installPath: record.installPath } : {}),
          guidance: 'reinstall',
        };
      }
      await config.storage.write(sentinel({ ...record, bootAttempts }));
      return { kind: 'probation', toVersion: record.toVersion, bootAttempts };
    },
    async acknowledgeHealthy() {
      await config.storage.clear();
    },
  };
}
