// Ambient types for the JavaScript drift check shipped by @rightkit/git.
declare module "@rightkit/git/drift.mjs" {
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
