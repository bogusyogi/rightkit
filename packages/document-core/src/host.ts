// Host identity for every document package. One store, read at use time (never at module load), so
// a host's `configureDocumentHost()` call at startup reaches every module that was already imported.
// Before 0.2.0 each of the five document packages kept its own `appName` store, and the names the
// packages share with the host's native side (window and Tauri event names, the binary stream
// scheme, the path header, QA globals, storage keys) were fixed to one app's prefix.

export interface DocumentHostConfig {
  /** Product name in user-facing copy (legal gate, settings, errors). Default `"App"`. */
  appName?: string;
  /**
   * The host's native identifier. Every name the document packages share with the host's native
   * side or with each other is derived from it:
   *
   * - window and Tauri event names: `<id>:<event>` (`documentEvent`);
   * - custom URL schemes: `<id>-<suffix>`, e.g. `<id>-stream`, `<id>-site`, `<id>-preview` (`hostScheme`);
   * - the raw-body path header: `x-<id>-path-hex` (`hostHeader`);
   * - storage keys: `<id>:<key>` (`hostStorageKey`);
   * - QA globals on `window`: `__<camelCaseId><Name>` (`hostQaGlobal`).
   *
   * Lower case letters, digits and single hyphens, starting with a letter. Default `"rk-document"`.
   * A host whose native side already uses names with its own prefix passes that prefix here.
   */
  id?: string;
}

const ID_PATTERN = /^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/;

let appName = "App";
let id = "rk-document";

/** Set the host identity once at startup, before the first render. Absent fields keep their value. */
export function configureDocumentHost(config: DocumentHostConfig): void {
  if (config.appName !== undefined) {
    if (!config.appName.trim()) throw new Error("configureDocumentHost: appName must not be empty");
    appName = config.appName;
  }
  if (config.id !== undefined) {
    if (!ID_PATTERN.test(config.id)) throw new Error(`configureDocumentHost: invalid id ${JSON.stringify(config.id)}`);
    id = config.id;
  }
}

/** Product name for user-facing copy. Read it where the copy is built, not at module load. */
export function hostAppName(): string {
  return appName;
}

/** Same as `configureDocumentHost({ appName: name })`. */
export function setHostAppName(name: string): void {
  configureDocumentHost({ appName: name });
}

/** The host's native identifier (see `DocumentHostConfig.id`). */
export function hostId(): string {
  return id;
}

/** Name of a window or Tauri event shared by the document packages and the host: `<id>:<name>`. */
export function documentEvent(name: string): string {
  return `${id}:${name}`;
}

/** Name of a host custom URL scheme: `<id>-<suffix>`. */
export function hostScheme(suffix: string): string {
  return `${id}-${suffix}`;
}

/** Name of a host request header: `x-<id>-<suffix>`. */
export function hostHeader(suffix: string): string {
  return `x-${id}-${suffix}`;
}

/** Key in web storage owned by the document packages: `<id>:<key>`. */
export function hostStorageKey(key: string): string {
  return `${id}:${key}`;
}

/** Name of a QA global on `window`: `__<camelCaseId><Name>`. */
export function hostQaGlobal(name: string): string {
  const camel = id.replace(/-([a-z0-9])/g, (_, c: string) => c.toUpperCase());
  return `__${camel}${name}`;
}
