import type { SettingsBackend, SettingsSnapshot } from '@rightkit/settings-ui';
import { invoke } from '@tauri-apps/api/core';
import { isTauri, loadBrowserMock } from './api.js';
import { hostAppName } from "../appName.js";

// App-level settings behind the RightKit SettingsBackend port. @rightkit/settings-ui 0.7 targets the shell's
// own settings host (`rightkit_settings_*`), which would put a second settings store in the shell process;
// the host app settings live in the engine (the rightkit-app-settings store the engine and its CLI read), so this
// file carries each call through the host app `engine_request` command (engine methods are `command` + a name):
// `rightkit.settings.get` / `rightkit.settings.patch`, same wire as before. There is no WebView-storage
// fallback: a failed call rejects and the caller surfaces it. Browser dev mode (`vite`,
// import.meta.env?.DEV) uses the dev-only mock engine; release builds outside Tauri have no store.
// The engine pushes no settings-changed events, so there is no `subscribe`.

async function call<T>(name: string, input: unknown): Promise<T> {
  if (isTauri()) {
    return invoke<T>('engine_request', { command: { method: 'command', params: { name, input } } });
  }
  if (import.meta.env?.DEV) {
    const mock = await loadBrowserMock();
    return mock.mockEngine<T>('command', { name, input });
  }
  throw new Error(`${hostAppName()} needs the desktop app.`);
}

export const settingsBackend: SettingsBackend = {
  load: () => call<SettingsSnapshot>('rightkit.settings.get', {}),
  patch: (patch, expectedRevision) =>
    call<SettingsSnapshot>('rightkit.settings.patch', { patch, expectedRevision: expectedRevision ?? null }),
};

/** One app UI flag from the settings store; `fallback` when the key is unset. A store failure rejects. */
export async function readUiSetting<T>(key: string, fallback: T): Promise<T> {
  const v = (await settingsBackend.load()).values[key];
  return v === undefined || v === null ? fallback : (v as T);
}

/** Persists one app UI flag (null clears it). A store failure rejects. */
export async function writeUiSetting(key: string, value: unknown): Promise<void> {
  await settingsBackend.patch({ [key]: value });
}
