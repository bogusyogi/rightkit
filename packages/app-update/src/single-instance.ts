/** Keep in step with package.json; a test enforces this. */
export const UPDATES_PACKAGE_VERSION = '0.3.0';

const REGISTRY_KEY = Symbol.for('@rightkit/app-update.instances');

/**
 * Registers this copy of @rightkit/app-update in a process-wide registry and returns
 * every distinct version loaded so far. A result longer than one means duplicate
 * copies are bundled (for example via a stale `@rightkit/tauri` pin); hosts can
 * log or assert on it. Opt-in: nothing calls this implicitly.
 */
export function registerUpdatesInstance(version: string = UPDATES_PACKAGE_VERSION): string[] {
  const scope = globalThis as unknown as Record<symbol, Set<string> | undefined>;
  const registry = scope[REGISTRY_KEY] ?? (scope[REGISTRY_KEY] = new Set<string>());
  registry.add(version);
  return [...registry];
}

/** Throws when more than one version of @rightkit/app-update has registered. */
export function assertSingleUpdatesVersion(version: string = UPDATES_PACKAGE_VERSION): void {
  const versions = registerUpdatesInstance(version);
  if (versions.length > 1) {
    throw new Error(`multiple @rightkit/app-update versions loaded: ${versions.join(', ')}`);
  }
}
