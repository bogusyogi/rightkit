import { type BootProbationRecord, createBootProbation } from "@rightkit/app-update";
import { useEffect } from "react";
import { logs } from "./logs";

const STORAGE_KEY = "coderight.bootProbation";

function readRecord(): BootProbationRecord | null {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    return raw ? (JSON.parse(raw) as BootProbationRecord) : null;
  } catch {
    return null;
  }
}

/**
 * Boot probation is RightKit's `createBootProbation` (@rightkit/app-update): `begin` is recorded
 * right before the post-install relaunch, `evaluateBoot` counts unacknowledged boots at every start,
 * and `acknowledgeHealthy` clears the record once the shell's initial data is ready.
 */
export const bootProbation = createBootProbation({
  storage: {
    read: readRecord,
    write: (record) => {
      try {
        window.localStorage.setItem(STORAGE_KEY, JSON.stringify(record));
      } catch {
        // Storage unavailable: probation simply cannot count boots.
      }
    },
    clear: () => {
      try {
        window.localStorage.removeItem(STORAGE_KEY);
      } catch {
        // Nothing to clear.
      }
    },
  },
  onBootFailure: (info) => {
    logs.error("boot_failed", { surface: "updater", kind: "reinstall_required", count: info.bootAttempts });
  },
});

async function runningVersion(): Promise<string> {
  const { getVersion } = await import("@tauri-apps/api/app");
  return getVersion();
}

export async function beginBootProbation(toVersion: string): Promise<void> {
  try {
    await bootProbation.begin(await runningVersion(), toVersion);
  } catch {
    // Probation bookkeeping must never block the relaunch.
  }
}

let evaluated: Promise<unknown> | null = null;

/** Evaluates boot at mount; acknowledges once `ready` (initial data loaded, first paint possible). */
export function useBootProbation(ready: boolean): void {
  useEffect(() => {
    evaluated ??= runningVersion()
      .then((version) => bootProbation.evaluateBoot(version))
      .catch(() => undefined);
  }, []);
  useEffect(() => {
    if (!ready) return;
    void (evaluated ?? Promise.resolve())
      .then(() => bootProbation.acknowledgeHealthy())
      .catch(() => undefined);
  }, [ready]);
}
