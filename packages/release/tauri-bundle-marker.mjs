// Tauri stamps the bundle kind into the compiled binary as an ASCII marker, and
// its NSIS bundler rewrites that marker WHILE bundling. Doing that to an EXE we
// already Authenticode-signed silently invalidates the signature — the installer
// then ships a binary Windows reports as unsigned/corrupt, which is exactly the
// failure this module exists to make impossible.
//
// The contract: patch the marker to its final value BEFORE signing, so Tauri's
// own rewrite finds nothing left to change and the signed bytes survive intact.
// Fail closed on anything ambiguous — a missing marker, more than one marker, or
// a byte changing outside the marker all mean the assumption no longer holds.

import { createHash } from "node:crypto";
import { readFileSync, writeFileSync, realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const MARKER_PREFIX = "__TAURI_BUNDLE_TYPE_VAR_";

export const BUNDLE_MARKERS = Object.freeze({
  unknown: "UNK",
  nsis: "NSS",
  msi: "MSI",
  app: "APP",
  dmg: "DMG",
  deb: "DEB",
  rpm: "RPM",
  appimage: "AIM",
});

export function bundleMarkerBuffer(code) {
  if (typeof code !== "string" || code.length !== 3) {
    throw new Error(`Tauri bundle marker code must be exactly 3 ASCII characters, received ${JSON.stringify(code)}`);
  }
  return Buffer.from(`${MARKER_PREFIX}${code}`, "ascii");
}

/**
 * Rewrite the single unknown bundle marker to the requested bundle kind.
 *
 * Idempotent by design: a binary already carrying the target marker (and no
 * unknown marker) is reported as `alreadyPatched` rather than failing, so a
 * retried build does not have to distinguish "never patched" from "resumed".
 */
export function patchTauriBundleType(file, { bundle = "nsis", read = readFileSync, write = writeFileSync } = {}) {
  const targetCode = BUNDLE_MARKERS[bundle];
  if (!targetCode) {
    throw new Error(`unsupported Tauri bundle kind: ${bundle} (expected one of ${Object.keys(BUNDLE_MARKERS).join(", ")})`);
  }
  const resolved = path.resolve(file);
  const unknownMarker = bundleMarkerBuffer(BUNDLE_MARKERS.unknown);
  const targetMarker = bundleMarkerBuffer(targetCode);

  const before = read(resolved);
  const beforeSha256 = sha256(before);
  const unknownOffsets = findAll(before, unknownMarker);
  const targetOffsets = findAll(before, targetMarker);

  if (unknownOffsets.length === 0) {
    if (targetOffsets.length === 1) {
      return {
        file: resolved,
        bundle,
        offset: targetOffsets[0],
        changedBytes: 0,
        alreadyPatched: true,
        size: before.length,
        beforeSha256,
        afterSha256: beforeSha256,
      };
    }
    throw new Error(
      `no Tauri unknown bundle marker in ${resolved} and ${targetOffsets.length} ${targetCode} markers; refusing to guess the bundle identity`,
    );
  }
  if (unknownOffsets.length !== 1) {
    throw new Error(`expected exactly one Tauri unknown bundle marker in ${resolved}; found ${unknownOffsets.length}`);
  }

  const offset = unknownOffsets[0];
  const after = Buffer.from(before);
  targetMarker.copy(after, offset);

  const expectedChanges = markerChangeOffsets(unknownMarker, targetMarker, offset);
  const actualChanges = [];
  for (let index = 0; index < before.length; index += 1) {
    if (before[index] !== after[index]) actualChanges.push(index);
  }
  if (actualChanges.join(",") !== expectedChanges.join(",")) {
    throw new Error(`unexpected Tauri bundle marker mutation in ${resolved}`);
  }

  write(resolved, after);
  const written = read(resolved);
  if (!written.equals(after) || findAll(written, unknownMarker).length !== 0) {
    throw new Error(`failed to verify Tauri ${targetCode} bundle marker in ${resolved}`);
  }

  return {
    file: resolved,
    bundle,
    offset,
    changedBytes: actualChanges.length,
    alreadyPatched: false,
    size: written.length,
    beforeSha256,
    afterSha256: sha256(written),
  };
}

/** Back-compat alias for the original app-local helper name. */
export function patchTauriBundleTypeForNsis(file, options = {}) {
  return patchTauriBundleType(file, { ...options, bundle: "nsis" });
}

/**
 * Report which markers a binary currently carries, without writing. Used by the
 * post-package audit to prove the packaged binary is the one that was signed.
 */
export function readTauriBundleMarkers(file, { read = readFileSync } = {}) {
  const resolved = path.resolve(file);
  const bytes = read(resolved);
  const markers = {};
  for (const [bundle, code] of Object.entries(BUNDLE_MARKERS)) {
    const offsets = findAll(bytes, bundleMarkerBuffer(code));
    if (offsets.length) markers[bundle] = offsets;
  }
  return { file: resolved, size: bytes.length, sha256: sha256(bytes), markers };
}

function findAll(haystack, needle) {
  const offsets = [];
  for (let offset = haystack.indexOf(needle); offset !== -1; offset = haystack.indexOf(needle, offset + 1)) {
    offsets.push(offset);
  }
  return offsets;
}

function markerChangeOffsets(from, to, offset) {
  const changes = [];
  for (let index = 0; index < from.length; index += 1) {
    if (from[index] !== to[index]) changes.push(offset + index);
  }
  return changes;
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  const argv = process.argv.slice(2);
  const bundleIndex = argv.indexOf("--bundle");
  const bundle = bundleIndex >= 0 ? argv[bundleIndex + 1] : "nsis";
  const file = argv.filter((arg, index) => arg !== "--bundle" && index !== bundleIndex + 1)[0];
  if (!file) {
    console.error("usage: node tauri-bundle-marker.mjs [--bundle nsis] <binary>");
    process.exit(2);
  }
  try {
    console.log(JSON.stringify(patchTauriBundleType(file, { bundle })));
  } catch (error) {
    console.error(`tauri-bundle-marker: ${error.message}`);
    process.exit(1);
  }
}
