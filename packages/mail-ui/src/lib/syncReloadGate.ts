export type SyncReloadGate = {
  pendingAccountIds: Set<string>;
  pendingWhileHidden: boolean;
};

export function createSyncReloadGate(): SyncReloadGate {
  return {
    pendingAccountIds: new Set<string>(),
    pendingWhileHidden: false,
  };
}

export function noteSyncAccount(
  gate: SyncReloadGate,
  accountId: string,
): void {
  if (accountId) gate.pendingAccountIds.add(accountId);
}

export function shouldReplayHiddenSync(
  gate: SyncReloadGate,
  visibilityState: DocumentVisibilityState,
): boolean {
  return (
    visibilityState !== "hidden" &&
    (gate.pendingWhileHidden || gate.pendingAccountIds.size > 0)
  );
}

export function consumeSyncReload(
  gate: SyncReloadGate,
  visibilityState: DocumentVisibilityState,
  appUnlocked: boolean,
): boolean {
  if (!appUnlocked) return false;
  if (visibilityState === "hidden") {
    gate.pendingWhileHidden =
      gate.pendingWhileHidden || gate.pendingAccountIds.size > 0;
    return false;
  }
  if (!gate.pendingWhileHidden && gate.pendingAccountIds.size === 0) {
    return false;
  }
  gate.pendingWhileHidden = false;
  gate.pendingAccountIds.clear();
  return true;
}
