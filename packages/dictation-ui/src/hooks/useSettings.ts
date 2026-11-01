// useSettings - owns the settings blob, optimistic patching, and load state.
//
// Behavior preserved from the original SettingsPage: load via get_settings,
// optimistic local update on patch then reconcile with the server's returned
// blob, clear error on each patch attempt. `patch` is stable (useCallback);
// `get` is data-derived (recomputed when data changes) and reads a field with
// a typed fallback.
//
// Concurrent mounts and patches share one coordinator so stale loads and
// out-of-order patch completions cannot overwrite newer settings snapshots.

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "../lib/tauri";
import type { SettingsBlob, PatchValue } from "../types/settings";
import {
  applyOptimisticSettingsPatch,
  createSettingsCoordinator,
  publishSettingsUpdate,
  rollbackOptimisticSettingsPatch,
  settingsSnapshotEqual,
  subscribeToSettingsUpdates,
} from "./settingsSync";

export interface UseSettings {
  data: SettingsBlob;
  loaded: boolean;
  error: string;
  setError: (message: string) => void;
  patch: (key: keyof SettingsBlob, value: PatchValue) => Promise<void>;
  get: <T extends PatchValue>(key: keyof SettingsBlob, fallback: T) => T;
}

const settingsCoordinator = createSettingsCoordinator<SettingsBlob>();

export function useSettings(): UseSettings {
  const [data, setData] = useState<SettingsBlob>({});
  const dataRef = useRef<SettingsBlob>({});
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string>("");

  const replaceData = useCallback((settings: SettingsBlob) => {
    // Reference-equal gate: `hr-settings-updated` fans every patch out to all
    // ~13 useSettings consumers regardless of which key changed. When the
    // published snapshot's values match this consumer's current data (e.g.
    // the echo of a patch this consumer already applied optimistically, or a
    // blob carrying keys this mount already holds), keep the existing object
    // so React's same-reference bailout skips the re-render.
    if (settingsSnapshotEqual(dataRef.current, settings)) return;
    dataRef.current = settings;
    setData(settings);
  }, []);

  useEffect(() => {
    let cancelled = false;
    const unsubscribe = subscribeToSettingsUpdates(window, (settings: SettingsBlob) => {
      settingsCoordinator.publishUpdate(settings, replaceData);
    });
    settingsCoordinator
      .loadCurrent(() => invoke<SettingsBlob>("get_settings"))
      .then((blob) => {
        if (cancelled) return;
        replaceData(blob);
        setLoaded(true);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(String(e));
        setLoaded(true);
      });
    return () => {
      cancelled = true;
      unsubscribe();
    };
  }, [replaceData]);

  const patch = useCallback(async (key: keyof SettingsBlob, value: PatchValue) => {
    setError("");
    const optimistic = applyOptimisticSettingsPatch(dataRef.current, key, value);
    replaceData(optimistic.settings);
    const outcome = await settingsCoordinator.runPatch(
      () =>
        invoke<SettingsBlob>("apply_settings_patch", {
          patch: { [key]: value },
        }),
      (settings) => {
        replaceData(settings);
        publishSettingsUpdate(window, settings);
      },
      () => invoke<SettingsBlob>("get_settings"),
    );
    if (outcome.kind === "failed") {
      setError(String(outcome.error));
      if (!outcome.reconciled) {
        if (outcome.fallback !== null) {
          replaceData(outcome.fallback);
          publishSettingsUpdate(window, outcome.fallback);
        } else {
          replaceData(rollbackOptimisticSettingsPatch(dataRef.current, optimistic.rollback));
        }
      }
    }
  }, [replaceData]);

  const get = useCallback(
    <T extends PatchValue>(key: keyof SettingsBlob, fallback: T): T =>
      (data[key] != null ? (data[key] as T) : fallback),
    [data],
  );

  return { data, loaded, error, setError, patch, get };
}
