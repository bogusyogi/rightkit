import { loadReleaseConfigSync } from "./release-config.mjs";

/** Read and validate an M24 JSON release config with the standard admission rules. */
export function inspectReleaseConfig(file) {
  return loadReleaseConfigSync(file);
}
