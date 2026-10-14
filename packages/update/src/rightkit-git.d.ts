// Ambient types for the JavaScript drift check shipped by @rightkit/ci.
declare module "@rightkit/ci/drift.mjs" {
  export function driftCommand(
    repoPath: string,
    deps?: Record<string, unknown>,
  ): Promise<{
    ok: boolean;
    driftDetected: boolean | null;
    visibility: { allowed: boolean };
    findings: Array<{ filename: string; status: string }>;
    errors: string[];
  }>;
}
