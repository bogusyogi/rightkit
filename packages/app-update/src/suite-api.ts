// Right Suite API host helpers (contract right-suite-api.v1). Dependency-free: no Tauri, no Node built-ins.
// Constants come from the generated projection; edit contracts/right-suite-api.v1.json, never this list of hosts.
import {
  APP_ARTIFACT_HOSTS,
  RIGHT_SUITE_API_ORIGIN,
  RIGHT_SUITE_ARTIFACT_PATH_PREFIX,
  RIGHT_SUITE_PUBLIC_INSTALLER_BASE,
  RIGHT_SUITE_RELEASE_STATUS_PATH,
  RIGHT_SUITE_UPDATER_MANIFEST_PATH,
} from './suite-api.generated.js';

export * from './suite-api.generated.js';

const APP_KEY = /^[a-z][a-z0-9-]*$/u;
// One installer path segment: no separators, no dot-only names, so a caller cannot escape the app's installer tree.
const INSTALLER_SEGMENT = /^[A-Za-z0-9][A-Za-z0-9._-]*$/u;

/** Validates a Right Suite app key and returns it; throws for anything outside `^[a-z][a-z0-9-]*$`. */
export function assertAppKey(app: string): string {
  if (typeof app !== 'string' || !APP_KEY.test(app)) throw new TypeError(`invalid Right Suite app key: ${String(app)}`);
  return app;
}

/** Absolute URL of the app's release status document on the Right Suite API. */
export function releaseStatusUrl(app: string): string {
  return RIGHT_SUITE_API_ORIGIN + RIGHT_SUITE_RELEASE_STATUS_PATH.split('{app}').join(assertAppKey(app));
}

/**
 * Tauri updater endpoint template for the app. `{{target}}` is kept verbatim: the updater fills it at runtime.
 */
export function updaterEndpoint(app: string): string {
  return RIGHT_SUITE_API_ORIGIN + RIGHT_SUITE_UPDATER_MANIFEST_PATH.split('{app}').join(assertAppKey(app));
}

/**
 * Hostnames the app may download artifacts from. Per app (contract `apps.<key>.artifactHosts`); an app that the
 * contract does not list gets none, and the result is always a fresh array the caller may not widen the contract with.
 */
export function artifactHosts(app: string): string[] {
  const key = assertAppKey(app);
  const sets = APP_ARTIFACT_HOSTS as Readonly<Record<string, readonly string[]>>;
  return Object.hasOwn(sets, key) ? [...sets[key]] : [];
}

/**
 * Public installer download URL. `path` is relative to the app's installer tree and has the form
 * `<platform>/current/<file>`, for example `mac/current/ViewRight_universal.dmg`. Each segment must match
 * `[A-Za-z0-9][A-Za-z0-9._-]*`, so `..`, separators and empty segments are refused.
 */
export function publicInstallerUrl(app: string, path: string): string {
  const key = assertAppKey(app);
  if (typeof path !== 'string') throw new TypeError('installer path must be a string');
  const segments = path.split('/');
  if (segments.length < 1 || segments.some((segment) => !INSTALLER_SEGMENT.test(segment))) {
    throw new TypeError(`invalid installer path: ${path}`);
  }
  const prefix = RIGHT_SUITE_ARTIFACT_PATH_PREFIX.split('{app}').join(key);
  return `${RIGHT_SUITE_PUBLIC_INSTALLER_BASE}${prefix}/installers/${segments.join('/')}`;
}
