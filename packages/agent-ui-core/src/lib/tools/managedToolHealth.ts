/**
 * Renderer projection of a managed executable's installation/version probe.
 * The probe is injected by Tauri; this module never shells out or guesses that
 * an executable is available.
 */

export interface ManagedToolHealth {
  toolId: string;
  installed: boolean;
  version: string | null;
  output: string;
  executable?: string | null;
  checkedAt?: string;
}

export interface ManagedToolProbeResult {
  installed: boolean;
  version?: string | null;
  output?: string;
  executable?: string | null;
}

export type ManagedToolProbe = () => Promise<ManagedToolProbeResult>;

export function projectManagedToolHealth(
  toolId: string,
  result: ManagedToolProbeResult,
  checkedAt = new Date().toISOString(),
): ManagedToolHealth {
  return {
    toolId,
    installed: result.installed,
    version: result.version ?? null,
    output: result.output ?? "",
    executable: result.executable ?? null,
    checkedAt,
  };
}

export function managedToolStatusLabel(health: ManagedToolHealth | null): string {
  if (!health) return "Unknown";
  return health.installed ? "Installed" : "Not installed";
}

export function managedToolErrorMessage(error: unknown): string {
  return error instanceof Error && error.message.trim()
    ? error.message
    : "Could not check the managed tool";
}
