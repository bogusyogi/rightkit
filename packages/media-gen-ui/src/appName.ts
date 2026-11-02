// Product name shown in user-facing copy. Hosts call setHostAppName() once at startup.
let current = "App";

export function setHostAppName(name: string): void {
  current = name;
}

export function hostAppName(): string {
  return current;
}

// The host app's version (its package.json). Hosts call setHostAppVersion() once at startup.
let currentVersion = "0.0.0";

export function setHostAppVersion(version: string): void {
  currentVersion = version;
}

export function hostAppVersion(): string {
  return currentVersion;
}
