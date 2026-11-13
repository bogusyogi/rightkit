// useSettings: the dictation pages' view of the host's settings store.
//
// State, optimistic writes, rollback and cross-window reloads are `@rightkit/settings-ui`'s
// `useSettings` over the `SettingsBackend` the host supplies in `DictationClient.settings`; this hook
// only types the values as `SettingsBlob` and keeps the `get(key, fallback)` reader the pages use.

import { useCallback, useState } from "react";
import { useSettings as useSettingsStore } from "@rightkit/settings-ui/core";
import { dictationClient, hostHooks } from "../host.js";
import type { PatchValue, SettingsBlob } from "../types/settings.js";

export interface UseSettings {
  data: SettingsBlob;
  loaded: boolean;
  error: string;
  setError: (message: string) => void;
  patch: (key: keyof SettingsBlob, value: PatchValue) => Promise<void>;
  get: <T extends PatchValue>(key: keyof SettingsBlob, fallback: T) => T;
}

/** The package's own settings hook (the default of `DictationHooks.useSettings`). */
export function usePackageSettings(): UseSettings {
  const store = useSettingsStore(dictationClient().settings);
  const data = store.values as SettingsBlob;
  const { set, clearError } = store;
  const patch = useCallback((key: keyof SettingsBlob, value: PatchValue) => set(key, value), [set]);
  // Page-raised messages show in the same slot as store errors; clearing clears both.
  const [pageError, setPageError] = useState("");
  const setError = useCallback((message: string) => {
    setPageError(message);
    if (!message) clearError();
  }, [clearError]);
  const get = useCallback(
    <T extends PatchValue>(key: keyof SettingsBlob, fallback: T): T => (data[key] != null ? (data[key] as T) : fallback),
    [data],
  );
  return { data, loaded: store.loaded, error: pageError || (store.error ?? ""), setError, patch, get };
}

/** The settings the pages use: the host's `hooks.useSettings` when it supplies one, else the package's. */
export function useSettings(): UseSettings {
  return (hostHooks().useSettings ?? usePackageSettings)();
}
