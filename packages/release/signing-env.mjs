import { existsSync } from "node:fs";
import { delimiter, dirname, join } from "node:path";

// DefaultAzureCredential's AzureCliCredential resolves `az` through the child
// PATH. Signing children must inherit an augmented PATH or signing fails with
// a bare "unexpected internal error" whenever `az` is installed outside the
// ambient PATH.
export function signingEnv(environment, env) {
  const azDir = dirnameOfFirstExisting(azureCliCandidates(env));
  // Windows env blocks are case-insensitive; preserve the ambient key casing
  // (usually "Path") so the augmented value replaces it instead of producing
  // a shadowed duplicate the resolver never reads.
  const pathKey = Object.keys(environment).find((key) => key.toLowerCase() === "path") ?? "PATH";
  const basePath = env("PATH") || environment[pathKey] || "";
  return { ...environment, [pathKey]: [azDir, basePath].filter(Boolean).join(delimiter) };
}

export function azureCliCandidates(env) {
  return [
    join(env("LOCALAPPDATA") || "", "AzureCLI", "bin", "az.cmd"),
    join(env("ProgramFiles(x86)") || "C:\\Program Files (x86)", "Microsoft SDKs", "Azure", "CLI2", "wbin", "az.cmd"),
    join(env("ProgramFiles") || "C:\\Program Files", "Microsoft SDKs", "Azure", "CLI2", "wbin", "az.cmd"),
  ];
}

function dirnameOfFirstExisting(candidates) {
  const found = candidates.find((candidate) => candidate && existsSync(candidate));
  return found ? dirname(found) : null;
}
