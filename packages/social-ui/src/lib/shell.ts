import { createShell } from "@rightkit/tauri-shell";

/** Created on first use (outside a Tauri webview createShell throws, so importing this package must not call it). */
function lazy<T extends object>(make: () => T): T {
  let value: T | null = null;
  const get = (): T => (value ??= make());
  return new Proxy({} as T, {
    get: (_t, key) => {
      const v = Reflect.get(get(), key);
      return typeof v === "function" ? v.bind(get()) : v;
    },
    has: (_t, key) => key in get(),
  });
}

/** The native shell bridge (window controls, external links, menu actions, native theme). */
export const shell = lazy(() => createShell());
export const shellBridge = lazy(() => shell.bridge());
