import { statSync } from "node:fs";
import path from "node:path";
import { replaceFileInPlace } from "./in-place-install.mjs";
import { resolveTargetRoot } from "./cargo-target.mjs";

/**
 * A stable staged path for a development or QA binary.
 *
 * The build broker writes Cargo output to per-generation folders (`<managed>/rightkit-build-control/ws/<hash>/g/<gen>/...`)
 * and `cargo-guard` uses a hashed `dev-targets/<project>-<hash>` folder. Those are build output only: their path is not
 * stable, so NOTHING may run from them. macOS ties privacy grants to the path an app runs from, so a dev or QA binary is
 * copied in place to `<root>/.rightkit-dev-bin/<triple-or-host>/<name>` (keeping `<name>.previous`) and run from there.
 */
export const DEV_STAGE_DIRECTORY = ".rightkit-dev-bin";

export function stagedBinaryPath({ root, name, triple = null }) {
  if (typeof name !== "string" || !name || name !== path.basename(name) || name === "." || name === "..") throw new Error("staged binary name must be a bare file name");
  if (triple !== null && !/^[A-Za-z0-9_.-]+$/.test(triple)) throw new Error("staged binary triple is not a safe path segment");
  return path.join(path.resolve(root), DEV_STAGE_DIRECTORY, triple ?? "host", name);
}

/** Copy `source` to its stable staged path in place and return that path. Never launches or keeps anything per build. */
export function stageDevelopmentBinary({ root, source, name = path.basename(source), triple = null }) {
  const from = path.resolve(root, source);
  if (!statSync(from).isFile()) throw new Error(`staged binary source is not a file: ${from}`);
  const destination = stagedBinaryPath({ root, name, triple });
  replaceFileInPlace(from, destination);
  return destination;
}

// The dev and test profiles are written to debug and bench to release (same mapping as development.mjs).
const profileDirectory = (profile) => (profile === "dev" || profile === "test" ? "debug" : profile === "bench" ? "release" : profile);

/** `{ source }` or `{ manifestPath, bin, targetTriple?, profile? }` (resolved through Cargo's own target directory, broker or not). */
export function resolveStageSource(entry, { root, resolveTarget = resolveTargetRoot, platform = process.platform } = {}) {
  if (entry.source) return path.resolve(root, entry.source);
  if (!entry.manifestPath || !entry.bin) throw new Error("development stage entry needs source, or manifestPath and bin");
  const target = resolveTarget(path.resolve(root, entry.manifestPath));
  const file = platform === "win32" ? `${entry.bin}.exe` : entry.bin;
  return path.join(target, ...(entry.targetTriple ? [entry.targetTriple] : []), profileDirectory(entry.profile ?? "dev"), file);
}

export function validateStageEntries(entries, label) {
  if (entries === undefined) return [];
  if (!Array.isArray(entries)) throw new Error(`${label} must be an array`);
  for (const [index, entry] of entries.entries()) {
    if (!entry || typeof entry !== "object" || (!entry.source && !(entry.manifestPath && entry.bin))) throw new Error(`${label}[${index}] needs source, or manifestPath and bin`);
    if (entry.name !== undefined) stagedBinaryPath({ root: ".", name: entry.name });
    if (entry.targetTriple !== undefined) stagedBinaryPath({ root: ".", name: "x", triple: entry.targetTriple });
  }
  return entries;
}

export function stageDevelopmentBinaries(entries, { root, platform = process.platform, resolveTarget } = {}) {
  return entries.map((entry) => {
    const source = resolveStageSource(entry, { root, platform, resolveTarget });
    const name = entry.name ?? path.basename(source);
    return stageDevelopmentBinary({ root, source, name, triple: entry.targetTriple ?? null });
  });
}
