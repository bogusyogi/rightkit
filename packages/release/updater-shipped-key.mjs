// rightkit#1443 (hrmac, 2026-11-02): the pre-gate R2 build verified the updater signature against the public key the app SHIPS (its
// tauri.conf plugins.updater.pubkey, the key every installed copy checks an update with); the gate only checked a requested key id.
// After the updater is signed, its `.sig` must verify (minisign, github-updater-release.mjs assertUpdaterSignature) over the archive's
// bytes with the shipped key, or the build stops: an update signed by any other key would be refused by every installed app.
import fs from "node:fs";
import path from "node:path";
import { assertUpdaterSignature } from "./github-updater-release.mjs";

/** The app's shipped updater public key: plugins.updater.pubkey of the first tauri.conf.json found (src-tauri/, then the app root). */
export function shippedUpdaterPublicKey(appRoot, { readFile = (f) => fs.readFileSync(f, "utf8"), exists = fs.existsSync } = {}) {
  for (const rel of [["src-tauri", "tauri.conf.json"], ["tauri.conf.json"]]) {
    const file = path.join(appRoot, ...rel);
    if (!exists(file)) continue;
    const key = JSON.parse(readFile(file))?.plugins?.updater?.pubkey;
    if (typeof key === "string" && key.trim()) return { key: key.trim(), file };
    return { key: null, file };
  }
  return { key: null, file: null };
}

/** Throws unless `<file>.sig` verifies over `file`'s bytes with the app's shipped updater key. */
export function assertUpdaterMatchesShippedKey({ appRoot, file, readFile = (f) => fs.readFileSync(f), readText = (f) => fs.readFileSync(f, "utf8"), exists = fs.existsSync }) {
  const shipped = shippedUpdaterPublicKey(appRoot, { readFile: readText, exists });
  if (!shipped.key) throw new Error(`the app ships no updater public key (plugins.updater.pubkey in ${shipped.file ?? "src-tauri/tauri.conf.json"}), so an updater artifact cannot be verified; nothing is shipped`);
  assertUpdaterSignature(readFile(file), readText(`${file}.sig`), shipped.key, `updater ${path.basename(file)} against the shipped key in ${shipped.file}`);
}
