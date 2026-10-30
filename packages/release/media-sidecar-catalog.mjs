// Find the rightkit-ffmpeg crate (its catalog and notice files) for a release.
//
// Order: `mediaSidecar.catalog` in the release config (the crate's catalog/artifacts.json; the crate directory is the
// catalog's grandparent), otherwise the crate Cargo resolves for the app's native manifest (`cargo metadata --offline`,
// so the catalog is exactly the one the app was compiled against, at the version in its Cargo.lock).
import { readFileSync } from "node:fs";
import path from "node:path";
import { runCargoSync } from "./managed-cargo.mjs";

const invalid = (code, message) => Object.assign(new Error(message), { code });

export function cargoFfmpegCrate(manifestPath, { run = runCargoSync } = {}) {
  const result = run(["metadata", "--frozen", "--format-version", "1", "--manifest-path", manifestPath], {
    cwd: path.dirname(manifestPath),
    encoding: "utf8",
    maxBuffer: 256 * 1024 * 1024,
    windowsHide: true,
  });
  if (result.error) throw invalid("MEDIA_SIDECAR_CRATE_NOT_FOUND", `could not run cargo metadata: ${result.error.message}`);
  if (result.status !== 0) throw invalid("MEDIA_SIDECAR_CRATE_NOT_FOUND", `cargo metadata failed: ${String(result.stderr ?? "").trim()}`);
  const metadata = JSON.parse(result.stdout);
  const crates = metadata.packages.filter((pkg) => pkg.name === "rightkit-ffmpeg");
  if (crates.length !== 1) throw invalid("MEDIA_SIDECAR_CRATE_NOT_FOUND", `the app's dependency graph has ${crates.length} rightkit-ffmpeg packages (expected exactly one)`);
  return { crateDir: path.dirname(crates[0].manifest_path), crateVersion: crates[0].version };
}

/** Returns { catalog, catalogBytes, crateDir, crateVersion }. Injectable: read, crateFor. */
export function loadSidecarCatalog({ config, root, manifestPath, read = readFileSync, crateFor = cargoFfmpegCrate }) {
  let crateDir;
  let crateVersion;
  let catalogPath;
  if (config.catalog) {
    catalogPath = path.resolve(root, config.catalog);
    crateDir = path.dirname(path.dirname(catalogPath));
  } else {
    if (!manifestPath) throw invalid("MEDIA_SIDECAR_CRATE_NOT_FOUND", "no mediaSidecar.catalog and no native Cargo manifest to resolve rightkit-ffmpeg from");
    ({ crateDir, crateVersion } = crateFor(manifestPath));
    catalogPath = path.join(crateDir, "catalog", "artifacts.json");
  }
  let catalogBytes;
  try { catalogBytes = read(catalogPath); } catch (error) { throw invalid("MEDIA_SIDECAR_CATALOG_INVALID", `cannot read ${catalogPath}: ${error.message}`); }
  let catalog;
  try { catalog = JSON.parse(String(catalogBytes)); } catch { throw invalid("MEDIA_SIDECAR_CATALOG_INVALID", `${catalogPath} is not JSON`); }
  return { catalog, catalogBytes, crateDir, crateVersion };
}
