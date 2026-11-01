export type ManagedCliUpdateState = "not-installed" | "current" | "update-available" | "unavailable";

export interface ManagedCliProvider {
  providerId: string;
  displayName: string;
  installed: boolean;
  binaryVersion: string | null;
}

export interface ManagedCliUpdate {
  providerId: string;
  displayName: string;
  installedVersion: string | null;
  latestVersion: string | null;
  state: ManagedCliUpdateState;
  canInstall: false;
  reason: string;
}

/** Compare numeric dotted version parts without treating an unknown version as newer. */
export function compareCliVersions(left: string, right: string): number | null {
  const parse = (value: string) => {
    const parts = value.replace(/^v/i, "").split(/[.-]/);
    if (parts.length === 0 || parts.some((part) => !/^\d+$/.test(part))) return null;
    return parts.map(Number);
  };
  const a = parse(left);
  const b = parse(right);
  if (!a || !b) return null;
  const length = Math.max(a.length, b.length);
  for (let index = 0; index < length; index += 1) {
    const difference = (a[index] ?? 0) - (b[index] ?? 0);
    if (difference !== 0) return difference > 0 ? 1 : -1;
  }
  return 0;
}

/**
 * Project the daemon's managed-CLI inventory without guessing a latest
 * version. `latestVersion` is intentionally an optional future connector
 * input: today's `/cli-providers` contract reports only the installed binary.
 */
export function projectCliUpdate(
  provider: ManagedCliProvider,
  latestVersion?: string | null,
): ManagedCliUpdate {
  if (!provider.installed) {
    return {
      providerId: provider.providerId,
      displayName: provider.displayName,
      installedVersion: provider.binaryVersion,
      latestVersion: latestVersion ?? null,
      state: "not-installed",
      canInstall: false,
      reason: "This CLI is not installed.",
    };
  }
  if (!latestVersion || !provider.binaryVersion) {
    return {
      providerId: provider.providerId,
      displayName: provider.displayName,
      installedVersion: provider.binaryVersion,
      latestVersion: latestVersion ?? null,
      state: "unavailable",
      canInstall: false,
      reason: "The daemon does not currently expose a latest-version check for this CLI.",
    };
  }

  const comparison = compareCliVersions(latestVersion, provider.binaryVersion);
  if (comparison === null) {
    return {
      providerId: provider.providerId,
      displayName: provider.displayName,
      installedVersion: provider.binaryVersion,
      latestVersion,
      state: "unavailable",
      canInstall: false,
      reason: "The reported CLI version is not a comparable numeric version.",
    };
  }
  const updateAvailable = comparison > 0;
  return {
    providerId: provider.providerId,
    displayName: provider.displayName,
    installedVersion: provider.binaryVersion,
    latestVersion,
    state: updateAvailable ? "update-available" : "current",
    canInstall: false,
    reason: updateAvailable
      ? "A newer version was reported; installation is not exposed by the daemon."
      : "Installed version matches the reported latest version.",
  };
}

export function projectCliUpdates(
  providers: ManagedCliProvider[],
  latestVersions: Record<string, string | null | undefined> = {},
): ManagedCliUpdate[] {
  return providers.map((provider) => projectCliUpdate(provider, latestVersions[provider.providerId]));
}
