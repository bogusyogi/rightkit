// Loads and validates a repo's .rightax.json (DESIGN §4) against
// manifest.schema.json. Every command that needs the per-repo contract goes
// through this loader so an invalid manifest fails the same way everywhere.

import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { validateAgainstSchema } from "./lib/json-schema-lite.mjs";

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
  if (!valid) return { manifest: null, errors, path };
  return { manifest: parsed, errors: [], path };
}
