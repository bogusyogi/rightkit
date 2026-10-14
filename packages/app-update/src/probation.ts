export interface BootProbationRecord {
  fromVersion: string;
  toVersion: string;
  /** Boots of `toVersion` seen without a healthy acknowledgement. */
  bootAttempts: number;
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
  | { kind: 'rollback'; toVersion: string; restoreVersion: string; bootAttempts: number }
  /** A record existed for a different version (already rolled back or superseded); it was cleared. */
  | { kind: 'stale' };

export interface BootProbationConfig {
  storage: BootProbationStorage;
  /** Unacknowledged boots tolerated before rollback is requested. Default 2. */
  maxBootAttempts?: number;
  /** Called once when a rollback is decided. The host performs the actual restore. */
  onRollback?: (info: { fromVersion: string; toVersion: string }) => Promise<void> | void;
}

export interface BootProbation {
  /** Record an install that is about to take effect. Call right before relaunch. */
  begin(fromVersion: string, toVersion: string): Promise<void>;
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
    async begin(fromVersion, toVersion) {
      await config.storage.write({ fromVersion, toVersion, bootAttempts: 0 });
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
        await config.onRollback?.({ fromVersion: record.fromVersion, toVersion: record.toVersion });
        return {
          kind: 'rollback',
          toVersion: record.toVersion,
          restoreVersion: record.fromVersion,
          bootAttempts,
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
