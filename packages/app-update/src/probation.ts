/** Suffix of the one rollback copy kept beside the fixed install path. */
export const PREVIOUS_SUFFIX = '.previous';

/**
 * The rollback location for an install: `<installPath>.previous`, beside the fixed install path. Every app
 * updates in place at ONE fixed path under one identity (macOS and Windows tie privacy grants to the path and
 * signature), so the copy the update replaces is renamed to this path, never to a versioned or temporary
 * folder, and nothing is ever executed from it. Rolling back is renaming it back over the install path.
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
  /** The fixed install path this update replaced in place (when the host passed it to `begin`). */
  installPath?: string;
  /** `<installPath>.previous`: the rollback copy, kept beside the fixed path. Never run from. */
  previousPath?: string;
}

/** Host-supplied persistence (file, plugin-store, localStorage). The package never touches disk. */
export interface BootProbationStorage {
  read(): Promise<BootProbationRecord | null> | BootProbationRecord | null;
  write(record: BootProbationRecord): Promise<void> | void;
  clear(): Promise<void> | void;
}

export type BootProbationDecision =
  | { kind: 'none' }
  /** The installed version is on probation; acknowledge once the app is healthy. */
  | { kind: 'probation'; toVersion: string; bootAttempts: number }
  /** The new version never acknowledged within the allowed boots; restore `restoreVersion`. */
  | {
      kind: 'rollback';
      toVersion: string;
      restoreVersion: string;
      bootAttempts: number;
      /** Rename `restorePath` back over `installPath`, then relaunch from `installPath`; absent when `begin` got no install path. */
      installPath?: string;
      restorePath?: string;
    }
  /** A record existed for a different version (already rolled back or superseded); it was cleared. */
  | { kind: 'stale' };

export interface BootProbationConfig {
  storage: BootProbationStorage;
  /** Unacknowledged boots tolerated before rollback is requested. Default 2. */
  maxBootAttempts?: number;
  /** Called once when a rollback is decided. The host performs the actual restore. */
  onRollback?: (info: { fromVersion: string; toVersion: string; installPath?: string; previousPath?: string }) => Promise<void> | void;
}

export interface BootProbation {
  /**
   * Record an install that is about to take effect. Call right before relaunch. Pass the fixed `installPath` the
   * update replaced in place so a rollback restores `<installPath>.previous` over it (see `previousInstallPath`).
   */
  begin(fromVersion: string, toVersion: string, options?: { installPath?: string }): Promise<void>;
  /** Call once at every app start with the running version. */
  evaluateBoot(currentVersion: string): Promise<BootProbationDecision>;
  /** Call when the new version has proven healthy. Clears the probation record. */
  acknowledgeHealthy(): Promise<void>;
}

/**
 * Boot-probation state machine for runtime updates. Pure policy: it counts boots
 * and decides; the host owns persistence and the actual restore. Opt-in; the
 * update client behaves identically without it.
 */
export function createBootProbation(config: BootProbationConfig): BootProbation {
  const maxBootAttempts = config.maxBootAttempts ?? 2;
  if (!Number.isInteger(maxBootAttempts) || maxBootAttempts < 1) {
    throw new Error('boot probation: maxBootAttempts must be a positive integer');
  }
  return {
    async begin(fromVersion, toVersion, options) {
      const installPath = options?.installPath;
      const rollback = installPath === undefined ? {} : { installPath, previousPath: previousInstallPath(installPath) };
      await config.storage.write({ fromVersion, toVersion, bootAttempts: 0, ...rollback });
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
        await config.onRollback?.({
          fromVersion: record.fromVersion,
          toVersion: record.toVersion,
          ...(record.installPath ? { installPath: record.installPath, previousPath: record.previousPath } : {}),
        });
        return {
          kind: 'rollback',
          toVersion: record.toVersion,
          restoreVersion: record.fromVersion,
          bootAttempts,
          ...(record.installPath ? { installPath: record.installPath, restorePath: record.previousPath } : {}),
        };
      }
      await config.storage.write({ ...record, bootAttempts });
      return { kind: 'probation', toVersion: record.toVersion, bootAttempts };
    },
    async acknowledgeHealthy() {
      await config.storage.clear();
    },
  };
}
