// Host configuration for the library list and reader: the product name in copy and the
// browser-storage keys the list keeps. Call `configureLibraryHost` once before rendering.

/** Browser-storage values the list keeps (per-viewer conveniences only). */
export type LibraryStorageKey = "skipDeleteConfirm" | "groupSelection" | "recoveryDismissed";

export interface LibraryHostConfig {
  /** Product name in user-facing copy. Default `"App"`. */
  appName?: string;
  /** Prefix for keys without an explicit entry in `storageKeys`. Default `"rk-library"`. */
  storagePrefix?: string;
  /** Exact keys, e.g. a host's existing keys for stored choices. */
  storageKeys?: Partial<Record<LibraryStorageKey, string>>;
}

let config: LibraryHostConfig = {};

export function configureLibraryHost(next: LibraryHostConfig): void {
  config = { ...config, ...next };
}

export function libraryAppName(): string {
  return config.appName ?? "App";
}

export function libraryStorageKey(key: LibraryStorageKey): string {
  return config.storageKeys?.[key] ?? `${config.storagePrefix ?? "rk-library"}.${key}`;
}

export function readStored(key: LibraryStorageKey): string | null {
  try {
    return localStorage.getItem(libraryStorageKey(key));
  } catch {
    return null;
  }
}

export function writeStored(key: LibraryStorageKey, value: string): void {
  try {
    localStorage.setItem(libraryStorageKey(key), value);
  } catch {
    // Best effort: an unavailable store only means the choice is asked again.
  }
}
