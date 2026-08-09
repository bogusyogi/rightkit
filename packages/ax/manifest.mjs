// Loads and validates a repo's .rightax.json (DESIGN §4) against
// manifest.schema.json. Every command that needs the per-repo contract goes
// through this loader so an invalid manifest fails the same way everywhere.

import { existsSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { validateAgainstSchema } from "./lib/json-schema-lite.mjs";

function generatedPathErrors(manifest) {
  return (manifest?.ax?.generatedPaths ?? []).flatMap((value) => typeof value !== "string" || !value || value.includes("\\") || value.startsWith("/") || /^[A-Za-z]:/.test(value) || value.split("/").some((part) => !part || part === "." || part === ".." || /[*?\[\]{}]/.test(part)) ? [`$.ax.generatedPaths: invalid exact repo-relative POSIX path ${JSON.stringify(value)}`] : []);
}
function matrixErrors(manifest) {
  const matrix = manifest?.ax?.matrix; if (!matrix) return [];
  const errors = []; const ids = new Set(); const fingerprints = new Set();
  for (const [index, adapter] of (matrix.adapters ?? []).entries()) for (const field of ["id", "model", "version", "command"]) {
    const value = adapter?.[field]; const path = `$.ax.matrix.adapters[${index}].${field}`;
    if (typeof value !== "string" || !value.trim()) errors.push(`${path}: must be a non-blank trimmed string`);
    if (field === "id" && typeof value === "string" && value.trim()) { const normalized = value.trim().toLowerCase(); if (ids.has(normalized)) errors.push(`${path}: duplicate normalized adapter id ${JSON.stringify(normalized)}`); ids.add(normalized); }
  }
  for (const adapter of matrix.adapters ?? []) {
    if (![adapter?.model, adapter?.version, adapter?.command].every((value) => typeof value === "string" && value.trim()) || !Array.isArray(adapter?.args)) continue;
    const identity = JSON.stringify({ model: adapter.model.normalize("NFKC").trim(), version: adapter.version.normalize("NFKC").trim(), command: adapter.command.normalize("NFKC").trim(), args: adapter.args });
    const fingerprint = createHash("sha256").update(identity).digest("hex");
    if (fingerprints.has(fingerprint)) errors.push("$.ax.matrix.adapters: duplicate execution identity");
    fingerprints.add(fingerprint);
  }
  return errors;
}

const HERE = dirname(fileURLToPath(import.meta.url));
const SCHEMA_PATH = join(HERE, "manifest.schema.json");

export function manifestSchema() {
  return JSON.parse(readFileSync(SCHEMA_PATH, "utf8"));
}

export function manifestPathFor(repoPath) {
  return join(repoPath, ".rightax.json");
}

export function loadManifest(repoPath) {
  const path = manifestPathFor(repoPath);
  if (!existsSync(path)) return { manifest: null, errors: [`.rightax.json not found at ${path}`], path };
  let parsed;
  try {
    parsed = JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    return { manifest: null, errors: [`.rightax.json is not valid JSON: ${String(error?.message ?? error)}`], path };
  }
  const { valid, errors } = validateAgainstSchema(parsed, manifestSchema());
  const semanticErrors = valid ? [...generatedPathErrors(parsed), ...matrixErrors(parsed)] : [];
  if (!valid || semanticErrors.length) return { manifest: null, errors: [...errors, ...semanticErrors], path };
  return { manifest: parsed, errors: [], path };
}
