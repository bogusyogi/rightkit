// A line reading of the [package] table of a Cargo.toml, for the scans that must run without a TOML parser (the license scan and
// the secret scan of the repository gate). This module has no imports on purpose: publish-cargo.mjs is in the trusted runtime
// helper closure (scripts/ci/runtime-supplement.mjs), which may only follow static relative imports of plain modules.

/**
 * `{ hasPackage, unpublished }` for the text of a Cargo.toml: whether it has a `[package]` table, and whether that table says
 * `publish = false` or `publish = []`. A crate that can never be published ships its licence to no one, like a package.json with
 * `"private": true`. An array-of-tables header (`[[bin]]`, `[[package.metadata.x]]`) starts a new table like any other, so a
 * `publish` key under it is not the package's.
 */
export function cargoManifestState(text) {
  let table = "";
  let hasPackage = false;
  let unpublished = false;
  for (const line of String(text).split(/\r?\n/)) {
    const header = /^\s*\[\[?([^\]]+)\]\]?\s*(?:#.*)?$/.exec(line);
    if (header) {
      table = header[1].trim();
      if (table === "package") hasPackage = true;
      continue;
    }
    if (table === "package" && /^\s*publish\s*=\s*(?:false\b|\[\s*\])/.test(line)) unpublished = true;
  }
  return { hasPackage, unpublished };
}
