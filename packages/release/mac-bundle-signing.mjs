// Directory entries (.app bundles) in `sign.prePackageFiles`: sign nested code inside-out first
// (no entitlements), then the bundle itself, reusing the swift-mac-app inside-out discovery.
import { existsSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { discoverNestedCode } from "./swift-mac-app.mjs";

const fail = (message) => Object.assign(new Error(message), { code: "MAC_SIGN_ENTRY_INVALID" });

export function mainExecutableName(app) {
  try {
    const plist = readFileSync(path.join(app, "Contents", "Info.plist"), "utf8");
    const match = /<key>CFBundleExecutable<\/key>\s*<string>([^<]+)<\/string>/.exec(plist);
    if (match) return match[1];
  } catch { /* binary plist or unreadable: fall back to the bundle name */ }
  return path.basename(app, ".app");
}

/**
 * Split signing entries into nested code (deepest first, signed without entitlements) and the entries
 * themselves. A directory must be a `.app` bundle; anything else is rejected with a clear error.
 * Injectable: isDirectory, discover, exists.
 */
export function expandMacSigningEntries(files, { isDirectory = (f) => statSync(f).isDirectory(), exists = existsSync, discover = discoverNestedCode, mainName = mainExecutableName } = {}) {
  const nested = [];
  for (const file of files) {
    if (!isDirectory(file)) continue;
    if (!file.endsWith(".app") || !exists(path.join(file, "Contents", "Info.plist"))) {
      throw fail(`sign.prePackageFiles entry is a directory but not a .app bundle with Contents/Info.plist: ${file}`);
    }
    nested.push(...discover(file, mainName(file)));
  }
  return { nested: [...new Set(nested)], files };
}
